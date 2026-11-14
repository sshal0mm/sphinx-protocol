//! Typed NEAR access for Sphinx services (relayer, job API, MCP server).
//!
//! - [`Escrow`]: read the escrow contract and call it with a signer.
//! - [`views`]: the JSON shapes the contract returns.
//! - [`outlayer`]: run the published grader inside OutLayer's enclave.
//!
//! Every failure is a catalog [`SphinxError`]: contract panics are decoded
//! from the `SPHINX_E_*` text; transport problems become
//! `SPHINX_E_RELAY_RPC_FAILED`.

pub mod enclave;
pub mod hidden_suite;
pub mod intents;
pub mod nep413;
pub mod outlayer;
pub mod relay;
pub mod uid;
pub mod views;

use near_api::types::{AccountId, NearGas, NearToken};
use near_api::{Contract, NetworkConfig, Signer};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use sphinx_protocol::error::parse_panic;
use sphinx_protocol::{Code, SphinxError};
use std::sync::Arc;

pub use near_api;

/// Which NEAR network and escrow contract to talk to. With several RPC keys
/// it holds one network per key and takes them in turn, so the load (and a
/// provider's usage limit) is spread across the keys.
#[derive(Clone, Debug)]
pub struct Escrow {
    networks: Arc<Vec<NetworkConfig>>,
    next: Arc<std::sync::atomic::AtomicUsize>,
    contract: AccountId,
}

/// The `FastNEAR` RPC keys from the environment: `SPHINX_RPC_KEY` and the
/// comma-separated `SPHINX_RPC_KEYS`, in that order, without blanks or
/// repeats. Keys are only ever sent to `FastNEAR` hosts.
pub fn rpc_keys_from_env() -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let one = std::env::var("SPHINX_RPC_KEY").unwrap_or_default();
    let many = std::env::var("SPHINX_RPC_KEYS").unwrap_or_default();
    for k in std::iter::once(one.as_str()).chain(many.split(',')) {
        let k = k.trim();
        if !k.is_empty() && !keys.iter().any(|x| x == k) {
            keys.push(k.to_owned());
        }
    }
    keys
}

/// Tells the relayer something on chain may need it now (a submission
/// relayed, code stored for one, a job posted), so it reads the chain at once
/// instead of on a timer. Fire and forget: `SPHINX_RELAYER_NUDGE` (the
/// relayer's internal `/nudge` URL) unset means no nudge, and a relayer that
/// does not answer within two seconds is skipped; it still looks on its own,
/// only later. Needs a running Tokio runtime.
pub fn nudge_relayer() {
    static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Some(url) = std::env::var("SPHINX_RELAYER_NUDGE").ok().filter(|u| !u.is_empty()) else {
        return;
    };
    // At most one nudge every five seconds from this process, so requests
    // anyone can send (such as storing code) cannot keep the relayer
    // reading the chain flat out.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let last = LAST.load(std::sync::atomic::Ordering::Relaxed);
    if now < last.saturating_add(5)
        || LAST
            .compare_exchange(
                last,
                now,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_err()
    {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await;
    });
}

/// Hands out RPC keys in turn.
#[derive(Debug, Default)]
pub struct RpcKeys {
    keys: Vec<String>,
    next: std::sync::atomic::AtomicUsize,
}

impl RpcKeys {
    pub fn new(keys: Vec<String>) -> Self {
        Self { keys, next: std::sync::atomic::AtomicUsize::new(0) }
    }

    /// From [`rpc_keys_from_env`].
    pub fn from_env() -> Self {
        Self::new(rpc_keys_from_env())
    }

    /// The next key, if there are any.
    pub fn next(&self) -> Option<&str> {
        if self.keys.is_empty() {
            return None;
        }
        let i = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.keys.len();
        Some(&self.keys[i])
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// The result of a successful signed call.
#[derive(Clone, Debug, PartialEq)]
pub struct CallOutcome {
    pub tx_hash: String,
    /// The method's JSON return value, if it returned one.
    pub value: Option<Value>,
}

/// Public testnet RPC endpoints, in the order they are tried.
pub const TESTNET_RPC: &[&str] = &[
    "https://rpc.testnet.fastnear.com",
    "https://test.rpc.fastnear.com",
    "https://rpc.testnet.near.org",
];

/// Public NEAR mainnet RPC endpoints, tried in order.
pub const MAINNET_RPC: &[&str] =
    &["https://rpc.mainnet.fastnear.com", "https://free.rpc.fastnear.com", "https://near.drpc.org"];

/// The gas to attach to an escrow call. NEAR makes the sender hold all of it
/// at a pessimistic price before sending (on mainnet about 0.001 NEAR per
/// Tgas), so each call gets a cap a few times what it burns on testnet, not
/// more. Calls that pay out reserve 5 Tgas per transfer for its callback, and
/// one call makes at most five (a reward and four bonds).
#[must_use]
pub fn call_gas(method: &str) -> NearGas {
    let tgas = match method {
        // Each burns under 4 Tgas.
        "submit"
        | "submit_from_balance"
        | "deposit_balance"
        | "post_job"
        | "commit_suite"
        | "register_agent"
        | "unregister_agent"
        | "record_suite_check"
        | "set_grader_key" => 20,
        // NEAR's DCAP verifier burns about 176 Tgas; the rest is the call
        // and its callback.
        "confirm_run" => 250,
        "sync_measurements" => 80,
        // One transfer and its callback.
        "reclaim_bond" | "withdraw" | "withdraw_balance" => 30,
        // Up to five transfers and their callbacks: a paid score, settling,
        // cancelling, refunding.
        _ => 50,
    };
    NearGas::from_tgas(tgas)
}

/// Whether `account` lives on NEAR mainnet: its top-level name is `near`
/// (`escrow.sphinx.near`). Everything else, `.testnet` included, is testnet.
#[must_use]
pub fn is_mainnet(account: &str) -> bool {
    account.rsplit('.').next() == Some("near")
}

/// Whether `url` is a `FastNEAR` endpoint, the only host an RPC key is sent to.
#[must_use]
pub fn is_fastnear(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix("https://") else { return false };
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    host == "fastnear.com" || host.ends_with(".fastnear.com")
}

/// Testnet with `urls` (comma-separated) as its RPC endpoints, or
/// [`TESTNET_RPC`] when none parse. With `key`, `FastNEAR` endpoints are
/// called with it as a bearer header (never in the URL), lifting the
/// public rate limit; other hosts never see it.
pub fn testnet_network(urls: Option<&str>, key: Option<&str>) -> NetworkConfig {
    network_with(false, urls, key)
}

/// Mainnet with `urls` as its RPC endpoints, or [`MAINNET_RPC`]; the key is
/// handled as in [`testnet_network`].
pub fn mainnet_network(urls: Option<&str>, key: Option<&str>) -> NetworkConfig {
    network_with(true, urls, key)
}

fn network_with(mainnet: bool, urls: Option<&str>, key: Option<&str>) -> NetworkConfig {
    let key = key.map(str::trim).filter(|k| !k.is_empty());
    let endpoint = |u: &str| -> Option<near_api::RPCEndpoint> {
        let e = near_api::RPCEndpoint::new(u.trim().parse().ok()?);
        Some(match key {
            Some(k) if is_fastnear(u) => e.with_api_key(k.to_owned()),
            _ => e,
        })
    };
    let given: Vec<near_api::RPCEndpoint> =
        urls.unwrap_or_default().split(',').filter_map(endpoint).collect();
    let defaults = if mainnet { MAINNET_RPC } else { TESTNET_RPC };
    let endpoints = if given.is_empty() {
        defaults.iter().copied().filter_map(endpoint).collect()
    } else {
        given
    };
    let base = if mainnet { NetworkConfig::mainnet() } else { NetworkConfig::testnet() };
    NetworkConfig { rpc_endpoints: endpoints, ..base }
}

impl Escrow {
    pub fn new(network: NetworkConfig, contract: AccountId) -> Self {
        Self::rotating(vec![network], contract)
    }

    /// One network per RPC key, used in turn.
    ///
    /// # Panics
    /// With no networks.
    pub fn rotating(networks: Vec<NetworkConfig>, contract: AccountId) -> Self {
        assert!(!networks.is_empty(), "an escrow needs a network");
        Self { networks: Arc::new(networks), next: Arc::default(), contract }
    }

    /// The networks for `mainnet`, one per key from the environment (or one
    /// without a key).
    fn from_env(mainnet: bool) -> Vec<NetworkConfig> {
        let urls = std::env::var("SPHINX_RPC_URLS").ok();
        let keys = rpc_keys_from_env();
        let build = |k: Option<&str>| {
            if mainnet {
                mainnet_network(urls.as_deref(), k)
            } else {
                testnet_network(urls.as_deref(), k)
            }
        };
        if keys.is_empty() {
            vec![build(None)]
        } else {
            keys.iter().map(|k| build(Some(k))).collect()
        }
    }

    /// The public NEAR testnet. RPC endpoints are tried in order, moving to
    /// the next when one fails or rate-limits: `SPHINX_RPC_URLS`
    /// (comma-separated) when set, otherwise [`TESTNET_RPC`].
    /// `SPHINX_RPC_KEY`, when set, is sent to `FastNEAR` endpoints.
    pub fn testnet(contract: AccountId) -> Self {
        Self::rotating(Self::from_env(false), contract)
    }

    /// The network the contract's own account name says it is on: mainnet
    /// for a `.near` account, testnet otherwise. RPC settings as in
    /// [`Escrow::testnet`].
    pub fn for_contract(contract: AccountId) -> Self {
        let mainnet = is_mainnet(contract.as_str());
        Self::rotating(Self::from_env(mainnet), contract)
    }

    /// Whether this escrow is on mainnet.
    pub fn is_mainnet(&self) -> bool {
        is_mainnet(self.contract.as_str())
    }

    pub fn contract_id(&self) -> &AccountId {
        &self.contract
    }

    /// The network to use for the next request: with several RPC keys,
    /// each call takes the next one.
    pub fn network(&self) -> &NetworkConfig {
        let i = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.networks.len();
        &self.networks[i]
    }

    /// Calls a view method and decodes its JSON result.
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` when the RPC fails or the result does not
    /// decode into `T`.
    pub async fn view<T: DeserializeOwned + Send + Sync>(
        &self,
        method: &str,
        args: impl Serialize,
    ) -> Result<T, SphinxError> {
        Contract(self.contract.clone())
            .call_function(method, args)
            .read_only::<T>()
            .fetch_from(self.network())
            .await
            .map(|data| data.data)
            .map_err(|e| decode_failure(method, &e.to_string()))
    }

    /// Calls a change method as `signer_id`, waiting for the final outcome.
    ///
    /// # Errors
    /// The contract's own `SPHINX_E_*` error when the call panics, otherwise
    /// `SPHINX_E_RELAY_RPC_FAILED`.
    pub async fn call(
        &self,
        signer_id: AccountId,
        signer: Arc<Signer>,
        method: &str,
        args: impl Serialize,
        deposit: NearToken,
        gas: NearGas,
    ) -> Result<CallOutcome, SphinxError> {
        let result = Contract(self.contract.clone())
            .call_function(method, args)
            .transaction()
            .deposit(deposit)
            .gas(gas)
            .with_signer(signer_id, signer)
            .send_to(self.network())
            .await
            .map_err(|e| decode_failure(method, &e.to_string()))?;
        outcome_value(method, result)
    }
}

/// Turns a send result into a [`CallOutcome`] or the contract's error.
pub(crate) fn outcome_value(
    method: &str,
    result: near_api::types::transaction::result::TransactionResult,
) -> Result<CallOutcome, SphinxError> {
    let full = result.into_full().ok_or_else(|| {
        SphinxError::new(Code::RelayRpcFailed, format!("{method} did not reach a final outcome"))
            .ctx("method", method)
    })?;
    let tx_hash =
        full.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
    match full.into_result() {
        Ok(success) => Ok(CallOutcome { tx_hash, value: success.json::<Value>().ok() }),
        Err(failure) => {
            Err(decode_failure(method, &format!("{failure:?}")).ctx("tx_hash", tx_hash))
        }
    }
}

/// yoctoNEAR as a decimal NEAR string without trailing zeros, e.g. "1.25".
pub fn format_near(yocto: u128) -> String {
    const ONE: u128 = 1_000_000_000_000_000_000_000_000;
    let frac = format!("{:024}", yocto % ONE);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        (yocto / ONE).to_string()
    } else {
        format!("{}.{frac}", yocto / ONE)
    }
}

/// Parses a NEAR amount written as a decimal string (`"0.3"`, `"12"`) into
/// yoctoNEAR, exactly, without floating point.
///
/// # Errors
/// `SPHINX_E_CLI_AMOUNT_INVALID` for anything else, more than 24 decimal
/// places, or an amount that does not fit.
pub fn parse_near(text: &str) -> Result<u128, SphinxError> {
    let invalid = || {
        SphinxError::new(Code::CliAmountInvalid, format!("{text:?} is not a NEAR amount"))
            .ctx("amount", text)
    };
    let t = text.trim();
    let (whole, frac) = t.split_once('.').unwrap_or((t, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && frac.is_empty()) || !digits(whole) || !digits(frac) || frac.len() > 24 {
        return Err(invalid());
    }
    let whole: u128 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| invalid())? };
    let frac: u128 =
        if frac.is_empty() { 0 } else { format!("{frac:0<24}").parse().map_err(|_| invalid())? };
    whole.checked_mul(10u128.pow(24)).and_then(|w| w.checked_add(frac)).ok_or_else(invalid)
}

/// Maps failure text to the contract's catalog error when one is present.
pub fn decode_failure(method: &str, text: &str) -> SphinxError {
    match parse_panic(text).and_then(|r| r.code.parse::<Code>().ok().map(|c| (c, r))) {
        Some((code, report)) => {
            let mut e = SphinxError::new(code, report.message).retry(report.retry);
            for (k, v) in report.ctx {
                e = e.ctx(&k, v);
            }
            e
        }
        None => rpc_failed(method, &text),
    }
}

fn rpc_failed(method: &str, e: &dyn std::fmt::Display) -> SphinxError {
    let detail: String = e.to_string().chars().take(300).collect();
    SphinxError::new(Code::RelayRpcFailed, format!("{method} failed: {detail}"))
        .ctx("method", method)
}

#[cfg(test)]
mod amount_tests {
    use super::parse_near;

    #[test]
    fn decimal_near_amounts_convert_exactly() {
        let one = 10u128.pow(24);
        assert_eq!(parse_near("1").unwrap(), one);
        assert_eq!(parse_near("0.3").unwrap(), 3 * one / 10);
        assert_eq!(parse_near(".01").unwrap(), one / 100);
        assert_eq!(parse_near(" 2.5 ").unwrap(), 5 * one / 2);
        assert_eq!(parse_near("0.000000000000000000000001").unwrap(), 1);
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            "",
            ".",
            "-1",
            "1e3",
            "0.3 NEAR",
            "1.2.3",
            "0.0000000000000000000000001",
            "999999999999999999999",
        ] {
            assert_eq!(
                parse_near(bad).unwrap_err().code().as_str(),
                "SPHINX_E_CLI_AMOUNT_INVALID",
                "{bad:?}"
            );
        }
    }
}

#[cfg(test)]
mod network_choice {
    use super::*;

    #[test]
    fn rpc_keys_take_turns() {
        let keys = RpcKeys::new(vec!["a".into(), "b".into(), "c".into()]);
        let used: Vec<&str> = (0..6).filter_map(|_| keys.next()).collect();
        assert_eq!(used, ["a", "b", "c", "a", "b", "c"]);
        assert_eq!(RpcKeys::new(vec![]).next(), None);
    }

    #[test]
    fn an_escrow_uses_its_networks_in_turn() {
        let contract: AccountId = "escrow.testnet".parse().unwrap();
        let nets: Vec<NetworkConfig> =
            ["k1", "k2"].iter().map(|k| testnet_network(None, Some(k))).collect();
        let e = Escrow::rotating(nets, contract);
        let key = |n: &NetworkConfig| n.rpc_endpoints[0].bearer_header.clone();
        let first = key(e.network());
        let second = key(e.network());
        let third = key(e.network());
        assert_ne!(first, second);
        assert_eq!(first, third);
    }

    #[test]
    fn the_contract_name_picks_the_network() {
        assert!(is_mainnet("escrow.sphinx.near") && is_mainnet("near"));
        for testnet in ["escrow.sphinx999.testnet", "near.testnet", "a.nearx", ""] {
            assert!(!is_mainnet(testnet), "{testnet}");
        }
        let main =
            Escrow::for_contract("escrow.sphinx.near".parse().unwrap_or_else(|_| unreachable!()));
        assert_eq!(main.network().network_name, "mainnet");
        assert!(main.is_mainnet());
        let test = Escrow::for_contract(
            "escrow.sphinx999.testnet".parse().unwrap_or_else(|_| unreachable!()),
        );
        assert_eq!(test.network().network_name, "testnet");
        assert_eq!(
            hidden_suite::Keystore::for_account("escrow.sphinx.near").api,
            "https://api.outlayer.ai"
        );
        assert_eq!(
            hidden_suite::Keystore::for_account("x.testnet").contract.as_str(),
            "outlayer.testnet"
        );
    }
}

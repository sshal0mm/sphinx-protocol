//! Typed NEAR access for Sphinx services (relayer, job API, MCP server).
//!
//! - [`Escrow`]: read the escrow contract and call it with a signer.
//! - [`views`]: the JSON shapes the contract returns.
//! - [`outlayer`]: run the published grader inside OutLayer's enclave.
//!
//! Every failure is a catalog [`SphinxError`]: contract panics are decoded
//! from the `SPHINX_E_*` text; transport problems become
//! `SPHINX_E_RELAY_RPC_FAILED`.

pub mod hidden_suite;
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

/// Which NEAR network and escrow contract to talk to.
#[derive(Clone, Debug)]
pub struct Escrow {
    network: NetworkConfig,
    contract: AccountId,
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
pub const MAINNET_RPC: &[&str] = &[
    "https://rpc.mainnet.fastnear.com",
    "https://free.rpc.fastnear.com",
    "https://rpc.mainnet.near.org",
];

/// The gas to attach to an escrow call. NEAR makes the sender hold all of it
/// at a pessimistic price before sending (on mainnet about 0.001 NEAR per
/// Tgas), so each call gets a cap a few times what it burns on testnet, not
/// more. Calls that pay out reserve 5 Tgas per transfer for its callback, and
/// one call makes at most five (a reward and four bonds).
#[must_use]
pub fn call_gas(method: &str) -> NearGas {
    let tgas = match method {
        // Each burns under 4 Tgas.
        "submit" | "post_job" | "commit_suite" | "register_agent" | "unregister_agent"
        | "record_suite_check" | "set_grader_key" => 20,
        // One transfer and its callback.
        "reclaim_bond" | "withdraw" => 30,
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
        Self { network, contract }
    }

    /// The public NEAR testnet. RPC endpoints are tried in order, moving to
    /// the next when one fails or rate-limits: `SPHINX_RPC_URLS`
    /// (comma-separated) when set, otherwise [`TESTNET_RPC`].
    /// `SPHINX_RPC_KEY`, when set, is sent to `FastNEAR` endpoints.
    pub fn testnet(contract: AccountId) -> Self {
        let urls = std::env::var("SPHINX_RPC_URLS").ok();
        let key = std::env::var("SPHINX_RPC_KEY").ok();
        Self::new(testnet_network(urls.as_deref(), key.as_deref()), contract)
    }

    /// The network the contract's own account name says it is on: mainnet
    /// for a `.near` account, testnet otherwise. RPC settings as in
    /// [`Escrow::testnet`].
    pub fn for_contract(contract: AccountId) -> Self {
        let urls = std::env::var("SPHINX_RPC_URLS").ok();
        let key = std::env::var("SPHINX_RPC_KEY").ok();
        let network = if is_mainnet(contract.as_str()) {
            mainnet_network(urls.as_deref(), key.as_deref())
        } else {
            testnet_network(urls.as_deref(), key.as_deref())
        };
        Self::new(network, contract)
    }

    /// Whether this escrow is on mainnet.
    pub fn is_mainnet(&self) -> bool {
        is_mainnet(self.contract.as_str())
    }

    pub fn contract_id(&self) -> &AccountId {
        &self.contract
    }

    pub fn network(&self) -> &NetworkConfig {
        &self.network
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
            .fetch_from(&self.network)
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
            .send_to(&self.network)
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

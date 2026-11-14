//! NEAR Intents through the 1Click API, shared by the house (the site) and
//! the MCP server: the chains and tokens Sphinx offers, address checks, the
//! 1Click calls, and the one `wrap.near` transaction that cashes NEAR out.
//!
//! The escrow pays in NEAR; Intents sits at its edges. Sphinx never holds the
//! money: deposit addresses are 1Click's, recipients and refund accounts are
//! the user's own. NEAR Intents has no testnet.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use near_api::types::transaction::actions::{Action, FunctionCallAction};
use near_api::types::{AccountId, NearGas, NearToken};
use near_api::{NetworkConfig, Signer, Transaction};
use serde_json::{json, Value};

/// The NEAR token 1Click swaps: wNEAR, which unwraps 1:1 to NEAR.
pub const WNEAR: &str = "nep141:wrap.near";
/// The wNEAR contract on mainnet.
pub const WRAP: &str = "wrap.near";
/// What `wrap.near` charges to register an account (NEP-145).
pub const WRAP_STORAGE_YOCTO: u128 = 1_250_000_000_000_000_000_000;
/// 1Click's public endpoint.
pub const ONE_CLICK: &str = "https://1click.chaindefuser.com";
const YOCTO: u128 = 1_000_000_000_000_000_000_000_000;
/// Smallest and largest amount one swap may move, in yoctoNEAR.
pub const MIN_YOCTO: u128 = YOCTO / 10;
pub const MAX_YOCTO: u128 = 200 * YOCTO;

/// A chain Sphinx offers, as 1Click names it, and how its addresses look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chain {
    /// Our id (`base`), used in URLs and tool arguments.
    pub id: &'static str,
    /// 1Click's `blockchain` value.
    pub one_click: &'static str,
    pub name: &'static str,
    pub kind: AddressKind,
}

/// The shape of a recipient address on a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressKind {
    Evm,
    Solana,
    Near,
}

/// Chains with no 1Click minimum (BNB Chain and Tron have $1,000 and $100
/// minimums, too large for a job).
pub const CHAINS: &[Chain] = &[
    Chain { id: "base", one_click: "base", name: "Base", kind: AddressKind::Evm },
    Chain { id: "arbitrum", one_click: "arb", name: "Arbitrum", kind: AddressKind::Evm },
    Chain { id: "ethereum", one_click: "eth", name: "Ethereum", kind: AddressKind::Evm },
    Chain { id: "optimism", one_click: "op", name: "Optimism", kind: AddressKind::Evm },
    Chain { id: "polygon", one_click: "pol", name: "Polygon", kind: AddressKind::Evm },
    Chain { id: "avalanche", one_click: "avax", name: "Avalanche", kind: AddressKind::Evm },
    Chain { id: "solana", one_click: "sol", name: "Solana", kind: AddressKind::Solana },
    Chain { id: "near", one_click: "near", name: "NEAR", kind: AddressKind::Near },
];

/// Tokens offered on every chain.
pub const TOKENS: &[&str] = &["USDC", "USDT"];

/// The chain with our id `id`.
///
/// # Errors
/// The ids on offer, as a sentence.
pub fn chain(id: &str) -> Result<&'static Chain, String> {
    CHAINS.iter().find(|c| c.id == id).ok_or_else(|| {
        let names: Vec<&str> = CHAINS.iter().map(|c| c.id).collect();
        format!("choose a chain: {}", names.join(", "))
    })
}

/// Whether `address` looks like an address on a chain of `kind`. 1Click
/// checks it again; this only catches a paste into the wrong box.
pub fn address_ok(kind: AddressKind, address: &str) -> bool {
    match kind {
        AddressKind::Evm => {
            address.len() == 42
                && address.starts_with("0x")
                && address[2..].bytes().all(|b| b.is_ascii_hexdigit())
        }
        AddressKind::Solana => {
            (32..=44).contains(&address.len())
                && bs58::decode(address).into_vec().is_ok_and(|b| b.len() == 32)
        }
        AddressKind::Near => mainnet_account(address),
    }
}

/// A NEAR mainnet account: a named `.near` account or a 64-hex implicit one.
pub fn mainnet_account(account: &str) -> bool {
    let implicit = account.len() == 64 && account.bytes().all(|b| b.is_ascii_hexdigit());
    implicit || (account.rsplit('.').next() == Some("near") && account.parse::<AccountId>().is_ok())
}

/// An amount of NEAR within the swap limits.
///
/// # Errors
/// Not a NEAR amount, or outside 0.1 to 200 NEAR.
pub fn swap_amount(near: &str) -> Result<u128, String> {
    let yocto = crate::parse_near(near.trim()).map_err(|e| e.message().to_owned())?;
    if (MIN_YOCTO..=MAX_YOCTO).contains(&yocto) {
        Ok(yocto)
    } else {
        Err("swap between 0.1 and 200 NEAR".to_owned())
    }
}

/// `units` of a token with `decimals`, as a decimal string.
pub fn format_units(units: Option<&str>, decimals: u32) -> Option<String> {
    let n: u128 = units?.parse().ok()?;
    let scale = 10u128.checked_pow(decimals)?;
    let frac = format!("{:0width$}", n % scale, width = decimals as usize);
    let frac = frac.trim_end_matches('0');
    Some(if frac.is_empty() { (n / scale).to_string() } else { format!("{}.{frac}", n / scale) })
}

/// Why a 1Click call did not give what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// 1Click refused the request (a bad amount, address or route); the
    /// message is 1Click's own.
    Refused(String),
    /// 1Click could not be reached or answered something unexpected.
    Unreachable(String),
    /// 1Click does not know this deposit address.
    Unknown,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) => write!(f, "NEAR Intents: {m}"),
            Self::Unreachable(m) => write!(f, "NEAR Intents could not be reached: {m}"),
            Self::Unknown => f.write_str("NEAR Intents does not know this deposit address"),
        }
    }
}

/// 1Click's token list and when it was fetched.
type TokenCache = Arc<Mutex<Option<(Vec<Value>, Instant)>>>;

/// A 1Click client with a ten-minute token-list cache.
#[derive(Debug, Clone)]
pub struct OneClick {
    base: String,
    /// A 1Click partner key, if any: without one 1Click adds 0.25% to every
    /// swap.
    key: Option<String>,
    http: reqwest::Client,
    tokens: TokenCache,
}

impl OneClick {
    pub fn new(base: &str, key: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            key: key.filter(|k| !k.is_empty()),
            http: reqwest::Client::new(),
            tokens: Arc::default(),
        }
    }

    /// Whether requests carry a partner key.
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    fn request(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let req = req.header("user-agent", "sphinx").timeout(Duration::from_secs(20));
        match &self.key {
            Some(k) => req.header("x-api-key", k),
            None => req,
        }
    }

    async fn json(&self, req: reqwest::RequestBuilder) -> Result<(u16, Value), Failure> {
        let resp =
            self.request(req).send().await.map_err(|e| Failure::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        let v = resp.json().await.map_err(|e| Failure::Unreachable(e.to_string()))?;
        Ok((status, v))
    }

    /// 1Click's token list.
    ///
    /// # Errors
    /// 1Click could not be reached.
    pub async fn tokens(&self) -> Result<Vec<Value>, Failure> {
        if let Ok(t) = self.tokens.lock() {
            if let Some((list, at)) = t.as_ref() {
                if at.elapsed() < Duration::from_secs(600) {
                    return Ok(list.clone());
                }
            }
        }
        let (_, v) = self.json(self.http.get(format!("{}/v0/tokens", self.base))).await?;
        let list =
            v.as_array().cloned().ok_or_else(|| Failure::Unreachable("no token list".into()))?;
        if let Ok(mut t) = self.tokens.lock() {
            *t = Some((list.clone(), Instant::now()));
        }
        Ok(list)
    }

    /// The 1Click asset id and decimals of `token` on `chain`.
    ///
    /// # Errors
    /// The token is not one Sphinx offers or not on that chain now.
    pub async fn asset(&self, chain: &Chain, token: &str) -> Result<(String, u32), Failure> {
        if !TOKENS.contains(&token) {
            return Err(Failure::Refused(format!("choose a token: {}", TOKENS.join(", "))));
        }
        self.tokens()
            .await?
            .iter()
            .find(|t| t["blockchain"] == chain.one_click && t["symbol"] == token)
            .and_then(|t| {
                Some((
                    t["assetId"].as_str()?.to_owned(),
                    u32::try_from(t["decimals"].as_u64()?).ok()?,
                ))
            })
            .ok_or_else(|| {
                Failure::Refused(format!("{token} on {} is not offered right now", chain.name))
            })
    }

    /// NEAR's price in dollars.
    ///
    /// # Errors
    /// 1Click could not be reached.
    pub async fn near_usd(&self) -> Result<Option<f64>, Failure> {
        Ok(self
            .tokens()
            .await?
            .iter()
            .find(|t| t["assetId"] == WNEAR)
            .and_then(|t| t["price"].as_f64()))
    }

    /// A quote. A live one (`dry: false`) carries a deposit address; nothing
    /// moves until someone deposits. A dry one is a price only.
    ///
    /// # Errors
    /// 1Click refused, or a live quote gave no deposit address.
    pub async fn quote(&self, request: &Value) -> Result<Value, Failure> {
        let (_, reply) =
            self.json(self.http.post(format!("{}/v0/quote", self.base)).json(request)).await?;
        let dry = request["dry"].as_bool().unwrap_or(false);
        let missing = if dry {
            reply["quote"]["amountOut"].as_str().is_none()
        } else {
            reply["quote"]["depositAddress"].as_str().is_none_or(str::is_empty)
        };
        if missing {
            let why = reply["message"].as_str().unwrap_or("no quote was given");
            return Err(Failure::Refused(why.to_owned()));
        }
        Ok(reply)
    }

    /// The swap's progress.
    ///
    /// # Errors
    /// Unknown address, or 1Click could not be reached.
    pub async fn status(&self, deposit: &str) -> Result<Value, Failure> {
        let req =
            self.http.get(format!("{}/v0/status", self.base)).query(&[("depositAddress", deposit)]);
        let (code, v) = self.json(req).await?;
        if code == 404 {
            return Err(Failure::Unknown);
        }
        Ok(v)
    }

    /// Tells 1Click a deposit was sent, so it is picked up sooner.
    ///
    /// # Errors
    /// 1Click refused it.
    pub async fn submit(
        &self,
        deposit: &str,
        tx_hash: &str,
        near_sender: Option<&str>,
    ) -> Result<Value, Failure> {
        let mut body = json!({ "depositAddress": deposit, "txHash": tx_hash });
        if let Some(sender) = near_sender {
            body["nearSenderAccount"] = json!(sender);
        }
        let (code, v) = self
            .json(self.http.post(format!("{}/v0/deposit/submit", self.base)).json(&body))
            .await?;
        if (200..300).contains(&code) {
            Ok(v)
        } else {
            Err(Failure::Refused(
                v["message"].as_str().unwrap_or("the deposit was not accepted").to_owned(),
            ))
        }
    }
}

/// The quote request that turns `near` from `account` into `asset` for
/// `recipient`; a failed swap refunds to `account` on NEAR. A `dry` quote
/// is a price only, with no deposit address.
pub fn withdraw_request(
    dry: bool,
    account: &str,
    near: u128,
    asset: &str,
    recipient: &str,
    deadline: &str,
    app_fees: &Value,
) -> Value {
    json!({
        "dry": dry, "swapType": "EXACT_INPUT", "slippageTolerance": 100,
        "originAsset": WNEAR, "depositType": "ORIGIN_CHAIN",
        "destinationAsset": asset, "amount": near.to_string(),
        "recipient": recipient, "recipientType": "DESTINATION_CHAIN",
        "refundTo": account, "refundType": "ORIGIN_CHAIN",
        "deadline": deadline, "appFees": app_fees,
    })
}

/// The quote request that delivers exactly `near` to `account` for `asset`
/// sent on its chain; a failed or short deposit refunds to `account` inside
/// NEAR Intents.
pub fn fund_request(
    account: &str,
    near: u128,
    asset: &str,
    deadline: &str,
    app_fees: &Value,
) -> Value {
    json!({
        "dry": false, "swapType": "EXACT_OUTPUT", "slippageTolerance": 100,
        "originAsset": asset, "depositType": "ORIGIN_CHAIN",
        "destinationAsset": WNEAR, "amount": near.to_string(),
        "recipient": account, "recipientType": "DESTINATION_CHAIN",
        "refundTo": account, "refundType": "INTENTS",
        "deadline": deadline, "appFees": app_fees,
    })
}

const TGAS: u64 = 1_000_000_000_000;

/// The three calls of a withdrawal, as (method, args, gas, deposit): register
/// the deposit address on `wrap.near`, wrap exactly `near`, and send exactly
/// that much wNEAR to the deposit address.
pub fn withdraw_calls(deposit: &str, near: u128) -> [(&'static str, Value, u64, u128); 3] {
    [
        (
            "storage_deposit",
            json!({ "account_id": deposit, "registration_only": true }),
            10 * TGAS,
            WRAP_STORAGE_YOCTO,
        ),
        ("near_deposit", json!({}), 10 * TGAS, near),
        (
            "ft_transfer",
            json!({ "receiver_id": deposit, "amount": near.to_string(), "memo": "sphinx withdraw" }),
            30 * TGAS,
            1,
        ),
    ]
}

/// The same calls in the shape a browser wallet signs.
pub fn withdraw_wallet_transactions(deposit: &str, near: u128) -> Value {
    let actions: Vec<Value> = withdraw_calls(deposit, near)
        .into_iter()
        .map(|(method, args, gas, deposit)| {
            json!({ "type": "FunctionCall", "params": {
                "methodName": method, "args": args,
                "gas": gas.to_string(), "deposit": deposit.to_string() } })
        })
        .collect();
    json!([{ "receiverId": WRAP, "actions": actions }])
}

/// Signs and sends the withdrawal transaction from `account`, returning its
/// hash.
///
/// # Errors
/// The transaction failed or could not be sent.
pub async fn send_withdrawal(
    network: &NetworkConfig,
    account: AccountId,
    signer: Arc<Signer>,
    deposit: &str,
    near: u128,
) -> Result<String, String> {
    let wrap: AccountId = WRAP.parse().map_err(|_| "wrap.near is not an account id".to_owned())?;
    let mut tx = Transaction::construct(account, wrap);
    for (method, args, gas, attached) in withdraw_calls(deposit, near) {
        tx = tx.add_action(Action::FunctionCall(Box::new(FunctionCallAction {
            method_name: method.into(),
            args: args.to_string().into_bytes(),
            gas: NearGas::from_gas(gas),
            deposit: NearToken::from_yoctonear(attached),
        })));
    }
    let result = tx.with_signer(signer).send_to(network).await.map_err(|e| e.to_string())?;
    let full = result.into_full().ok_or("the withdrawal did not reach a final outcome")?;
    let hash = full.outcomes().first().map(|o| o.transaction_hash.to_string()).unwrap_or_default();
    full.into_result().map_err(|f| format!("the withdrawal failed ({hash}): {f:?}"))?;
    Ok(hash)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    #[test]
    fn addresses_are_checked_per_chain() {
        let evm = "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed";
        assert!(address_ok(AddressKind::Evm, evm));
        assert!(!address_ok(AddressKind::Evm, "0x123"));
        assert!(!address_ok(AddressKind::Evm, "alice.near"));
        assert!(address_ok(AddressKind::Solana, "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM"));
        assert!(!address_ok(AddressKind::Solana, evm));
        assert!(address_ok(AddressKind::Near, "alice.near"));
        assert!(address_ok(AddressKind::Near, &"ab".repeat(32)));
        assert!(!address_ok(AddressKind::Near, "alice.testnet"));
    }

    #[test]
    fn amounts_stay_within_limits() {
        assert!(swap_amount("0.05").is_err());
        assert_eq!(swap_amount("0.1").unwrap(), YOCTO / 10);
        assert_eq!(swap_amount("200").unwrap(), 200 * YOCTO);
        assert!(swap_amount("200.1").is_err());
        assert!(swap_amount("lots").is_err());
    }

    #[test]
    fn a_withdraw_sends_exactly_the_amount_to_the_deposit_address() {
        let dep = "d6".repeat(32);
        let txs = withdraw_wallet_transactions(&dep, YOCTO / 2);
        let tx = &txs[0];
        assert_eq!(tx["receiverId"], "wrap.near");
        let a = tx["actions"].as_array().unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[0]["params"]["args"]["account_id"], dep);
        assert_eq!(a[1]["params"]["deposit"], (YOCTO / 2).to_string());
        assert_eq!(a[2]["params"]["args"]["receiver_id"], dep);
        assert_eq!(a[2]["params"]["args"]["amount"], (YOCTO / 2).to_string());
        assert_eq!(a[2]["params"]["deposit"], "1");
    }

    #[test]
    fn token_units_format_without_trailing_zeros() {
        assert_eq!(format_units(Some("1691096"), 6).as_deref(), Some("1.691096"));
        assert_eq!(format_units(Some("2000000"), 6).as_deref(), Some("2"));
        assert_eq!(format_units(None, 6), None);
    }

    #[test]
    fn refunds_never_go_to_anyone_but_the_user() {
        let fees = json!([{ "recipient": "fee.near", "fee": 50 }]);
        let f = fund_request("alice.near", YOCTO, "nep141:x", "d", &fees);
        assert_eq!(
            (f["recipient"].as_str(), f["refundTo"].as_str()),
            (Some("alice.near"), Some("alice.near"))
        );
        let c = withdraw_request(false, "alice.near", YOCTO, "nep141:x", "0xabc", "d", &fees);
        assert_eq!(c["refundTo"], "alice.near");
        assert_eq!(c["originAsset"], WNEAR);
    }
}

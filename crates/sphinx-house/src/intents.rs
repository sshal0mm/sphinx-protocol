//! Money in and out across chains through NEAR Intents (the 1Click API),
//! for the site. The chain table, address checks and 1Click client are
//! shared with the MCP server in [`sphinx_near::intents`].
//!
//! The escrow pays in NEAR and its mainnet code can never change, so Intents
//! sits on both edges of it:
//!
//! - **Fund**: a buyer with USDC or USDT on another chain gets an exact
//!   amount of NEAR delivered to their own NEAR account, then posts the job
//!   from that account as usual. `POST /newcomer/intents/fund`
//!   `{account, near, chain, token}` answers a live 1Click deposit address
//!   and the amount to send. Refunds go back to the buyer's own account
//!   inside NEAR Intents, so nothing is asked for on the other chain.
//! - **Withdraw**: an agent (or a refunded buyer) turns NEAR from their own
//!   wallet into USDC or USDT on another chain. `POST
//!   /newcomer/intents/withdraw` `{account, near, chain, token, recipient}`
//!   answers the deposit address and the one transaction their wallet signs:
//!   wrap the NEAR, register the deposit address on `wrap.near`, and send it
//!   there. A failed swap refunds to their NEAR account.
//! - `GET /newcomer/intents/options`: the chains and tokens offered.
//! - `GET /newcomer/intents/status/{deposit_address}`: the swap's progress.
//! - `POST /newcomer/intents/submit` `{deposit_address, tx_hash}`: tells
//!   1Click a deposit was sent, so it is picked up sooner.
//! - `GET /newcomer/intents/balance/{account}`: the account's wNEAR, which
//!   the pay page unwraps first when a swap delivered wNEAR, and the NEAR it
//!   can withdraw (the withdraw panel's Max).
//!
//! Sphinx never holds the money: every deposit address is 1Click's, every
//! recipient and refund account is the user's own. NEAR Intents has no
//! testnet, so on testnet these routes refuse with a plain message.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sphinx_near::intents::{
    address_ok, chain, format_units, fund_request, mainnet_account, swap_amount, withdraw_request,
    withdraw_wallet_transactions, Failure, OneClick, CHAINS, TOKENS, WRAP, WRAP_STORAGE_YOCTO,
};

use crate::{bad, client, format_near, rfc3339, upstream, Refusal};

/// Swaps one visitor may start in an hour (each one is a fresh 1Click
/// deposit address).
const SWAPS_PER_HOUR: u32 = 20;
/// What NEAR locks per byte of account storage (protocol storage staking).
const STORAGE_YOCTO_PER_BYTE: u128 = 10_000_000_000_000_000_000;
/// NEAR left in the account for gas when withdrawing everything.
const GAS_ROOM: u128 = 50_000_000_000_000_000_000_000;
/// How long a deposit address waits for its deposit.
const DEPOSIT_WINDOW: Duration = Duration::from_hours(1);

#[allow(clippy::needless_pass_by_value, reason = "used as map_err(refused)")]
fn refused(f: Failure) -> Refusal {
    match f {
        Failure::Refused(_) => bad(f.to_string()),
        Failure::Unknown => Refusal::new(StatusCode::NOT_FOUND, "not_found", f.to_string()),
        Failure::Unreachable(_) => upstream(f.to_string()),
    }
}

/// Swaps in and out of NEAR through 1Click.
#[derive(Debug)]
pub struct Intents {
    one_click: OneClick,
    /// Sphinx's app fee on a swap, in basis points, and where it goes.
    pub fee_bps: u32,
    pub fee_account: String,
    /// NEAR Intents runs on mainnet only.
    pub mainnet: bool,
    /// NEAR RPC, and its `FastNEAR` key if any, to read wNEAR balances.
    pub rpc: String,
    pub rpc_key: Option<String>,
    http: reqwest::Client,
    starts: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Intents {
    pub fn new(
        base: &str,
        key: Option<String>,
        fee_bps: u32,
        fee_account: String,
        mainnet: bool,
        rpc: String,
        rpc_key: Option<String>,
    ) -> Self {
        Self {
            one_click: OneClick::new(base, key),
            fee_bps,
            fee_account,
            mainnet,
            rpc,
            rpc_key,
            http: reqwest::Client::new(),
            starts: Mutex::new(HashMap::new()),
        }
    }

    fn mainnet_only(&self) -> Result<(), Refusal> {
        if self.mainnet {
            Ok(())
        } else {
            Err(Refusal::new(
                StatusCode::CONFLICT,
                "mainnet_only",
                "NEAR Intents runs on mainnet only, switch the site to mainnet to pay or withdraw across chains",
            ))
        }
    }

    /// Counts a new swap for `client`; false past the hourly limit.
    fn admit(&self, client: &str) -> bool {
        let Ok(mut starts) = self.starts.lock() else { return false };
        starts.retain(|_, (at, _)| at.elapsed() < Duration::from_hours(1));
        let entry = starts.entry(client.to_owned()).or_insert((Instant::now(), 0));
        entry.1 += 1;
        entry.1 <= SWAPS_PER_HOUR
    }

    fn app_fees(&self) -> Value {
        if self.fee_bps == 0 {
            json!([])
        } else {
            json!([{ "recipient": self.fee_account, "fee": self.fee_bps }])
        }
    }

    fn deadline() -> String {
        rfc3339(SystemTime::now() + DEPOSIT_WINDOW)
    }

    /// The chains and tokens offered, and NEAR's price in dollars.
    ///
    /// # Errors
    /// 1Click could not be reached.
    pub async fn options(&self) -> Result<Value, Refusal> {
        let list = self.one_click.tokens().await.map_err(refused)?;
        let near_usd = self.one_click.near_usd().await.map_err(refused)?;
        let chains: Vec<Value> = CHAINS
            .iter()
            .map(|c| {
                let tokens: Vec<&str> = TOKENS
                    .iter()
                    .copied()
                    .filter(|tok| {
                        list.iter().any(|t| t["blockchain"] == c.one_click && t["symbol"] == *tok)
                    })
                    .collect();
                json!({ "id": c.id, "name": c.name, "tokens": tokens })
            })
            .filter(|c| c["tokens"].as_array().is_some_and(|t| !t.is_empty()))
            .collect();
        Ok(
            json!({ "mainnet": self.mainnet, "near_usd": near_usd, "chains": chains, "app_fee_bps": self.fee_bps }),
        )
    }

    /// A deposit address that delivers exactly `near` to `account` for
    /// `token` sent on `chain`.
    ///
    /// # Errors
    /// Testnet, a bad account, amount, chain or token, too many swaps, or
    /// 1Click refused.
    pub async fn fund(&self, who: &str, b: &FundBody) -> Result<Value, Refusal> {
        self.mainnet_only()?;
        if !mainnet_account(&b.account) {
            return Err(bad("connect a NEAR mainnet account, the NEAR arrives there"));
        }
        let near = swap_amount(&b.near).map_err(bad)?;
        let chain = chain(&b.chain).map_err(bad)?;
        let (origin, decimals) = self.one_click.asset(chain, &b.token).await.map_err(refused)?;
        if !self.admit(who) {
            return Err(too_many());
        }
        let reply = self
            .one_click
            .quote(&fund_request(&b.account, near, &origin, &Self::deadline(), &self.app_fees()))
            .await
            .map_err(refused)?;
        let q = &reply["quote"];
        Ok(json!({
            "direction": "fund",
            "chain": chain.id, "chain_name": chain.name, "token": b.token, "decimals": decimals,
            "deposit_address": q["depositAddress"], "deposit_memo": q["depositMemo"],
            "amount_in": q["amountIn"], "amount_in_formatted": q["amountInFormatted"],
            "amount_in_usd": q["amountInUsd"],
            "near_out": format_near(near), "recipient": b.account,
            "seconds": q["timeEstimate"], "deadline": q["deadline"],
            "app_fee_bps": self.fee_bps, "partner_fee": !self.one_click.has_key(),
        }))
    }

    /// A deposit address that turns `near` from `account` into `token` for
    /// `recipient` on `chain`, and the transaction the wallet signs.
    ///
    /// # Errors
    /// Testnet, a bad account, amount, chain, token or recipient, too many
    /// swaps, or 1Click refused.
    pub async fn withdraw(&self, who: &str, b: &WithdrawBody) -> Result<Value, Refusal> {
        self.mainnet_only()?;
        if !mainnet_account(&b.account) {
            return Err(bad("connect a NEAR mainnet account, the NEAR is sent from it"));
        }
        let near = swap_amount(&b.near).map_err(bad)?;
        let chain = chain(&b.chain).map_err(bad)?;
        let recipient = b.recipient.trim();
        if !address_ok(chain.kind, recipient) {
            return Err(bad(format!("that is not a {} address", chain.name)));
        }
        let (destination, decimals) =
            self.one_click.asset(chain, &b.token).await.map_err(refused)?;
        if !self.admit(who) {
            return Err(too_many());
        }
        let request = withdraw_request(
            false,
            &b.account,
            near,
            &destination,
            recipient,
            &Self::deadline(),
            &self.app_fees(),
        );
        let reply = self.one_click.quote(&request).await.map_err(refused)?;
        let q = &reply["quote"];
        let deposit = q["depositAddress"].as_str().unwrap_or_default().to_owned();
        if !mainnet_account(&deposit) {
            return Err(upstream("NEAR Intents gave a deposit address that is not a NEAR account"));
        }
        Ok(json!({
            "direction": "withdraw",
            "chain": chain.id, "chain_name": chain.name, "token": b.token, "decimals": decimals,
            "deposit_address": deposit,
            "near_in": format_near(near), "recipient": recipient,
            "amount_out": q["amountOut"], "amount_out_formatted": q["amountOutFormatted"],
            "min_amount_out_formatted": format_units(q["minAmountOut"].as_str(), decimals),
            "amount_out_usd": q["amountOutUsd"],
            "seconds": q["timeEstimate"], "deadline": q["deadline"],
            "app_fee_bps": self.fee_bps, "partner_fee": !self.one_click.has_key(),
            "transactions": withdraw_wallet_transactions(&deposit, near),
        }))
    }

    /// The swap's progress, from 1Click.
    ///
    /// # Errors
    /// A malformed address, or 1Click could not be reached.
    pub async fn status(&self, deposit: &str) -> Result<Value, Refusal> {
        if !plain(deposit) {
            return Err(bad("not a deposit address"));
        }
        let s = self.one_click.status(deposit).await.map_err(refused)?;
        let d = &s["swapDetails"];
        Ok(json!({
            "status": s["status"],
            "updated_at": s["updatedAt"],
            "deposited_formatted": d["depositedAmountFormatted"],
            "amount_in_formatted": d["amountInFormatted"],
            "amount_out_formatted": d["amountOutFormatted"],
            "refunded_formatted": d["refundedAmountFormatted"],
            "refund_reason": d["refundReason"],
            "origin_txs": d["originChainTxHashes"],
            "destination_txs": d["destinationChainTxHashes"],
            "near_txs": d["nearTxHashes"],
            "explorer": format!("https://explorer.near-intents.org/transactions/{deposit}"),
        }))
    }

    async fn rpc_query(&self, params: Value) -> Result<Value, Refusal> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "query", "params": params });
        let mut req = self.http.post(&self.rpc).json(&body).timeout(Duration::from_secs(15));
        if let Some(k) = self.rpc_key.as_deref().filter(|_| self.rpc.contains("fastnear")) {
            req = req.bearer_auth(k);
        }
        let v: Value = req
            .send()
            .await
            .map_err(|e| upstream(format!("NEAR RPC could not be reached: {e}")))?
            .json()
            .await
            .map_err(|e| upstream(format!("NEAR RPC answered something unexpected: {e}")))?;
        v.get("result").cloned().ok_or_else(|| upstream("NEAR RPC could not read the account"))
    }

    /// The NEAR `account` can withdraw: its balance less what NEAR locks
    /// for its storage, room for gas, and the deposit address's one-time
    /// `wrap.near` registration.
    async fn spendable(&self, account: &str) -> Result<u128, Refusal> {
        let a = self
            .rpc_query(json!({ "request_type": "view_account", "finality": "final", "account_id": account }))
            .await?;
        let amount: u128 = a["amount"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
        let storage = u128::from(a["storage_usage"].as_u64().unwrap_or(0)) * STORAGE_YOCTO_PER_BYTE;
        Ok(amount
            .saturating_sub(storage)
            .saturating_sub(GAS_ROOM)
            .saturating_sub(WRAP_STORAGE_YOCTO))
    }

    /// The wNEAR `account` holds on `wrap.near` and the NEAR it can cash
    /// out, in yoctoNEAR.
    ///
    /// # Errors
    /// Not a mainnet account, or the RPC could not say.
    pub async fn balance(&self, account: &str) -> Result<Value, Refusal> {
        self.mainnet_only()?;
        if !mainnet_account(account) {
            return Err(bad("not a NEAR mainnet account"));
        }
        let spendable = self.spendable(account).await?;
        let args = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            json!({ "account_id": account }).to_string(),
        );
        let r = self
            .rpc_query(json!({ "request_type": "call_function", "finality": "final",
                               "account_id": WRAP, "method_name": "ft_balance_of", "args_base64": args }))
            .await?;
        let bytes: Vec<u8> = r["result"]
            .as_array()
            .map(|a| {
                a.iter().filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok())).collect()
            })
            .ok_or_else(|| upstream("NEAR RPC could not read the wNEAR balance"))?;
        let yocto: String = serde_json::from_slice(&bytes).unwrap_or_else(|_| "0".to_owned());
        let n: u128 = yocto.parse().unwrap_or(0);
        Ok(json!({
            "account": account,
            "wnear_yocto": n.to_string(), "wnear": format_near(n),
            "spendable_yocto": spendable.to_string(), "spendable": format_near(spendable),
        }))
    }

    /// Tells 1Click a deposit was sent.
    ///
    /// # Errors
    /// A malformed request, or 1Click refused.
    pub async fn submit(&self, b: &SubmitBody) -> Result<Value, Refusal> {
        self.mainnet_only()?;
        if !plain(&b.deposit_address) || !plain(&b.tx_hash) {
            return Err(bad("send the deposit address and the transaction hash"));
        }
        let sender = b.near_sender.as_deref().filter(|s| mainnet_account(s));
        let v =
            self.one_click.submit(&b.deposit_address, &b.tx_hash, sender).await.map_err(refused)?;
        Ok(json!({ "ok": true, "status": v["status"] }))
    }
}

/// A deposit address or hash: letters and digits only.
fn plain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|c| c.is_ascii_alphanumeric())
}

fn too_many() -> Refusal {
    Refusal::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many",
        "too many swaps started from here, try again in an hour",
    )
}

#[derive(Debug, Deserialize)]
pub struct FundBody {
    pub account: String,
    pub near: String,
    pub chain: String,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct WithdrawBody {
    pub account: String,
    pub near: String,
    pub chain: String,
    pub token: String,
    pub recipient: String,
}

#[derive(Debug, Deserialize)]
pub struct SubmitBody {
    pub deposit_address: String,
    pub tx_hash: String,
    #[serde(default)]
    pub near_sender: Option<String>,
}

/// The routes, under `/newcomer/intents`.
pub fn router(intents: Arc<Intents>) -> Router {
    Router::new()
        .route("/newcomer/intents/options", get(options))
        .route("/newcomer/intents/fund", post(fund))
        .route("/newcomer/intents/withdraw", post(withdraw))
        .route("/newcomer/intents/status/{deposit}", get(status))
        .route("/newcomer/intents/submit", post(submit))
        .route("/newcomer/intents/balance/{account}", get(balance))
        .with_state(intents)
}

async fn options(State(i): State<Arc<Intents>>) -> Result<Json<Value>, Refusal> {
    i.options().await.map(Json)
}

async fn fund(
    State(i): State<Arc<Intents>>,
    headers: HeaderMap,
    Json(b): Json<FundBody>,
) -> Result<Json<Value>, Refusal> {
    i.fund(&client(&headers), &b).await.map(Json)
}

async fn withdraw(
    State(i): State<Arc<Intents>>,
    headers: HeaderMap,
    Json(b): Json<WithdrawBody>,
) -> Result<Json<Value>, Refusal> {
    i.withdraw(&client(&headers), &b).await.map(Json)
}

async fn status(
    State(i): State<Arc<Intents>>,
    Path(deposit): Path<String>,
) -> Result<Json<Value>, Refusal> {
    i.status(&deposit).await.map(Json)
}

async fn balance(
    State(i): State<Arc<Intents>>,
    Path(account): Path<String>,
) -> Result<Json<Value>, Refusal> {
    i.balance(&account).await.map(Json)
}

async fn submit(
    State(i): State<Arc<Intents>>,
    Json(b): Json<SubmitBody>,
) -> Result<Json<Value>, Refusal> {
    i.submit(&b).await.map(Json)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    #[test]
    fn testnet_refuses_before_any_request() {
        let i = Intents::new(
            "http://127.0.0.1:9",
            None,
            50,
            "fee.near".into(),
            false,
            "http://127.0.0.1:9".into(),
            None,
        );
        assert_eq!(i.mainnet_only().unwrap_err().code, "mainnet_only");
    }

    #[test]
    fn only_plain_addresses_reach_one_click() {
        assert!(plain("0x346dd67b6b46d92D13A5f1ceFFF4f13c2ef75d6a"));
        assert!(!plain("../v0/quote"));
        assert!(!plain(""));
    }
}

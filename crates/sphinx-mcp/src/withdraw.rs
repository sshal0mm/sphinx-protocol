//! `sphinx_withdraw`: turn NEAR from your own account into USDC or USDT on
//! another chain through NEAR Intents (1Click).
//!
//! On a hosted connector, whose key can call only the escrow, it never
//! signs: `quote` gives a price and a link to the site's withdraw panel,
//! filled in, where the owner approves once in their own wallet. On an MCP
//! server that holds the owner's key it can sign itself:
//!
//! Three steps, so a model cannot move money on its own:
//!
//! 1. `quote` (near, chain, token, recipient) asks 1Click for a live deposit
//!    address and says what arrives. Nothing is signed.
//! 2. `send` (deposit_address, confirm: true) signs one transaction to
//!    `wrap.near`: register the deposit address, wrap exactly the quoted
//!    NEAR, and send it there. Only a deposit address quoted by this server
//!    in this session is accepted, so text a model read cannot redirect the
//!    money, and the amount counts against the server's spending cap.
//! 3. `status` (deposit_address) follows the swap until it lands.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use sphinx_near::intents::{
    address_ok, chain, format_units, mainnet_account, send_withdrawal, swap_amount,
    withdraw_request, Failure, OneClick, WITHDRAW_EXTRA_YOCTO,
};
use sphinx_protocol::{Code, SphinxError};

use crate::{need_identity, SphinxMcp};

/// How long a quote may be sent after it was made.
const QUOTE_FOR: Duration = Duration::from_mins(30);

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WithdrawAction {
    /// One of: quote, send, status.
    pub action: String,
    /// For quote: NEAR to withdraw, e.g. "1.5" (0.1 to 200).
    #[serde(default, deserialize_with = "crate::optional_near_amount")]
    #[schemars(with = "Option<String>")]
    pub near: Option<String>,
    /// For quote: base, arbitrum, ethereum, optimism, polygon, avalanche, solana or near.
    #[serde(default)]
    pub chain: Option<String>,
    /// For quote: USDC or USDT.
    #[serde(default)]
    pub token: Option<String>,
    /// For quote: the user's own address on that chain that receives the tokens.
    #[serde(default)]
    pub recipient: Option<String>,
    /// For send and status: the deposit address from quote.
    #[serde(default)]
    pub deposit_address: Option<String>,
    /// For send: true only after the user has agreed to this exact quote.
    #[serde(default)]
    pub confirm: bool,
}

/// A quote this server made, waiting to be sent.
#[derive(Debug, Clone)]
pub(crate) struct Quoted {
    near: u128,
    summary: Value,
    at: Instant,
}

/// The quotes made in this session, by deposit address.
#[derive(Debug, Default)]
pub(crate) struct Withdrawals {
    quoted: Mutex<HashMap<String, Quoted>>,
}

fn usage(message: impl Into<String>) -> SphinxError {
    SphinxError::new(Code::CliUsage, message.into())
}

fn intents(f: &Failure) -> SphinxError {
    match f {
        Failure::Refused(_) | Failure::Unknown => usage(f.to_string()),
        Failure::Unreachable(_) => SphinxError::new(Code::RelayRpcFailed, f.to_string()),
    }
}

fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // 1Click takes any ISO 8601 time; seconds since the epoch in UTC.
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil(i64::try_from(days).unwrap_or(0));
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The civil date of `days` since 1970-01-01.
#[allow(clippy::many_single_char_names, reason = "the civil-date formula's own names")]
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The site's withdraw panel with a withdrawal filled in. The values ride
/// in the fragment: the site redirects page URLs with a query string, and a
/// fragment never reaches any server.
fn withdraw_link(
    site: &str,
    account: &str,
    near: &str,
    chain: &str,
    token: &str,
    to: &str,
) -> String {
    let q = |v: &str| -> String {
        v.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    };
    format!(
        "{}/account#withdraw?network=mainnet&account={}&near={}&chain={}&token={}&to={}",
        site.trim_end_matches('/'),
        q(account),
        q(near),
        q(chain),
        q(token),
        q(to)
    )
}

impl SphinxMcp {
    pub(crate) async fn withdraw(&self, p: WithdrawAction) -> Result<Value, SphinxError> {
        let id = self.settings.identity.as_ref().ok_or_else(need_identity)?;
        if !self.settings.escrow.is_mainnet() {
            return Err(usage("NEAR Intents runs on mainnet only; this server is on testnet"));
        }
        let account = id.account.to_string();
        if !mainnet_account(&account) {
            return Err(usage("withdrawing needs a NEAR mainnet account"));
        }
        let one_click = OneClick::new(
            &std::env::var("SPHINX_ONE_CLICK")
                .unwrap_or_else(|_| sphinx_near::intents::ONE_CLICK.to_owned()),
            std::env::var("SPHINX_ONE_CLICK_KEY").ok(),
        );
        match p.action.as_str() {
            "quote" => self.withdraw_quote(&one_click, &account, &p).await,
            "send" => self.withdraw_send(&one_click, &p).await,
            "status" => {
                let deposit =
                    p.deposit_address.as_deref().ok_or_else(|| usage("give deposit_address"))?;
                let s = one_click.status(deposit).await.map_err(|e| intents(&e))?;
                let d = &s["swapDetails"];
                Ok(json!({
                    "status": s["status"],
                    "amount_out": d["amountOutFormatted"],
                    "refunded": d["refundedAmountFormatted"],
                    "destination_txs": d["destinationChainTxHashes"],
                    "explorer": format!("https://explorer.near-intents.org/transactions/{deposit}"),
                    "done": matches!(s["status"].as_str(), Some("SUCCESS" | "REFUNDED" | "FAILED")),
                }))
            }
            other => Err(usage(format!("unknown action {other:?}; use quote, send or status"))),
        }
    }

    async fn withdraw_quote(
        &self,
        one_click: &OneClick,
        account: &str,
        p: &WithdrawAction,
    ) -> Result<Value, SphinxError> {
        let near = swap_amount(p.near.as_deref().ok_or_else(|| usage("give near, e.g. \"1\""))?)
            .map_err(usage)?;
        let chain = chain(p.chain.as_deref().unwrap_or_default()).map_err(usage)?;
        let token = p.token.as_deref().unwrap_or("USDC");
        let recipient = p.recipient.as_deref().map(str::trim).unwrap_or_default();
        if !address_ok(chain.kind, recipient) {
            return Err(usage(format!("recipient must be the user's own {} address", chain.name)));
        }
        let (asset, decimals) = one_click.asset(chain, token).await.map_err(|e| intents(&e))?;
        let deadline = rfc3339(SystemTime::now() + Duration::from_hours(1));
        // A connector's key cannot sign this; its owner signs on the site.
        let link_only = self.settings.pay_from_balance;
        let reply = one_click
            .quote(&withdraw_request(
                link_only,
                account,
                near,
                &asset,
                recipient,
                &deadline,
                &json!([]),
            ))
            .await
            .map_err(|e| intents(&e))?;
        let q = &reply["quote"];
        if link_only {
            let site = std::env::var("SPHINX_SITE")
                .unwrap_or_else(|_| "https://sphinx.sshal0mm.dev".to_owned());
            let link = withdraw_link(
                &site,
                account,
                &sphinx_near::format_near(near),
                chain.id,
                token,
                recipient,
            );
            return Ok(json!({
                "quote": {
                    "from": account,
                    "near": sphinx_near::format_near(near),
                    "receive_about": format!("{} {token}", q["amountOutFormatted"].as_str().unwrap_or("?")),
                    "on": chain.name,
                    "to": recipient,
                    "seconds": q["timeEstimate"],
                },
                "sign_url": link,
                "next": "Give the user sign_url. It opens the withdraw panel on the Sphinx site with this withdrawal filled in; they connect the wallet of this account and approve once. This connector cannot sign withdrawals: its key works only with the Sphinx escrow, so nobody holding the connector can move the user's NEAR.",
            }));
        }
        let deposit = q["depositAddress"].as_str().unwrap_or_default().to_owned();
        if !mainnet_account(&deposit) {
            return Err(SphinxError::new(
                Code::RelayRpcFailed,
                "NEAR Intents gave a deposit address that is not a NEAR account",
            ));
        }
        let summary = json!({
            "from": account,
            "near": sphinx_near::format_near(near),
            "plus_registration_up_to_near": sphinx_near::format_near(WITHDRAW_EXTRA_YOCTO),
            "receive_about": format!("{} {token}", q["amountOutFormatted"].as_str().unwrap_or("?")),
            "receive_at_least": format!("{} {token}", format_units(q["minAmountOut"].as_str(), decimals).unwrap_or_default()),
            "on": chain.name,
            "to": recipient,
            "seconds": q["timeEstimate"],
            "deposit_address": deposit,
        });
        if let Ok(mut quoted) = self.withdrawals.quoted.lock() {
            quoted.retain(|_, q| q.at.elapsed() < QUOTE_FOR);
            quoted.insert(
                deposit.clone(),
                Quoted { near, summary: summary.clone(), at: Instant::now() },
            );
        }
        Ok(json!({
            "quote": summary,
            "next": "Show this to the user. Only if they agree, call sphinx_withdraw with action \"send\", this deposit_address and confirm true. The quote can be sent for 30 minutes.",
        }))
    }

    async fn withdraw_send(
        &self,
        one_click: &OneClick,
        p: &WithdrawAction,
    ) -> Result<Value, SphinxError> {
        let id = self.settings.identity.as_ref().ok_or_else(need_identity)?;
        if self.settings.pay_from_balance {
            return Err(usage(
                "a connector cannot sign withdrawals; give the user the sign_url from quote, they approve once in their own wallet",
            ));
        }
        let deposit =
            p.deposit_address.as_deref().ok_or_else(|| usage("give deposit_address from quote"))?;
        if !p.confirm {
            return Err(usage("set confirm to true only after the user agreed to this quote"));
        }
        let quoted = self
            .withdrawals
            .quoted
            .lock()
            .ok()
            .and_then(|mut q| q.remove(deposit))
            .filter(|q| q.at.elapsed() < QUOTE_FOR)
            .ok_or_else(|| {
                usage("only a deposit address quoted by this server in the last 30 minutes can be sent to; ask for a new quote")
            })?;
        let spend = quoted.near + WITHDRAW_EXTRA_YOCTO;
        self.reserve(spend)?;
        let sent = send_withdrawal(
            self.settings.escrow.network(),
            id.account.clone(),
            id.signer.clone(),
            deposit,
            quoted.near,
        )
        .await;
        let tx_hash = match sent {
            Ok(h) => h,
            Err(e) => {
                self.release(spend);
                return Err(SphinxError::new(Code::RelayRpcFailed, e));
            }
        };
        // Telling 1Click is only a speed-up; the deposit is seen either way.
        let _ = one_click.submit(deposit, &tx_hash, Some(id.account.as_str())).await;
        Ok(json!({
            "sent": quoted.summary,
            "tx_hash": tx_hash,
            "explorer": format!("https://explorer.near-intents.org/transactions/{deposit}"),
            "next": "Follow with sphinx_withdraw action \"status\" and this deposit_address until done.",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_withdraw_link_fills_the_panel_and_escapes_its_values() {
        let l = withdraw_link("https://s.dev/", "a.near", "1.5", "base", "USDC", "0xAb&c=1");
        assert_eq!(
            l,
            "https://s.dev/account#withdraw?network=mainnet&account=a.near&near=1.5&chain=base&token=USDC&to=0xAb%26c%3D1"
        );
    }

    #[test]
    fn deadlines_are_utc_iso_times() {
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = SystemTime::UNIX_EPOCH + Duration::from_hours(498_506);
        assert_eq!(rfc3339(t), "2026-11-14T02:00:00Z");
    }
}

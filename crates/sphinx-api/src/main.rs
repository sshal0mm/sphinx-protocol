//! Runs the Sphinx job API.
//!
//! Environment:
//! - `SPHINX_CONTRACT` (required): escrow account, e.g. `escrow3.sphinx999.testnet`
//! - `SPHINX_API_ADDR` (default `127.0.0.1:8787`)
//! - `SPHINX_API_PUBLIC_BASE` (default `http://<addr>`): base URL used in `code_uri`
//! - `SPHINX_CODE_DIR` (default `./data/code`)
//! - `SPHINX_RELAYER_ACCOUNT` + `SPHINX_RELAYER_KEY` (optional, `ed25519:...`):
//!   enable gasless relaying at `POST /v1/relay`
//! - `SPHINX_RELAY_BURST`, `SPHINX_RELAY_PER_MINUTE`, `SPHINX_RELAY_GLOBAL_PER_MINUTE`,
//!   `SPHINX_RELAY_MAX_FAILURES`, `SPHINX_RELAY_COOLDOWN_SECS` (optional): relay
//!   limits; defaults in [`sphinx_api::limits::RelayLimits`]
#![allow(clippy::print_stderr, reason = "startup and fatal errors go to stderr")]

use sphinx_api::code_store::CodeStore;
use sphinx_api::limits::{RelayLimiter, RelayLimits};
use sphinx_api::{router, AppState, Relay};
use sphinx_near::near_api::Signer;
use sphinx_near::Escrow;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let Ok(contract) = std::env::var("SPHINX_CONTRACT") else {
        eprintln!("error[SPHINX_E_CLI_CONFIG_MISSING]: SPHINX_CONTRACT is not set");
        return std::process::ExitCode::from(78);
    };
    let Ok(contract) = contract.parse() else {
        eprintln!(
            "error[SPHINX_E_CLI_CONFIG_MALFORMED]: SPHINX_CONTRACT is not a valid account id"
        );
        return std::process::ExitCode::from(78);
    };
    let addr = std::env::var("SPHINX_API_ADDR").unwrap_or_else(|_| "127.0.0.1:8787".to_owned());
    let public_base =
        std::env::var("SPHINX_API_PUBLIC_BASE").unwrap_or_else(|_| format!("http://{addr}"));
    // Task text and submissions are named on chain by this address, so a
    // loopback one would leave every job pointing at a machine nobody else
    // can reach. Only a local run may use it, and must say so.
    if is_loopback(&public_base) && std::env::var("SPHINX_API_ALLOW_LOOPBACK").as_deref() != Ok("1")
    {
        eprintln!(
            "error[SPHINX_E_CLI_CONFIG_MALFORMED]: SPHINX_API_PUBLIC_BASE is {public_base}, which only this machine can reach, and jobs record it on chain. Set it to the API's public address, or set SPHINX_API_ALLOW_LOOPBACK=1 for a local-only run"
        );
        return std::process::ExitCode::from(78);
    }
    let code_dir = std::env::var("SPHINX_CODE_DIR").unwrap_or_else(|_| "data/code".to_owned());
    let escrow = Escrow::for_contract(contract);
    let relay = match relay_from_env(&escrow) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("error[SPHINX_E_CLI_CONFIG_MALFORMED]: {msg}");
            return std::process::ExitCode::from(78);
        }
    };
    eprintln!("relaying: {}", if relay.is_some() { "enabled" } else { "disabled" });
    let state = Arc::new(AppState {
        chain: sphinx_api::cache::Cached::new(
            escrow,
            std::time::Duration::from_secs(15),
            std::time::Duration::from_mins(10),
        ),
        code: CodeStore::new(code_dir.into())
            .with_reader_token(std::env::var("SPHINX_CODE_READER_TOKEN").ok()),
        public_base,
        relay,
        uids: sphinx_api::uids::UidIndex::default(),
    });
    // No background reads: the chain is read when someone asks, answers are
    // reused for `fresh` and served while refreshing for up to `stale`, and
    // our own actions (a relayed call, stored code) mark them stale.
    // Uploads nothing on chain refers to are removed once a day old.
    let collector = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            match sphinx_api::referenced_code(&collector).await {
                Ok(refs) => {
                    if let Ok(n) =
                        collector.code.collect(&refs, std::time::Duration::from_hours(24)).await
                    {
                        if n > 0 {
                            eprintln!("code store: removed {n} unreferenced uploads");
                        }
                    }
                }
                Err(e) => eprintln!("code store cleanup skipped: {}", e.code()),
            }
        }
    });
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot listen on {addr}: {e}");
            return std::process::ExitCode::from(75);
        }
    };
    eprintln!("sphinx-api listening on http://{addr}");
    if let Err(e) = axum::serve(listener, router(state)).await {
        eprintln!("error: server stopped: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// Reads the optional relayer account and key.
fn relay_from_env(escrow: &Escrow) -> Result<Option<Relay>, String> {
    // Unset or empty (as compose passes an unset variable): no relay, and the
    // API runs read-only for relayed calls.
    let set = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let (Some(account), Some(key)) = (set("SPHINX_RELAYER_ACCOUNT"), set("SPHINX_RELAYER_KEY"))
    else {
        return Ok(None);
    };
    let relayer = account
        .parse()
        .map_err(|_| "SPHINX_RELAYER_ACCOUNT is not a valid account id".to_owned())?;
    let secret = key
        .parse()
        .map_err(|_| "SPHINX_RELAYER_KEY is not an ed25519:... secret key".to_owned())?;
    let signer = Signer::from_secret_key(secret)
        .map_err(|e| format!("SPHINX_RELAYER_KEY cannot sign: {e}"))?;
    let now = std::time::Instant::now();
    let limiter = RelayLimiter::new(relay_limits_from_env()?, now);
    // Per client: a burst of 10, then 20 a minute; all clients together 300 a minute.
    let clients = sphinx_api::limits::ClientLimiter::new(10, 20, 300, now);
    let daily_max =
        std::env::var("SPHINX_RELAY_DAILY_MAX").ok().and_then(|v| v.parse().ok()).unwrap_or(2_000);
    let daily = sphinx_api::limits::DailyCap::new(daily_max);
    Ok(Some(Relay { escrow: escrow.clone(), relayer, signer, limiter, clients, daily }))
}

/// Relay limits, each overridable by an environment variable.
fn relay_limits_from_env() -> Result<RelayLimits, String> {
    fn var(name: &str, default: u32) -> Result<u32, String> {
        std::env::var(name).map_or(Ok(default), |v| {
            v.parse()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{name} must be a positive integer"))
        })
    }
    let d = RelayLimits::default();
    let cooldown_secs = u32::try_from(d.cooldown.as_secs()).unwrap_or(u32::MAX);
    Ok(RelayLimits {
        burst: var("SPHINX_RELAY_BURST", d.burst)?,
        per_minute: var("SPHINX_RELAY_PER_MINUTE", d.per_minute)?,
        global_per_minute: var("SPHINX_RELAY_GLOBAL_PER_MINUTE", d.global_per_minute)?,
        max_failures: var("SPHINX_RELAY_MAX_FAILURES", d.max_failures)?,
        cooldown: std::time::Duration::from_secs(u64::from(var(
            "SPHINX_RELAY_COOLDOWN_SECS",
            cooldown_secs,
        )?)),
    })
}

/// Whether `base` names this machine: localhost, a 127/8 address, `[::1]`
/// or 0.0.0.0.
fn is_loopback(base: &str) -> bool {
    let host = base.split("://").nth(1).unwrap_or(base);
    let host = host.split('/').next().unwrap_or(host);
    let host = if host.starts_with('[') {
        host.split(']').next().map_or(host, |h| h.trim_start_matches('['))
    } else {
        host.split(':').next().unwrap_or(host)
    };
    host.eq_ignore_ascii_case("localhost")
        || host.starts_with("127.")
        || host == "::1"
        || host == "0.0.0.0"
}

#[cfg(test)]
mod tests {
    use super::is_loopback;

    #[test]
    fn loopback_addresses_are_recognised() {
        for b in [
            "http://127.0.0.1:8787",
            "http://localhost:8787",
            "http://[::1]:8787",
            "http://0.0.0.0",
        ] {
            assert!(is_loopback(b), "{b}");
        }
        for b in ["https://sphinx.sshal0mm.dev", "https://api.example/v1"] {
            assert!(!is_loopback(b), "{b}");
        }
    }
}

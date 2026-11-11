//! `sphinx-house`: the newcomer box's service. Runs next to the job API on
//! the internal network; the web app proxies `/newcomer` to it. It holds a
//! model key only: buyers fund and post their jobs from their own wallets.
//!
//! Environment:
//! - `SPHINX_MODEL_KEY` (and `SPHINX_MODEL_BASE`, `SPHINX_MODEL`): the
//!   concierge's model; it must run in a TEE, since it writes hidden tests
//! - `SPHINX_CONTRACT`, `SPHINX_API` (internal job API), `SPHINX_API_PUBLIC_BASE`
//! - `SPHINX_RPC` (default `FastNEAR` testnet), `SPHINX_RPC_KEY` (`FastNEAR` key),
//!   `SPHINX_RELAYER_ACCOUNT`
//! - `HOUSE_ADDR` (default `127.0.0.1:8792`), `HOUSE_DIR` (default `data/house`)
//! - `HOUSE_SUGGESTED_REWARD_NEAR` (0.5), `HOUSE_BOND_NEAR` (0.01),
//!   `HOUSE_DEADLINE_HOURS` (48), `HOUSE_DRAFTING_FEE_NEAR` (0.05)
//! - `HOUSE_SWAP_FEE_BPS` (50), `HOUSE_SWAP_FEE_ACCOUNT` (the fee account on mainnet)
//! - `HOUSE_DRAFTS_PER_VISITOR` (3), `HOUSE_DRAFTS_PER_DAY` (40)
#![allow(clippy::print_stderr, reason = "startup and fatal errors go to stderr")]

use std::sync::Arc;

use sphinx_house::{router, Caps, Endpoints, House, Terms};
use sphinx_near::hidden_suite::Keystore;
use sphinx_near::Escrow;

fn env(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_owned())
}

fn number(name: &str, default: u32) -> u32 {
    env(name, &default.to_string()).parse().unwrap_or(default)
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::from(78)
        }
    }
}

async fn run() -> Result<(), String> {
    let model =
        sphinx_model::NearAiCloud::from_env().ok_or("set SPHINX_MODEL_KEY for the concierge")?;
    // Drafting waits until the model is confirmed private; the rest of the
    // box (quotes, published tests, payments in flight) runs meanwhile.
    if !model.confirm_privacy().await {
        eprintln!(
            "drafting paused: {}; it would see the hidden tests it writes. Checking again every 5 minutes (SPHINX_ALLOW_UNTRUSTED_MODEL=1 accepts it)",
            model.privacy_note()
        );
    }
    let contract: sphinx_near::near_api::types::AccountId =
        env("SPHINX_CONTRACT", "escrow3.sphinx999.testnet")
            .parse()
            .map_err(|_| "SPHINX_CONTRACT is not an account id")?;
    let api = env("SPHINX_API", "http://127.0.0.1:8787");
    let mainnet = sphinx_near::is_mainnet(contract.as_str());
    let keystore = Keystore::for_account(contract.as_str());
    let at = Endpoints {
        escrow: Escrow::for_contract(contract),
        rpc: env(
            "SPHINX_RPC",
            if mainnet {
                "https://rpc.mainnet.fastnear.com"
            } else {
                "https://rpc.testnet.fastnear.com"
            },
        ),
        rpc_key: std::env::var("SPHINX_RPC_KEY").ok().filter(|k| !k.is_empty()),
        api_public: env("SPHINX_API_PUBLIC_BASE", &api),
        api,
        keystore,
        relayer: env("SPHINX_RELAYER_ACCOUNT", "relayer.sphinx999.testnet")
            .parse()
            .map_err(|_| "SPHINX_RELAYER_ACCOUNT is not an account id")?,
        one_click: env("HOUSE_ONE_CLICK", "https://1click.chaindefuser.com"),
    };
    let terms = Terms {
        suggested_reward_near: env("HOUSE_SUGGESTED_REWARD_NEAR", "0.5"),
        bond_near: env("HOUSE_BOND_NEAR", "0.01"),
        max_subs_per_agent: 3,
        deadline_hours: number("HOUSE_DEADLINE_HOURS", 48),
        drafting_fee_near: env("HOUSE_DRAFTING_FEE_NEAR", "0.05"),
        swap_fee_bps: number("HOUSE_SWAP_FEE_BPS", 50),
        swap_fee_account: env("HOUSE_SWAP_FEE_ACCOUNT", "sphinx999.near"),
    };
    let caps = Caps {
        drafts_per_visitor: number("HOUSE_DRAFTS_PER_VISITOR", 3),
        drafts_per_day: number("HOUSE_DRAFTS_PER_DAY", 40),
    };
    let dir = std::path::PathBuf::from(env("HOUSE_DIR", "data/house"));
    let house = Arc::new(House::new(model, at, dir, terms, caps));
    let checker = house.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            tick.tick().await;
            if !checker.model.is_private() && checker.model.confirm_privacy().await {
                eprintln!("drafting resumed: the model is confirmed private");
            }
        }
    });
    // Tests a buyer did not publish are forgotten a week after the job ends.
    let keeper = house.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
        loop {
            tick.tick().await;
            keeper.forget_old_tests().await;
        }
    });
    let addr = env("HOUSE_ADDR", "127.0.0.1:8792");
    let listener =
        tokio::net::TcpListener::bind(&addr).await.map_err(|e| format!("{addr}: {e}"))?;
    eprintln!("sphinx-house listening on http://{addr}");
    axum::serve(listener, router(house)).await.map_err(|e| e.to_string())
}

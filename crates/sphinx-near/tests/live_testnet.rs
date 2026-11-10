//! Read-only checks against the deployed testnet contract. Network access is
//! required, so they are ignored by default: `cargo test -p sphinx-near -- --ignored`.
#![allow(clippy::unwrap_used, reason = "test")]

use serde_json::json;
use sphinx_near::views::{Config, Job};
use sphinx_near::Escrow;

fn escrow() -> Escrow {
    Escrow::testnet("escrow.sphinx999.testnet".parse().unwrap())
}

#[tokio::test]
#[ignore = "needs network"]
async fn reads_config_and_jobs() {
    let e = escrow();
    let config: Config = e.view("get_config", json!({})).await.unwrap();
    assert!(config.supported_runtimes.contains(&"js-boa/1".to_owned()));
    assert_eq!(config.grader.map(|g| g.mode), Some("outlayer_tdx".to_owned()));
    let jobs: Vec<Job> = e.view("list_jobs", json!({"from": null, "limit": 50})).await.unwrap();
    assert_ne!(jobs.len(), 0);
}

#[tokio::test]
#[ignore = "needs network"]
async fn view_errors_are_catalog_errors() {
    let err = escrow().view::<Config>("no_such_method", json!({})).await.unwrap_err();
    assert_eq!(err.code().as_str(), "SPHINX_E_RELAY_RPC_FAILED");
}

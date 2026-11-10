//! The agent end to end on testnet, with a scripted model in place of NEAR AI
//! Cloud: it starts the real MCP server, finds an open job, submits, and waits
//! for the enclave's score. Needs a running job API and relayer.
//!
//! Run `cargo test -p sphinx-agent --test live_testnet -- --ignored` with
//! `SPHINX_API`, `SPHINX_ACCOUNT`, `SPHINX_ACCOUNT_KEY`, `SPHINX_MCP` and
//! `SPHINX_AGENT_SOLUTION` (a file holding the scripted model's JSON reply).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout, reason = "live test")]

use sphinx_agent::{Agent, Event, Limits, McpSphinx, Scripted, Sphinx};
use std::time::Duration;

#[tokio::test]
#[ignore = "needs testnet, a job API, a relayer and an agent key"]
async fn the_agent_earns_on_testnet() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let env: Vec<(String, String)> =
        ["SPHINX_CONTRACT", "SPHINX_API", "SPHINX_ACCOUNT", "SPHINX_ACCOUNT_KEY"]
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
            .collect();
    let sphinx = McpSphinx::start(&var("SPHINX_MCP"), &env).await.unwrap();
    let reply = std::fs::read_to_string(var("SPHINX_AGENT_SOLUTION")).unwrap();
    let model = Scripted::new([reply]);
    let storage = 10_000_000_000_000_000_000_000; // 0.01 NEAR, the testnet escrow's deposit
    let mut agent = Agent::new(
        &model,
        &sphinx,
        Limits {
            min_reward: 0,
            max_bond_bps: 10_000,
            budget: 50_000_000_000_000_000_000_000,
            attempts: 1,
            poll: Duration::from_secs(5),
            wait: Duration::from_mins(10),
        },
    );
    let jobs = sphinx.open_jobs().await.unwrap();
    let job = agent.choose(&jobs, &[], storage).expect("an open job").clone();
    let mut events = vec![];
    let paid = agent
        .work(&job, storage, &mut |e| {
            println!("{e:?}");
            events.push(e);
        })
        .await
        .unwrap();
    assert_eq!(paid, sphinx_agent::Outcome::Paid, "{events:?}");
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Submitted { sub, .. } if sub.gas_paid_by == "sphinx")));
}

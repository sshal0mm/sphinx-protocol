//! The compiled escrow on a local NEAR sandbox node, at the largest job the
//! contract allows: 64 submissions. A payout and a refund each walk one job's
//! submissions, so these are the most expensive calls there are. They must
//! fit the gas the relayer attaches (100 Tgas) with room to spare.
//!
//! Run with `make sandbox` (builds the contract first). The first run
//! downloads NEAR's sandbox node.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::cast_precision_loss,
    reason = "a test that reports measurements"
)]

use ed25519_dalek::{Signer as _, SigningKey};
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, Contract, Worker};
use serde_json::{json, Value};
use sphinx_protocol::{Hash32, Score, SuiteCheck};

const SUBS: u32 = 64;
const RELAYER_GAS: Gas = Gas::from_tgas(100);
const WASM: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/near/sphinx_contract/sphinx_contract.wasm");

struct Setup {
    worker: Worker<Sandbox>,
    contract: Contract,
    buyer: Account,
    agents: Vec<Account>,
    key: SigningKey,
}

async fn setup() -> Setup {
    let worker = near_workspaces::sandbox().await.unwrap();
    let wasm = std::fs::read(WASM).expect("build the contract first: make contract-wasm");
    let contract = worker.dev_deploy(&wasm).await.unwrap();
    let root = worker.root_account().unwrap();
    let account = |name: &'static str, near: u128| {
        let root = root.clone();
        async move {
            root.create_subaccount(name)
                .initial_balance(NearToken::from_near(near))
                .transact()
                .await
                .unwrap()
                .into_result()
                .unwrap()
        }
    };
    let owner = account("owner", 10).await;
    let verifier = account("verifier", 5).await;
    let buyer = account("buyer", 50).await;
    let mut agents = Vec::new();
    for i in 0..SUBS {
        let name: &'static str = Box::leak(format!("agent{i}").into_boxed_str());
        agents.push(account(name, 2).await);
    }
    contract
        .call("new")
        .args_json(json!({ "owner": owner.id(), "verifier": verifier.id(), "fee_bps": 500, "allow_dev_keys": true }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let key = SigningKey::from_bytes(&[7; 32]);
    owner
        .call(contract.id(), "set_grader_key")
        .args_json(json!({
            "public_key_hex": hex::encode(key.verifying_key().to_bytes()),
            "box_public_key_hex": "22".repeat(32),
            "mode": "dev",
            "wasm_sha256": "33".repeat(32),
            "attestation_ref": "sandbox"
        }))
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    Setup { worker, contract, buyer, agents, key }
}

fn ok(r: &ExecutionFinalResult, what: &str) {
    assert!(r.is_success(), "{what} failed: {:?}", r.failures());
}

async fn view(s: &Setup, method: &str, args: Value) -> Value {
    s.contract.view(method).args_json(args).await.unwrap().json().unwrap()
}

fn yocto(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

/// Posts a job sized for `SUBS` submissions, commits it, opens it with a
/// signed suite check and has every agent submit once. Returns its id.
async fn full_job(s: &Setup, deadline_from_now_s: u64) -> String {
    let config = view(s, "get_config", json!({})).await;
    let now = s.worker.view_block().await.unwrap().timestamp();
    let reward = NearToken::from_near(1).as_yoctonear();
    let bond = NearToken::from_millinear(10).as_yoctonear();
    let r = s
        .buyer
        .call(s.contract.id(), "post_job")
        .args_json(json!({
            "title": "Largest job", "spec_uri": "sandbox", "runtime": "js-boa/1", "reward": reward.to_string(), "bond": bond.to_string(),
            "max_subs_per_agent": 1, "max_subs_total": SUBS,
            "deadline_ns": (now + deadline_from_now_s * 1_000_000_000).to_string()
        }))
        .deposit(NearToken::from_yoctonear(reward + yocto(&config["job_storage_deposit"])))
        .max_gas()
        .transact()
        .await
        .unwrap();
    ok(&r, "post_job");
    let job: String = r.json().unwrap();
    let suite = Hash32::digest(b"suite");
    let reference = Hash32::digest(b"reference");
    let commit = s
        .buyer
        .call(s.contract.id(), "commit_suite")
        .args_json(json!({
            "job_id": job, "suite_hash": suite.to_string(), "ref_solution_hash": reference.to_string(),
            "grader_public_key": hex::encode(s.key.verifying_key().to_bytes())
        }))
        .max_gas()
        .transact()
        .await
        .unwrap();
    ok(&commit, "commit_suite");
    let receipt: Value = commit.json().unwrap();
    let check = SuiteCheck {
        contract: s.contract.id().to_string(),
        job_id: job.parse().unwrap(),
        valid: true,
        suite_hash: suite,
        ref_solution_hash: reference,
        nonce: receipt["suite_check_nonce"].as_str().unwrap().parse().unwrap(),
    };
    let sig = hex::encode(s.key.sign(check.signing_message().as_bytes()).to_bytes());
    let r = s
        .buyer
        .call(s.contract.id(), "record_suite_check")
        .args_json(json!({ "check": check, "signature_hex": sig }))
        .gas(RELAYER_GAS)
        .transact()
        .await
        .unwrap();
    ok(&r, "record_suite_check");
    let deposit = bond + yocto(&config["submission_storage_deposit"]);
    for (i, agent) in s.agents.iter().enumerate() {
        let code = Hash32::digest(format!("code {job} {i}").as_bytes());
        let r = agent
            .call(s.contract.id(), "submit")
            .args_json(
                json!({ "job_id": job, "code_uri": "sandbox", "code_hash": code.to_string() }),
            )
            .deposit(NearToken::from_yoctonear(deposit))
            .max_gas()
            .transact()
            .await
            .unwrap();
        ok(&r, "submit");
    }
    job
}

/// Signs and records submission `sub_id`'s score; returns the gas it burnt.
async fn score(s: &Setup, job: &str, sub_id: u32, passed: u32) -> Gas {
    let sub = view(s, "get_submission", json!({ "job_id": job, "sub_id": sub_id })).await;
    let job_view = view(s, "get_job", json!({ "job_id": job })).await;
    let score = Score {
        contract: s.contract.id().to_string(),
        job_id: job.parse().unwrap(),
        sub_id,
        runtime: "js-boa/1".into(),
        passed,
        total: 12,
        suite_hash: job_view["commit"]["suite_hash"].as_str().unwrap().parse().unwrap(),
        code_hash: sub["code_hash"].as_str().unwrap().parse().unwrap(),
        nonce: sub["nonce"].as_str().unwrap().parse().unwrap(),
    };
    let sig = hex::encode(s.key.sign(score.signing_message().as_bytes()).to_bytes());
    let r = s
        .buyer
        .call(s.contract.id(), "record_score")
        .args_json(json!({ "score": score, "signature_hex": sig }))
        .gas(RELAYER_GAS)
        .transact()
        .await
        .unwrap();
    ok(&r, "record_score");
    r.total_gas_burnt
}

fn tgas(g: Gas) -> f64 {
    g.as_gas() as f64 / 1e12
}

/// Adds one measurement to `deployments/sandbox-gas.json`, which
/// `scripts/gas-table.sh` puts in docs/GAS.md.
fn record(key: &str, gas: Gas) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deployments/sandbox-gas.json");
    let mut all: Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    all[key] = json!((tgas(gas) * 100.0).round() / 100.0);
    all["submissions"] = json!(SUBS);
    std::fs::write(path, serde_json::to_string_pretty(&all).unwrap() + "\n").unwrap();
}

#[tokio::test]
#[ignore = "needs the compiled contract and a sandbox node: make sandbox"]
async fn paying_the_last_of_64_submissions_fits_the_relayers_gas() {
    let s = setup().await;
    let job = full_job(&s, 3600).await;
    let mut fail_gas = Vec::new();
    for sub_id in 0..SUBS - 1 {
        fail_gas.push(score(&s, &job, sub_id, 5).await);
    }
    let pay = score(&s, &job, SUBS - 1, 12).await;
    let job_view = view(&s, "get_job", json!({ "job_id": job })).await;
    assert_eq!(job_view["status"], "Paid");
    assert_eq!(job_view["winning_sub_id"], SUBS - 1);
    let stats = view(&s, "get_stats", json!({})).await;
    assert_eq!(stats["solvent"], true);
    let max_fail = fail_gas.iter().copied().max_by_key(|g| g.as_gas()).unwrap();
    println!("record_score, a failure: at most {:.2} Tgas", tgas(max_fail));
    println!("record_score, the payout after {} failures: {:.2} Tgas", SUBS - 1, tgas(pay));
    record("record_score_failure_max_tgas", max_fail);
    record("record_score_payout_tgas", pay);
    assert!(pay.as_gas() < RELAYER_GAS.as_gas() / 2, "the payout used {:.2} Tgas", tgas(pay));
}

#[tokio::test]
#[ignore = "needs the compiled contract and a sandbox node: make sandbox"]
async fn refunding_a_job_with_64_failed_submissions_fits_the_relayers_gas() {
    let s = setup().await;
    // The shortest job allowed is 10 minutes.
    let job = full_job(&s, 601).await;
    for sub_id in 0..SUBS {
        score(&s, &job, sub_id, 5).await;
    }
    // Past the deadline and the grading window.
    let deadline: u64 = view(&s, "get_job", json!({ "job_id": job })).await["deadline_ns"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    while s.worker.view_block().await.unwrap().timestamp() < deadline + 2 * 31 * 60 * 1_000_000_000
    {
        s.worker.fast_forward(2_000).await.unwrap();
    }
    let r = s
        .buyer
        .call(s.contract.id(), "refund_on_timeout")
        .args_json(json!({ "job_id": job }))
        .gas(RELAYER_GAS)
        .transact()
        .await
        .unwrap();
    ok(&r, "refund_on_timeout");
    let job_view = view(&s, "get_job", json!({ "job_id": job })).await;
    assert_eq!(job_view["status"], "Refunded");
    println!("refund_on_timeout, crediting {SUBS} held bonds: {:.2} Tgas", tgas(r.total_gas_burnt));
    record("refund_tgas", r.total_gas_burnt);
    assert!(r.total_gas_burnt.as_gas() < RELAYER_GAS.as_gas() / 2);
    let stats = view(&s, "get_stats", json!({})).await;
    assert_eq!(stats["solvent"], true);
}

#[tokio::test]
#[ignore = "needs the compiled contract and a sandbox node: make sandbox"]
async fn settling_a_pass_behind_63_expired_submissions_fits_the_relayers_gas() {
    let s = setup().await;
    let job = full_job(&s, 3600).await;
    // The last submission passes while every earlier one is still pending,
    // so the pass waits.
    score(&s, &job, SUBS - 1, 12).await;
    assert_eq!(view(&s, "get_job", json!({ "job_id": job })).await["status"], "Open");
    // One grading window later, settling expires all 63 and pays the pass.
    let submitted: u64 = view(&s, "get_submission", json!({ "job_id": job, "sub_id": SUBS - 1 }))
        .await["submitted_at_ns"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    while s.worker.view_block().await.unwrap().timestamp() < submitted + 31 * 60 * 1_000_000_000 {
        s.worker.fast_forward(2_000).await.unwrap();
    }
    let r = s
        .buyer
        .call(s.contract.id(), "settle")
        .args_json(json!({ "job_id": job }))
        .gas(RELAYER_GAS)
        .transact()
        .await
        .unwrap();
    ok(&r, "settle");
    let job_view = view(&s, "get_job", json!({ "job_id": job })).await;
    assert_eq!(job_view["status"], "Paid");
    assert_eq!(job_view["winning_sub_id"], SUBS - 1);
    println!(
        "settle, expiring {} submissions and paying: {:.2} Tgas",
        SUBS - 1,
        tgas(r.total_gas_burnt)
    );
    record("settle_tgas", r.total_gas_burnt);
    assert!(r.total_gas_burnt.as_gas() < RELAYER_GAS.as_gas() * 3 / 4);
    assert_eq!(view(&s, "get_stats", json!({})).await["solvent"], true);
    bonds_beyond_the_transfers_are_withdrawable(&s).await;
}

/// The first few returned bonds are transferred; the rest are credited and
/// each agent can withdraw its own.
async fn bonds_beyond_the_transfers_are_withdrawable(s: &Setup) {
    let mut credited = 0;
    for agent in &s.agents {
        let owed = yocto(&view(s, "get_unclaimed", json!({ "account": agent.id() })).await);
        if owed == 0 {
            continue;
        }
        credited += 1;
        let r = agent.call(s.contract.id(), "withdraw").max_gas().transact().await.unwrap();
        ok(&r, "withdraw");
        assert_eq!(yocto(&view(s, "get_unclaimed", json!({ "account": agent.id() })).await), 0);
    }
    println!("{credited} bonds were credited and withdrawn");
    assert!(credited > 0);
    let stats = view(s, "get_stats", json!({})).await;
    assert_eq!(stats["solvent"], true);
    assert_eq!(stats["total_unclaimed"], "0");
}

#[tokio::test]
#[ignore = "needs the compiled contract and a sandbox node: make sandbox"]
async fn paying_the_first_of_64_passing_submissions_fits_the_relayers_gas() {
    let s = setup().await;
    let job = full_job(&s, 3600).await;
    // Every later submission passes first and waits; then the earliest passes.
    for sub_id in 1..SUBS {
        score(&s, &job, sub_id, 12).await;
    }
    let pay = score(&s, &job, 0, 12).await;
    let job_view = view(&s, "get_job", json!({ "job_id": job })).await;
    assert_eq!(job_view["status"], "Paid");
    assert_eq!(job_view["winning_sub_id"], 0);
    println!(
        "record_score, paying the earliest pass and returning {} later bonds: {:.2} Tgas",
        SUBS - 1,
        tgas(pay)
    );
    record("record_score_payout_later_passes_tgas", pay);
    assert!(pay.as_gas() < RELAYER_GAS.as_gas() * 3 / 4);
    bonds_beyond_the_transfers_are_withdrawable(&s).await;
}

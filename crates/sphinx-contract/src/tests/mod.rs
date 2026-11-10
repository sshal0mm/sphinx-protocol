//! Unit tests. Helpers build a contract with a deterministic grader key,
//! drive it through the public methods, and inspect the receipts it creates
//! so payouts are asserted, not just status changes.

mod balances;
mod bounds;
mod directory;
mod lifecycle;
mod payouts;
mod preflight;
mod security;

use ed25519_dalek::{Signer, SigningKey};
use near_sdk::json_types::{U128, U64};
use near_sdk::mock::MockAction;
use near_sdk::test_utils::{get_created_receipts, get_logs, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken};
use sphinx_protocol::{Code, Hash32, Score, SuiteCheck, RUNTIME_JS_BOA_V1};

use crate::constants::{GRADING_WINDOW_NS, JOB_STORAGE_DEPOSIT, SUBMISSION_STORAGE_DEPOSIT};
use crate::errors::ContractError;
use crate::types::{KeyMode, SubmitReceipt};
use crate::Sphinx;

pub const CONTRACT: &str = "sphinx.example.testnet";
pub const NEAR: u128 = 10u128.pow(24);
pub const REWARD: u128 = NEAR;
pub const BOND: u128 = NEAR / 20;
pub const START_NS: u64 = 1_000_000_000_000;
pub const DAY_NS: u64 = 86_400_000_000_000;
pub const DEADLINE_NS: u64 = START_NS + DAY_NS;

pub fn acct(name: &str) -> AccountId {
    format!("{name}.example.testnet").parse().unwrap()
}

pub fn owner() -> AccountId {
    acct("owner")
}
pub fn buyer() -> AccountId {
    acct("buyer")
}
pub fn verifier() -> AccountId {
    acct("verifier")
}

/// Sets the calling account, attached deposit and block time.
pub fn ctx(caller: &AccountId, deposit: u128, now_ns: u64) {
    testing_env!(VMContextBuilder::new()
        .current_account_id(CONTRACT.parse().unwrap())
        .predecessor_account_id(caller.clone())
        .signer_account_id(caller.clone())
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .account_balance(NearToken::from_near(1_000))
        .block_timestamp(now_ns)
        .random_seed([7; 32])
        .build());
}

pub fn grader_key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

pub fn other_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

pub fn sign(key: &SigningKey, message: &str) -> String {
    hex::encode(key.sign(message.as_bytes()).to_bytes())
}

pub fn suite_hash() -> Hash32 {
    Hash32::digest(b"hidden suite")
}
pub fn ref_hash() -> Hash32 {
    Hash32::digest(b"reference solution")
}
pub fn code(n: u8) -> Hash32 {
    Hash32::digest(&[n])
}

/// The error code of a failed call.
pub fn code_of<T: std::fmt::Debug>(r: Result<T, ContractError>) -> Code {
    r.expect_err("call should fail").0.code()
}

/// (receiver, yoctoNEAR) for every transfer the last call created.
pub fn transfers() -> Vec<(AccountId, u128)> {
    get_created_receipts()
        .into_iter()
        .flat_map(|r| {
            let to = r.receiver_id.clone();
            r.actions.into_iter().filter_map(move |a| match a {
                MockAction::Transfer { deposit, .. } => Some((to.clone(), deposit.as_yoctonear())),
                _ => None,
            })
        })
        .collect()
}

/// Names of the NEP-297 events the last call emitted.
pub fn events() -> Vec<String> {
    get_logs()
        .iter()
        .filter_map(|l| l.strip_prefix("EVENT_JSON:"))
        .map(|j| {
            let v: serde_json::Value = serde_json::from_str(j).expect("event is valid JSON");
            assert_eq!(v["standard"], "sphinx");
            v["event"].as_str().unwrap().to_owned()
        })
        .collect()
}

/// A test box public key: 32 bytes as 64 hex characters.
pub fn box_key() -> String {
    "b0".repeat(32)
}

/// The test grader key as 64 hex characters, as `commit_suite` expects it.
pub fn gk() -> String {
    hex::encode(grader_key().verifying_key().to_bytes())
}

/// A freshly initialised contract with the test grader key registered.
pub fn contract() -> Sphinx {
    ctx(&owner(), 1, START_NS);
    let mut c = Sphinx::new(owner(), verifier(), 500, true).unwrap();
    c.set_grader_key(
        hex::encode(grader_key().verifying_key().to_bytes()),
        box_key(),
        KeyMode::Dev,
        Hash32::digest(b"grader.wasm").to_string(),
        "test".into(),
    )
    .unwrap();
    c
}

/// Posts a job from `buyer()` with caps (per agent, total) and returns its id.
pub fn post(c: &mut Sphinx, per_agent: u32, total: u32) -> U64 {
    ctx(&buyer(), REWARD + JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
    c.post_job(
        "ipfs://spec".into(),
        RUNTIME_JS_BOA_V1.into(),
        U128(REWARD),
        U128(BOND),
        per_agent,
        total,
        U64(DEADLINE_NS),
        None,
    )
    .unwrap()
}

pub fn suite_check_for(c: &Sphinx, job: U64, valid: bool) -> SuiteCheck {
    let commit = c.get_job(job).unwrap().commit.unwrap();
    SuiteCheck {
        contract: CONTRACT.into(),
        job_id: job.0,
        valid,
        suite_hash: commit.suite_hash,
        ref_solution_hash: commit.ref_solution_hash,
        nonce: commit.suite_check_nonce,
    }
}

/// Posts, commits and validates a job; returns its id in `Open`.
pub fn open_job(c: &mut Sphinx, per_agent: u32, total: u32) -> U64 {
    let job = post(c, per_agent, total);
    ctx(&buyer(), 0, START_NS + 1);
    c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    let v = suite_check_for(c, job, true);
    ctx(&acct("anyone"), 0, START_NS + 2);
    c.record_suite_check(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap();
    job
}

pub fn submit(
    c: &mut Sphinx,
    job: U64,
    agent: &AccountId,
    code_hash: Hash32,
    now: u64,
) -> SubmitReceipt {
    ctx(agent, BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), now);
    c.submit(job, format!("ipfs://{code_hash}"), code_hash.to_string()).unwrap()
}

pub fn score_for(
    job: U64,
    receipt: &SubmitReceipt,
    code_hash: Hash32,
    passed: u32,
    total: u32,
) -> Score {
    Score {
        contract: CONTRACT.into(),
        job_id: job.0,
        sub_id: receipt.sub_id,
        runtime: RUNTIME_JS_BOA_V1.into(),
        passed,
        total,
        suite_hash: suite_hash(),
        code_hash,
        nonce: receipt.nonce,
    }
}

/// Records a correctly signed result, posted by an arbitrary account.
pub fn grade(c: &mut Sphinx, v: &Score, now: u64) {
    ctx(&acct("relayer"), 0, now);
    c.record_score(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap();
}

pub const fn after_window(t: u64) -> u64 {
    t + GRADING_WINDOW_NS
}

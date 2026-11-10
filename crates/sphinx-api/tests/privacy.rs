//! Who may read stored code: the relayer, the submitting agent, and the
//! buyer of a paid job for the winning code. Task text is public, and so is
//! a hidden suite once every job that committed it is finished.
#![allow(
    clippy::unwrap_used,
    clippy::unused_async_trait_impl,
    clippy::cast_possible_truncation,
    reason = "test fixtures"
)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sphinx_api::chain::Chain;
use sphinx_api::code_store::CodeStore;
use sphinx_api::{router, AppState, READER_HEADER};
use sphinx_near::near_api::types::crypto::secret_key::SecretKey;
use sphinx_near::nep413;
use sphinx_near::views::{Config, Job, Submission};
use sphinx_protocol::{Code, Hash32, SphinxError};
use std::sync::Arc;
use tower::ServiceExt;

const AGENT_KEY: u8 = 1;
const BUYER_KEY: u8 = 2;
const STRANGER_KEY: u8 = 3;

/// A test key from a fixed seed.
fn key(seed: u8) -> SecretKey {
    SecretKey::ED25519(
        sphinx_near::near_api::types::crypto::secret_key::ED25519SecretKey::from_secret_key(
            [seed; 32],
        ),
    )
}

struct FakeChain {
    jobs: Vec<Job>,
    subs: Vec<Submission>,
    keys: Vec<(String, String)>,
}

fn job(id: u64, status: &str, spec: &str, winning: Option<u32>, subs: u32) -> Job {
    serde_json::from_value(json!({
        "id": id.to_string(), "buyer": "buyer.testnet", "spec_uri": spec, "runtime": "js-boa/1",
        "reward": "1", "bond": "1", "fee_bps": 500, "max_subs_per_agent": 2, "max_subs_total": 5,
        "deadline_ns": "1", "created_at_ns": "0", "status": status, "commit": null,
        "sub_count": subs, "pending_count": 0, "graded_count": subs,
        "winner": null, "winning_sub_id": winning
    }))
    .unwrap()
}

/// A job committed to `suite` (and a reference solution).
fn committed(id: u64, status: &str, suite: &[u8], reference: &[u8]) -> Job {
    let mut j = serde_json::to_value(job(id, status, "repo:spec.md", None, 0)).unwrap();
    j["commit"] = json!({
        "suite_hash": Hash32::digest(suite).to_string(),
        "ref_solution_hash": Hash32::digest(reference).to_string(),
        "suite_check_nonce": "ab".repeat(32),
        "grader": {
            "public_key": "00".repeat(32), "box_public_key": "", "mode": "outlayer_tdx",
            "wasm_sha256": "11".repeat(32), "attestation_ref": "x", "registered_at_ns": "0"
        }
    });
    serde_json::from_value(j).unwrap()
}

fn sub(job: u64, id: u32, agent: &str, code: &[u8], status: &str) -> Submission {
    serde_json::from_value(json!({
        "job_id": job.to_string(), "sub_id": id, "agent": agent, "code_uri": "x",
        "code_hash": Hash32::digest(code).to_string(), "nonce": "cd".repeat(32),
        "status": status, "passed": 0, "total": 1, "bond": "1",
        "submitted_at_ns": "0", "settled_at_ns": null
    }))
    .unwrap()
}

impl Chain for FakeChain {
    fn contract(&self) -> &'static str {
        "escrow.testnet"
    }
    async fn config(&self) -> Result<Config, SphinxError> {
        Err(SphinxError::new(Code::RelayRpcFailed, "offline"))
    }
    async fn jobs(&self, from: u64, limit: u32) -> Result<Vec<Job>, SphinxError> {
        Ok(self.jobs.iter().skip(from as usize).take(limit as usize).cloned().collect())
    }
    async fn job(&self, id: u64) -> Result<Job, SphinxError> {
        Ok(self.jobs[id as usize].clone())
    }
    async fn submissions(
        &self,
        job: u64,
        from: u32,
        limit: u32,
    ) -> Result<Vec<Submission>, SphinxError> {
        Ok(self
            .subs
            .iter()
            .filter(|s| s.job_id == job.to_string())
            .skip(from as usize)
            .take(limit as usize)
            .cloned()
            .collect())
    }
    async fn submission(&self, _: u64, _: u32) -> Result<Submission, SphinxError> {
        Err(SphinxError::new(Code::ScoreNoSubmission, "none"))
    }
    async fn agents(&self, _: u32) -> Result<Vec<sphinx_near::views::AgentView>, SphinxError> {
        Ok(Vec::new())
    }
    async fn agent(&self, _: &str) -> Result<Option<sphinx_near::views::AgentView>, SphinxError> {
        Ok(None)
    }
    async fn agent_stats(&self, _: &str) -> Result<sphinx_near::views::AgentStats, SphinxError> {
        Ok(sphinx_near::views::AgentStats::default())
    }
    async fn invites(&self, _: u64) -> Result<Vec<String>, SphinxError> {
        Ok(Vec::new())
    }
    async fn is_access_key(&self, account: &str, public_key: &str) -> Result<bool, SphinxError> {
        Ok(self.keys.iter().any(|(a, k)| a == account && k == public_key))
    }
    async fn check(&self, _: &'static str, _: Value) -> Result<(), SphinxError> {
        Ok(())
    }
    async fn access_key(
        &self,
        _: &sphinx_near::near_api::types::transaction::delegate_action::SignedDelegateAction,
    ) -> Result<
        (Option<sphinx_near::near_api::types::transaction::actions::AccessKey>, u64),
        SphinxError,
    > {
        Ok((None, 0))
    }
}

const PASSED: &[u8] = b"function solve(){ return 1 } // winner";
const FAILED: &[u8] = b"function solve(){ return 0 } // near miss";
const SPEC: &[u8] = b"Write solve() returning 1.";
const LOOSE: &[u8] = b"function solve(){ return 2 } // uploaded, never submitted";
/// Committed only by finished jobs: public.
const SUITE_DONE: &[u8] =
    br#"{"version":1,"runtime":"js-boa/1","entry":"solve","tests":[{"args":[2],"expected":2}]}"#;
/// Committed by a finished job and by a reposted one still open: hidden.
const SUITE_REUSED: &[u8] =
    br#"{"version":1,"runtime":"js-boa/1","entry":"solve","tests":[{"args":[3],"expected":3}]}"#;
/// Committed by an open job only: hidden.
const SUITE_LIVE: &[u8] =
    br#"{"version":1,"runtime":"js-boa/1","entry":"solve","tests":[{"args":[4],"expected":4}]}"#;
/// Uploaded, committed by no job: hidden.
const SUITE_NONE: &[u8] =
    br#"{"version":1,"runtime":"js-boa/1","entry":"solve","tests":[{"args":[5],"expected":5}]}"#;
const REFERENCE: &[u8] = b"function solve(){ return 1 } // reference";
/// The reference of job 1 only, which has ended.
const REFERENCE_ENDED: &[u8] = b"function solve(){ return 1 } // reference of an ended job";

/// Job 0 is paid to agent-b's submission 1; agent-a's submission 0 failed.
async fn app() -> axum::Router {
    let dir = std::env::temp_dir().join(format!("sphinx-privacy-{}", std::process::id()));
    let code = CodeStore::new(dir).with_reader_token(Some("relayer-token-0123456789".into()));
    for bytes in [
        PASSED,
        FAILED,
        SPEC,
        LOOSE,
        SUITE_DONE,
        SUITE_REUSED,
        SUITE_LIVE,
        SUITE_NONE,
        REFERENCE,
        REFERENCE_ENDED,
    ] {
        code.put(bytes, "setup").await.unwrap();
    }
    let spec_uri = format!("https://api.example/v1/code/{}", Hash32::digest(SPEC));
    let keys = [("agent-a.testnet", AGENT_KEY), ("buyer.testnet", BUYER_KEY)]
        .map(|(a, k)| (a.to_owned(), key(k).public_key().to_string()))
        .to_vec();
    router(Arc::new(AppState {
        chain: FakeChain {
            jobs: vec![
                job(0, "Paid", &spec_uri, Some(1), 2),
                committed(1, "Refunded", SUITE_DONE, REFERENCE_ENDED),
                committed(2, "Paid", SUITE_REUSED, REFERENCE),
                committed(3, "Open", SUITE_REUSED, REFERENCE),
                committed(4, "Open", SUITE_LIVE, REFERENCE),
                // An attacker's jobs naming other people's hashes: as task
                // text (suffix and exact forms) and as a committed suite.
                job(5, "Draft", &format!("x/{}", Hash32::digest(FAILED)), None, 0),
                job(
                    6,
                    "Cancelled",
                    &format!("https://evil.example/v1/code/{}", Hash32::digest(PASSED)),
                    None,
                    0,
                ),
                job(
                    7,
                    "Cancelled",
                    &format!("https://evil.example/v1/code/{}", Hash32::digest(REFERENCE)),
                    None,
                    0,
                ),
                committed(8, "Cancelled", FAILED, LOOSE),
                committed(9, "Cancelled", REFERENCE, LOOSE),
            ],
            subs: vec![
                sub(0, 0, "agent-a.testnet", FAILED, "Failed"),
                sub(0, 1, "agent-b.testnet", PASSED, "Passed"),
            ],
            keys,
        },
        code,
        public_base: "https://api.example".into(),
        relay: None,
        uids: sphinx_api::uids::UidIndex::default(),
    }))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// GET the code with `bytes`, signed as `account` with `secret` if given.
async fn read(
    app: axum::Router,
    bytes: &[u8],
    who: Option<(&str, u8)>,
    token: Option<&str>,
) -> StatusCode {
    let hash = Hash32::digest(bytes).to_string();
    let mut req = Request::builder().uri(format!("/v1/code/{hash}"));
    if let Some((account, secret)) = who {
        let header = nep413::sign_header(
            account,
            &key(secret),
            &nep413::code_message(&hash, now_ms()),
            [9; 32],
        );
        req = req.header(nep413::HEADER, header);
    }
    if let Some(t) = token {
        req = req.header(READER_HEADER, t);
    }
    let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let _ = res.into_body().collect().await;
    status
}

#[tokio::test]
async fn task_text_is_public() {
    assert_eq!(read(app().await, SPEC, None, None).await, StatusCode::OK);
}

#[tokio::test]
async fn nobody_anonymous_reads_submitted_or_loose_code() {
    for bytes in [PASSED, FAILED, LOOSE] {
        assert_eq!(read(app().await, bytes, None, None).await, StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn an_agent_reads_its_own_submission_and_nothing_else() {
    let agent = Some(("agent-a.testnet", AGENT_KEY));
    assert_eq!(read(app().await, FAILED, agent, None).await, StatusCode::OK);
    assert_eq!(read(app().await, PASSED, agent, None).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_buyer_reads_the_winning_code_of_a_paid_job_but_not_failed_attempts() {
    let buyer = Some(("buyer.testnet", BUYER_KEY));
    assert_eq!(read(app().await, PASSED, buyer, None).await, StatusCode::OK);
    assert_eq!(read(app().await, FAILED, buyer, None).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_key_that_is_not_the_accounts_or_a_forged_name_is_refused() {
    // A valid signature, but the key is not one of agent-a's access keys.
    let stranger = Some(("agent-a.testnet", STRANGER_KEY));
    assert_eq!(read(app().await, FAILED, stranger, None).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_relayer_token_reads_everything_and_a_wrong_token_nothing() {
    for bytes in [PASSED, FAILED, LOOSE] {
        assert_eq!(
            read(app().await, bytes, None, Some("relayer-token-0123456789")).await,
            StatusCode::OK
        );
    }
    assert_eq!(
        read(app().await, FAILED, None, Some("relayer-token-012345678")).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(read(app().await, FAILED, None, Some("x")).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_suite_is_public_once_every_job_that_committed_it_is_finished() {
    assert_eq!(read(app().await, SUITE_DONE, None, None).await, StatusCode::OK);
}

#[tokio::test]
async fn a_suite_stays_hidden_while_any_job_using_it_is_live_or_none_committed_it() {
    for bytes in [SUITE_REUSED, SUITE_LIVE, SUITE_NONE] {
        assert_eq!(read(app().await, bytes, None, None).await, StatusCode::NOT_FOUND);
    }
    // Not even the buyer reads a live suite through the API.
    assert_eq!(
        read(app().await, SUITE_LIVE, Some(("buyer.testnet", BUYER_KEY)), None).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_reference_solution_is_never_served() {
    // Every job that committed REFERENCE_ENDED has ended, and it is stored,
    // yet it is the buyer's paid code: never public. Not even to the buyer
    // through this rule.
    assert_eq!(read(app().await, REFERENCE_ENDED, None, None).await, StatusCode::NOT_FOUND);
    assert_eq!(read(app().await, REFERENCE, None, None).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn naming_someone_elses_hash_in_a_job_does_not_open_it() {
    // Jobs 5-9 point their task text or committed suite at a failed
    // submission, the winning submission and a live reference, then end.
    for bytes in [FAILED, PASSED, REFERENCE, LOOSE] {
        assert_eq!(read(app().await, bytes, None, None).await, StatusCode::NOT_FOUND);
    }
    // Task text named exactly is still public.
    assert_eq!(read(app().await, SPEC, None, None).await, StatusCode::OK);
}

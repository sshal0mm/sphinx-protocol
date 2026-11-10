//! The job API against an in-memory chain.
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
use sphinx_api::{router, AppState};
use sphinx_near::views::{Config, Job, JobStatus, Submission};
use sphinx_protocol::{Code, SphinxError};
use std::sync::Arc;
use tower::ServiceExt;

struct FakeChain {
    jobs: Vec<Job>,
}

fn job(id: u64, status: JobStatus) -> Job {
    serde_json::from_value(json!({
        "id": id.to_string(), "buyer": "buyer.testnet", "spec_uri": "repo:spec.md", "runtime": "js-boa/1",
        "reward": "300000000000000000000000", "bond": "10000000000000000000000", "fee_bps": 500,
        "max_subs_per_agent": 2, "max_subs_total": 5, "deadline_ns": "1", "created_at_ns": "0",
        "status": status, "commit": null, "sub_count": 0, "pending_count": 0, "graded_count": 0,
        "winner": null, "winning_sub_id": null
    }))
    .unwrap()
}

impl Chain for FakeChain {
    async fn config(&self) -> Result<Config, SphinxError> {
        Err(SphinxError::new(Code::RelayRpcFailed, "get_config failed: offline"))
    }
    async fn jobs(&self, from: u64, limit: u32) -> Result<Vec<Job>, SphinxError> {
        Ok(self.jobs.iter().skip(from as usize).take(limit as usize).cloned().collect())
    }
    async fn job(&self, id: u64) -> Result<Job, SphinxError> {
        self.jobs
            .get(id as usize)
            .cloned()
            .ok_or_else(|| SphinxError::new(Code::JobNotFound, format!("job {id} does not exist")))
    }
    fn contract(&self) -> &'static str {
        "escrow.testnet"
    }
    async fn agents(&self, from: u32) -> Result<Vec<sphinx_near::views::AgentView>, SphinxError> {
        let solver = serde_json::from_value(json!({
            "profile": {
                "account": "solver.testnet", "name": "solver", "about": "", "skills": [],
                "min_reward": "0", "contact": "", "registered_at_ns": "1", "updated_at_ns": "1"
            },
            "stats": { "submissions": 0, "passes": 0, "fails": 0, "expired": 0, "earned": "0", "bonds_lost": "0" }
        }))
        .unwrap();
        Ok(if from == 0 { vec![solver] } else { Vec::new() })
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
    async fn submissions(&self, _: u64, _: u32, _: u32) -> Result<Vec<Submission>, SphinxError> {
        Ok(Vec::new())
    }
    async fn submission(&self, job: u64, sub: u32) -> Result<Submission, SphinxError> {
        Err(SphinxError::new(Code::ScoreNoSubmission, format!("job {job} has no submission {sub}")))
    }
    async fn check(&self, _view: &'static str, _args: Value) -> Result<(), SphinxError> {
        Ok(())
    }
    async fn is_access_key(&self, _: &str, _: &str) -> Result<bool, SphinxError> {
        Ok(false)
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

fn app() -> axum::Router {
    let dir = std::env::temp_dir().join(format!("sphinx-api-test-{}", std::process::id()));
    router(Arc::new(AppState {
        chain: FakeChain {
            jobs: vec![job(0, JobStatus::Paid), job(1, JobStatus::Open), job(2, JobStatus::Open)],
        },
        code: CodeStore::new(dir),
        public_base: "https://api.example".into(),
        relay: None,
        uids: sphinx_api::uids::UidIndex::default(),
    }))
}

async fn call(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Body,
) -> (StatusCode, Value, Vec<u8>) {
    let res =
        app.oneshot(Request::builder().method(method).uri(uri).body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, bytes)
}

#[tokio::test]
async fn lists_and_filters_jobs() {
    let (s, v, _) = call(app(), "GET", "/v1/jobs/open", Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<&str> =
        v["jobs"].as_array().unwrap().iter().map(|j| j["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["1", "2"]);
    assert_eq!(v["next"], Value::Null);
    let (s, v, _) = call(app(), "GET", "/v1/jobs/paid/after/0", Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["jobs"].as_array().unwrap().len(), 1);
    let (s, _, _) = call(app(), "GET", "/v1/jobs/nonsense/after/0", Body::empty()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_things_are_404_with_catalog_bodies() {
    let missing_job = sphinx_near::uid::job_uid("escrow.testnet", 9);
    let (s, v, _) = call(app(), "GET", &format!("/v1/jobs/{missing_job}"), Body::empty()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["code"], "SPHINX_E_JOB_NOT_FOUND");
    assert!(v["hint"].as_str().unwrap().len() > 5);
    let missing_sub = sphinx_near::uid::submission_uid("escrow.testnet", 1, 0);
    let (s, v, _) =
        call(app(), "GET", &format!("/v1/submissions/{missing_sub}"), Body::empty()).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("SPHINX_E_SCORE_NO_SUBMISSION"))
    );
}

#[tokio::test]
async fn upstream_failures_are_502() {
    let (s, v, _) = call(app(), "GET", "/v1/config", Body::empty()).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert_eq!(v["code"], "SPHINX_E_RELAY_RPC_FAILED");
    assert_eq!(v["retry"], "transient");
}

#[tokio::test]
async fn code_is_stored_by_hash_and_not_served_to_anonymous_callers() {
    let code = "function solve(){ return 42 }";
    let (s, v, _) = call(app(), "POST", "/v1/code", Body::from(code)).await;
    assert_eq!(s, StatusCode::CREATED);
    let hash = v["code_hash"].as_str().unwrap().to_owned();
    assert_eq!(hash, sphinx_protocol::Hash32::digest(code.as_bytes()).to_string());
    assert_eq!(v["code_uri"], format!("https://api.example/v1/code/{hash}"));
    // Code that no job uses as its task text is private (tests/privacy.rs has
    // the read rules).
    let (s, _, _) = call(app(), "GET", &format!("/v1/code/{hash}"), Body::empty()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn code_errors_are_coded() {
    let (s, v, _) = call(app(), "POST", "/v1/code", Body::from(vec![b'x'; 70_000])).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("SPHINX_E_GRADER_CODE_TOO_LARGE"))
    );
    let (s, v, _) =
        call(app(), "GET", &format!("/v1/code/{}", "ab".repeat(32)), Body::empty()).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("SPHINX_E_API_CODE_NOT_FOUND"))
    );
    let (s, v, _) = call(app(), "GET", "/v1/code/NOT-A-HASH", Body::empty()).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("SPHINX_E_API_BAD_REQUEST"))
    );
}

#[tokio::test]
async fn explains_error_codes() {
    let (s, v, _) = call(app(), "GET", "/v1/errors/SPHINX_E_SUBMIT_AGENT_CAP", Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((v["retry"].as_str(), v["bond_effect"].as_str()), (Some("never"), Some("none")));
    let (s, _, _) = call(app(), "GET", "/v1/errors/NOPE", Body::empty()).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn relay_is_off_without_a_relayer_key() {
    let (s, v, _) =
        call(app(), "POST", "/v1/relay", Body::from(r#"{"signed_delegate_action":"AAAA"}"#)).await;
    assert_eq!(
        (s, v["code"].as_str()),
        (StatusCode::CONFLICT, Some("SPHINX_E_API_RELAY_DISABLED"))
    );
}

#[tokio::test]
async fn a_jobs_submissions_are_listed_and_unknown_jobs_are_404() {
    let job = sphinx_near::uid::job_uid("escrow.testnet", 1);
    let (status, body, _) =
        call(app(), "GET", &format!("/v1/jobs/{job}/submissions"), Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "submissions": [], "next": null }));
    let missing = sphinx_near::uid::job_uid("escrow.testnet", 9);
    let (status, body, _) =
        call(app(), "GET", &format!("/v1/jobs/{missing}/submissions"), Body::empty()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "SPHINX_E_JOB_NOT_FOUND");
}

#[tokio::test]
async fn every_error_code_is_listed() {
    let (status, body, _) = call(app(), "GET", "/v1/errors", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    let errors = body["errors"].as_array().unwrap();
    assert_eq!(errors.len(), Code::ALL.len());
    assert!(errors
        .iter()
        .any(|e| e["code"] == "SPHINX_E_SUBMIT_AGENT_CAP" && e["retry"] == "never"));
}

#[tokio::test]
async fn jobs_are_addressed_by_uid_never_by_number() {
    let (_, body, _) = call(app(), "GET", "/v1/jobs", Body::empty()).await;
    let uid = body["jobs"][1]["uid"].as_str().unwrap().to_owned();
    assert_eq!(uid, sphinx_near::uid::job_uid("escrow.testnet", 1));
    let (status, job, _) = call(app(), "GET", &format!("/v1/jobs/{uid}"), Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((job["id"].as_str(), job["uid"].as_str()), (Some("1"), Some(uid.as_str())));
    for path in
        ["/v1/jobs/1", "/v1/jobs/not-a-uid", "/v1/jobs/00000000-0000-8000-8000-000000000000"]
    {
        let (status, _, _) = call(app(), "GET", path, Body::empty()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn the_directory_and_any_accounts_record_are_served() {
    let (s, v, _) = call(app(), "GET", "/v1/agents", Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    let uid = sphinx_near::uid::agent_uid("escrow.testnet", "solver.testnet");
    assert_eq!(v["agents"][0]["uid"], uid.as_str());
    assert_eq!(v["agents"][0]["profile"]["account"], "solver.testnet");
    assert_eq!(v["next"], Value::Null);
    let (s, v, _) = call(app(), "GET", &format!("/v1/agents/{uid}"), Body::empty()).await;
    assert_eq!(
        (s, v["account"].as_str(), v["uid"].as_str()),
        (StatusCode::OK, Some("solver.testnet"), Some(uid.as_str()))
    );
    let unknown = sphinx_near::uid::agent_uid("escrow.testnet", "nobody.testnet");
    let (s, _, _) = call(app(), "GET", &format!("/v1/agents/{unknown}"), Body::empty()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v, _) = call(app(), "GET", "/v1/agents/agent-a.testnet", Body::empty()).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!((v["account"].as_str(), v["profile"].is_null()), (Some("agent-a.testnet"), true));
    assert_eq!(v["stats"]["passes"], 0);
    let (s, _, _) = call(app(), "GET", "/v1/agents/UPPER-CASE.testnet", Body::empty()).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn whole_lists_for_an_agent_and_the_newest_results_are_one_request() {
    let (s, v, _) = call(app(), "GET", "/v1/submissions/recent", Body::empty()).await;
    assert_eq!((s, v), (StatusCode::OK, json!({ "submissions": [] })));
    let uid = sphinx_near::uid::agent_uid("escrow.testnet", "solver.testnet");
    let (s, v, _) =
        call(app(), "GET", &format!("/v1/agents/{uid}/submissions"), Body::empty()).await;
    assert_eq!((s, v), (StatusCode::OK, json!({ "account": "solver.testnet", "submissions": [] })));
    let (s, _, _) = call(app(), "GET", "/v1/agents/UPPER.testnet/submissions", Body::empty()).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

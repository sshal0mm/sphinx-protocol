//! Sphinx job API. Plain HTTP + JSON over the escrow so any agent framework
//! can find jobs, store submission code and follow a submission to its score.
//! Every error body is the shared `ErrorReport`.

pub mod cache;
pub mod chain;
pub mod code_store;
pub mod error;
pub mod limits;
pub mod uids;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sphinx_near::views::{Job, JobStatus, Submission};
use sphinx_protocol::catalog::Code;
use sphinx_protocol::{Hash32, SphinxError};
use std::sync::Arc;
use std::time::Instant;

use crate::chain::Chain;
use crate::code_store::CodeStore;
use crate::error::{ApiError, ApiResult};

pub const API_VERSION: &str = "sphinx.api/1";

/// Pays gas for agents' signed calls (NEP-366). Absent when the instance has
/// no relayer key, in which case `/v1/relay` answers `SPHINX_E_API_RELAY_DISABLED`.
pub struct Relay {
    pub escrow: sphinx_near::Escrow,
    pub relayer: sphinx_near::near_api::types::AccountId,
    pub signer: Arc<sphinx_near::near_api::Signer>,
    pub limiter: limits::RelayLimiter,
    /// A rate per client (the address the web proxy reports), applied before
    /// any check or RPC lookup, so junk requests cannot use up the shared rate.
    pub clients: limits::ClientLimiter,
    /// Most relayed transactions per day, after which the relay stops until
    /// the next day: a hard bound on the gas it can spend.
    pub daily: limits::DailyCap,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay").field("relayer", &self.relayer).finish_non_exhaustive()
    }
}

/// Shared handler state.
#[derive(Debug)]
pub struct AppState<C> {
    pub chain: C,
    pub code: CodeStore,
    /// Public base URL of this API, used to build `code_uri` values.
    pub public_base: String,
    pub relay: Option<Relay>,
    pub uids: uids::UidIndex,
}

/// Builds the router. Every address is a plain path: lists are filtered and
/// paged by path segments, and single jobs and submissions are addressed by
/// their public identifiers (`sphinx_near::uid`).
pub fn router<C: Chain>(state: Arc<AppState<C>>) -> Router {
    Router::new()
        .route("/v1", get(index))
        .route("/v1/config", get(config::<C>))
        .route("/v1/jobs", get(list_all::<C>))
        .route("/v1/jobs/{key}", get(jobs_key::<C>))
        .route("/v1/jobs/{filter}/after/{from}", get(list_after::<C>))
        .route("/v1/jobs/{uid}/submissions", get(submissions_first::<C>))
        .route("/v1/jobs/{uid}/submissions/after/{from}", get(submissions_after::<C>))
        .route("/v1/submissions/recent", get(recent_submissions::<C>))
        .route("/v1/submissions/{uid}", get(submission_by_uid::<C>))
        .route("/v1/agents", get(agents_first::<C>))
        .route("/v1/agents/after/{from}", get(agents_after::<C>))
        .route("/v1/agents/{account}", get(agent_by_account::<C>))
        .route("/v1/agents/{account}/submissions", get(agent_submissions::<C>))
        .route("/v1/code", post(put_code::<C>))
        .route("/v1/code/{hash}", get(get_code::<C>))
        .route("/v1/errors", get(list_errors))
        .route("/v1/errors/{code}", get(get_error))
        .route("/v1/relay", post(relay::<C>))
        .with_state(state)
}

async fn index() -> Json<Value> {
    Json(json!({
        "api": API_VERSION,
        "endpoints": [
            "GET /v1/config",
            "GET /v1/jobs", "GET /v1/jobs/{filter}", "GET /v1/jobs/{filter}/after/{from}",
            "GET /v1/jobs/{job_uid}", "GET /v1/jobs/{job_uid}/submissions",
            "GET /v1/jobs/{job_uid}/submissions/after/{from}", "GET /v1/submissions/{submission_uid}",
            "GET /v1/agents", "GET /v1/agents/after/{from}", "GET /v1/agents/{agent_uid_or_account}",
            "GET /v1/agents/{agent_uid_or_account}/submissions", "GET /v1/submissions/recent",
            "POST /v1/code", "GET /v1/code/{hash}", "GET /v1/errors", "GET /v1/errors/{code}", "POST /v1/relay"
        ],
        "filters": ["all", "open", "paid", "committed", "draft", "rejected", "refunded", "cancelled"]
    }))
}

async fn config<C: Chain>(State(s): State<Arc<AppState<C>>>) -> ApiResult<impl IntoResponse> {
    Ok(Json(s.chain.config().await?))
}

/// Most items one page holds.
const PAGE: u32 = 50;

/// Which jobs a list path keeps.
#[derive(Debug, Clone, Copy)]
enum Filter {
    All,
    Only(JobStatus),
}

impl Filter {
    /// The filter a path segment names, if any.
    fn named(word: &str) -> Option<Self> {
        Some(match word {
            "all" => Self::All,
            "open" => Self::Only(JobStatus::Open),
            "paid" => Self::Only(JobStatus::Paid),
            "committed" => Self::Only(JobStatus::Committed),
            "draft" => Self::Only(JobStatus::Draft),
            "rejected" => Self::Only(JobStatus::Rejected),
            "refunded" => Self::Only(JobStatus::Refunded),
            "cancelled" => Self::Only(JobStatus::Cancelled),
            _ => return None,
        })
    }

    fn keeps(self, job: &Job) -> bool {
        match self {
            Self::All => true,
            Self::Only(status) => job.status == status,
        }
    }
}

fn not_found(what: &str) -> ApiError {
    ApiError(
        SphinxError::new(Code::JobNotFound, format!("{what} does not exist"))
            .ctx("hint", "list jobs at /v1/jobs and use the uid each one carries"),
    )
}

#[derive(Debug, Serialize)]
struct JobList {
    jobs: Vec<Value>,
    /// The next page's address, if there may be more.
    next: Option<String>,
}

/// One page of jobs with ids from `from`, keeping those that match `status`.
async fn job_page<C: Chain>(
    s: &AppState<C>,
    name: &str,
    filter: Filter,
    from: u64,
) -> ApiResult<Json<JobList>> {
    let page = s.chain.jobs(from, PAGE).await?;
    let next = (page.len() == PAGE as usize)
        .then(|| format!("/v1/jobs/{name}/after/{}", from.saturating_add(u64::from(PAGE))));
    let contract = s.chain.contract();
    let jobs = page.iter().filter(|j| filter.keeps(j)).map(|j| job_json(contract, j)).collect();
    Ok(Json(JobList { jobs, next }))
}

async fn list_all<C: Chain>(State(s): State<Arc<AppState<C>>>) -> ApiResult<Json<JobList>> {
    job_page(&s, "all", Filter::All, 0).await
}

async fn list_after<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    Path((name, from)): Path<(String, u64)>,
) -> ApiResult<Json<JobList>> {
    let filter = Filter::named(&name).ok_or_else(|| not_found(&format!("list {name}")))?;
    job_page(&s, &name, filter, from).await
}

/// `/v1/jobs/{key}`: a filtered list when `key` names a filter, else the job
/// whose uid it is.
async fn jobs_key<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    if let Some(filter) = Filter::named(&key) {
        let Json(list) = job_page(&s, &key, filter, 0).await?;
        return Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)));
    }
    let id = job_for_uid(&s, &key).await?;
    let (job, invited) = tokio::try_join!(s.chain.job(id), s.chain.invites(id))?;
    let mut job = job_json(s.chain.contract(), &job);
    job["invited"] = json!(invited);
    Ok(Json(job))
}

#[derive(Debug, Serialize)]
struct AgentList {
    agents: Vec<Value>,
    /// The next page's address, if there may be more.
    next: Option<String>,
}

async fn agent_page<C: Chain>(s: &AppState<C>, from: u32) -> ApiResult<Json<AgentList>> {
    let agents = s.chain.agents(from).await?;
    let next = (agents.len() == PAGE as usize)
        .then(|| format!("/v1/agents/after/{}", from.saturating_add(PAGE)));
    let contract = s.chain.contract();
    let agents = agents
        .into_iter()
        .map(|a| {
            json!({
                "uid": sphinx_near::uid::agent_uid(contract, &a.profile.account),
                "profile": a.profile,
                "stats": a.stats,
            })
        })
        .collect();
    Ok(Json(AgentList { agents, next }))
}

async fn agents_first<C: Chain>(State(s): State<Arc<AppState<C>>>) -> ApiResult<Json<AgentList>> {
    agent_page(&s, 0).await
}

async fn agents_after<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    Path(from): Path<u32>,
) -> ApiResult<Json<AgentList>> {
    agent_page(&s, from).await
}

/// `{ "uid", "account", "profile": … or null, "stats": … }` for an agent
/// uid or any NEAR account: the record is kept for every account that has
/// submitted, registered or not.
async fn agent_by_account<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    Path(agent): Path<String>,
) -> ApiResult<Json<Value>> {
    let account = agent_account(&s, &agent).await?;
    let (profile, stats) =
        tokio::try_join!(s.chain.agent(&account), s.chain.agent_stats(&account))?;
    Ok(Json(json!({
        "uid": sphinx_near::uid::agent_uid(s.chain.contract(), &account),
        "account": account,
        "profile": profile.map(|v| v.profile),
        "stats": stats,
    })))
}

/// The account an agent uid names, or `key` itself when it is an account id.
async fn agent_account<C: Chain>(s: &AppState<C>, key: &str) -> ApiResult<String> {
    if sphinx_near::uid::is_uid(key) {
        return match s.uids.resolve(&s.chain, key).await? {
            Some(uids::Target::Agent(account)) => Ok(account),
            _ => Err(not_found(&format!("agent {key}"))),
        };
    }
    let parsed: sphinx_near::near_api::types::AccountId = key.parse().map_err(|_| {
        ApiError(SphinxError::new(
            Code::ApiBadRequest,
            format!("{key} is not a NEAR account id or an agent uid"),
        ))
    })?;
    Ok(parsed.to_string())
}

/// Whether a job can no longer change: finished, with nothing waiting for a score.
fn is_settled(job: &Job) -> bool {
    is_finished(job) && job.pending_count == 0
}

/// A page of a job's submissions, kept for good once the job is settled.
async fn submissions_of<C: Chain>(
    s: &AppState<C>,
    job: &Job,
    from: u32,
) -> Result<Vec<Submission>, SphinxError> {
    if is_settled(job) {
        s.chain.settled_submissions(job.id_u64(), from, PAGE).await
    } else {
        s.chain.submissions(job.id_u64(), from, PAGE).await
    }
}

/// Reads what the dashboard opens with (config, the directory and its
/// agents, every job and submission), so a cache in front of the chain is
/// warm before anyone asks.
///
/// # Errors
/// The first chain read that fails.
pub async fn warm<C: Chain>(s: &AppState<C>) -> Result<(), SphinxError> {
    use futures::stream::{self, StreamExt, TryStreamExt};
    s.chain.config().await?;
    let agents = s.chain.agents(0).await?;
    let accounts: Vec<String> = agents.into_iter().map(|a| a.profile.account).collect();
    stream::iter(accounts)
        .map(|account| async move {
            tokio::try_join!(s.chain.agent(&account), s.chain.agent_stats(&account)).map(|_| ())
        })
        .buffer_unordered(SCAN_CONCURRENCY)
        .try_collect::<Vec<()>>()
        .await?;
    // Only jobs that can still change are read again: a finished job with
    // nothing pending never changes, and public RPCs rate-limit heavy readers.
    let mut live = Vec::new();
    // Settled jobs' submissions are read once and then kept for good.
    let mut settled = Vec::new();
    let mut from = 0u64;
    loop {
        let page = s.chain.jobs(from, PAGE).await?;
        let n = page.len();
        for job in page {
            if !is_settled(&job) {
                live.push(job);
            } else if job.sub_count > 0 {
                settled.push(job);
            }
        }
        if n < PAGE as usize {
            break;
        }
        from += u64::from(PAGE);
    }
    stream::iter(live)
        .map(|job| async move {
            let id = job.id_u64();
            tokio::try_join!(s.chain.job(id), s.chain.invites(id), async {
                if job.sub_count > 0 {
                    s.chain.submissions(id, 0, PAGE).await.map(|_| ())
                } else {
                    Ok(())
                }
            })
            .map(|_| ())
        })
        .buffer_unordered(SCAN_CONCURRENCY)
        .try_collect::<Vec<()>>()
        .await?;
    stream::iter(settled)
        .map(|job| async move {
            let mut from = 0u32;
            while from < job.sub_count {
                submissions_of(s, &job, from).await?;
                from = from.saturating_add(PAGE);
            }
            Ok::<_, SphinxError>(())
        })
        .buffer_unordered(SCAN_CONCURRENCY)
        .try_collect::<Vec<()>>()
        .await?;
    Ok(())
}

/// Every hash something on chain refers to: submissions' code and jobs'
/// task text stored in this API.
///
/// # Errors
/// The first chain read that fails.
pub async fn referenced_code<C: Chain>(
    s: &AppState<C>,
) -> Result<std::collections::HashSet<Hash32>, SphinxError> {
    let mut refs: std::collections::HashSet<Hash32> = all_submissions(s)
        .await
        .map_err(|ApiError(e)| e)?
        .iter()
        .map(|(_, sub)| sub.code_hash)
        .collect();
    let mut from = 0u64;
    loop {
        let page = s.chain.jobs(from, PAGE).await?;
        let n = page.len();
        refs.extend(page.iter().filter_map(|j| {
            j.spec_uri.rsplit_once("/v1/code/").and_then(|(_, h)| h.parse::<Hash32>().ok())
        }));
        // Published suites are kept too.
        refs.extend(page.iter().filter_map(|j| j.commit.as_ref()).map(|c| c.suite_hash));
        if n < PAGE as usize {
            break;
        }
        from += u64::from(PAGE);
    }
    Ok(refs)
}

/// Chain reads a scan runs at once; kept low so public RPCs do not rate-limit us.
const SCAN_CONCURRENCY: usize = 3;

/// Every submission on the escrow with its job, newest first.
async fn all_submissions<C: Chain>(s: &AppState<C>) -> ApiResult<Vec<(Job, Submission)>> {
    use futures::stream::{self, StreamExt, TryStreamExt};
    let mut jobs = Vec::new();
    let mut from = 0u64;
    loop {
        let page = s.chain.jobs(from, PAGE).await?;
        let n = page.len();
        jobs.extend(page.into_iter().filter(|j| j.sub_count > 0));
        if n < PAGE as usize {
            break;
        }
        from += u64::from(PAGE);
    }
    let lists: Vec<Vec<(Job, Submission)>> = stream::iter(jobs)
        .map(|job| async move {
            let mut subs = Vec::new();
            let mut sub_from = 0u32;
            while sub_from < job.sub_count {
                let page = submissions_of(s, &job, sub_from).await?;
                if page.is_empty() {
                    break;
                }
                subs.extend(page.into_iter().map(|sub| (job.clone(), sub)));
                sub_from = sub_from.saturating_add(PAGE);
            }
            Ok::<_, SphinxError>(subs)
        })
        .buffer_unordered(SCAN_CONCURRENCY)
        .try_collect()
        .await?;
    let mut all: Vec<(Job, Submission)> = lists.into_iter().flatten().collect();
    all.sort_by_key(|(_, sub)| std::cmp::Reverse(sub.submitted_at_ns.parse::<u64>().unwrap_or(0)));
    Ok(all)
}

fn pair_json(contract: &str, job: &Job, sub: &Submission) -> Value {
    json!({ "submission": submission_json(contract, sub), "job": job_json(contract, job) })
}

/// `{ "account", "submissions": [{ "submission", "job" }] }`: everything one
/// agent submitted, newest first.
async fn agent_submissions<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    Path(agent): Path<String>,
) -> ApiResult<Json<Value>> {
    let account = agent_account(&s, &agent).await?;
    let contract = s.chain.contract();
    let list: Vec<Value> = all_submissions(&s)
        .await?
        .iter()
        .filter(|(_, sub)| sub.agent == account)
        .map(|(job, sub)| pair_json(contract, job, sub))
        .collect();
    Ok(Json(json!({ "account": account, "submissions": list })))
}

/// Scored results the recent list shows.
const RECENT: usize = 20;

/// `{ "submissions": [{ "submission", "job" }] }`: the newest scored
/// results across every job.
async fn recent_submissions<C: Chain>(State(s): State<Arc<AppState<C>>>) -> ApiResult<Json<Value>> {
    use sphinx_near::views::SubStatus;
    let contract = s.chain.contract();
    let mut scored: Vec<(Job, Submission)> = all_submissions(&s)
        .await?
        .into_iter()
        .filter(|(_, sub)| matches!(sub.status, SubStatus::Passed | SubStatus::Failed))
        .collect();
    let at = |s: &Submission| {
        s.settled_at_ns.as_deref().and_then(|t| t.parse::<u64>().ok()).unwrap_or(0)
    };
    scored.sort_by_key(|(_, sub)| std::cmp::Reverse(at(sub)));
    let list: Vec<Value> =
        scored.iter().take(RECENT).map(|(job, sub)| pair_json(contract, job, sub)).collect();
    Ok(Json(json!({ "submissions": list })))
}

/// The job id a job uid names.
async fn job_for_uid<C: Chain>(s: &AppState<C>, uid: &str) -> ApiResult<u64> {
    if !sphinx_near::uid::is_uid(uid) {
        return Err(not_found(&format!("job {uid}")));
    }
    match s.uids.resolve(&s.chain, uid).await? {
        Some(uids::Target::Job(id)) => Ok(id),
        _ => Err(not_found(&format!("job {uid}"))),
    }
}

/// A job as JSON, with its public identifier.
fn job_json(contract: &str, job: &Job) -> Value {
    let mut v = serde_json::to_value(job).unwrap_or(Value::Null);
    v["uid"] = sphinx_near::uid::job_uid(contract, job.id_u64()).into();
    v
}

/// A submission as JSON, with its own and its job's public identifiers.
fn submission_json(contract: &str, sub: &Submission) -> Value {
    let job_id = sub.job_id.parse().unwrap_or(0);
    let mut v = serde_json::to_value(sub).unwrap_or(Value::Null);
    v["uid"] = sphinx_near::uid::submission_uid(contract, job_id, sub.sub_id).into();
    v["job_uid"] = sphinx_near::uid::job_uid(contract, job_id).into();
    v["agent_uid"] = sphinx_near::uid::agent_uid(contract, &sub.agent).into();
    v
}

#[derive(Debug, Serialize)]
struct SubmissionList {
    submissions: Vec<Value>,
    /// The next page's address, if there may be more.
    next: Option<String>,
}

async fn submission_page<C: Chain>(
    s: &AppState<C>,
    uid: &str,
    from: u32,
) -> ApiResult<Json<SubmissionList>> {
    let id = job_for_uid(s, uid).await?;
    let job = s.chain.job(id).await?;
    let page = submissions_of(s, &job, from).await?;
    let end = from.saturating_add(PAGE);
    let next = (end < job.sub_count).then(|| format!("/v1/jobs/{uid}/submissions/after/{end}"));
    let contract = s.chain.contract();
    let submissions = page.iter().map(|x| submission_json(contract, x)).collect();
    Ok(Json(SubmissionList { submissions, next }))
}

async fn submissions_first<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    Path(uid): Path<String>,
) -> ApiResult<Json<SubmissionList>> {
    submission_page(&s, &uid, 0).await
}

async fn submissions_after<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    Path((uid, from)): Path<(String, u32)>,
) -> ApiResult<Json<SubmissionList>> {
    submission_page(&s, &uid, from).await
}

/// `{ "submission": …, "job": … }` for a submission uid.
async fn submission_by_uid<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    Path(uid): Path<String>,
) -> ApiResult<Json<Value>> {
    let missing = || {
        ApiError(SphinxError::new(
            Code::ScoreNoSubmission,
            format!("submission {uid} does not exist"),
        ))
    };
    if !sphinx_near::uid::is_uid(&uid) {
        return Err(missing());
    }
    let Some(uids::Target::Submission(id, sub)) = s.uids.resolve(&s.chain, &uid).await? else {
        return Err(missing());
    };
    let contract = s.chain.contract();
    let (job, submission) = (s.chain.job(id).await?, s.chain.submission(id, sub).await?);
    Ok(Json(json!({
        "submission": submission_json(contract, &submission),
        "job": job_json(contract, &job),
    })))
}

#[derive(Debug, Serialize)]
struct StoredCode {
    code_hash: Hash32,
    code_uri: String,
}

/// Header the web proxy sets to the visitor's address; uploads are limited per value.
pub const CLIENT_HEADER: &str = "x-sphinx-client";
/// Header that carries the code reader token (the relayer's).
pub const READER_HEADER: &str = "x-sphinx-reader";

async fn put_code<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> ApiResult<impl IntoResponse> {
    let client = headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("direct");
    let hash = s.code.put(&body, client).await?;
    let code_uri = format!("{}/v1/code/{hash}", s.public_base.trim_end_matches('/'));
    Ok((StatusCode::CREATED, Json(StoredCode { code_hash: hash, code_uri })))
}

async fn get_code<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    headers: axum::http::HeaderMap,
    Path(hash): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let hash: Hash32 = hash.parse().map_err(|e| {
        ApiError(SphinxError::new(
            Code::ApiBadRequest,
            format!("code hash is not a SHA-256 hash: {e}"),
        ))
    })?;
    let missing = || {
        ApiError(
            SphinxError::new(Code::ApiCodeNotFound, format!("no stored code has hash {hash}"))
                .ctx("code_hash", hash),
        )
    };
    // Private by default. Task text a job points to is public. Submission
    // code is readable by the relayer (reader token), by the agent that
    // submitted it, and by the buyer once the job was paid to that
    // submission. A job's hidden suite, if its buyer chooses to publish it,
    // is public after every job that committed it is finished; reference
    // solutions never are. Nothing else is served, so a buyer cannot read failed
    // near-solutions and nobody reads code before it is scored. The rules
    // rest on facts that cannot be undone (an author, a spec, a paid job, a
    // finished job), so a stale cache can only refuse, never leak; the one
    // exception, a new job reusing an already public suite, gains nothing
    // from secrecy, and the posting tools refuse to reuse a public suite.
    let reader = s.code.is_reader(headers.get(READER_HEADER).and_then(|v| v.to_str().ok()));
    if !reader && !may_read(&s, &headers, hash).await? {
        return Err(missing());
    }
    let code = s.code.get(&hash).await?.ok_or_else(missing)?;
    Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], code))
}

/// Whether the caller may read stored bytes with `hash` (see `get_code`).
///
/// The public routes (task text, a published suite) never serve a hash that
/// is anyone's submitted code or a reference solution: a job's `spec_uri` or
/// `suite_hash` is whatever its buyer typed, so naming someone else's hash
/// there must not open it. A published suite must also parse as a suite.
async fn may_read<C: Chain>(
    s: &AppState<C>,
    headers: &axum::http::HeaderMap,
    hash: Hash32,
) -> ApiResult<bool> {
    use sphinx_near::views::SubStatus;
    let subs = all_submissions(s).await?;
    // Bytes that parse as a suite are public only once revealed; anything
    // else only as some job's task text.
    let public = !is_secret(s, hash, &subs).await?
        && if is_suite(s, hash).await? {
            is_revealed(s, hash).await?
        } else {
            is_spec(s, hash).await?
        };
    if public {
        return Ok(true);
    }
    let Some(caller) = signed_caller(s, headers, &hash.to_string()).await? else {
        return Ok(false);
    };
    Ok(subs.iter().any(|(job, sub)| {
        sub.code_hash == hash
            && (sub.agent == caller
                || (job.buyer == caller
                    && job.status == JobStatus::Paid
                    && sub.status == SubStatus::Passed
                    && job.winning_sub_id == Some(sub.sub_id)))
    }))
}

/// Whether `hash` is submitted code or a reference solution. Known
/// submissions come from the scan; submissions to jobs still taking or
/// grading work are read again past the cache, so code submitted moments ago
/// is never mistaken for public text.
async fn is_secret<C: Chain>(
    s: &AppState<C>,
    hash: Hash32,
    subs: &[(Job, Submission)],
) -> ApiResult<bool> {
    if subs.iter().any(|(_, sub)| sub.code_hash == hash) {
        return Ok(true);
    }
    let mut from = 0u64;
    loop {
        let page = s.chain.jobs(from, PAGE).await?;
        for job in &page {
            if job.commit.as_ref().is_some_and(|c| c.ref_solution_hash == hash) {
                return Ok(true);
            }
            if !is_finished(job) || job.pending_count > 0 {
                let mut sub_from = 0u32;
                loop {
                    let fresh = s.chain.fresh_submissions(job.id_u64(), sub_from, PAGE).await?;
                    if fresh.iter().any(|sub| sub.code_hash == hash) {
                        return Ok(true);
                    }
                    if fresh.len() < PAGE as usize {
                        break;
                    }
                    sub_from = sub_from.saturating_add(PAGE);
                }
            }
        }
        if page.len() < PAGE as usize {
            return Ok(false);
        }
        from += u64::from(PAGE);
    }
}

/// Whether the stored bytes under `hash` are a test suite.
async fn is_suite<C: Chain>(s: &AppState<C>, hash: Hash32) -> ApiResult<bool> {
    Ok(s.code.get(&hash).await?.is_some_and(|b| sphinx_protocol::Suite::parse(&b).is_ok()))
}

/// Whether `hash` is a hidden suite that may now be read by anyone: its buyer
/// stored it to publish it, at least one job committed it, and every job that
/// did is finished. While any job using it is still live it stays hidden, so
/// a suite reused by a later job is never served early. Reference solutions
/// are never served: they are the buyer's own passing code.
async fn is_revealed<C: Chain>(s: &AppState<C>, hash: Hash32) -> ApiResult<bool> {
    let mut from = 0u64;
    let mut committed = false;
    loop {
        let page = s.chain.jobs(from, PAGE).await?;
        for job in &page {
            let Some(c) = &job.commit else { continue };
            if c.suite_hash == hash {
                if !is_finished(job) {
                    return Ok(false);
                }
                committed = true;
            }
        }
        if page.len() < PAGE as usize {
            return Ok(committed);
        }
        from += u64::from(PAGE);
    }
}

/// Whether a job is over: paid, refunded, cancelled or rejected.
fn is_finished(job: &Job) -> bool {
    matches!(
        job.status,
        JobStatus::Paid | JobStatus::Refunded | JobStatus::Cancelled | JobStatus::Rejected
    )
}

/// The stored hash a `spec_uri` names exactly: `<base>/v1/code/<hash>`.
fn spec_hash(spec_uri: &str) -> Option<Hash32> {
    spec_uri.rsplit_once("/v1/code/").and_then(|(_, h)| h.parse().ok())
}

/// Whether some job's task text is stored under `hash`, named exactly by its
/// `spec_uri` (jobs without submissions are not in the submissions scan).
async fn is_spec<C: Chain>(s: &AppState<C>, hash: Hash32) -> ApiResult<bool> {
    let mut from = 0u64;
    loop {
        let page = s.chain.jobs(from, PAGE).await?;
        if page.iter().any(|job| spec_hash(&job.spec_uri) == Some(hash)) {
            return Ok(true);
        }
        if page.len() < PAGE as usize {
            return Ok(false);
        }
        from += u64::from(PAGE);
    }
}

/// The account a valid `x-sphinx-auth` header speaks for, if any: a NEP-413
/// message asking for this hash, fresh, signed by one of the account's
/// current access keys.
async fn signed_caller<C: Chain>(
    s: &AppState<C>,
    headers: &axum::http::HeaderMap,
    hash: &str,
) -> ApiResult<Option<String>> {
    let Some(value) = headers.get(sphinx_near::nep413::HEADER).and_then(|v| v.to_str().ok()) else {
        return Ok(None);
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let Ok(m) = sphinx_near::nep413::verify_code_header(value, hash, now_ms) else {
        return Ok(None);
    };
    if !s.chain.is_access_key(&m.account_id, &m.public_key).await? {
        return Ok(None);
    }
    // A signed request works once: its nonce is remembered for as long as
    // the message is fresh, so a copied header cannot be replayed.
    if !first_use(&m.account_id, &m.nonce, now_ms) {
        return Ok(None);
    }
    Ok(Some(m.account_id))
}

/// Records `(account, nonce)` and says whether it was new. Entries older
/// than a signed message can be are dropped.
fn first_use(account: &str, nonce: &str, now_ms: u64) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashMap<(String, String), u64>>> = OnceLock::new();
    let Ok(mut seen) = SEEN.get_or_init(Mutex::default).lock() else { return false };
    let window = sphinx_near::nep413::MAX_AGE_MS + 60_000;
    seen.retain(|_, at| now_ms.saturating_sub(*at) <= window);
    seen.insert((account.to_owned(), nonce.to_owned()), now_ms).is_none()
}

/// One catalog entry as the API serves it.
fn catalog_entry(code: Code) -> Value {
    json!({
        "code": code.as_str(),
        "component": code.component().as_str(),
        "retry": code.retry(),
        "bond_effect": code.bond_effect(),
        "hint": code.hint(),
        "docs": sphinx_protocol::error::docs_anchor(code),
    })
}

async fn list_errors() -> Json<Value> {
    Json(json!({ "errors": Code::ALL.iter().copied().map(catalog_entry).collect::<Vec<_>>() }))
}

async fn get_error(Path(code): Path<String>) -> ApiResult<impl IntoResponse> {
    let parsed: Code = code.parse().map_err(|()| {
        ApiError(SphinxError::new(
            Code::CliUnknownCode,
            format!("{code} is not a Sphinx error code"),
        ))
    })?;
    Ok(Json(catalog_entry(parsed)))
}

#[derive(Debug, Deserialize)]
struct RelayRequest {
    /// Base64 Borsh `SignedDelegateAction` (NEP-366).
    signed_delegate_action: String,
}

async fn relay<C: Chain>(
    State(s): State<Arc<AppState<C>>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> ApiResult<impl IntoResponse> {
    let relay = s.relay.as_ref().ok_or_else(|| {
        ApiError(SphinxError::new(
            Code::ApiRelayDisabled,
            "this API instance does not relay transactions",
        ))
    })?;
    let client = headers.get(CLIENT_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("direct");
    relay.clients.admit(client, Instant::now())?;
    let req: RelayRequest = serde_json::from_slice(&body).map_err(|e| {
        ApiError(SphinxError::new(
            Code::ApiBadRequest,
            format!("body must be {{\"signed_delegate_action\": base64}}: {e}"),
        ))
    })?;
    let sda = sphinx_near::relay::decode(&req.signed_delegate_action)?;
    // The signature, and then the key's ownership on chain, are checked
    // before any limit applies, so only the account itself can use up its
    // allowance, and a key that is not the sender's never costs gas.
    sphinx_near::relay::check(&sda, relay.escrow.contract_id(), &relay.relayer)?;
    let sender = sda.delegate_action.sender_id.to_string();
    relay.limiter.admit_global(Instant::now())?;
    let (key, height) = s.chain.access_key(&sda).await?;
    sphinx_near::relay::check_key(&sda, relay.escrow.contract_id(), key.as_ref(), height)?;
    relay.limiter.admit(&sender, Instant::now())?;
    // A call the contract would refuse is refused here, before any gas is spent.
    for p in sphinx_near::relay::preflight(&sda)? {
        s.chain.check(p.view, p.args).await?;
    }
    relay.daily.take(std::time::SystemTime::now())?;
    let sent =
        sphinx_near::relay::send(&relay.escrow, relay.relayer.clone(), relay.signer.clone(), sda)
            .await;
    match sent {
        Ok(outcome) => Ok(Json(json!({ "tx_hash": outcome.tx_hash, "result": outcome.value }))),
        Err(e) => {
            // Only a failure that reached the chain cost the relay gas.
            if e.context().contains_key("tx_hash") {
                relay.limiter.record_failure(&sender, Instant::now());
            }
            Err(e.into())
        }
    }
}

//! Sphinx relayer: keeps every job moving without a human in the loop.
//!
//! Each pass reads the escrow, plans the work ([`plan`]), runs the published
//! grader inside OutLayer's enclave with the buyer's hidden suite, and posts
//! the signed result. The relayer holds no state the chain does not: a
//! result posted twice is rejected by the contract and treated as done.
//! Posting is permissionless; the relayer matters only because enclave runs
//! it submits are the ones the hidden suite and the registered key accept.

pub mod plan;

use serde_json::{json, Value};
use sphinx_near::hidden_suite::{encrypt_submission, profile_for_job, Keystore};
use sphinx_near::near_api::types::{AccountId, NearToken};
use sphinx_near::near_api::Signer;
use sphinx_near::outlayer::{Grader, SecretsRef};
use sphinx_near::views::{Job, JobStatus, SubStatus, Submission};
use sphinx_near::Escrow;
use sphinx_protocol::{Code, Hash32, SphinxError};
use std::sync::Arc;

use crate::plan::Work;

/// Everything a pass needs.
pub struct Relayer {
    pub escrow: Escrow,
    /// The current grader build, which new jobs are committed to.
    pub grader: Grader,
    /// Earlier builds, for jobs committed before a release: each job is graded
    /// by the build its key was frozen with, or not at all.
    pub previous_graders: Vec<Grader>,
    pub account: AccountId,
    pub signer: Arc<Signer>,
    /// Where submission code is read from (see [`CodeSource`]).
    pub code: CodeSource,
}

/// Largest submission the relayer reads; the grader refuses anything larger.
pub const MAX_CODE_BYTES: usize = 64 * 1024;

/// Where the relayer reads submission code. Code is always looked up by the
/// hash committed on chain; the agent-supplied `code_uri` never chooses a
/// host or a file, so a submission cannot make the relayer call an internal
/// address, read a local file, or wait on a server that never answers.
#[derive(Debug, Clone)]
pub struct CodeSource {
    /// The Sphinx job API whose code store holds submissions
    /// (`{api}/v1/code/{code_hash}`).
    pub api: String,
    /// Directory that `repo:` paths resolve against, for local demos only;
    /// `None` refuses `repo:` URIs.
    pub repo_root: Option<std::path::PathBuf>,
    /// Lets the relayer read submissions the code store keeps private until scored.
    pub reader_token: Option<String>,
    pub http: reqwest::Client,
}

impl CodeSource {
    /// Reads code from `api`: no redirects, a 5 s connect and 15 s total timeout.
    ///
    /// # Errors
    /// When the HTTP client cannot be built.
    pub fn new(api: &str, repo_root: Option<std::path::PathBuf>) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {e}"))?;
        let reader_token = std::env::var("SPHINX_CODE_READER_TOKEN").ok().filter(|t| !t.is_empty());
        Ok(Self { api: api.trim_end_matches('/').to_owned(), repo_root, reader_token, http })
    }
}

/// A `repo:` path that stays inside its root: relative, with only normal parts.
fn demo_path(root: &std::path::Path, path: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let p = std::path::Path::new(path);
    let plain = !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)));
    plain.then(|| root.join(p))
}

impl std::fmt::Debug for Relayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relayer")
            .field("account", &self.account)
            .field("escrow", &self.escrow)
            .finish_non_exhaustive()
    }
}

/// Largest page the escrow's list views return.
const PAGE: u32 = 50;

/// What happened to one unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Posted {
        tx_hash: String,
        summary: String,
    },
    /// Someone already posted it; nothing to do.
    AlreadyDone,
    /// The job or submission moved on before this work ran (for example the
    /// job was paid to another agent); no enclave run was paid for.
    NotNeeded(String),
}

impl Relayer {
    /// Reads every active job and its pending submissions.
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` on view failures.
    pub async fn pending_work(&self) -> Result<Vec<Work>, SphinxError> {
        let mut work = Vec::new();
        let mut from = 0u64;
        loop {
            let jobs: Vec<Job> = self
                .escrow
                .view("list_jobs", json!({ "from": from.to_string(), "limit": PAGE }))
                .await?;
            let n = jobs.len();
            for job in jobs.iter().filter(|j| plan::is_active(j)) {
                let subs = if plan::needs_submissions(job) {
                    self.submissions(job).await?
                } else {
                    Vec::new()
                };
                work.extend(plan::plan(job, &subs, now_ns()));
            }
            if n < PAGE as usize {
                break;
            }
            from += u64::from(PAGE);
        }
        Ok(work)
    }

    /// Every submission of a job, read page by page.
    async fn submissions(&self, job: &Job) -> Result<Vec<Submission>, SphinxError> {
        let mut all = Vec::with_capacity(job.sub_count as usize);
        let mut from = 0u32;
        while from < job.sub_count {
            let page: Vec<Submission> = self
                .escrow
                .view("list_submissions", json!({ "job_id": job.id, "from": from, "limit": PAGE }))
                .await?;
            if page.is_empty() {
                break;
            }
            from = from.saturating_add(PAGE);
            all.extend(page);
        }
        Ok(all)
    }

    /// Performs one unit of work.
    ///
    /// # Errors
    /// The grader's or contract's catalog error.
    pub async fn perform(&self, work: &Work) -> Result<Outcome, SphinxError> {
        match *work {
            Work::CheckSuite { job_id } => self.check_suite(job_id).await,
            Work::Score { job_id, sub_id } => self.score(job_id, sub_id).await,
            Work::ReturnBond { job_id, sub_id } => self.return_bond(job_id, sub_id).await,
            Work::Settle { job_id } => self.settle(job_id).await,
        }
    }

    /// Pays a passing submission that was waiting for earlier ones.
    async fn settle(&self, job_id: u64) -> Result<Outcome, SphinxError> {
        self.post(
            "settle",
            json!({ "job_id": job_id.to_string() }),
            format!("job {job_id}: earlier submissions finished; the waiting pass is paid"),
        )
        .await
    }

    async fn job(&self, job_id: u64) -> Result<Job, SphinxError> {
        let job: Option<Job> =
            self.escrow.view("get_job", json!({ "job_id": job_id.to_string() })).await?;
        job.ok_or_else(|| {
            SphinxError::new(Code::JobNotFound, format!("job {job_id} does not exist"))
        })
    }

    async fn check_suite(&self, job_id: u64) -> Result<Outcome, SphinxError> {
        let job = self.job(job_id).await?;
        if job.status != JobStatus::Committed {
            return Ok(Outcome::NotNeeded(format!("job {job_id} is {:?}", job.status)));
        }
        let commit =
            job.commit.as_ref().ok_or_else(|| invariant("committed job without commit"))?;
        let request = json!({
            "op": "validate", "schema": "sphinx.grader/1", "contract": self.escrow.contract_id(),
            "job_id": job.id, "nonce": commit.suite_check_nonce,
            "expected_suite_hash": commit.suite_hash, "expected_ref_hash": commit.ref_solution_hash,
            "epoch_ms": epoch_ms(&job),
        });
        let response = self.run_grader(&job, &request).await?;
        let valid = response["result"]["valid"].as_bool().unwrap_or(false);
        self.post(
            "record_suite_check",
            json!({ "check": response["result"], "signature_hex": response["signature"] }),
            format!("job {job_id}: suite {}", if valid { "valid" } else { "rejected" }),
        )
        .await
    }

    async fn score(&self, job_id: u64, sub_id: u32) -> Result<Outcome, SphinxError> {
        let job = self.job(job_id).await?;
        let commit = job.commit.as_ref().ok_or_else(|| invariant("open job without commit"))?;
        let sub: Submission = self
            .escrow
            .view::<Option<Submission>>(
                "get_submission",
                json!({ "job_id": job.id, "sub_id": sub_id }),
            )
            .await?
            .ok_or_else(|| {
                SphinxError::new(
                    Code::ScoreNoSubmission,
                    format!("job {job_id} has no submission {sub_id}"),
                )
            })?;
        if job.status != JobStatus::Open || sub.status != SubStatus::Pending {
            return Ok(Outcome::NotNeeded(format!(
                "job {job_id} is {:?}, submission {sub_id} is {:?}",
                job.status, sub.status
            )));
        }
        // The request becomes public transaction arguments, so the code goes
        // encrypted to the box key of the grader frozen into this job.
        let box_key = &commit.grader.box_public_key;
        if sphinx_box::parse_public_key(box_key).is_err() {
            return Err(SphinxError::new(
                Code::RelayNoBoxKey,
                format!(
                    "job {job_id}'s grader key has no box key; its code cannot be sent encrypted"
                ),
            )
            .ctx("job_id", job_id));
        }
        let code = self.fetch_code(&sub).await?;
        let request =
            grade_request(self.escrow.contract_id().as_str(), &job, &sub, &code, box_key)?;
        let response = self.run_grader(&job, &request).await?;
        let summary = format!(
            "job {job_id} sub {sub_id} ({}): {}/{}",
            sub.agent, response["result"]["passed"], response["result"]["total"]
        );
        self.post(
            "record_score",
            json!({ "score": response["result"], "signature_hex": response["signature"] }),
            summary,
        )
        .await
    }

    /// Returns the bond of a submission that was still pending when its job
    /// ended. The contract pays the bond to the submitting agent.
    async fn return_bond(&self, job_id: u64, sub_id: u32) -> Result<Outcome, SphinxError> {
        self.post(
            "reclaim_bond",
            json!({ "job_id": job_id.to_string(), "sub_id": sub_id }),
            format!("job {job_id} sub {sub_id}: job ended first; bond returned to the agent"),
        )
        .await
    }

    /// The grader build this job's key was frozen with, if this relayer runs it.
    fn grader_for(&self, job: &Job) -> Result<&Grader, SphinxError> {
        let wanted = job
            .commit
            .as_ref()
            .map(|c| c.grader.wasm_sha256.to_string())
            .ok_or_else(|| invariant("job has no committed grader"))?;
        std::iter::once(&self.grader)
            .chain(&self.previous_graders)
            .find(|g| g.wasm_sha256.eq_ignore_ascii_case(&wanted))
            .ok_or_else(|| {
                SphinxError::new(
                    Code::GraderHiddenInputMissing,
                    format!(
                        "job {} is committed to grader build {wanted}, which this relayer does not run; add it to SPHINX_GRADER_PREVIOUS",
                        job.id
                    ),
                )
                .retry(sphinx_protocol::Retry::Wait)
                .ctx("job_id", &job.id)
            })
    }

    /// Whether the buyer has stored this job's hidden suite for the job's grader
    /// build. Checked before paying for an enclave run, which would otherwise
    /// fail closed and waste the run fee.
    async fn suite_is_stored(&self, job: &Job, grader: &Grader) -> Result<bool, SphinxError> {
        let owner: AccountId =
            job.buyer.parse().map_err(|_| invariant("job buyer is not an account id"))?;
        let keystore = Keystore {
            contract: grader.outlayer.clone(),
            ..Keystore::for_account(grader.outlayer.as_str())
        };
        keystore
            .is_stored(
                self.escrow.network(),
                &owner,
                &profile_for_job(job.id_u64()),
                &grader.wasm_sha256,
            )
            .await
    }

    /// Runs the grader with the job's hidden suite and returns a successful response.
    async fn run_grader(&self, job: &Job, request: &Value) -> Result<Value, SphinxError> {
        let grader = self.grader_for(job)?;
        if !self.suite_is_stored(job, grader).await? {
            return Err(SphinxError::new(
                Code::GraderHiddenInputMissing,
                format!(
                    "job {} has no hidden suite stored for grader {}; waiting for the buyer to run `sphinx suite store`",
                    job.id, grader.wasm_sha256
                ),
            )
            .retry(sphinx_protocol::Retry::Wait)
            .ctx("job_id", &job.id));
        }
        let secrets =
            SecretsRef { profile: profile_for_job(job.id_u64()), account_id: job.buyer.clone() };
        let response = grader
            .run(
                self.escrow.network(),
                self.account.clone(),
                self.signer.clone(),
                request,
                Some(&secrets),
            )
            .await?;
        if response["ok"] == true && response["signature"].is_string() {
            return Ok(response);
        }
        let report = &response["error"];
        let code = report["code"]
            .as_str()
            .and_then(|c| c.parse::<Code>().ok())
            .unwrap_or(Code::RelayNoScore);
        let message =
            report["message"].as_str().unwrap_or("the grader returned no signed result").to_owned();
        Err(SphinxError::new(code, message).ctx("job_id", &job.id))
    }

    /// Reads submission code by its committed hash and checks it.
    async fn fetch_code(&self, sub: &Submission) -> Result<String, SphinxError> {
        let bytes = match (sub.code_uri.strip_prefix("repo:"), &self.code.repo_root) {
            (Some(path), Some(root)) => {
                let file = demo_path(root, path)
                    .ok_or_else(|| fetch_failed(&sub.code_uri, &"a repo: path must be relative"))?;
                let bytes =
                    tokio::fs::read(file).await.map_err(|e| fetch_failed(&sub.code_uri, &e))?;
                if bytes.len() > MAX_CODE_BYTES {
                    return Err(fetch_failed(&sub.code_uri, &"larger than 64 KiB"));
                }
                bytes
            }
            _ => self.read_store(sub).await?,
        };
        if Hash32::digest(&bytes) != sub.code_hash {
            return Err(SphinxError::new(
                Code::GraderCodeHashMismatch,
                format!("code at {} does not match the submitted code_hash", sub.code_uri),
            )
            .ctx("code_uri", &sub.code_uri));
        }
        String::from_utf8(bytes)
            .map_err(|_| SphinxError::new(Code::CodeSyntax, "submission is not valid UTF-8"))
    }

    /// Reads `{api}/v1/code/{code_hash}`, stopping at 64 KiB.
    async fn read_store(&self, sub: &Submission) -> Result<Vec<u8>, SphinxError> {
        let url = format!("{}/v1/code/{}", self.code.api, sub.code_hash);
        let mut req = self.code.http.get(&url);
        if let Some(token) = &self.code.reader_token {
            req = req.header("x-sphinx-reader", token);
        }
        let res = req.send().await.map_err(|e| fetch_failed(&url, &e))?;
        // The agent uploads before it submits, but a submission can reach the
        // chain before its upload is visible; a missing file is retried, not
        // graded as a failure.
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(fetch_failed(&url, &"not uploaded yet").retry(sphinx_protocol::Retry::Wait));
        }
        let mut res = res.error_for_status().map_err(|e| fetch_failed(&url, &e))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(|e| fetch_failed(&url, &e))? {
            if bytes.len() + chunk.len() > MAX_CODE_BYTES {
                return Err(fetch_failed(&url, &"larger than 64 KiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn post(
        &self,
        method: &str,
        args: Value,
        summary: String,
    ) -> Result<Outcome, SphinxError> {
        match self
            .escrow
            .call(
                self.account.clone(),
                self.signer.clone(),
                method,
                args,
                NearToken::from_yoctonear(0),
                sphinx_near::call_gas(method),
            )
            .await
        {
            Ok(o) => Ok(Outcome::Posted { tx_hash: o.tx_hash, summary }),
            Err(e)
                if matches!(
                    e.code(),
                    Code::ScoreAlreadyGraded | Code::JobWrongState | Code::ReclaimNotPending
                ) =>
            {
                Ok(Outcome::AlreadyDone)
            }
            Err(e) => Err(e),
        }
    }
}

/// The `grade` request for one submission, with its code encrypted to the job's
/// grader box key. It never carries the code in plaintext.
///
/// # Errors
/// `SPHINX_E_RELAY_NO_BOX_KEY` if `box_key` is not a usable key.
pub fn grade_request(
    contract: &str,
    job: &Job,
    sub: &Submission,
    code: &str,
    box_key: &str,
) -> Result<Value, SphinxError> {
    let commit = job.commit.as_ref().ok_or_else(|| invariant("open job without commit"))?;
    let encrypted = encrypt_submission(box_key, code.as_bytes())?;
    Ok(json!({
        "op": "grade", "schema": "sphinx.grader/1", "contract": contract,
        "job_id": job.id, "sub_id": sub.sub_id, "nonce": sub.nonce,
        "expected_suite_hash": commit.suite_hash,
        "submission_box": encrypted, "expected_code_hash": sub.code_hash,
        "epoch_ms": epoch_ms(job),
    }))
}

/// The fixed clock a job's runs use, so every run of it is identical.
fn epoch_ms(job: &Job) -> u64 {
    job.created_at_ns.parse::<u64>().unwrap_or(0) / 1_000_000
}

fn fetch_failed(uri: &str, e: &dyn std::fmt::Display) -> SphinxError {
    SphinxError::new(Code::RelayRpcFailed, format!("cannot fetch submission code from {uri}: {e}"))
        .ctx("code_uri", uri)
}

fn invariant(what: &str) -> SphinxError {
    SphinxError::new(Code::InternalInvariant, what.to_owned())
}

/// Wall-clock time in nanoseconds, used as an estimate of chain time when
/// planning. Planning early only costs a refused call that is retried.
fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_and_sub() -> (Job, Submission) {
        let h = |s: &str| Hash32::digest(s.as_bytes());
        let job: Job = serde_json::from_value(json!({
            "id": "7", "buyer": "buyer.testnet", "spec_uri": "", "runtime": "js-boa/1",
            "reward": "1", "bond": "1", "fee_bps": 0, "max_subs_per_agent": 1, "max_subs_total": 1,
            "deadline_ns": "0", "created_at_ns": "1700000000000000000", "status": "Open",
            "commit": {"suite_hash": h("suite"), "ref_solution_hash": h("ref"),
                       "suite_check_nonce": h("n"), "grader": {
                "public_key": "11".repeat(32), "box_public_key": "", "mode": "outlayer_tdx",
                "wasm_sha256": h("w"), "attestation_ref": "", "registered_at_ns": "0"}},
            "sub_count": 1, "pending_count": 1, "graded_count": 0,
            "winner": null, "winning_sub_id": null,
        }))
        .unwrap();
        let sub: Submission = serde_json::from_value(json!({
            "job_id": "7", "sub_id": 3, "agent": "a.testnet", "code_uri": "x",
            "code_hash": h("function solve(){}"), "nonce": h("sub nonce"), "status": "Pending",
            "passed": 0, "total": 0, "bond": "1", "submitted_at_ns": "0", "settled_at_ns": null,
        }))
        .unwrap();
        (job, sub)
    }

    #[test]
    fn the_grade_request_carries_only_encrypted_code() {
        let (job, sub) = job_and_sub();
        let secret = sphinx_box::BoxSecret::derive(&[4; 32]);
        let code = "function solve(){ return 'CANARY' }";
        let req =
            grade_request("escrow.testnet", &job, &sub, code, &secret.public_key_hex()).unwrap();
        let text = req.to_string();
        assert!(!text.contains("CANARY"), "plaintext code must never be sent");
        assert!(req.get("submission").is_none() && req.get("suite").is_none());
        let opened = secret
            .open(req["submission_box"].as_str().unwrap(), sphinx_box::SUBMISSION_INFO)
            .unwrap();
        assert_eq!(opened, code.as_bytes());
        assert_eq!(req["sub_id"], 3);
        assert_eq!(req["nonce"], json!(sub.nonce));
        assert_eq!(req["expected_code_hash"], json!(sub.code_hash));
        assert_eq!(req["epoch_ms"], 1_700_000_000_000u64);
        let err = grade_request("escrow.testnet", &job, &sub, code, "").unwrap_err();
        assert_eq!(err.code(), Code::RelayNoBoxKey);
    }

    #[test]
    fn demo_paths_cannot_leave_their_root() {
        let root = std::path::Path::new("/repo");
        assert_eq!(demo_path(root, "fixtures/fib/a.js"), Some(root.join("fixtures/fib/a.js")));
        for bad in ["/dev/zero", "../.env", "fixtures/../../x", "", "./a.js"] {
            assert_eq!(demo_path(root, bad), None, "{bad}");
        }
    }
}

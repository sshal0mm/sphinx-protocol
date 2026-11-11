//! Buyer tools: post a job with a hidden suite, and delete the suite once the job ends.
//!
//! Posting is three transactions: `post_job` locks the reward, `commit_suite`
//! fixes the suite and reference hashes and freezes the grader key, and the
//! suite and reference are stored, encrypted, for that grader build. If a later step
//! fails, calling again with the returned `job_id` finishes only the steps
//! still missing.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use sphinx_near::hidden_suite::{
    grader_target, profile_for_job, secret_values, Keystore, SuiteTarget,
};
use sphinx_near::views::{Config, Job, JobStatus};
use sphinx_protocol::{Code, SphinxError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{internal, need_identity, optional_job_id, yocto, SphinxMcp};

const NS_PER_HOUR: u64 = 3_600_000_000_000;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PostJob {
    /// The hidden test suite as JSON text: {"version":1,"runtime":"js-boa/1","entry":"<function name>","tests":[{"args":[...],"expected":...}]}. Agents never see it.
    pub suite: String,
    /// A reference solution (JavaScript) that passes every test. It is checked here before anything is posted, and again in the enclave. Agents never see it.
    pub reference: String,
    /// A short title for the job, 1 to 80 characters, shown wherever it is listed. Optional when `spec` starts with a `# ` heading, which is used instead.
    #[serde(default)]
    pub title: Option<String>,
    /// Where agents read the task description (an https:// URL). Give this or `spec`.
    pub spec_uri: Option<String>,
    /// The task description itself; it is stored through the job API and its URL is used. Give this or `spec_uri`.
    pub spec: Option<String>,
    /// Reward paid to the first submission that passes every test, in NEAR as a decimal string, e.g. "0.5".
    pub reward_near: String,
    /// Bond each submission locks, in NEAR as a decimal string, e.g. "0.01". A failed bond is held, then kept by the contract if another submission passes (never paid to you) or returned to its agent if nobody does.
    pub bond_near: String,
    /// Graded attempts each agent may make.
    pub max_subs_per_agent: u32,
    /// Submissions the job accepts in total.
    pub max_subs_total: u32,
    /// Hours until submissions close, from 0.25 (15 minutes) to 720 (30 days); fractions allowed.
    pub deadline_hours: f64,
    /// Resume a job this tool posted earlier whose later steps did not finish. The suite and reference must be the same.
    #[serde(default, deserialize_with = "optional_job_id")]
    pub job_id: Option<u64>,
    /// Hire directly: only these agent accounts may submit (at most 8). Omit to open the job to every agent.
    #[serde(default)]
    pub invited: Option<Vec<String>>,
    /// Make this a skill assessment: every agent that passes all hidden tests earns a badge for `skill` from you; it pays no reward (reward_near "0"), allows one attempt per agent (max_subs_per_agent 1), and a failed attempt forfeits its bond.
    #[serde(default)]
    pub assessment: Option<AssessmentInput>,
    /// Only agents holding these current badges, and at least `min_wins` paid wins, may submit.
    #[serde(default)]
    pub requires: Option<RequiresInput>,
}

/// A skill assessment's terms.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AssessmentInput {
    /// One lowercase tag of letters, digits and hyphens, e.g. "data-parsing".
    pub skill: String,
    /// How many days a badge from this assessment stays current (1 to 365).
    pub validity_days: u32,
    /// A badge the agent must already hold to take this assessment (a level below).
    #[serde(default)]
    pub prerequisite: Option<BadgeInput>,
}

/// A badge's name.
#[derive(Debug, Clone, Deserialize, JsonSchema, serde::Serialize)]
pub struct BadgeInput {
    /// The account that issued the assessment.
    pub issuer: String,
    pub skill: String,
}

/// What an agent must hold to submit.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RequiresInput {
    /// Current badges, every one required (at most four).
    #[serde(default)]
    pub badges: Vec<BadgeInput>,
    /// Paid wins at or above the contract's win floor.
    #[serde(default)]
    pub min_wins: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteSuite {
    /// The ended job whose hidden suite should be deleted.
    #[serde(deserialize_with = "crate::job_id")]
    pub job_id: u64,
}

fn now_ns() -> Result<u64, SphinxError> {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).map_err(internal)?;
    u64::try_from(since.as_nanos()).map_err(internal)
}

impl SphinxMcp {
    /// Commits the suite and reference hashes with the grader key whose
    /// attestation was just checked. The contract refuses the commit if the
    /// registered key changed in between.
    async fn commit_checked(
        &self,
        job: &Job,
        score: &sphinx_grader::LocalScore,
    ) -> Result<Value, SphinxError> {
        let config: Config = self.settings.escrow.view("get_config", json!({})).await?;
        let grader = config.grader.as_ref().ok_or_else(|| {
            SphinxError::new(
                Code::CommitNoGraderKey,
                "no grader key is registered on the escrow yet",
            )
        })?;
        grader_target(grader).await?;
        self.write(
            "commit_suite",
            json!({
                "job_id": job.id,
                "suite_hash": score.suite_hash,
                "ref_solution_hash": score.code_hash,
                "grader_public_key": grader.public_key,
            }),
            0,
        )
        .await
    }

    pub(crate) async fn post_job(&self, p: PostJob) -> Result<Value, SphinxError> {
        let id = self.settings.identity.as_ref().ok_or_else(need_identity)?;
        let score = sphinx_grader::local_score(&p.suite, &p.reference, 0)?;
        if score.passed != score.total {
            return Err(SphinxError::new(
                Code::CliReferenceFails,
                format!(
                    "the reference passes {} of {} tests; it must pass all of them",
                    score.passed, score.total
                ),
            )
            .ctx("passed", score.passed)
            .ctx("total", score.total));
        }
        let mut steps = Vec::new();
        let mut job = if let Some(job_id) = p.job_id {
            self.own_job(job_id, id.account.as_ref()).await?
        } else {
            let job_id = self.post(&p).await?;
            steps.push(json!({ "step": "post_job", "job_id": job_id.to_string() }));
            self.job(job_id).await?
        };
        let job_id = job.id_u64();
        let resume = |e: SphinxError| e.ctx("resume_with_job_id", job_id);

        if job.status == JobStatus::Draft {
            let receipt = self.commit_checked(&job, &score).await.map_err(resume)?;
            steps.push(json!({ "step": "commit_suite", "tx_hash": receipt["tx_hash"] }));
            job = self.job(job_id).await.map_err(resume)?;
        }
        let commit = job.commit.as_ref().ok_or_else(|| {
            resume(SphinxError::new(
                Code::JobWrongState,
                format!("job {job_id} is {:?} and has no committed suite", job.status),
            ))
        })?;
        if commit.suite_hash != score.suite_hash || commit.ref_solution_hash != score.code_hash {
            return Err(SphinxError::new(
                Code::CliUsage,
                format!("job {job_id} was committed with a different suite or reference"),
            )
            .ctx("job_id", job_id)
            .ctx("committed_suite_hash", commit.suite_hash)
            .ctx("given_suite_hash", score.suite_hash));
        }

        let target = grader_target(&commit.grader).await.map_err(resume)?;
        let keystore = Keystore::for_account(self.settings.escrow.contract_id().as_str());
        let profile = profile_for_job(job_id);
        let network = self.settings.escrow.network();
        if keystore
            .is_stored(network, &id.account, &profile, &target.wasm_sha256)
            .await
            .map_err(resume)?
        {
            steps.push(json!({ "step": "store_suite", "already": true }));
        } else {
            let values = secret_values(
                self.settings.escrow.contract_id().as_str(),
                job_id,
                p.suite.clone(),
                p.reference.clone(),
            );
            let outcome = keystore
                .store(
                    network,
                    id.account.clone(),
                    id.signer.clone(),
                    &profile,
                    &target.wasm_sha256,
                    &target.caller,
                    &values,
                )
                .await
                .map_err(resume)?;
            steps.push(json!({ "step": "store_suite", "tx_hash": outcome.tx_hash }));
        }
        Ok(json!({
            "job_id": job.id,
            "status": job.status,
            "spec_uri": job.spec_uri,
            "suite_hash": score.suite_hash,
            "ref_solution_hash": score.code_hash,
            "tests": score.total,
            "readable_by": readable_by(&target),
            "steps": steps,
            "next_step": "the Sphinx relayer now checks the reference against the hidden suite inside the enclave; the job opens to agents when it passes. Follow it with sphinx_jobs(job_id). After the job ends, call sphinx_manage_job with action delete_suite to delete the suite and get its storage deposit back.",
        }))
    }

    pub(crate) async fn delete_suite(&self, job_id: u64) -> Result<Value, SphinxError> {
        let id = self.settings.identity.as_ref().ok_or_else(need_identity)?;
        let job = self.own_job(job_id, id.account.as_ref()).await?;
        let ended = matches!(
            job.status,
            JobStatus::Paid | JobStatus::Rejected | JobStatus::Refunded | JobStatus::Cancelled
        );
        if !ended {
            return Err(SphinxError::new(
                Code::CliSuiteDeleteTooEarly,
                format!("job {job_id} is {:?}; it still needs its hidden suite", job.status),
            )
            .ctx("job_id", job_id)
            .ctx("status", format!("{:?}", job.status)));
        }
        let Some(commit) = job.commit.as_ref() else {
            return Ok(
                json!({ "job_id": job.id, "deleted": false, "reason": "the job never had a suite" }),
            );
        };
        let wasm = commit.grader.wasm_sha256.to_string();
        let keystore = Keystore::for_account(self.settings.escrow.contract_id().as_str());
        let profile = profile_for_job(job_id);
        let network = self.settings.escrow.network();
        if !keystore.is_stored(network, &id.account, &profile, &wasm).await? {
            return Ok(
                json!({ "job_id": job.id, "deleted": false, "reason": "no hidden suite is stored for this job" }),
            );
        }
        let outcome = keystore
            .delete(network, id.account.clone(), id.signer.clone(), &profile, &wasm)
            .await?;
        Ok(json!({ "job_id": job.id, "deleted": true, "tx_hash": outcome.tx_hash }))
    }

    /// Posts the job and returns its id.
    /// Refuses before anything is locked when the buyer cannot also pay to
    /// store the hidden suite: OutLayer's keystore charges about 1.8e19
    /// yoctoNEAR per byte of suite and reference (measured on mainnet), and
    /// each call must hold its gas cap first. A job posted without the means
    /// to store its suite would sit committed and never open.
    async fn check_funds_for_job(
        &self,
        deposit: u128,
        secret_bytes: usize,
    ) -> Result<(), SphinxError> {
        const PER_BYTE: u128 = 19_000_000_000_000_000_000;
        const MARGIN: u128 = 30_000_000_000_000_000_000_000; // 0.03 NEAR: gas caps and fees
        let id = self.settings.identity.as_ref().ok_or_else(need_identity)?;
        let keystore = (secret_bytes as u128).saturating_mul(PER_BYTE);
        let needed = deposit.saturating_add(keystore).saturating_add(MARGIN);
        let balance = sphinx_near::near_api::Tokens::account(id.account.clone())
            .near_balance()
            .fetch_from(self.settings.escrow.network())
            .await
            .map_err(|e| {
                SphinxError::new(Code::RelayRpcFailed, format!("view_account failed: {e}"))
            })?
            .total
            .as_yoctonear();
        if balance < needed {
            return Err(SphinxError::new(
                Code::CliUsage,
                format!(
                    "{} holds {} NEAR; posting this job needs about {} NEAR (reward and job storage {}, keystore deposit about {}, gas and fees 0.03); nothing was posted",
                    id.account,
                    sphinx_near::format_near(balance),
                    sphinx_near::format_near(needed),
                    sphinx_near::format_near(deposit),
                    sphinx_near::format_near(keystore)
                ),
            )
            .ctx("balance_yocto", balance)
            .ctx("needed_yocto", needed));
        }
        Ok(())
    }

    async fn post(&self, p: &PostJob) -> Result<u64, SphinxError> {
        let spec_uri = match (&p.spec_uri, &p.spec) {
            (Some(uri), None) => uri.clone(),
            (None, Some(text)) => self.store_code(text.clone()).await?,
            _ => {
                return Err(SphinxError::new(
                    Code::CliUsage,
                    "give exactly one of spec_uri (a URL) or spec (the description text)",
                ))
            }
        };
        deadline_span_ns(p.deadline_hours)?;
        let config: Config = self.settings.escrow.view("get_config", json!({})).await?;
        let reward = sphinx_near::parse_near(&p.reward_near)?;
        let bond = sphinx_near::parse_near(&p.bond_near)?;
        let deadline_ns = now_ns()?.saturating_add(deadline_span_ns(p.deadline_hours)?);
        let runtime = serde_json::from_str::<Value>(&p.suite)
            .ok()
            .and_then(|v| v["runtime"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let deposit = reward.saturating_add(yocto(&config.job_storage_deposit));
        self.check_funds_for_job(deposit, p.suite.len() + p.reference.len()).await?;
        let title = p
            .title
            .as_deref()
            .and_then(sphinx_protocol::title::clean)
            .or_else(|| p.spec.as_deref().and_then(sphinx_protocol::title::from_spec))
            .ok_or_else(|| {
                SphinxError::new(
                    Code::JobBadTitle,
                    "give the job a title, or start spec with a `# ` heading that names it",
                )
            })?;
        let receipt = self
            .write(
                "post_job",
                json!({
                    "title": title,
                    "spec_uri": spec_uri,
                    "runtime": runtime,
                    "reward": reward.to_string(),
                    "bond": bond.to_string(),
                    "max_subs_per_agent": p.max_subs_per_agent,
                    "max_subs_total": p.max_subs_total,
                    "deadline_ns": deadline_ns.to_string(),
                    "invited": p.invited,
                    "assessment": p.assessment.as_ref().map(|e| json!({
                        "skill": e.skill,
                        "validity_ns": (u64::from(e.validity_days) * 86_400_000_000_000).to_string(),
                        "prerequisite": e.prerequisite,
                    })),
                    "requires": p.requires.as_ref().map(|r| json!({
                        "badges": r.badges,
                        "min_wins": r.min_wins,
                    })),
                }),
                deposit,
            )
            .await?;
        receipt["returned"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| internal("post_job returned no job id"))
    }

    /// A job owned by `account`.
    async fn own_job(&self, job_id: u64, account: &str) -> Result<Job, SphinxError> {
        let job = self.job(job_id).await?;
        if job.buyer != account {
            return Err(SphinxError::new(
                Code::CliUsage,
                format!("job {job_id} belongs to {}, not {account}", job.buyer),
            )
            .ctx("job_id", job_id)
            .ctx("buyer", &job.buyer));
        }
        Ok(job)
    }
}

fn readable_by(t: &SuiteTarget) -> Value {
    json!({ "grader_wasm_sha256": t.wasm_sha256, "caller": t.caller, "attestation": t.attestation_ref })
}

/// `hours` as nanoseconds, refused outside 15 minutes to 30 days.
pub(crate) fn deadline_span_ns(hours: f64) -> Result<u64, SphinxError> {
    if !hours.is_finite() || !(0.25..=720.0).contains(&hours) {
        return Err(SphinxError::new(
            Code::CliUsage,
            format!("deadline_hours is {hours}; give 0.25 (15 minutes) to 720 (30 days)"),
        )
        .ctx("field", "deadline_hours"));
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "checked range"
    )]
    Ok((hours * NS_PER_HOUR as f64).round() as u64)
}

//! The newcomer box: someone types what they want in plain words, and the
//! concierge drafts a real Sphinx job for them. Drafting and checking are
//! free (and capped); the buyer funds the reward and a small drafting fee
//! from their own wallet, and their own account posts the job.
//!
//! - `POST /newcomer` with `{"request": "..."}` starts a draft (background).
//! - `GET /newcomer/{id}`: drafting progress, the task text, the checks in
//!   plain English, the terms, and once posted the job's progress on chain.
//! - `POST /newcomer/{id}/edit` with `{"change": "..."}` redrafts once.
//! - `POST /newcomer/{id}/reward` with `{"reward_near": "0.5"}` sets the reward.
//! - `POST /newcomer/{id}/pay` with `{"account": "..."}` answers the
//!   transactions the buyer's wallet signs: `post_job` with the reward, and
//!   the drafting fee to the verifier.
//! - `POST /newcomer/{id}/paid` with both transaction hashes checks them on
//!   chain and answers the next two: `commit_suite`, and `store_secrets` with
//!   the hidden suite already encrypted for the enclave under the buyer's
//!   account, so the buyer never sees the tests.
//! - `POST /newcomer/{id}/stored` with those hashes checks them; the house
//!   then clears the reference and keeps the tests until the job ends (plus
//!   seven days unless the buyer publishes them).
//! - `GET /newcomer/{id}/quote/{chain}`: what the job would cost in USDC
//!   from another chain, a dry NEAR Intents 1Click price quote. Paying this
//!   way is not built: nothing is swapped.
//!
//! The winning code is read by the buyer's wallet (NEP-413), like any buyer.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sphinx_concierge::{Concierge, Progress};
use sphinx_model::Model;
use sphinx_near::hidden_suite::{profile_for_job, secret_values, Keystore};
use sphinx_near::near_api::types::AccountId;
use sphinx_near::views::{Config, Job, JobStatus, Submission};
use sphinx_near::Escrow;
use sphinx_protocol::Hash32;

/// Longest request a visitor may type, in bytes.
pub const MAX_REQUEST: usize = 2_000;
/// Longest change asked for in the one edit, in bytes.
pub const MAX_CHANGE: usize = 1_000;
const DAY: Duration = Duration::from_hours(24);
const YOCTO: u128 = 1_000_000_000_000_000_000_000_000;

/// How jobs from the box are shaped and priced.
#[derive(Debug, Clone)]
pub struct Terms {
    /// The reward the concierge suggests, in NEAR.
    pub suggested_reward_near: String,
    pub bond_near: String,
    pub max_subs_per_agent: u32,
    pub deadline_hours: u32,
    /// Paid to the verifier for drafting and checking, with the post.
    pub drafting_fee_near: String,
    /// 1Click app fee on a USDC funding swap, in basis points.
    pub swap_fee_bps: u32,
    /// Who receives the swap fee (a NEAR account, mainnet).
    pub swap_fee_account: String,
}

/// How much the free drafting may be used.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Drafts one visitor may start in a day.
    pub drafts_per_visitor: u32,
    /// Drafts everyone together may start in a day (each costs model calls).
    pub drafts_per_day: u32,
}

/// Where a request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Drafting,
    Drafted,
    Failed,
    /// Waiting for the buyer's `post_job` and fee.
    Paying,
    /// Waiting for the buyer's `commit_suite` and `store_secrets`.
    Storing,
    Posted,
}

/// The draft as the visitor sees it, and the hidden parts until posting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftView {
    pub title: String,
    pub spec: String,
    pub entry: String,
    pub tests: usize,
    pub checks: Vec<String>,
    /// Separate solver runs agreed with the reference on every test, and no
    /// disputed input was dropped. Only then can it be posted: a wrong answer
    /// key would fail honest agents.
    #[serde(default)]
    pub agreed: bool,
    /// The task text changed while differences were settled.
    #[serde(default)]
    pub task_rewritten: bool,
    /// Hidden while the job runs. After it ends the buyer may publish it;
    /// otherwise it is removed a week later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suite: Option<String>,
    /// Hidden; removed once the job is posted (the enclave has its copy).
    /// Never published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// One visitor's request, as stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub client: String,
    pub request: String,
    pub change: Option<String>,
    pub edits_left: u32,
    pub stage: Stage,
    pub progress: Vec<String>,
    pub error: Option<String>,
    pub draft: Option<DraftView>,
    #[serde(default)]
    pub reward_near: String,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub spec_uri: Option<String>,
    pub job_id: Option<u64>,
    pub created_at_s: u64,
    /// The job ended and its buyer published the tests.
    #[serde(default)]
    pub tests_revealed: bool,
    /// When the box first saw the job ended (unix seconds).
    #[serde(default)]
    pub ended_at_s: Option<u64>,
    /// The drafting model's attestation when the draft was finished; every
    /// reply it gave was checked against this key.
    #[serde(default)]
    pub attested_by: Option<Value>,
    /// Transactions this request was paid and stored with. Each counts for
    /// one request only.
    #[serde(default)]
    pub txs: Vec<String>,
}

/// Errors as the visitor's page reads them.
#[derive(Debug)]
pub struct Refusal {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into() }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": { "code": self.code, "message": self.message } })))
            .into_response()
    }
}

fn bad(message: impl Into<String>) -> Refusal {
    Refusal::new(StatusCode::BAD_REQUEST, "bad_request", message)
}

fn wrong_stage(message: impl Into<String>) -> Refusal {
    Refusal::new(StatusCode::CONFLICT, "wrong_stage", message)
}

fn busy() -> Refusal {
    Refusal::new(StatusCode::SERVICE_UNAVAILABLE, "busy", "try again in a moment")
}

fn upstream(message: impl Into<String>) -> Refusal {
    Refusal::new(StatusCode::BAD_GATEWAY, "upstream", message)
}

/// Daily counters, reset a day after they start.
#[derive(Debug, Default)]
struct Counters {
    since: Option<Instant>,
    drafts: HashMap<String, u32>,
    drafts_total: u32,
}

impl Counters {
    fn roll(&mut self, now: Instant) {
        if self.since.is_none_or(|s| now.duration_since(s) >= DAY) {
            *self = Self { since: Some(now), ..Self::default() };
        }
    }
}

/// Where the house reaches the chain and the other services.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub escrow: Escrow,
    /// NEAR RPC, to check the buyer's transactions.
    pub rpc: String,
    /// `FastNEAR` API key, sent as a bearer header to `FastNEAR` RPC only.
    pub rpc_key: Option<String>,
    /// The job API, internal address (to store task text).
    pub api: String,
    /// The job API's public address, used in `spec_uri`.
    pub api_public: String,
    /// `OutLayer`'s keystore, where hidden suites are stored.
    pub keystore: Keystore,
    /// The account whose enclave runs may read hidden suites.
    pub relayer: AccountId,
    /// 1Click, for USDC quotes.
    pub one_click: String,
}

/// The house: its model, and the visitors' requests.
#[allow(missing_debug_implementations, reason = "holds a model client")]
pub struct House<M: Model> {
    pub model: M,
    pub at: Endpoints,
    pub dir: PathBuf,
    pub terms: Terms,
    pub caps: Caps,
    records: Mutex<HashMap<String, Record>>,
    counters: Mutex<Counters>,
    price: Mutex<Option<(f64, Instant)>>,
    http: reqwest::Client,
}

impl<M: Model + Send + Sync + 'static> House<M> {
    /// Loads stored requests from `dir`.
    pub fn new(model: M, at: Endpoints, dir: PathBuf, terms: Terms, caps: Caps) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let mut records = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
                let Ok(mut record) = serde_json::from_str::<Record>(&text) else { continue };
                // A draft cut short by a restart cannot resume.
                if record.stage == Stage::Drafting {
                    record.stage = Stage::Failed;
                    record.error = Some("the server restarted while drafting; start again".into());
                }
                if record.reward_near.is_empty() {
                    record.reward_near.clone_from(&terms.suggested_reward_near);
                }
                records.insert(record.id.clone(), record);
            }
        }
        Self {
            model,
            at,
            dir,
            terms,
            caps,
            records: Mutex::new(records),
            counters: Mutex::new(Counters::default()),
            price: Mutex::new(None),
            http: reqwest::Client::new(),
        }
    }

    fn save(&self, record: &Record) {
        let path = self.dir.join(format!("{}.json", record.id));
        if let Ok(text) = serde_json::to_string(record) {
            let _ = std::fs::write(&path, text);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Record)) -> Option<Record> {
        let mut records = self.records.lock().ok()?;
        let record = records.get_mut(id)?;
        f(record);
        let copy = record.clone();
        drop(records);
        self.save(&copy);
        Some(copy)
    }

    /// Refuses a transaction another request was already paid or stored with.
    fn unused(&self, id: &str, hashes: &[&str]) -> Result<(), Refusal> {
        let records = self.records.lock().map_err(|_| busy())?;
        let taken = records
            .values()
            .filter(|r| r.id != id)
            .any(|r| r.txs.iter().any(|t| hashes.contains(&t.as_str())));
        if taken {
            return Err(bad("that transaction was already used for another request"));
        }
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Record, Refusal> {
        self.records.lock().ok().and_then(|r| r.get(id).cloned()).ok_or_else(|| {
            Refusal::new(StatusCode::NOT_FOUND, "not_found", "no request with this id")
        })
    }

    /// Starts a draft for `request`; returns its id.
    ///
    /// # Errors
    /// An empty or long request, or a daily cap reached.
    pub fn start(self: &Arc<Self>, client: &str, request: &str) -> Result<String, Refusal> {
        if !self.model.ready() {
            return Err(Refusal::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "model_unconfirmed",
                "drafting is paused until the drafting model is confirmed to run in NEAR AI's secure hardware, try again in a few minutes",
            ));
        }
        let request = request.trim();
        if request.is_empty() || request.len() > MAX_REQUEST {
            return Err(bad(format!("describe the job in 1 to {MAX_REQUEST} characters")));
        }
        {
            let mut c = self.counters.lock().map_err(|_| busy())?;
            c.roll(Instant::now());
            let mine = c.drafts.get(client).copied().unwrap_or(0);
            if mine >= self.caps.drafts_per_visitor {
                return Err(Refusal::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "visitor_limit",
                    "you have used today's free drafts; come back tomorrow",
                ));
            }
            if c.drafts_total >= self.caps.drafts_per_day {
                return Err(Refusal::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "daily_limit",
                    "the box has drafted all it can today; come back tomorrow",
                ));
            }
            c.drafts.insert(client.to_owned(), mine + 1);
            c.drafts_total += 1;
        }
        let id = new_id();
        let record = Record {
            id: id.clone(),
            client: client.to_owned(),
            request: request.to_owned(),
            change: None,
            edits_left: 1,
            stage: Stage::Drafting,
            progress: Vec::new(),
            error: None,
            draft: None,
            reward_near: self.terms.suggested_reward_near.clone(),
            account: None,
            spec_uri: None,
            job_id: None,
            created_at_s: now_s(),
            tests_revealed: false,
            ended_at_s: None,
            attested_by: None,
            txs: Vec::new(),
        };
        self.records.lock().map_err(|_| busy())?.insert(id.clone(), record.clone());
        self.save(&record);
        self.spawn_draft(id.clone(), request.to_owned());
        Ok(id)
    }

    /// Redrafts once with the visitor's change.
    ///
    /// # Errors
    /// Unknown id, no edit left, or not drafted yet.
    pub fn edit(self: &Arc<Self>, id: &str, change: &str) -> Result<(), Refusal> {
        if !self.model.ready() {
            return Err(Refusal::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "model_unconfirmed",
                "drafting is paused until the drafting model is confirmed to run in NEAR AI's secure hardware, try again in a few minutes",
            ));
        }
        let change = change.trim();
        if change.is_empty() || change.len() > MAX_CHANGE {
            return Err(bad(format!("say what to change in 1 to {MAX_CHANGE} characters")));
        }
        // Checked and spent under one lock, so two edits at once cannot both
        // pass the check.
        let mut records = self.records.lock().map_err(|_| busy())?;
        let record = records.get_mut(id).ok_or_else(|| {
            Refusal::new(StatusCode::NOT_FOUND, "not_found", "no request with this id")
        })?;
        if record.stage != Stage::Drafted && record.stage != Stage::Failed {
            return Err(wrong_stage("this request cannot be edited now"));
        }
        if record.edits_left == 0 {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "no_edits_left",
                "the one edit has been used",
            ));
        }
        let request =
            format!("{}\n\nChange requested after the first draft: {change}", record.request);
        record.edits_left = record.edits_left.saturating_sub(1);
        record.change = Some(change.to_owned());
        record.stage = Stage::Drafting;
        record.error = None;
        record.progress.clear();
        record.draft = None;
        let copy = record.clone();
        drop(records);
        self.save(&copy);
        self.spawn_draft(id.to_owned(), request);
        Ok(())
    }

    fn spawn_draft(self: &Arc<Self>, id: String, request: String) {
        let house = Arc::clone(self);
        tokio::spawn(async move {
            let concierge = Concierge::new(&house.model);
            let progress_id = id.clone();
            let result = concierge
                .draft(&request, &mut |p| {
                    let line = describe(&p);
                    house.update(&progress_id, |r| r.progress.push(line));
                })
                .await;
            match result {
                Ok(d) => {
                    let checks = checks(
                        d.tests,
                        d.second_opinion.agreed,
                        d.second_opinion.total,
                        d.differences.len(),
                    );
                    let view = DraftView {
                        title: d.title,
                        spec: d.spec,
                        entry: d.entry,
                        tests: d.tests,
                        checks,
                        agreed: d.differences.is_empty(),
                        task_rewritten: d.task_rewritten,
                        suite: Some(d.suite),
                        reference: Some(sphinx_mcp::with_salt(&d.reference)),
                    };
                    let attested = house.model.attestation_json();
                    house.update(&id, |r| {
                        r.stage = Stage::Drafted;
                        r.draft = Some(view);
                        r.attested_by = attested;
                    });
                }
                Err(e) => {
                    house.update(&id, |r| {
                        r.stage = Stage::Failed;
                        r.error = Some(format!("the draft could not be finished: {}", e.message()));
                    });
                }
            }
        });
    }

    /// Sets the reward the buyer will lock.
    ///
    /// # Errors
    /// Not a drafted request, or a reward outside 0.1 to 100 NEAR.
    pub fn set_reward(&self, id: &str, reward_near: &str) -> Result<Record, Refusal> {
        let record = self.get(id)?;
        if record.stage != Stage::Drafted {
            return Err(wrong_stage("the reward can be changed until you pay"));
        }
        let reward =
            sphinx_near::parse_near(reward_near).map_err(|e| bad(e.message().to_owned()))?;
        if !(YOCTO / 10..=100 * YOCTO).contains(&reward) {
            return Err(bad("the reward must be between 0.1 and 100 NEAR"));
        }
        self.update(id, |r| reward_near.trim().clone_into(&mut r.reward_near)).ok_or_else(busy)
    }

    fn near(amount: &str) -> Result<u128, Refusal> {
        sphinx_near::parse_near(amount).map_err(|e| bad(e.message().to_owned()))
    }

    async fn config(&self) -> Result<Config, Refusal> {
        self.at
            .escrow
            .view("get_config", json!({}))
            .await
            .map_err(|e| upstream(e.message().to_owned()))
    }

    /// How long the box keeps an ended job's tests for its buyer to publish.
    pub const KEEP_TESTS_S: u64 = 7 * 24 * 3600;

    /// Whether `job_id` has ended (paid, refunded, cancelled or rejected), and
    /// the suite hash it committed.
    async fn ended(&self, job_id: u64) -> Option<(bool, Option<Hash32>)> {
        let job = self
            .at
            .escrow
            .view::<Option<Job>>("get_job", json!({ "job_id": job_id.to_string() }))
            .await
            .ok()??;
        let ended = matches!(
            job.status,
            JobStatus::Paid | JobStatus::Refunded | JobStatus::Cancelled | JobStatus::Rejected
        );
        Some((ended, job.commit.map(|c| c.suite_hash)))
    }

    /// Publishes an ended job's tests, at its buyer's request, so anyone can
    /// check them against the hash the job committed. The reference solution
    /// is never published: it is only the oracle that fixed the expected
    /// outputs, and it is discarded once the job is posted.
    ///
    /// # Errors
    /// The job has not ended, the tests are gone, or the API cannot store them.
    pub async fn publish_tests(&self, id: &str) -> Result<Record, Refusal> {
        let record = self.get(id)?;
        if record.tests_revealed {
            return Ok(record);
        }
        let (Some(job_id), Some(suite)) =
            (record.job_id, record.draft.as_ref().and_then(|d| d.suite.clone()))
        else {
            return Err(wrong_stage("there are no tests to publish"));
        };
        let (ended, committed) = self.ended(job_id).await.ok_or_else(busy)?;
        if !ended {
            return Err(wrong_stage("the tests stay hidden until the job ends"));
        }
        if committed != Some(Hash32::digest(suite.as_bytes())) {
            return Err(wrong_stage("these tests are not the ones the job committed"));
        }
        self.store_text(&suite).await?;
        self.update(id, |r| {
            r.tests_revealed = true;
            if let Some(d) = r.draft.as_mut() {
                d.suite = None;
            }
        })
        .ok_or_else(busy)
    }

    /// Forgets the tests of jobs that ended more than [`Self::KEEP_TESTS_S`]
    /// ago without their buyer publishing them. Meant to run on a timer.
    pub async fn forget_old_tests(&self) {
        let waiting: Vec<(String, u64, Option<u64>)> = self
            .records
            .lock()
            .map(|records| {
                records
                    .values()
                    .filter(|r| r.draft.as_ref().is_some_and(|d| d.suite.is_some()))
                    .filter(|r| r.stage == Stage::Posted)
                    .filter_map(|r| Some((r.id.clone(), r.job_id?, r.ended_at_s)))
                    .collect()
            })
            .unwrap_or_default();
        let now = now_s();
        for (id, job_id, ended_at) in waiting {
            match ended_at {
                Some(at) if now.saturating_sub(at) > Self::KEEP_TESTS_S => {
                    self.update(&id, |r| {
                        if let Some(d) = r.draft.as_mut() {
                            d.suite = None;
                        }
                    });
                }
                Some(_) => {}
                None => {
                    if let Some((true, _)) = self.ended(job_id).await {
                        self.update(&id, |r| r.ended_at_s = Some(now));
                    }
                }
            }
        }
    }

    /// Stores the task text by hash in the job API; returns its public address.
    async fn store_spec(&self, spec: &str) -> Result<String, Refusal> {
        self.store_text(spec).await
    }

    /// Stores text by hash in the job API; returns its public address.
    async fn store_text(&self, text: &str) -> Result<String, Refusal> {
        let stored: Value = self
            .http
            .post(format!("{}/v1/code", self.at.api.trim_end_matches('/')))
            .header("x-sphinx-client", "sphinx-house")
            .body(text.to_owned())
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| upstream(format!("the text could not be stored: {e}")))?
            .json()
            .await
            .map_err(|e| upstream(e.to_string()))?;
        let hash = stored["code_hash"].as_str().ok_or_else(|| upstream("no code_hash"))?;
        Ok(format!("{}/v1/code/{hash}", self.at.api_public.trim_end_matches('/')))
    }

    /// The transactions that post the job from the buyer's account: `post_job`
    /// locking the reward, and the drafting fee to the verifier.
    ///
    /// # Errors
    /// Not ready to post, or the chain or job API could not be reached.
    pub async fn pay(&self, id: &str, account: &str) -> Result<Value, Refusal> {
        let record = self.get(id)?;
        if record.stage != Stage::Drafted && record.stage != Stage::Paying {
            return Err(wrong_stage("only a finished draft can be posted"));
        }
        let draft = record.draft.clone().ok_or_else(busy)?;
        if !draft.agreed {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "not_agreed",
                "separate solver runs did not agree with our answers on every test; change the request first",
            ));
        }
        let account: AccountId = account.parse().map_err(|_| bad("not a NEAR account id"))?;
        let config = self.config().await?;
        let spec_uri = self.store_spec(&draft.spec).await?;
        if let Some(job) = self.posted_job(account.as_str(), &spec_uri).await {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "already_posted",
                format!("this task is already posted as job {job} from your account, so it is not posted again; continue from the transactions your wallet sent"),
            ));
        }
        let reward = Self::near(&record.reward_near)?;
        let bond = Self::near(&self.terms.bond_near)?;
        let fee = Self::near(&self.terms.drafting_fee_near)?;
        let job_deposit: u128 = config.job_storage_deposit.parse().unwrap_or(0);
        // Bonds times attempts may never exceed the reward.
        let max_total = u32::try_from((reward / bond.max(1)).min(12)).unwrap_or(12).max(1);
        let deadline_ns = now_ns() + u64::from(self.terms.deadline_hours) * 3_600_000_000_000;
        let title = sphinx_protocol::title::from_spec(&draft.spec)
            .unwrap_or_else(|| "A job drafted on Sphinx".to_owned());
        let post_args = json!({
            "title": title,
            "spec_uri": spec_uri, "runtime": "js-boa/1",
            "reward": reward.to_string(), "bond": bond.to_string(),
            "max_subs_per_agent": self.terms.max_subs_per_agent.min(max_total),
            "max_subs_total": max_total, "deadline_ns": deadline_ns.to_string(),
        });
        let contract = self.at.escrow.contract_id().to_string();
        self.update(id, |r| {
            r.stage = Stage::Paying;
            r.account = Some(account.to_string());
            r.spec_uri = Some(spec_uri.clone());
            r.error = None;
        });
        Ok(json!({
            "transactions": [
                { "receiverId": contract, "actions": [{ "type": "FunctionCall", "params": {
                    "methodName": "post_job", "args": post_args,
                    "gas": "50000000000000", "deposit": (reward + job_deposit).to_string() } }] },
                { "receiverId": config.verifier, "actions": [{ "type": "Transfer", "params": {
                    "deposit": fee.to_string() } }] },
            ],
            "total_near": format_near(reward + job_deposit + fee),
            "breakdown": {
                "reward_near": format_near(reward),
                "storage_deposit_near": format_near(job_deposit),
                "drafting_fee_near": format_near(fee),
            },
        }))
    }

    /// Checks the buyer's `post_job` and fee on chain, then answers the
    /// transactions that commit the suite and store it for the enclave.
    ///
    /// # Errors
    /// The transactions are not what was asked for, or did not succeed.
    pub async fn paid(&self, id: &str, post_tx: &str, fee_tx: &str) -> Result<Value, Refusal> {
        let record = self.get(id)?;
        if record.stage != Stage::Paying {
            return Err(wrong_stage("nothing is waiting for payment"));
        }
        let account = record.account.clone().ok_or_else(busy)?;
        if post_tx == fee_tx {
            return Err(bad("the job and the fee are two different transactions"));
        }
        self.unused(id, &[post_tx, fee_tx])?;
        let config = self.config().await?;
        let contract = self.at.escrow.contract_id().to_string();
        let post = self.tx(post_tx, &account).await?;
        let call = function_call(&post, &contract, "post_job").ok_or_else(|| {
            bad("that transaction is not a post_job to the escrow from your account")
        })?;
        if call["spec_uri"] != record.spec_uri.clone().unwrap_or_default().as_str() {
            return Err(bad("that post_job is for a different task"));
        }
        let job_id: u64 = success_json(&post)
            .and_then(|v| v.as_str().and_then(|s| s.parse().ok()))
            .ok_or_else(|| bad("that post_job did not succeed"))?;
        let fee = Self::near(&self.terms.drafting_fee_near)?;
        let fee_paid = self.tx(fee_tx, &account).await?;
        if !transfer_of_at_least(&fee_paid, &config.verifier, fee)
            || success_json(&fee_paid).is_none()
        {
            return Err(bad("that transaction is not the drafting fee to the verifier"));
        }
        self.update(id, |r| {
            r.stage = Stage::Storing;
            r.job_id = Some(job_id);
            r.txs.extend([post_tx.to_owned(), fee_tx.to_owned()]);
        });
        let record = self.get(id)?;
        self.store_transactions(&record, job_id, &config).await
    }

    /// The two transactions that commit the suite and store it for the
    /// enclave, again: for a buyer whose wallet closed before they were sent.
    ///
    /// # Errors
    /// Nothing is waiting to be stored.
    pub async fn store_again(&self, id: &str) -> Result<Value, Refusal> {
        let record = self.get(id)?;
        let (Stage::Storing, Some(job_id)) = (record.stage, record.job_id) else {
            return Err(wrong_stage("nothing is waiting to be stored"));
        };
        let config = self.config().await?;
        self.store_transactions(&record, job_id, &config).await
    }

    async fn store_transactions(
        &self,
        record: &Record,
        job_id: u64,
        config: &Config,
    ) -> Result<Value, Refusal> {
        let contract = self.at.escrow.contract_id().to_string();
        let account = record.account.clone().ok_or_else(busy)?;
        let draft = record.draft.clone().ok_or_else(busy)?;
        let suite = draft.suite.clone().ok_or_else(busy)?;
        let reference = draft.reference.clone().ok_or_else(busy)?;
        let grader =
            config.grader.clone().ok_or_else(|| upstream("no grader key is registered"))?;
        let buyer: AccountId = account.parse().map_err(|_| busy())?;
        let values = secret_values(&contract, job_id, suite.clone(), reference.clone());
        let (store_args, store_deposit) = self
            .at
            .keystore
            .prepare_store(
                self.at.escrow.network(),
                &buyer,
                &profile_for_job(job_id),
                &grader.wasm_sha256.to_string(),
                &self.at.relayer,
                &values,
            )
            .await
            .map_err(|e| upstream(e.message().to_owned()))?;
        let commit_args = json!({
            "job_id": job_id.to_string(),
            "suite_hash": Hash32::digest(suite.as_bytes()).to_string(),
            "ref_solution_hash": Hash32::digest(reference.as_bytes()).to_string(),
            "grader_public_key": grader.public_key,
        });
        let keystore = self.at.keystore.contract.to_string();
        Ok(json!({
            "job_id": job_id.to_string(),
            "transactions": [
                { "receiverId": contract, "actions": [{ "type": "FunctionCall", "params": {
                    "methodName": "commit_suite", "args": commit_args,
                    "gas": "50000000000000", "deposit": "0" } }] },
                { "receiverId": keystore, "actions": [{ "type": "FunctionCall", "params": {
                    "methodName": "store_secrets", "args": store_args,
                    "gas": "100000000000000",
                    "deposit": store_deposit.as_yoctonear().to_string() } }] },
            ],
        }))
    }

    /// The id of a job `account` already posted for `spec_uri`, if any, so a
    /// retried payment never posts a second job.
    async fn posted_job(&self, account: &str, spec_uri: &str) -> Option<u64> {
        let mut from = 0u64;
        loop {
            let page: Vec<Job> = self
                .at
                .escrow
                .view("list_jobs", json!({ "from": from.to_string(), "limit": 50 }))
                .await
                .ok()?;
            if let Some(j) = page.iter().find(|j| j.buyer == account && j.spec_uri == spec_uri) {
                return j.id.parse().ok();
            }
            if page.len() < 50 {
                return None;
            }
            from += 50;
        }
    }

    /// Checks the buyer's `commit_suite` and `store_secrets`; the house then
    /// clears the reference; the suite is kept until the job ends (plus seven
    /// days unless the buyer publishes it).
    ///
    /// # Errors
    /// The transactions are not what was asked for, or did not succeed.
    pub async fn stored(
        &self,
        id: &str,
        commit_tx: &str,
        store_tx: &str,
    ) -> Result<Record, Refusal> {
        let record = self.get(id)?;
        if record.stage != Stage::Storing {
            return Err(wrong_stage("nothing is waiting to be stored"));
        }
        let account = record.account.clone().ok_or_else(busy)?;
        self.unused(id, &[commit_tx, store_tx])?;
        let contract = self.at.escrow.contract_id().to_string();
        let commit = self.tx(commit_tx, &account).await?;
        let Some(args) = function_call(&commit, &contract, "commit_suite") else {
            return Err(bad("that transaction is not a commit_suite from your account"));
        };
        if success_json(&commit).is_none() {
            return Err(bad("that commit_suite did not succeed"));
        }
        // The commit must be this request's job and suite.
        let suite = record.draft.as_ref().and_then(|d| d.suite.clone()).unwrap_or_default();
        if args["job_id"].as_str() != record.job_id.map(|j| j.to_string()).as_deref()
            || args["suite_hash"] != Hash32::digest(suite.as_bytes()).to_string().as_str()
        {
            return Err(bad("that commit_suite is for a different job or suite"));
        }
        let store = self.tx(store_tx, &account).await?;
        let keystore = self.at.keystore.contract.to_string();
        if function_call(&store, &keystore, "store_secrets").is_none()
            || success_json(&store).is_none()
        {
            return Err(bad(
                "that transaction is not a successful store_secrets from your account",
            ));
        }
        // The reference is no longer needed: the enclave has its copy. The
        // suite is kept so the buyer can publish it once the job ends.
        self.update(id, |r| {
            r.stage = Stage::Posted;
            r.error = None;
            r.txs.extend([commit_tx.to_owned(), store_tx.to_owned()]);
            if let Some(d) = r.draft.as_mut() {
                d.reference = None;
            }
        })
        .ok_or_else(busy)
    }

    /// A finished transaction sent by `signer`, from the RPC.
    async fn tx(&self, hash: &str, signer: &str) -> Result<Value, Refusal> {
        if hash.len() < 40 || hash.len() > 50 || !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(bad("not a transaction hash"));
        }
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "EXPERIMENTAL_tx_status",
            "params": { "tx_hash": hash, "sender_account_id": signer, "wait_until": "FINAL" } });
        for _ in 0..5 {
            let reply: Option<Value> = async {
                let mut req = self.http.post(&self.at.rpc);
                if let Some(key) =
                    self.at.rpc_key.as_deref().filter(|_| sphinx_near::is_fastnear(&self.at.rpc))
                {
                    req = req.bearer_auth(key);
                }
                req.json(&body)
                    .timeout(Duration::from_secs(30))
                    .send()
                    .await
                    .ok()?
                    .json()
                    .await
                    .ok()
            }
            .await;
            if let Some(result) = reply.as_ref().and_then(|r| r.get("result")) {
                if result["transaction"]["signer_id"] == signer {
                    return Ok(result.clone());
                }
                return Err(bad("that transaction was not sent by your account"));
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        Err(upstream("the transaction could not be found on chain"))
    }

    /// NEAR in US dollars from 1Click's token list, cached for ten minutes.
    async fn usd_per_near(&self) -> Option<f64> {
        if let Ok(p) = self.price.lock() {
            if let Some((price, at)) = *p {
                if at.elapsed() < Duration::from_secs(600) {
                    return Some(price);
                }
            }
        }
        let tokens: Value = self
            .http
            .get(format!("{}/v0/tokens", self.at.one_click))
            .header("user-agent", "sphinx-house")
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let price = tokens.as_array()?.iter().find(|t| t["assetId"] == "nep141:wrap.near")?
            ["price"]
            .as_f64()?;
        if let Ok(mut p) = self.price.lock() {
            *p = Some((price, Instant::now()));
        }
        Some(price)
    }

    /// What posting costs in USDC from `chain`: a dry 1Click quote for an
    /// exact-output swap to wNEAR, a price only. Nothing is swapped and no
    /// deposit address is issued. The quote names the buyer's own account
    /// when it is a mainnet account; 1Click has no testnet, so for a testnet
    /// buyer it names `intents.near`, which only prices the swap. Sphinx's
    /// fee account is never the recipient or the refund address.
    ///
    /// # Errors
    /// Unknown chain or request, or 1Click could not be reached.
    pub async fn quote(&self, id: &str, chain: &str) -> Result<Value, Refusal> {
        let record = self.get(id)?;
        let origin = match chain {
            "base" => "nep141:base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near",
            "arbitrum" => "nep141:arb-0xaf88d065e77c8cc2239327c5edb3a432268e5831.omft.near",
            "ethereum" => "nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near",
            "solana" => "nep141:sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near",
            "near" => "nep141:17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1",
            _ => return Err(bad("pay from base, arbitrum, ethereum, solana or near")),
        };
        let reward = Self::near(&record.reward_near)?;
        let fee = Self::near(&self.terms.drafting_fee_near)?;
        // The reward, the drafting fee, and about 0.05 NEAR for storage and gas.
        let need = reward + fee + YOCTO / 20;
        let deadline = SystemTime::now() + Duration::from_mins(30);
        let receiver = quote_account(record.account.as_deref().unwrap_or(""));
        let request = json!({
            "dry": true, "swapType": "EXACT_OUTPUT", "slippageTolerance": 100,
            "originAsset": origin, "depositType": "ORIGIN_CHAIN",
            "destinationAsset": "nep141:wrap.near", "amount": need.to_string(),
            "refundTo": receiver, "refundType": "INTENTS",
            "recipient": receiver, "recipientType": "DESTINATION_CHAIN",
            "deadline": rfc3339(deadline),
            "appFees": [{ "recipient": self.terms.swap_fee_account, "fee": self.terms.swap_fee_bps }],
        });
        let reply: Value = self
            .http
            .post(format!("{}/v0/quote", self.at.one_click))
            .header("user-agent", "sphinx-house")
            .json(&request)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| upstream(format!("1Click could not be reached: {e}")))?
            .json()
            .await
            .map_err(|e| upstream(e.to_string()))?;
        let q = &reply["quote"];
        if q.is_null() {
            return Err(upstream(
                reply["message"].as_str().unwrap_or("1Click gave no quote").to_owned(),
            ));
        }
        Ok(json!({
            "chain": chain,
            "usdc_in": q["amountInFormatted"],
            "near_out": format_near(need),
            "seconds": q["timeEstimate"],
            "swap_fee_bps": self.terms.swap_fee_bps,
            "estimate_only": true,
        }))
    }

    /// Everything the visitor's page shows.
    ///
    /// # Errors
    /// Unknown id.
    pub async fn view(&self, id: &str) -> Result<Value, Refusal> {
        let record = self.get(id)?;
        let usd = self.usd_per_near().await;
        let fee = &self.terms.drafting_fee_near;
        let to_usd = |near: &str| -> Option<f64> {
            let n: f64 = near.parse().ok()?;
            usd.map(|p| (n * p * 100.0).round() / 100.0)
        };
        let mut out = json!({
            "id": record.id,
            "stage": record.stage,
            "request": record.request,
            "change": record.change,
            "edits_left": record.edits_left,
            "progress": record.progress,
            "error": record.error,
            "account": record.account,
            "terms": {
                "reward_near": record.reward_near, "reward_usd": to_usd(&record.reward_near),
                "drafting_fee_near": fee, "drafting_fee_usd": to_usd(fee),
                "suggested_reward_near": self.terms.suggested_reward_near,
                "fee_on_payout_percent": 5, "bond_near": self.terms.bond_near,
                "deadline_hours": self.terms.deadline_hours,
            },
            "house_wrote_the_tests": true,
            "tests_revealed": record.tests_revealed,
            "tests_held": record.draft.as_ref().is_some_and(|d| d.suite.is_some()),
            "attested_by": record.attested_by,
            "draft": record.draft.as_ref().map(|d| json!({
                "title": d.title, "spec": d.spec, "entry": d.entry, "tests": d.tests,
                "checks": d.checks, "agreed": d.agreed, "task_rewritten": d.task_rewritten,
            })),
        });
        if let Some(job_id) = record.job_id {
            out["job"] = self.job_progress(job_id).await.unwrap_or(Value::Null);
        }
        Ok(out)
    }

    async fn job_progress(&self, job_id: u64) -> Option<Value> {
        let job: Job = self
            .at
            .escrow
            .view::<Option<Job>>("get_job", json!({ "job_id": job_id.to_string() }))
            .await
            .ok()??;
        let subs: Vec<Submission> = self
            .at
            .escrow
            .view("list_submissions", json!({ "job_id": job_id.to_string(), "limit": 64 }))
            .await
            .unwrap_or_default();
        let best = subs.iter().filter(|s| s.total > 0).map(|s| (s.passed, s.total)).max();
        let contract = self.at.escrow.contract_id().to_string();
        let winning = job.winning_sub_id.and_then(|w| subs.iter().find(|s| s.sub_id == w));
        Some(json!({
            "id": job.id,
            "uid": sphinx_near::uid::job_uid(&contract, job_id),
            "status": job.status,
            "submissions": job.sub_count,
            "pending": job.pending_count,
            "best": best.map(|(p, t)| json!({ "passed": p, "total": t })),
            "paid": job.status == JobStatus::Paid,
            "winner": job.winner,
            "winning_code_hash": winning.map(|s| s.code_hash.to_string()),
            "winning_uid": winning.map(|s| sphinx_near::uid::submission_uid(&contract, job_id, s.sub_id)),
        }))
    }
}

/// The arguments of a function call to `receiver`'s `method` in `tx`.
fn function_call(tx: &Value, receiver: &str, method: &str) -> Option<Value> {
    let t = &tx["transaction"];
    if t["receiver_id"] != receiver {
        return None;
    }
    t["actions"].as_array()?.iter().find_map(|a| {
        let call = a.get("FunctionCall")?;
        if call["method_name"] != method {
            return None;
        }
        let bytes = base64_decode(call["args"].as_str()?)?;
        serde_json::from_slice(&bytes).ok()
    })
}

/// Whether `tx` transfers at least `amount` yoctoNEAR to `receiver`.
fn transfer_of_at_least(tx: &Value, receiver: &str, amount: u128) -> bool {
    let t = &tx["transaction"];
    t["receiver_id"] == receiver
        && t["actions"].as_array().is_some_and(|actions| {
            actions.iter().any(|a| {
                a["Transfer"]["deposit"]
                    .as_str()
                    .and_then(|d| d.parse::<u128>().ok())
                    .is_some_and(|d| d >= amount)
            })
        })
}

/// The decoded return value of a transaction that succeeded, `Value::Null`
/// for success without one; `None` if it failed.
fn success_json(tx: &Value) -> Option<Value> {
    let status = &tx["status"];
    if status.get("Failure").is_some() {
        return None;
    }
    if let Some(receipts) = tx["receipts_outcome"].as_array() {
        if receipts.iter().any(|r| r["outcome"]["status"].get("Failure").is_some()) {
            return None;
        }
    }
    let value = status.get("SuccessValue")?.as_str()?;
    if value.is_empty() {
        return Some(Value::Null);
    }
    base64_decode(value).and_then(|b| serde_json::from_slice(&b).ok()).or(Some(Value::Null))
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn format_near(yocto: u128) -> String {
    let whole = yocto / YOCTO;
    let frac = format!("{:024}", yocto % YOCTO);
    let frac = frac.trim_end_matches('0');
    let frac = &frac[..frac.len().min(5)];
    if frac.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

#[allow(clippy::many_single_char_names, reason = "the civil-date formula's own names")]
fn rfc3339(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01.
    let z = i64::try_from(secs / 86_400).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The drafting step in plain words.
pub fn describe(p: &Progress) -> String {
    match p {
        Progress::Writing { round: 1 } => {
            "Writing the task, a reference solution and the hidden tests".into()
        }
        Progress::Writing { round } => format!("Revising the draft (round {round})"),
        Progress::ReferenceFailed { failures } => {
            format!("The reference solution failed {failures} of its own tests; fixing it")
        }
        Progress::SecondOpinion => "A second solver is trying the task from the text alone".into(),
        Progress::ThirdOpinion => "A third solver is breaking the ties".into(),
        Progress::Compared { agreed, total } => {
            format!("The second solver matched the reference on {agreed} of {total} tests")
        }
        Progress::Reviewing { round, differences } => {
            format!("Settling {differences} differences between the two (round {round})")
        }
    }
}

/// The checks, in plain English: each one line (two at most, split by a
/// full stop), with no final full stop.
pub fn checks(tests: usize, agreed: usize, total: usize, differences: usize) -> Vec<String> {
    let mut out = vec![
        format!("We wrote {tests} hidden tests from your words, and agents only ever learn how many they pass"),
        "Our own solution passes every test, and the enclave checks that again before the job opens".to_owned(),
    ];
    out.push(if differences == 0 {
        format!("A second solver read only the task text and matched our answers on all {total} tests, so the task says what the tests check")
    } else {
        format!("Independent solvers disagreed with our answers on {} of {total} tests, so honest agents could fail this draft. Say exactly what you want and we redraft it", total - agreed)
    });
    out.push("Your reward pays only for code that passes every test, and comes back to you if nobody does by the deadline".to_owned());
    out
}

/// A random id in UUID form (so it fits the site's plain paths).
pub fn new_id() -> String {
    let b: [u8; 16] = rand::random();
    let h = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
            }
        })
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

fn client(headers: &HeaderMap) -> String {
    headers
        .get("x-sphinx-client")
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| "direct".to_owned(), |v| v.chars().take(64).collect())
}

#[derive(Deserialize)]
struct StartBody {
    request: String,
}

#[derive(Deserialize)]
struct EditBody {
    change: String,
}

#[derive(Deserialize)]
struct RewardBody {
    reward_near: String,
}

#[derive(Deserialize)]
struct PayBody {
    account: String,
}

#[derive(Deserialize)]
struct PaidBody {
    post_tx: String,
    fee_tx: String,
}

#[derive(Deserialize)]
struct StoredBody {
    commit_tx: String,
    store_tx: String,
}

/// The HTTP routes.
pub fn router<M: Model + Send + Sync + 'static>(house: Arc<House<M>>) -> Router {
    Router::new()
        .route("/newcomer", post(start::<M>))
        .route("/newcomer/{id}", get(view::<M>))
        .route("/newcomer/{id}/edit", post(edit::<M>))
        .route("/newcomer/{id}/reward", post(reward::<M>))
        .route("/newcomer/{id}/pay", post(pay::<M>))
        .route("/newcomer/{id}/paid", post(paid::<M>))
        .route("/newcomer/{id}/stored", post(stored::<M>))
        .route("/newcomer/{id}/store-again", post(store_again::<M>))
        .route("/newcomer/{id}/quote/{chain}", get(quote::<M>))
        .route("/newcomer/{id}/publish-tests", post(publish_tests::<M>))
        .with_state(house)
}

fn check_id(id: &str) -> Result<(), Refusal> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(Refusal::new(StatusCode::NOT_FOUND, "not_found", "no request with this id"))
    }
}

async fn start<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    headers: HeaderMap,
    Json(body): Json<StartBody>,
) -> Result<Json<Value>, Refusal> {
    let id = house.start(&client(&headers), &body.request)?;
    Ok(Json(json!({ "id": id })))
}

async fn view<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    house.view(&id).await.map(Json)
}

async fn edit<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path(id): Path<String>,
    Json(body): Json<EditBody>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    house.edit(&id, &body.change)?;
    Ok(Json(json!({ "id": id })))
}

async fn reward<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path(id): Path<String>,
    Json(body): Json<RewardBody>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    let record = house.set_reward(&id, &body.reward_near)?;
    Ok(Json(json!({ "reward_near": record.reward_near })))
}

async fn pay<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path(id): Path<String>,
    Json(body): Json<PayBody>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    house.pay(&id, &body.account).await.map(Json)
}

async fn paid<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path(id): Path<String>,
    Json(body): Json<PaidBody>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    house.paid(&id, &body.post_tx, &body.fee_tx).await.map(Json)
}

async fn store_again<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    house.store_again(&id).await.map(Json)
}

async fn stored<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path(id): Path<String>,
    Json(body): Json<StoredBody>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    let record = house.stored(&id, &body.commit_tx, &body.store_tx).await?;
    Ok(Json(json!({ "id": record.id, "job_id": record.job_id })))
}

async fn publish_tests<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    let record = house.publish_tests(&id).await?;
    Ok(Json(json!({ "id": record.id, "tests_revealed": record.tests_revealed })))
}

async fn quote<M: Model + Send + Sync + 'static>(
    State(house): State<Arc<House<M>>>,
    Path((id, chain)): Path<(String, String)>,
) -> Result<Json<Value>, Refusal> {
    check_id(&id)?;
    house.quote(&id, &chain).await.map(Json)
}

/// The account a 1Click quote names: the buyer's own on mainnet (a named
/// `.near` account or a 64-hex implicit one), else `intents.near`, which only
/// prices the swap.
fn quote_account(account: &str) -> String {
    let implicit = account.len() == 64 && account.bytes().all(|b| b.is_ascii_hexdigit());
    if account.rsplit('.').next() == Some("near") || implicit {
        account.to_owned()
    } else {
        "intents.near".to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    #[test]
    fn a_quote_names_the_buyer_on_mainnet_and_never_the_fee_account() {
        assert_eq!(quote_account("alice.near"), "alice.near");
        assert_eq!(quote_account(&"ab".repeat(32)), "ab".repeat(32));
        assert_eq!(quote_account("buyer.sphinx999.testnet"), "intents.near");
        assert_ne!(quote_account("buyer.sphinx999.testnet"), "sphinx999.near");
    }

    #[test]
    fn ids_are_uuid_shaped_and_checked() {
        let id = new_id();
        assert!(valid_id(&id), "{id}");
        assert!(!valid_id("../../etc/passwd"));
        assert!(!valid_id(&id.to_uppercase()));
        assert!(!valid_id(&id[1..]));
    }

    #[test]
    fn checks_say_when_the_solvers_disagreed() {
        assert!(checks(12, 12, 12, 0)[2].contains("all 12 tests"));
        assert!(checks(12, 10, 12, 2)[2]
            .contains("on 2 of 12 tests, so honest agents could fail this draft"));
    }

    #[test]
    fn counters_reset_after_a_day() {
        let mut c = Counters::default();
        let t0 = Instant::now();
        c.roll(t0);
        c.drafts_total = 5;
        c.roll(t0 + Duration::from_secs(60));
        assert_eq!(c.drafts_total, 5);
        c.roll(t0 + DAY);
        assert_eq!(c.drafts_total, 0);
    }

    #[test]
    fn near_amounts_and_dates_format() {
        assert_eq!(format_near(YOCTO / 2 + YOCTO), "1.5");
        assert_eq!(format_near(YOCTO * 2), "2");
        assert_eq!(format_near(12_345_678_900_000_000_000_000), "0.01234");
        assert_eq!(rfc3339(UNIX_EPOCH + Duration::from_mins(29_900_000)), "2026-11-06T21:20:00Z");
    }

    fn tx(receiver: &str, actions: &Value, status: &Value) -> Value {
        json!({ "transaction": { "receiver_id": receiver, "signer_id": "buyer.testnet", "actions": actions },
                "status": status, "receipts_outcome": [] })
    }

    #[test]
    fn transactions_are_read_strictly() {
        use base64::Engine as _;
        let args = base64::engine::general_purpose::STANDARD.encode(r#"{"spec_uri":"u"}"#);
        let ok_post = tx(
            "escrow.testnet",
            &json!([{ "FunctionCall": { "method_name": "post_job", "args": args } }]),
            &json!({ "SuccessValue": base64::engine::general_purpose::STANDARD.encode("\"7\"") }),
        );
        assert_eq!(function_call(&ok_post, "escrow.testnet", "post_job").unwrap()["spec_uri"], "u");
        assert!(function_call(&ok_post, "other.testnet", "post_job").is_none());
        assert!(function_call(&ok_post, "escrow.testnet", "submit").is_none());
        assert_eq!(success_json(&ok_post).unwrap(), "7");
        let failed = tx("escrow.testnet", &json!([]), &json!({ "Failure": {} }));
        assert!(success_json(&failed).is_none());
        let fee = tx(
            "verifier.testnet",
            &json!([{ "Transfer": { "deposit": "50" } }]),
            &json!({ "SuccessValue": "" }),
        );
        assert!(transfer_of_at_least(&fee, "verifier.testnet", 50));
        assert!(!transfer_of_at_least(&fee, "verifier.testnet", 51));
        assert!(!transfer_of_at_least(&fee, "someone.testnet", 1));
    }
}

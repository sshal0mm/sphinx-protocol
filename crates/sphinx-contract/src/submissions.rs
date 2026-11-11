//! Agent-side lifecycle: submit, the grader's signed score, and bond reclaim.

use near_sdk::json_types::{U128, U64};
use near_sdk::{env, near, AccountId};
use sphinx_protocol::{Code, Hash32, Score, SphinxError};

use crate::constants::{MAX_URI_LEN, SUBMISSION_STORAGE_DEPOSIT, WIN_FLOOR};
use crate::errors::{self, ensure, Result};
use crate::events::SphinxEvent;
use crate::types::{AgentUse, Job, JobStatus, SubStatus, Submission, SubmitReceipt};
use crate::{verify_signature, Sphinx, SphinxExt};

#[near]
impl Sphinx {
    /// Queues a submission for grading. Attach exactly the job's bond plus the
    /// submission storage deposit. Rejections refund the attached deposit.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_NOT_FOUND`
    /// - `SPHINX_E_JOB_WRONG_STATE`
    /// - `SPHINX_E_SUBMIT_DEADLINE_PASSED`
    /// - `SPHINX_E_SUBMIT_BUYER_SELF`
    /// - `SPHINX_E_SUBMIT_WRONG_DEPOSIT`
    /// - `SPHINX_E_SUBMIT_URI_TOO_LONG`
    /// - `SPHINX_E_SUBMIT_BAD_HASH`
    /// - `SPHINX_E_SUBMIT_JOB_CAP`
    /// - `SPHINX_E_SUBMIT_PENDING_EXISTS`
    /// - `SPHINX_E_SUBMIT_AGENT_CAP`
    /// - `SPHINX_E_SUBMIT_DUPLICATE_CODE`
    #[payable]
    #[handle_result(aliased)]
    pub fn submit(
        &mut self,
        job_id: U64,
        code_uri: String,
        code_hash: String,
    ) -> Result<SubmitReceipt> {
        let agent = env::predecessor_account_id();
        let attached = env::attached_deposit().as_yoctonear();
        self.submit_as(agent, attached, job_id, code_uri, code_hash)
    }
}

impl Sphinx {
    /// A submission by `agent` paying `attached` (the bond plus the storage
    /// deposit), whether attached to the call or drawn from a prepaid
    /// balance.
    pub(crate) fn submit_as(
        &mut self,
        agent: AccountId,
        attached: u128,
        job_id: U64,
        code_uri: String,
        code_hash: String,
    ) -> Result<SubmitReceipt> {
        let mut job = self.job(job_id.0)?;
        let now = env::block_timestamp();
        let (mut usage, code_hash) =
            self.check_submit_inner(&job, &agent, attached, &code_uri, &code_hash, now)?;
        let usage_key = (job_id.0, agent.clone());

        let sub_id = job.sub_count;
        let nonce = Self::fresh_nonce(job_id.0, u64::from(sub_id));
        job.sub_count =
            sub_id.checked_add(1).ok_or_else(|| errors::invariant("sub id overflow"))?;
        job.pending_count = job.pending_count.saturating_add(1);
        usage.pending = Some(sub_id);
        self.add_locked(job.bond.0)?;
        self.record_for(&agent, |s| s.submissions = s.submissions.saturating_add(1));
        let submission = Submission {
            job_id,
            sub_id,
            agent: agent.clone(),
            code_uri,
            code_hash,
            nonce,
            status: SubStatus::Pending,
            passed: 0,
            total: 0,
            bond: job.bond,
            submitted_at_ns: U64(now),
            settled_at_ns: None,
        };
        let receipt = SubmitReceipt {
            job_id,
            sub_id,
            nonce,
            reclaim_after_ns: U64(submission.reclaim_after_ns()),
        };
        SphinxEvent::Submitted { job_id, sub_id, agent, code_hash, nonce }.emit();
        self.code_hashes.insert((job_id.0, code_hash));
        self.agent_use.insert(usage_key, usage);
        self.submissions.insert((job_id.0, sub_id), submission);
        self.jobs.insert(job_id.0, job);
        Ok(receipt)
    }
}

#[near]
impl Sphinx {
    /// Records the grader's signed score for one submission. Anyone may post it;
    /// every field must match on-chain state and the signature must verify
    /// against the grader key snapshotted for the job.
    ///
    /// A full pass is recorded at once. It is paid as soon as no earlier
    /// submission on the job is still waiting for a result; until then the
    /// job stops taking submissions and `settle` pays it later. So the
    /// earliest submission that passes wins, whatever order results arrive
    /// in. Anything less than a full pass holds the agent's bond in the job:
    /// the bond is kept by the contract (paid to nobody) if the job
    /// is paid, and goes back to the agent if the job is refunded or
    /// cancelled.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_NOT_FOUND`
    /// - `SPHINX_E_JOB_WRONG_STATE`
    /// - `SPHINX_E_SCORE_NO_SUBMISSION`
    /// - `SPHINX_E_SCORE_ALREADY_GRADED`
    /// - `SPHINX_E_SCORE_BAD_COUNTS`
    /// - `SPHINX_E_SCORE_FIELD_MISMATCH`
    /// - `SPHINX_E_SIG_MALFORMED`
    /// - `SPHINX_E_SIG_INVALID`
    #[handle_result(aliased)]
    pub fn record_score(&mut self, score: Score, signature_hex: String) -> Result<()> {
        let job_id = score.job_id;
        let sub_id = score.sub_id;
        let mut job = self.job(job_id)?;
        ensure!(
            job.status == JobStatus::Open,
            errors::wrong_state(job_id, job.status, &[JobStatus::Open])
        );
        let mut sub = self.submissions.get(&(job_id, sub_id)).cloned().ok_or_else(|| {
            SphinxError::new(
                Code::ScoreNoSubmission,
                format!("job {job_id} has no submission {sub_id} (it has {})", job.sub_count),
            )
            .ctx("job_id", job_id)
            .ctx("sub_id", sub_id)
            .ctx("sub_count", job.sub_count)
        })?;
        Self::check_score(&job, &sub, &score, &signature_hex)?;
        self.take_confirmed_run(job_id, Some(sub_id), &score.signing_message())?;

        let now = env::block_timestamp();
        sub.passed = score.passed;
        sub.total = score.total;
        sub.settled_at_ns = Some(U64(now));
        job.pending_count = job
            .pending_count
            .checked_sub(1)
            .ok_or_else(|| errors::invariant("pending underflow"))?;
        job.graded_count = job.graded_count.saturating_add(1);
        let usage_key = (job_id, sub.agent.clone());
        let mut usage = self.agent_use.get(&usage_key).cloned().unwrap_or_default();
        usage.pending = None;
        usage.graded = usage.graded.saturating_add(1);
        self.results_recorded = self.results_recorded.saturating_add(1);

        if job.assessment.is_some() {
            self.agent_use.insert(usage_key, usage);
            self.settle_assessment_attempt(&mut job, sub, &score, now)?;
            self.jobs.insert(job_id, job);
            return Ok(());
        }
        if score.is_pass() {
            sub.status = SubStatus::Passed;
            self.passes = self.passes.saturating_add(1);
            self.record_for(&sub.agent, |s| s.passes = s.passes.saturating_add(1));
            if job.first_pass.is_none_or(|first| sub_id < first) {
                job.first_pass = Some(sub_id);
            }
            SphinxEvent::PassRecorded {
                job_id: U64(job_id),
                sub_id,
                agent: sub.agent.clone(),
                passed: score.passed,
                total: score.total,
            }
            .emit();
        } else {
            sub.status = SubStatus::Failed;
            self.fails = self.fails.saturating_add(1);
            job.held_bonds = U128(
                job.held_bonds
                    .0
                    .checked_add(sub.bond.0)
                    .ok_or_else(|| errors::invariant("held bonds overflow"))?,
            );
            self.record_for(&sub.agent, |s| s.fails = s.fails.saturating_add(1));
            SphinxEvent::Disputed {
                job_id: U64(job_id),
                sub_id,
                agent: sub.agent.clone(),
                passed: score.passed,
                total: score.total,
                bond_held: sub.bond,
            }
            .emit();
        }
        self.agent_use.insert(usage_key, usage);
        self.submissions.insert((job_id, sub_id), sub);
        if job.first_pass.is_some() {
            self.try_settle(&mut job, now)?;
        }
        self.jobs.insert(job_id, job);
        Ok(())
    }

    /// Pays the earliest passing submission once every earlier submission
    /// has finished. Earlier submissions still waiting for a result are
    /// expired (bond returned) once their own grading window has passed, so
    /// nothing can hold a proven pass for longer than one grading window.
    /// Anyone may call it.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_NOT_FOUND`
    /// - `SPHINX_E_SETTLE_NOTHING`
    /// - `SPHINX_E_SETTLE_BLOCKED`
    #[handle_result(aliased)]
    pub fn settle(&mut self, job_id: U64) -> Result<()> {
        let mut job = self.job(job_id.0)?;
        ensure!(
            job.status == JobStatus::Open && job.first_pass.is_some(),
            SphinxError::new(
                Code::SettleNothing,
                format!("job {} has no passing submission waiting to be paid", job_id.0),
            )
            .ctx("job_id", job_id.0)
            .ctx("status", job.status.as_str())
        );
        let now = env::block_timestamp();
        let blocked = self.try_settle(&mut job, now)?;
        self.jobs.insert(job_id.0, job);
        if let Some((blocking_sub_id, retry_after_ns)) = blocked {
            return Err(SphinxError::new(
                Code::SettleBlocked,
                format!(
                    "submission {blocking_sub_id} on job {} is still inside its grading window",
                    job_id.0
                ),
            )
            .ctx("job_id", job_id.0)
            .ctx("blocking_sub_id", blocking_sub_id)
            .ctx("retry_after_ns", retry_after_ns)
            .into());
        }
        Ok(())
    }

    /// Returns the bond of a submission that never received a result. Allowed
    /// once the job has ended; otherwise the agent may call it once the
    /// submission's grading window has passed, and anyone else one grading
    /// window later, so a third party cannot expire a submission whose score
    /// is about to land. When a later submission has already passed, anyone
    /// may expire an earlier one as soon as its own window has passed. The
    /// bond always goes to the agent. The attempt still counts toward the
    /// job's and the agent's caps, so runs that end without a result cannot be
    /// used to probe the hidden tests for free.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_NOT_FOUND`
    /// - `SPHINX_E_SCORE_NO_SUBMISSION`
    /// - `SPHINX_E_RECLAIM_NOT_PENDING`
    /// - `SPHINX_E_RECLAIM_TOO_EARLY`
    #[handle_result(aliased)]
    pub fn reclaim_bond(&mut self, job_id: U64, sub_id: u32) -> Result<()> {
        let mut job = self.job(job_id.0)?;
        let now = env::block_timestamp();
        let sub = self.check_reclaim_inner(&job, sub_id, now, &env::predecessor_account_id())?;
        self.expire(&mut job, sub, now, false)?;
        if job.status == JobStatus::Open && job.first_pass.is_some() {
            self.try_settle(&mut job, now)?;
        }
        self.jobs.insert(job_id.0, job);
        Ok(())
    }
}

impl Sphinx {
    /// Every check `submit` makes before it changes state. Shared by `submit`
    /// and the `check_submit` view, so a relay can refuse a call the contract
    /// would refuse without paying gas for it.
    pub(crate) fn check_submit_inner(
        &self,
        job: &Job,
        agent: &AccountId,
        attached: u128,
        code_uri: &str,
        code_hash: &str,
        now: u64,
    ) -> Result<(AgentUse, Hash32)> {
        let job_id = job.id;
        ensure!(
            job.status == JobStatus::Open,
            errors::wrong_state(job_id.0, job.status, &[JobStatus::Open])
        );
        ensure!(
            now < job.deadline_ns.0,
            SphinxError::new(
                Code::SubmitDeadlinePassed,
                format!("job {} closed at {}; now is {now}", job_id.0, job.deadline_ns.0),
            )
            .ctx("job_id", job_id.0)
            .ctx("deadline_ns", job.deadline_ns.0)
            .ctx("now_ns", now)
        );
        ensure!(
            job.first_pass.is_none(),
            SphinxError::new(
                Code::SubmitJobClosing,
                format!(
                    "job {} has a passing submission waiting for earlier ones to finish",
                    job_id.0
                ),
            )
            .ctx("job_id", job_id.0)
        );
        if let Some(invited) = self.invites.get(&job_id.0) {
            ensure!(
                invited.contains(agent),
                SphinxError::new(
                    Code::SubmitNotInvited,
                    format!(
                        "job {} is open only to the agents its buyer invited ({} of them)",
                        job_id.0,
                        invited.len()
                    ),
                )
                .ctx("job_id", job_id.0)
                .ctx("agent", agent)
            );
        }
        ensure!(
            agent != &job.buyer,
            SphinxError::new(
                Code::SubmitBuyerSelf,
                format!("the buyer {agent} cannot submit to its own job {}", job_id.0)
            )
            .ctx("job_id", job_id.0)
        );
        self.check_qualified(job, agent, now)?;
        let expected = Self::submit_deposit(job);
        ensure!(
            expected == attached,
            SphinxError::new(
                Code::SubmitWrongDeposit,
                format!(
                    "attached {attached} yoctoNEAR; job {} requires exactly {} (bond {} + storage deposit {}{})",
                    job_id.0,
                    expected,
                    job.bond.0,
                    SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(),
                    if job.assessment.is_some() { " + badge record deposit" } else { "" }
                ),
            )
            .ctx("job_id", job_id.0)
            .ctx("attached", attached)
            .ctx("expected", expected)
            .ctx("bond", job.bond.0)
            .ctx("storage_deposit", SUBMISSION_STORAGE_DEPOSIT.as_yoctonear())
        );
        ensure!(
            code_uri.len() <= MAX_URI_LEN,
            SphinxError::new(
                Code::SubmitUriTooLong,
                format!("code_uri is {} bytes; the maximum is {MAX_URI_LEN}", code_uri.len())
            )
            .ctx("len", code_uri.len())
            .ctx("max", MAX_URI_LEN)
        );
        let code_hash = code_hash
            .parse::<Hash32>()
            .map_err(|e| errors::bad_hash(Code::SubmitBadHash, "code_hash", &e))?;

        let usage = self.check_capacity(job, agent, code_hash)?;
        Ok((usage, code_hash))
    }

    /// Marks a pending submission expired and returns its bond. Returns true
    /// when the bond was credited rather than transferred. With `quiet`, the
    /// caller reports it in one event for many submissions.
    fn expire(
        &mut self,
        job: &mut Job,
        mut sub: Submission,
        now: u64,
        quiet: bool,
    ) -> Result<bool> {
        let job_id = job.id;
        sub.status = SubStatus::Expired;
        sub.settled_at_ns = Some(U64(now));
        job.pending_count = job
            .pending_count
            .checked_sub(1)
            .ok_or_else(|| errors::invariant("pending underflow"))?;
        let usage_key = (job_id.0, sub.agent.clone());
        let mut usage = self.agent_use.get(&usage_key).cloned().unwrap_or_default();
        usage.pending = None;
        // An assessment allows one attempt; one the grader never scored does not
        // use it up.
        if job.assessment.is_none() {
            usage.graded = usage.graded.saturating_add(1);
        }
        self.expired = self.expired.saturating_add(1);
        self.record_for(&sub.agent, |s| s.expired = s.expired.saturating_add(1));
        self.sub_locked(sub.bond.0)?;
        let credited = self.return_bond(sub.agent.clone(), sub.bond.0, "bond_returned");
        if !quiet {
            SphinxEvent::SubmissionExpired {
                job_id,
                sub_id: sub.sub_id,
                agent: sub.agent.clone(),
                bond_returned: sub.bond,
            }
            .emit();
        }
        self.agent_use.insert(usage_key, usage);
        self.submissions.insert((job_id.0, sub.sub_id), sub);
        Ok(credited)
    }

    /// Pays `job.first_pass` if no earlier submission is still pending,
    /// first expiring earlier pending submissions whose grading window has
    /// passed. Returns the earliest submission still blocking, and when it can
    /// be expired, or `None` once the job is paid. The caller stores `job`.
    pub(crate) fn try_settle(&mut self, job: &mut Job, now: u64) -> Result<Option<(u32, u64)>> {
        let Some(first) = job.first_pass else { return Ok(None) };
        let job_id = job.id.0;
        let (mut transferred, mut credited) = (Vec::new(), Vec::new());
        let mut blocked = None;
        for earlier in 0..first {
            let Some(sub) = self.submissions.get(&(job_id, earlier)).cloned() else { continue };
            if sub.status != SubStatus::Pending {
                continue;
            }
            if now < sub.reclaim_after_ns() {
                blocked = Some((earlier, sub.reclaim_after_ns()));
                break;
            }
            let sub_id = sub.sub_id;
            if self.expire(job, sub, now, true)? {
                credited.push(sub_id);
            } else {
                transferred.push(sub_id);
            }
        }
        if !transferred.is_empty() || !credited.is_empty() {
            SphinxEvent::SubmissionsExpired {
                job_id: job.id,
                bond_each: job.bond,
                transferred_sub_ids: transferred,
                credited_sub_ids: credited,
            }
            .emit();
        }
        if blocked.is_some() {
            return Ok(blocked);
        }
        self.pay_winner(job, first)?;
        Ok(None)
    }

    /// Closes `job` as paid to submission `winner`: reward minus fee plus its
    /// bond to the agent, the fee to the verifier, held bonds kept by the
    /// contract (never paid to the buyer), and bonds of later passing submissions back to their agents.
    fn pay_winner(&mut self, job: &mut Job, winner: u32) -> Result<()> {
        let job_id = job.id.0;
        let sub = self
            .submissions
            .get(&(job_id, winner))
            .cloned()
            .ok_or_else(|| errors::invariant("first pass without submission"))?;
        let reward = job.reward.0;
        let fee = reward
            .checked_mul(u128::from(job.fee_bps))
            .map(|x| x / 10_000)
            .ok_or_else(|| errors::invariant("fee overflow"))?;
        let payout = reward
            .checked_sub(fee)
            .and_then(|r| r.checked_add(sub.bond.0))
            .ok_or_else(|| errors::invariant("payout overflow"))?;
        job.status = JobStatus::Paid;
        job.winner = Some(sub.agent.clone());
        job.winning_sub_id = Some(winner);
        job.first_pass = None;
        self.fees_earned = self.fees_earned.saturating_add(fee);
        let earned = reward.saturating_sub(fee);
        let won = reward >= WIN_FLOOR.as_yoctonear();
        let at = env::block_timestamp();
        self.record_for(&sub.agent, |s| {
            s.earned = U128(s.earned.0.saturating_add(earned));
            if won {
                s.wins = s.wins.saturating_add(1);
                s.last_win_at_ns = U64(at);
            }
        });
        self.sub_locked(reward.saturating_add(sub.bond.0))?;
        Self::pay(sub.agent.clone(), payout, "reward");
        if fee > 0 {
            Self::pay(self.verifier.clone(), fee, "verifier_fee");
        }
        SphinxEvent::Released {
            job_id: job.id,
            sub_id: winner,
            agent: sub.agent.clone(),
            passed: sub.passed,
            total: sub.total,
            payout: U128(payout),
            fee: U128(fee),
        }
        .emit();

        // Held bonds are kept: they stay in the contract and are paid to
        // nobody. Paying them to the buyer would let a buyer pass its own job
        // from a second account and collect every failed agent's bond.
        let held = job.held_bonds.0;
        job.held_bonds = U128(0);
        // Later passing submissions lost the race, not the test: bonds back.
        let mut returned = 0u128;
        let mut credited = Vec::new();
        for id in 0..job.sub_count {
            if id == winner {
                continue;
            }
            let Some(other) = self.submissions.get(&(job_id, id)).cloned() else { continue };
            match other.status {
                SubStatus::Failed => self.record_for(&other.agent, |s| {
                    s.bonds_lost = U128(s.bonds_lost.0.saturating_add(other.bond.0));
                }),
                SubStatus::Passed => {
                    returned = returned.saturating_add(other.bond.0);
                    if self.return_bond(other.agent.clone(), other.bond.0, "bond_returned") {
                        credited.push(id);
                    }
                }
                SubStatus::Pending | SubStatus::Expired => {}
            }
        }
        self.sub_locked(held.saturating_add(returned))?;
        if held > 0 || returned > 0 {
            SphinxEvent::BondsReleased {
                job_id: job.id,
                burned: U128(held),
                returned_to_agents: U128(returned),
                credited_sub_ids: credited,
            }
            .emit();
        }
        Ok(())
    }

    /// Every check `reclaim_bond` makes before it changes state. Shared by
    /// `reclaim_bond` and the `check_reclaim_bond` view.
    pub(crate) fn check_reclaim_inner(
        &self,
        job: &Job,
        sub_id: u32,
        now: u64,
        caller: &AccountId,
    ) -> Result<Submission> {
        let job_id = job.id;
        let sub = self.submissions.get(&(job_id.0, sub_id)).cloned().ok_or_else(|| {
            SphinxError::new(
                Code::ScoreNoSubmission,
                format!("job {} has no submission {sub_id} (it has {})", job_id.0, job.sub_count),
            )
            .ctx("job_id", job_id.0)
            .ctx("sub_id", sub_id)
            .ctx("sub_count", job.sub_count)
        })?;
        ensure!(
            sub.status == SubStatus::Pending,
            SphinxError::new(
                Code::ReclaimNotPending,
                format!(
                    "submission {sub_id} on job {} is {}; only Pending bonds can be reclaimed",
                    job_id.0,
                    sub.status.as_str()
                ),
            )
            .ctx("job_id", job_id.0)
            .ctx("sub_id", sub_id)
            .ctx("status", sub.status.as_str())
        );
        let behind_a_pass = job.first_pass.is_some_and(|first| sub_id < first);
        let reclaim_after = if *caller == sub.agent || behind_a_pass {
            sub.reclaim_after_ns()
        } else {
            sub.reclaim_after_ns().saturating_add(crate::constants::GRADING_WINDOW_NS)
        };
        ensure!(
            job.status.is_terminal() || now >= reclaim_after,
            SphinxError::new(
                Code::ReclaimTooEarly,
                format!("submission {sub_id} is inside its grading window until {reclaim_after}; now is {now}"),
            )
            .ctx("job_id", job_id.0)
            .ctx("sub_id", sub_id)
            .ctx("reclaim_after_ns", reclaim_after)
            .ctx("now_ns", now)
        );
        Ok(sub)
    }

    /// Checks the job and agent attempt caps, the one-pending rule and
    /// duplicate code. Returns the agent's current usage on success.
    fn check_capacity(&self, job: &Job, agent: &AccountId, code_hash: Hash32) -> Result<AgentUse> {
        let job_id = job.id;
        // Every submission ever made counts, expired ones included.
        let slots_used = job.sub_count;
        ensure!(
            slots_used < job.max_subs_total,
            SphinxError::new(
                Code::SubmitJobCap,
                format!(
                    "job {} has used {slots_used} of its {} submission slots",
                    job_id.0, job.max_subs_total
                ),
            )
            .ctx("job_id", job_id.0)
            .ctx("used", slots_used)
            .ctx("cap", job.max_subs_total)
        );
        let usage = self.agent_use.get(&(job_id.0, agent.clone())).cloned().unwrap_or_default();
        if let Some(pending) = usage.pending {
            let reclaim_after =
                self.submissions.get(&(job_id.0, pending)).map_or(0, Submission::reclaim_after_ns);
            return Err(SphinxError::new(
                Code::SubmitPendingExists,
                format!("{agent} already has submission {pending} pending on job {}", job_id.0),
            )
            .ctx("job_id", job_id.0)
            .ctx("pending_sub_id", pending)
            .ctx("reclaim_after_ns", reclaim_after)
            .into());
        }
        ensure!(
            usage.graded < job.max_subs_per_agent,
            SphinxError::new(
                Code::SubmitAgentCap,
                format!(
                    "{agent} has used {}/{} attempts on job {}",
                    usage.graded, job.max_subs_per_agent, job_id.0
                ),
            )
            .ctx("job_id", job_id.0)
            .ctx("used", usage.graded)
            .ctx("cap", job.max_subs_per_agent)
        );
        ensure!(
            !self.code_hashes.contains(&(job_id.0, code_hash)),
            SphinxError::new(
                Code::SubmitDuplicateCode,
                format!("code_hash {code_hash} was already submitted to job {}", job_id.0),
            )
            .ctx("job_id", job_id.0)
            .ctx("code_hash", code_hash)
        );

        Ok(usage)
    }

    /// Checks every result field against stored state, then the signature.
    fn check_score(job: &Job, sub: &Submission, score: &Score, signature_hex: &str) -> Result<()> {
        let (job_id, sub_id) = (score.job_id, score.sub_id);
        ensure!(
            sub.status == SubStatus::Pending,
            SphinxError::new(
                Code::ScoreAlreadyGraded,
                format!(
                    "submission {sub_id} on job {job_id} is {}, not Pending",
                    sub.status.as_str()
                ),
            )
            .ctx("job_id", job_id)
            .ctx("sub_id", sub_id)
            .ctx("status", sub.status.as_str())
        );
        ensure!(
            score.total > 0 && score.passed <= score.total,
            SphinxError::new(
                Code::ScoreBadCounts,
                format!("passed {} / total {} is not a valid count", score.passed, score.total),
            )
            .ctx("passed", score.passed)
            .ctx("total", score.total)
        );
        let commit =
            job.commit.as_ref().ok_or_else(|| errors::invariant("open job without commit"))?;
        let me = env::current_account_id();
        ensure!(
            score.contract == me.as_str(),
            errors::field_mismatch("contract", &score.contract, &me)
        );
        ensure!(
            score.runtime == job.runtime,
            errors::field_mismatch("runtime", &score.runtime, &job.runtime)
        );
        ensure!(
            score.suite_hash == commit.suite_hash,
            errors::field_mismatch("suite_hash", &score.suite_hash, &commit.suite_hash)
        );
        ensure!(
            score.code_hash == sub.code_hash,
            errors::field_mismatch("code_hash", &score.code_hash, &sub.code_hash)
        );
        ensure!(
            score.nonce == sub.nonce,
            errors::field_mismatch("nonce", &score.nonce, &sub.nonce)
        );
        verify_signature(&commit.grader.public_key, &score.signing_message(), signature_hex)?;

        Ok(())
    }
}

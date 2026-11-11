//! Buyer-side lifecycle: post, commit the hidden suite, validation result,
//! cancel and refund.

use near_sdk::json_types::{U128, U64};
use near_sdk::{env, near, AccountId};
use sphinx_protocol::{Code, SphinxError, SuiteCheck, SUPPORTED_RUNTIMES};

use crate::constants::{
    GRADING_WINDOW_NS, JOB_STORAGE_DEPOSIT, MAX_INVITES, MAX_JOB_DURATION_NS, MAX_SUBS_TOTAL,
    MAX_TITLE_CHARS, MAX_URI_LEN, MIN_BOND, MIN_JOB_DURATION_NS, MIN_REWARD,
};
use crate::errors::{self, ensure, Result};
use crate::events::SphinxEvent;
use crate::types::{
    AssessmentTerms, BadgeKey, CommitReceipt, Job, JobStatus, Requirements, SuiteCommit,
};
use crate::{verify_signature, Sphinx, SphinxExt};

#[near]
impl Sphinx {
    /// Opens a job in `Draft`. Attach exactly `reward` plus the job storage
    /// deposit (see `get_config`). `bond x max_subs_total` may not exceed the
    /// reward, so the bonds agents can lose on one job never exceed its reward.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_WRONG_DEPOSIT`
    /// - `SPHINX_E_JOB_REWARD_TOO_SMALL`
    /// - `SPHINX_E_JOB_BOND_TOO_SMALL`
    /// - `SPHINX_E_JOB_BAD_CAPS`
    /// - `SPHINX_E_JOB_BOND_TOO_LARGE`
    /// - `SPHINX_E_JOB_DEADLINE_TOO_SOON`
    /// - `SPHINX_E_JOB_DEADLINE_TOO_FAR`
    /// - `SPHINX_E_JOB_URI_TOO_LONG`
    /// - `SPHINX_E_JOB_BAD_RUNTIME`
    /// - `SPHINX_E_JOB_TOO_MANY_INVITES`
    ///
    /// `invited`, when given and not empty, makes this a direct hire: only
    /// those agents may submit.
    #[payable]
    #[handle_result(aliased)]
    #[allow(clippy::too_many_arguments, reason = "mirrors the public JSON API")]
    pub fn post_job(
        &mut self,
        title: String,
        spec_uri: String,
        runtime: String,
        reward: U128,
        bond: U128,
        max_subs_per_agent: u32,
        max_subs_total: u32,
        deadline_ns: U64,
        invited: Option<Vec<AccountId>>,
        assessment: Option<AssessmentTerms>,
        requires: Option<Requirements>,
    ) -> Result<U64> {
        let now = env::block_timestamp();
        let mut invited = invited.unwrap_or_default();
        invited.sort();
        invited.dedup();
        ensure!(
            invited.len() <= MAX_INVITES,
            SphinxError::new(
                Code::JobTooManyInvites,
                format!("{} agents invited; the maximum is {MAX_INVITES}", invited.len()),
            )
            .ctx("invited", invited.len())
            .ctx("max", MAX_INVITES)
        );
        if let Some(t) = &assessment {
            crate::assessments::check_assessment_terms(t, max_subs_per_agent)?;
            ensure!(
                requires.is_none(),
                SphinxError::new(
                    Code::AssessmentBadTerms,
                    "an assessment's gate is its prerequisite, not requirements"
                )
            );
        }
        if let Some(r) = &requires {
            crate::assessments::check_requirements_terms(r)?;
        }
        check_new_job(&NewJob {
            reward: reward.0,
            bond: bond.0,
            attached: env::attached_deposit().as_yoctonear(),
            max_subs_per_agent,
            max_subs_total,
            deadline_ns: deadline_ns.0,
            now,
            spec_uri_len: spec_uri.len(),
            runtime: &runtime,
            title: &title,
            assessment: assessment.is_some(),
        })?;
        let reward_y = reward.0;

        let id = self.next_job_id;
        self.next_job_id = id.checked_add(1).ok_or_else(|| errors::invariant("job id overflow"))?;
        self.add_locked(reward_y)?;
        let buyer = env::predecessor_account_id();
        SphinxEvent::JobPosted {
            job_id: U64(id),
            buyer: buyer.clone(),
            reward,
            bond,
            runtime: runtime.clone(),
            deadline_ns,
        }
        .emit();
        self.jobs.insert(
            id,
            Job {
                id: U64(id),
                buyer: buyer.clone(),
                title,
                assessment: assessment.clone(),
                requires: requires.filter(|r| !r.badges.is_empty() || r.min_wins > 0),
                spec_uri,
                runtime,
                reward,
                bond,
                fee_bps: self.fee_bps,
                max_subs_per_agent,
                max_subs_total,
                deadline_ns,
                created_at_ns: U64(now),
                status: JobStatus::Draft,
                commit: None,
                sub_count: 0,
                pending_count: 0,
                graded_count: 0,
                winner: None,
                winning_sub_id: None,
                first_pass: None,
                held_bonds: U128(0),
            },
        );
        if !invited.is_empty() {
            SphinxEvent::AgentsInvited { job_id: U64(id), agents: invited.clone() }.emit();
            self.invites.insert(id, invited);
        }
        if let Some(t) = assessment {
            self.latest_assessment.insert(BadgeKey { issuer: buyer, skill: t.skill }, id);
        }
        Ok(U64(id))
    }

    /// Locks the hashes of the hidden suite and the reference solution, and
    /// snapshots the current grader key into the job. Must happen before any
    /// submission is possible.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_NOT_FOUND`
    /// - `SPHINX_E_AUTH_NOT_BUYER`
    /// - `SPHINX_E_JOB_WRONG_STATE`
    /// - `SPHINX_E_COMMIT_BAD_HASH`
    /// - `SPHINX_E_COMMIT_NO_GRADER_KEY`
    /// - `SPHINX_E_COMMIT_GRADER_KEY_CHANGED`
    ///
    /// `grader_public_key` is the key the buyer checked the attestation of
    /// (64 hex characters). The commit fails if the registered key is
    /// different, so a key change between checking and committing cannot
    /// slip into the job.
    #[handle_result(aliased)]
    pub fn commit_suite(
        &mut self,
        job_id: U64,
        suite_hash: String,
        ref_solution_hash: String,
        grader_public_key: String,
    ) -> Result<CommitReceipt> {
        let mut job = self.job(job_id.0)?;
        let caller = env::predecessor_account_id();
        ensure!(caller == job.buyer, errors::not_buyer(&caller, job_id.0, &job.buyer));
        ensure!(
            job.status == JobStatus::Draft,
            errors::wrong_state(job_id.0, job.status, &[JobStatus::Draft])
        );
        let suite_hash = suite_hash
            .parse()
            .map_err(|e| errors::bad_hash(Code::CommitBadHash, "suite_hash", &e))?;
        let ref_solution_hash = ref_solution_hash
            .parse()
            .map_err(|e| errors::bad_hash(Code::CommitBadHash, "ref_solution_hash", &e))?;
        let grader = self.grader.clone().ok_or_else(|| {
            SphinxError::new(
                Code::CommitNoGraderKey,
                "no grader key is registered; jobs cannot be committed yet",
            )
        })?;
        ensure!(
            grader.public_key == grader_public_key,
            SphinxError::new(
                Code::CommitGraderKeyChanged,
                format!(
                    "the registered grader key is {}, not {grader_public_key}",
                    grader.public_key
                ),
            )
            .ctx("current", &grader.public_key)
            .ctx("given", &grader_public_key)
        );
        let suite_check_nonce = Self::fresh_nonce(job_id.0, u64::MAX);
        SphinxEvent::SuiteCommitted { job_id, suite_hash, ref_solution_hash, suite_check_nonce }
            .emit();
        let receipt = CommitReceipt {
            job_id,
            suite_check_nonce,
            grader_public_key: grader.public_key.clone(),
            grader_mode: grader.mode,
        };
        job.commit = Some(SuiteCommit { suite_hash, ref_solution_hash, suite_check_nonce, grader });
        job.status = JobStatus::Committed;
        self.jobs.insert(job_id.0, job);
        Ok(receipt)
    }

    /// Records the grader's validation result. Anyone may post it; the
    /// signature is the authority. An invalid suite refunds the buyer.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_NOT_FOUND`
    /// - `SPHINX_E_JOB_WRONG_STATE`
    /// - `SPHINX_E_SUITE_CHECK_DEADLINE_PASSED`
    /// - `SPHINX_E_SCORE_FIELD_MISMATCH`
    /// - `SPHINX_E_SIG_MALFORMED`
    /// - `SPHINX_E_SIG_INVALID`
    #[handle_result(aliased)]
    pub fn record_suite_check(&mut self, check: SuiteCheck, signature_hex: String) -> Result<()> {
        let job_id = check.job_id;
        let mut job = self.job(job_id)?;
        ensure!(
            job.status == JobStatus::Committed,
            errors::wrong_state(job_id, job.status, &[JobStatus::Committed])
        );
        let now = env::block_timestamp();
        ensure!(
            now < job.deadline_ns.0,
            SphinxError::new(
                Code::SuiteCheckDeadlinePassed,
                format!(
                    "job {job_id} deadline {} passed before validation (now {now})",
                    job.deadline_ns.0
                ),
            )
            .ctx("job_id", job_id)
            .ctx("deadline_ns", job.deadline_ns.0)
            .ctx("now_ns", now)
        );
        let commit =
            job.commit.as_ref().ok_or_else(|| errors::invariant("committed job without commit"))?;
        let me = env::current_account_id();
        ensure!(
            check.contract == me.as_str(),
            errors::field_mismatch("contract", &check.contract, &me)
        );
        ensure!(
            check.suite_hash == commit.suite_hash,
            errors::field_mismatch("suite_hash", &check.suite_hash, &commit.suite_hash)
        );
        ensure!(
            check.ref_solution_hash == commit.ref_solution_hash,
            errors::field_mismatch(
                "ref_solution_hash",
                &check.ref_solution_hash,
                &commit.ref_solution_hash
            )
        );
        ensure!(
            check.nonce == commit.suite_check_nonce,
            errors::field_mismatch("nonce", &check.nonce, &commit.suite_check_nonce)
        );
        verify_signature(&commit.grader.public_key, &check.signing_message(), &signature_hex)?;
        self.take_confirmed_run(job_id, None, &check.signing_message())?;

        SphinxEvent::SuiteChecked { job_id: U64(job_id), valid: check.valid }.emit();
        if check.valid {
            job.status = JobStatus::Open;
        } else {
            job.status = JobStatus::Rejected;
            self.sub_locked(job.reward.0)?;
            Self::pay(job.buyer.clone(), job.reward.0, "suite_rejected");
        }
        self.jobs.insert(job_id, job);
        Ok(())
    }

    /// Cancels a job and refunds the reward: any time before a submission,
    /// and after submissions while none passed and none waits for a score.
    /// Bonds held from failed submissions go back to their agents.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_NOT_FOUND`
    /// - `SPHINX_E_AUTH_NOT_BUYER`
    /// - `SPHINX_E_JOB_WRONG_STATE`
    /// - `SPHINX_E_JOB_HAS_SUBMISSIONS`
    #[handle_result(aliased)]
    pub fn cancel_job(&mut self, job_id: U64) -> Result<()> {
        let mut job = self.job(job_id.0)?;
        let caller = env::predecessor_account_id();
        ensure!(caller == job.buyer, errors::not_buyer(&caller, job_id.0, &job.buyer));
        let cancellable = [JobStatus::Draft, JobStatus::Committed, JobStatus::Open];
        ensure!(
            cancellable.contains(&job.status),
            errors::wrong_state(job_id.0, job.status, &cancellable)
        );
        // With submissions, the buyer may still close the job while nothing
        // passed and nothing waits for a score, so agents that fill every
        // slot with failing code cannot hold the reward until the deadline.
        // An assessment with submissions runs to its deadline: cancelling it would
        // return the bonds its failed attempts forfeited.
        ensure!(
            job.assessment.is_none() || job.sub_count == 0,
            SphinxError::new(
                Code::JobHasSubmissions,
                format!(
                    "assessment {} has submissions; close it instead (close_assessment)",
                    job_id.0
                ),
            )
            .ctx("job_id", job_id.0)
        );
        ensure!(
            job.sub_count == 0 || (job.pending_count == 0 && job.first_pass.is_none()),
            SphinxError::new(
                Code::JobHasSubmissions,
                format!(
                    "job {} has {} submissions, {} waiting for a score{}, and cannot be cancelled now",
                    job_id.0,
                    job.sub_count,
                    job.pending_count,
                    if job.first_pass.is_some() { " and a pass" } else { "" }
                ),
            )
            .ctx("job_id", job_id.0)
            .ctx("sub_count", job.sub_count)
            .ctx("pending_count", job.pending_count)
        );
        job.status = JobStatus::Cancelled;
        self.sub_locked(job.reward.0)?;
        Self::pay(job.buyer.clone(), job.reward.0, "job_cancelled");
        SphinxEvent::JobCancelled { job_id, refund: job.reward }.emit();
        // Failed bonds go back to their agents, as on a refund.
        let returned = self.return_held_bonds(&job)?;
        job.held_bonds = U128(0);
        if returned > 0 {
            SphinxEvent::BondsReleased {
                job_id,
                burned: U128(0),
                returned_to_agents: U128(returned),
                credited_sub_ids: Vec::new(),
            }
            .emit();
        }
        self.jobs.insert(job_id.0, job);
        Ok(())
    }

    /// Refunds the buyer once the deadline and the grading window after it
    /// have passed without a winner (two windows while submissions are still
    /// pending). Anyone may call it. Bonds held from failed submissions go
    /// back to their agents as unclaimed balances (`withdraw`), since nobody
    /// proved the job passable. Bonds of submissions still pending stay
    /// reclaimable by their agents.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_JOB_NOT_FOUND`
    /// - `SPHINX_E_JOB_WRONG_STATE`
    /// - `SPHINX_E_REFUND_HAS_PASS`
    /// - `SPHINX_E_REFUND_TOO_EARLY`
    #[handle_result(aliased)]
    pub fn refund_on_timeout(&mut self, job_id: U64) -> Result<()> {
        let mut job = self.job(job_id.0)?;
        let open = [JobStatus::Draft, JobStatus::Committed, JobStatus::Open];
        ensure!(open.contains(&job.status), errors::wrong_state(job_id.0, job.status, &open));
        ensure!(
            job.first_pass.is_none(),
            SphinxError::new(
                Code::RefundHasPass,
                format!("job {} has a passing submission; call settle", job_id.0),
            )
            .ctx("job_id", job_id.0)
        );
        let now = env::block_timestamp();
        let refund_after = job.refund_after_ns();
        ensure!(
            now >= refund_after,
            SphinxError::new(
                Code::RefundTooEarly,
                format!(
                    "job {} can be refunded after {refund_after} (deadline {} + grading window {GRADING_WINDOW_NS}); now is {now}",
                    job_id.0, job.deadline_ns.0
                ),
            )
            .ctx("job_id", job_id.0)
            .ctx("refund_after_ns", refund_after)
            .ctx("now_ns", now)
        );
        job.status = JobStatus::Refunded;
        self.sub_locked(job.reward.0)?;
        Self::pay(job.buyer.clone(), job.reward.0, "job_refunded");
        SphinxEvent::Refunded { job_id, buyer: job.buyer.clone(), refund: job.reward }.emit();
        let returned = self.return_held_bonds(&job)?;
        job.held_bonds = U128(0);
        if returned > 0 {
            SphinxEvent::BondsReleased {
                job_id,
                burned: U128(0),
                returned_to_agents: U128(returned),
                // A refund credits every held bond for `withdraw`.
                credited_sub_ids: Vec::new(),
            }
            .emit();
        }
        self.jobs.insert(job_id.0, job);
        Ok(())
    }
}

impl Sphinx {
    /// Credits each failed submission's held bond back to its agent as an
    /// unclaimed balance, so a refund needs no transfer per agent. Returns
    /// the total.
    fn return_held_bonds(&mut self, job: &Job) -> Result<u128> {
        // An assessment holds no bonds: a failed attempt's bond was forfeited
        // when it was scored.
        if job.assessment.is_some() {
            return Ok(0);
        }
        let mut total = 0u128;
        for id in 0..job.sub_count {
            let Some(sub) = self.submissions.get(&(job.id.0, id)).cloned() else { continue };
            if sub.status != crate::types::SubStatus::Failed {
                continue;
            }
            let balance = self.unclaimed.get(&sub.agent).copied().unwrap_or(0);
            self.unclaimed.insert(sub.agent.clone(), balance.saturating_add(sub.bond.0));
            total = total.saturating_add(sub.bond.0);
        }
        ensure!(total == job.held_bonds.0, errors::invariant("held bonds do not add up"));
        self.sub_locked(total)?;
        self.total_unclaimed = self.total_unclaimed.saturating_add(total);
        Ok(total)
    }
}

/// Parameters of `post_job` that are checked before any state changes.
struct NewJob<'a> {
    reward: u128,
    bond: u128,
    attached: u128,
    max_subs_per_agent: u32,
    max_subs_total: u32,
    deadline_ns: u64,
    now: u64,
    spec_uri_len: usize,
    runtime: &'a str,
    title: &'a str,
    /// A skill assessment: no reward, so the reward rules do not apply.
    assessment: bool,
}

/// Enforces every bound on a new job; see the error catalog for each rule.
fn check_new_job(p: &NewJob<'_>) -> Result<()> {
    check_new_job_funds(p)?;
    check_new_job_shape(p)
}

/// Deposit, reward, bond and the bond-harvest bound.
fn check_new_job_funds(p: &NewJob<'_>) -> Result<()> {
    let (reward_y, bond_y, attached, max_subs_total) =
        (p.reward, p.bond, p.attached, p.max_subs_total);
    let expected = reward_y.checked_add(JOB_STORAGE_DEPOSIT.as_yoctonear());
    ensure!(
            expected == Some(attached),
            SphinxError::new(
                Code::JobWrongDeposit,
                format!(
                    "attached {attached} yoctoNEAR; expected {} (reward {reward_y} + storage deposit {})",
                    expected.map_or_else(|| "overflow".to_owned(), |e| e.to_string()),
                    JOB_STORAGE_DEPOSIT.as_yoctonear()
                ),
            )
            .ctx("attached", attached)
            .ctx("expected", expected.unwrap_or_default())
            .ctx("reward", reward_y)
            .ctx("storage_deposit", JOB_STORAGE_DEPOSIT.as_yoctonear())
        );
    if p.assessment {
        ensure!(
            reward_y == 0,
            SphinxError::new(
                Code::AssessmentBadTerms,
                "an assessment pays no reward; attach only the job storage deposit"
            )
        );
        ensure!(
            bond_y >= MIN_BOND.as_yoctonear(),
            SphinxError::new(
                Code::JobBondTooSmall,
                format!("bond {bond_y} is below the minimum {}", MIN_BOND.as_yoctonear())
            )
            .ctx("bond", bond_y)
            .ctx("min_bond", MIN_BOND.as_yoctonear())
        );
        let _ = max_subs_total;
        return Ok(());
    }
    ensure!(
        reward_y >= MIN_REWARD.as_yoctonear(),
        SphinxError::new(
            Code::JobRewardTooSmall,
            format!("reward {reward_y} is below the minimum {}", MIN_REWARD.as_yoctonear()),
        )
        .ctx("reward", reward_y)
        .ctx("min_reward", MIN_REWARD.as_yoctonear())
    );
    ensure!(
        bond_y >= MIN_BOND.as_yoctonear(),
        SphinxError::new(
            Code::JobBondTooSmall,
            format!("bond {bond_y} is below the minimum {}", MIN_BOND.as_yoctonear())
        )
        .ctx("bond", bond_y)
        .ctx("min_bond", MIN_BOND.as_yoctonear())
    );
    let harvest = bond_y.checked_mul(u128::from(max_subs_total));
    ensure!(
        harvest.is_some_and(|h| h <= reward_y),
        SphinxError::new(
            Code::JobBondTooLarge,
            format!("bond {bond_y} x max_subs_total {max_subs_total} exceeds reward {reward_y}"),
        )
        .ctx("bond", bond_y)
        .ctx("max_subs_total", max_subs_total)
        .ctx("reward", reward_y)
    );
    Ok(())
}

/// Caps, deadline window, URI length and runtime.
fn check_new_job_shape(p: &NewJob<'_>) -> Result<()> {
    let (max_subs_per_agent, max_subs_total, now) = (p.max_subs_per_agent, p.max_subs_total, p.now);
    ensure!(
            (1..=max_subs_total).contains(&max_subs_per_agent) && max_subs_total <= MAX_SUBS_TOTAL,
            SphinxError::new(
                Code::JobBadCaps,
                format!(
                    "caps must satisfy 1 <= max_subs_per_agent ({max_subs_per_agent}) <= max_subs_total ({max_subs_total}) <= {MAX_SUBS_TOTAL}"
                ),
            )
            .ctx("max_subs_per_agent", max_subs_per_agent)
            .ctx("max_subs_total", max_subs_total)
            .ctx("max", MAX_SUBS_TOTAL)
        );
    let duration = p.deadline_ns.saturating_sub(now);
    ensure!(
        p.deadline_ns > now && duration >= MIN_JOB_DURATION_NS,
        SphinxError::new(
            Code::JobDeadlineTooSoon,
            format!(
                "deadline {} is less than {MIN_JOB_DURATION_NS} ns after now {now}",
                p.deadline_ns
            ),
        )
        .ctx("deadline_ns", p.deadline_ns)
        .ctx("now_ns", now)
        .ctx("min_duration_ns", MIN_JOB_DURATION_NS)
    );
    ensure!(
        duration <= MAX_JOB_DURATION_NS,
        SphinxError::new(
            Code::JobDeadlineTooFar,
            format!(
                "deadline {} is more than {MAX_JOB_DURATION_NS} ns after now {now}",
                p.deadline_ns
            ),
        )
        .ctx("deadline_ns", p.deadline_ns)
        .ctx("now_ns", now)
        .ctx("max_duration_ns", MAX_JOB_DURATION_NS)
    );
    ensure!(
        p.spec_uri_len <= MAX_URI_LEN,
        SphinxError::new(
            Code::JobUriTooLong,
            format!("spec_uri is {} bytes; the maximum is {MAX_URI_LEN}", p.spec_uri_len)
        )
        .ctx("len", p.spec_uri_len)
        .ctx("max", MAX_URI_LEN)
    );
    let title_len = p.title.chars().count();
    ensure!(
        (1..=MAX_TITLE_CHARS).contains(&title_len)
            && p.title.trim() == p.title
            && !p.title.chars().any(char::is_control),
        SphinxError::new(
            Code::JobBadTitle,
            format!(
                "a title is 1 to {MAX_TITLE_CHARS} characters on one line, without leading or trailing spaces; got {title_len}"
            ),
        )
        .ctx("len", title_len)
        .ctx("max", MAX_TITLE_CHARS)
    );
    ensure!(
        SUPPORTED_RUNTIMES.contains(&p.runtime),
        SphinxError::new(
            Code::JobBadRuntime,
            format!(
                "runtime {} is not supported; known: {}",
                p.runtime,
                SUPPORTED_RUNTIMES.join(", ")
            ),
        )
        .ctx("runtime", p.runtime)
        .ctx("known", SUPPORTED_RUNTIMES.join(","))
    );

    Ok(())
}

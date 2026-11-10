//! Read-only methods. Lists are paginated and bounded.

use near_sdk::json_types::{U128, U64};
use near_sdk::{env, near, AccountId};
use sphinx_protocol::SUPPORTED_RUNTIMES;

use crate::constants::{
    AGENT_STORAGE_DEPOSIT, GRADING_WINDOW_NS, JOB_STORAGE_DEPOSIT, MAX_INVITES,
    MAX_JOB_DURATION_NS, MAX_SUBS_TOTAL, MAX_URI_LEN, MIN_BOND, MIN_JOB_DURATION_NS, MIN_REWARD,
    SUBMISSION_STORAGE_DEPOSIT,
};
use crate::errors::Result;
use crate::types::{AgentUse, Config, GraderKey, Job, Stats, Submission};
use crate::{Sphinx, SphinxExt};

const MAX_PAGE: u32 = 50;

#[near]
impl Sphinx {
    pub fn get_config(&self) -> Config {
        Config {
            owner: self.owner.clone(),
            verifier: self.verifier.clone(),
            fee_bps: self.fee_bps,
            grader: self.grader.clone(),
            allow_dev_keys: self.allow_dev_keys,
            enclave: self.enclave.clone(),
            retire_after_ns: self.retire_after_ns.map(U64),
            job_storage_deposit: U128(JOB_STORAGE_DEPOSIT.as_yoctonear()),
            submission_storage_deposit: U128(SUBMISSION_STORAGE_DEPOSIT.as_yoctonear()),
            agent_storage_deposit: U128(AGENT_STORAGE_DEPOSIT.as_yoctonear()),
            max_invites: u32::try_from(MAX_INVITES).unwrap_or(u32::MAX),
            min_reward: U128(MIN_REWARD.as_yoctonear()),
            min_bond: U128(MIN_BOND.as_yoctonear()),
            max_subs_total: MAX_SUBS_TOTAL,
            max_uri_len: u32::try_from(MAX_URI_LEN).unwrap_or(u32::MAX),
            min_job_duration_ns: U64(MIN_JOB_DURATION_NS),
            max_job_duration_ns: U64(MAX_JOB_DURATION_NS),
            grading_window_ns: U64(GRADING_WINDOW_NS),
            supported_runtimes: SUPPORTED_RUNTIMES.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    /// Every grader key ever registered, oldest first, at most 50 from `from`.
    pub fn get_grader_history(&self, from: Option<u32>, limit: Option<u32>) -> Vec<GraderKey> {
        let from = from.unwrap_or(0);
        let end = from
            .saturating_add(limit.unwrap_or(MAX_PAGE).min(MAX_PAGE))
            .min(self.grader_history.len());
        (from..end).filter_map(|i| self.grader_history.get(i).cloned()).collect()
    }

    pub fn get_job(&self, job_id: U64) -> Option<Job> {
        self.jobs.get(&job_id.0).cloned()
    }

    /// Jobs with ids in `[from, from + limit)`, at most 50.
    pub fn list_jobs(&self, from: Option<U64>, limit: Option<u32>) -> Vec<Job> {
        let from = from.map_or(0, |f| f.0);
        let end = from
            .saturating_add(u64::from(limit.unwrap_or(MAX_PAGE).min(MAX_PAGE)))
            .min(self.next_job_id);
        (from..end).filter_map(|id| self.jobs.get(&id).cloned()).collect()
    }

    pub fn get_submission(&self, job_id: U64, sub_id: u32) -> Option<Submission> {
        self.submissions.get(&(job_id.0, sub_id)).cloned()
    }

    /// Submissions of a job with ids in `[from, from + limit)`, at most 50.
    pub fn list_submissions(
        &self,
        job_id: U64,
        from: Option<u32>,
        limit: Option<u32>,
    ) -> Vec<Submission> {
        let count = self.jobs.get(&job_id.0).map_or(0, |j| j.sub_count);
        let from = from.unwrap_or(0);
        let end = from.saturating_add(limit.unwrap_or(MAX_PAGE).min(MAX_PAGE)).min(count);
        (from..end).filter_map(|id| self.submissions.get(&(job_id.0, id)).cloned()).collect()
    }

    /// The agents a direct-hire job is open to; empty for an open job.
    pub fn get_invites(&self, job_id: U64) -> Vec<AccountId> {
        self.invites.get(&job_id.0).cloned().unwrap_or_default()
    }

    pub fn get_agent_usage(&self, job_id: U64, agent: AccountId) -> AgentUse {
        self.agent_use.get(&(job_id.0, agent)).cloned().unwrap_or_default()
    }

    /// Runs every check `submit` would make for `agent` attaching `deposit`,
    /// without changing state. Fails with the same `SPHINX_E_*` error `submit`
    /// would, so callers (and gas relays) can refuse a doomed call for free.
    ///
    /// # Errors
    /// Exactly the errors listed on `submit`.
    #[handle_result(aliased)]
    pub fn check_submit(
        &self,
        job_id: U64,
        agent: AccountId,
        deposit: U128,
        code_uri: String,
        code_hash: String,
    ) -> Result<()> {
        let job = self.job(job_id.0)?;
        self.check_submit_inner(
            &job,
            &agent,
            deposit.0,
            &code_uri,
            &code_hash,
            env::block_timestamp(),
        )
        .map(|_| ())
    }

    /// Runs every check `reclaim_bond` would make, without changing state.
    ///
    /// # Errors
    /// Exactly the errors listed on `reclaim_bond`.
    #[handle_result(aliased)]
    pub fn check_reclaim_bond(
        &self,
        job_id: U64,
        sub_id: u32,
        caller: Option<AccountId>,
    ) -> Result<()> {
        let job = self.job(job_id.0)?;
        // Without a caller the stricter third-party rule applies.
        let caller = caller
            .unwrap_or_else(|| "anyone.near".parse().unwrap_or_else(|_| env::current_account_id()));
        self.check_reclaim_inner(&job, sub_id, env::block_timestamp(), &caller).map(|_| ())
    }

    pub fn get_unclaimed(&self, account: AccountId) -> U128 {
        U128(self.unclaimed.get(&account).copied().unwrap_or(0))
    }

    pub fn get_stats(&self) -> Stats {
        let balance = env::account_balance().as_yoctonear();
        let storage_cost = u128::from(env::storage_usage())
            .saturating_mul(env::storage_byte_cost().as_yoctonear());
        let owed =
            self.total_locked.saturating_add(self.total_unclaimed).saturating_add(storage_cost);
        Stats {
            jobs: U64(self.next_job_id),
            results_recorded: U64(self.results_recorded),
            passes: U64(self.passes),
            fails: U64(self.fails),
            expired: U64(self.expired),
            fees_earned: U128(self.fees_earned),
            total_locked: U128(self.total_locked),
            total_unclaimed: U128(self.total_unclaimed),
            balance: U128(balance),
            storage_cost: U128(storage_cost),
            solvent: balance >= owed,
        }
    }
}

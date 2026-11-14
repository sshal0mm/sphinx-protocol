//! Read access to the escrow, behind a trait so handlers can be tested
//! without a network.

use serde_json::{json, Value};
use sphinx_near::near_api::types::transaction::actions::AccessKey;
use sphinx_near::near_api::types::transaction::delegate_action::SignedDelegateAction;
use sphinx_near::views::{AgentStats, AgentView, BadgeView, Config, Job, Submission};
use sphinx_near::Escrow;
use sphinx_protocol::{Code, SphinxError};
use std::future::Future;

/// What the API needs to read from the chain.
pub trait Chain: Send + Sync + 'static {
    /// The escrow contract's account id.
    fn contract(&self) -> &str;
    /// Marks reused answers stale after an action that may have changed the
    /// chain. Nothing to do for a reader that reuses nothing.
    fn expire(&self) {}
    fn config(&self) -> impl Future<Output = Result<Config, SphinxError>> + Send;
    fn jobs(
        &self,
        from: u64,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Job>, SphinxError>> + Send;
    fn job(&self, id: u64) -> impl Future<Output = Result<Job, SphinxError>> + Send;
    fn submissions(
        &self,
        job: u64,
        from: u32,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Submission>, SphinxError>> + Send;
    /// A page of submissions of a job that can no longer change: finished,
    /// with nothing waiting for a score. A cache may keep these for good.
    fn settled_submissions(
        &self,
        job: u64,
        from: u32,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Submission>, SphinxError>> + Send {
        self.submissions(job, from, limit)
    }
    /// A page of submissions read past any cache, for checks that must not
    /// act on a stale list (whether bytes are someone's submitted code).
    fn fresh_submissions(
        &self,
        job: u64,
        from: u32,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Submission>, SphinxError>> + Send {
        self.submissions(job, from, limit)
    }
    fn submission(
        &self,
        job: u64,
        sub: u32,
    ) -> impl Future<Output = Result<Submission, SphinxError>> + Send;
    /// One page of directory entries, at most 50 from `from`.
    fn agents(&self, from: u32)
        -> impl Future<Output = Result<Vec<AgentView>, SphinxError>> + Send;
    /// A directory entry, if the account registered.
    fn agent(
        &self,
        account: &str,
    ) -> impl Future<Output = Result<Option<AgentView>, SphinxError>> + Send;
    /// The scored record of any account.
    fn agent_stats(
        &self,
        account: &str,
    ) -> impl Future<Output = Result<AgentStats, SphinxError>> + Send;
    /// The skill badges an account holds, current or not; empty on a
    /// contract that has no assessments.
    fn badges(
        &self,
        _account: &str,
    ) -> impl Future<Output = Result<Vec<BadgeView>, SphinxError>> + Send {
        async { Ok(Vec::new()) }
    }
    /// The account holding `username`, if any; always `None` on a contract
    /// without usernames.
    fn username_owner(
        &self,
        _username: &str,
    ) -> impl Future<Output = Result<Option<String>, SphinxError>> + Send {
        async { Ok(None) }
    }
    /// Whether an assessment takes no new submissions, and whether its badges
    /// are revoked.
    fn assessment_state(
        &self,
        _job: u64,
    ) -> impl Future<Output = Result<(bool, bool), SphinxError>> + Send {
        async { Ok((false, false)) }
    }
    /// The agents a direct-hire job is open to; empty for an open job.
    fn invites(&self, job: u64) -> impl Future<Output = Result<Vec<String>, SphinxError>> + Send;
    /// The delegate action sender's access key for its public key, and the
    /// block height it was read at; `None` when there is no such key.
    fn access_key(
        &self,
        sda: &SignedDelegateAction,
    ) -> impl Future<Output = Result<(Option<AccessKey>, u64), SphinxError>> + Send;
    /// Whether `public_key` (`ed25519:...`) is one of `account`'s access keys
    /// now. Used to accept a NEP-413 signed message as coming from `account`.
    fn is_access_key(
        &self,
        account: &str,
        public_key: &str,
    ) -> impl Future<Output = Result<bool, SphinxError>> + Send;
    /// Runs a read-only `check_*` view; fails with the error the previewed
    /// call would fail with.
    fn check(
        &self,
        view: &'static str,
        args: Value,
    ) -> impl Future<Output = Result<(), SphinxError>> + Send;
}

impl Chain for Escrow {
    fn contract(&self) -> &str {
        self.contract_id().as_str()
    }

    async fn access_key(
        &self,
        sda: &SignedDelegateAction,
    ) -> Result<(Option<AccessKey>, u64), SphinxError> {
        sphinx_near::relay::access_key(self, sda).await
    }

    async fn is_access_key(&self, account: &str, public_key: &str) -> Result<bool, SphinxError> {
        use sphinx_near::near_api::Account;
        let (Ok(account), Ok(key)) = (
            account.parse::<sphinx_near::near_api::types::AccountId>(),
            public_key.parse::<sphinx_near::near_api::types::PublicKey>(),
        ) else {
            return Ok(false);
        };
        // Only a full-access key speaks for the account: a function-call key
        // handed to an app must not read the account's code.
        match Account(account).access_key(key).fetch_from(self.network()).await {
            Ok(found) => Ok(matches!(
                found.data.permission,
                sphinx_near::near_api::types::transaction::actions::AccessKeyPermission::FullAccess
            )),
            Err(e) => {
                let text = e.to_string();
                if text.contains("UnknownAccessKey")
                    || text.contains("UnknownAccount")
                    || text.contains("does not exist")
                {
                    Ok(false)
                } else {
                    Err(SphinxError::new(
                        Code::RelayRpcFailed,
                        format!("view_access_key failed: {text}"),
                    ))
                }
            }
        }
    }

    async fn config(&self) -> Result<Config, SphinxError> {
        self.view("get_config", json!({})).await
    }

    async fn jobs(&self, from: u64, limit: u32) -> Result<Vec<Job>, SphinxError> {
        self.view("list_jobs", json!({ "from": from.to_string(), "limit": limit })).await
    }

    async fn job(&self, id: u64) -> Result<Job, SphinxError> {
        let job: Option<Job> = self.view("get_job", json!({ "job_id": id.to_string() })).await?;
        job.ok_or_else(|| {
            SphinxError::new(Code::JobNotFound, format!("job {id} does not exist"))
                .ctx("job_id", id)
        })
    }

    async fn submissions(
        &self,
        job: u64,
        from: u32,
        limit: u32,
    ) -> Result<Vec<Submission>, SphinxError> {
        self.view(
            "list_submissions",
            json!({ "job_id": job.to_string(), "from": from, "limit": limit }),
        )
        .await
    }

    async fn agents(&self, from: u32) -> Result<Vec<AgentView>, SphinxError> {
        self.view("list_agents", json!({ "from": from, "limit": 50 })).await
    }

    async fn agent(&self, account: &str) -> Result<Option<AgentView>, SphinxError> {
        self.view("get_agent", json!({ "account": account })).await
    }

    async fn agent_stats(&self, account: &str) -> Result<AgentStats, SphinxError> {
        self.view("get_agent_stats", json!({ "account": account })).await
    }

    async fn badges(&self, account: &str) -> Result<Vec<BadgeView>, SphinxError> {
        self.view("get_badges", json!({ "account": account })).await
    }

    async fn username_owner(&self, username: &str) -> Result<Option<String>, SphinxError> {
        let v: Option<AgentView> =
            self.view("get_agent_by_username", json!({ "username": username })).await?;
        Ok(v.map(|a| a.profile.account))
    }

    async fn assessment_state(&self, job: u64) -> Result<(bool, bool), SphinxError> {
        self.view("get_assessment_state", json!({ "job_id": job.to_string() })).await
    }

    async fn invites(&self, job: u64) -> Result<Vec<String>, SphinxError> {
        self.view("get_invites", json!({ "job_id": job.to_string() })).await
    }

    async fn check(&self, view: &'static str, args: Value) -> Result<(), SphinxError> {
        self.view::<Value>(view, args).await.map(|_| ())
    }

    async fn submission(&self, job: u64, sub: u32) -> Result<Submission, SphinxError> {
        let s: Option<Submission> = self
            .view("get_submission", json!({ "job_id": job.to_string(), "sub_id": sub }))
            .await?;
        s.ok_or_else(|| {
            SphinxError::new(Code::ScoreNoSubmission, format!("job {job} has no submission {sub}"))
                .ctx("job_id", job)
                .ctx("sub_id", sub)
        })
    }
}

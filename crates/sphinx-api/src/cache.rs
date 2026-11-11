//! A cache in front of the chain.
//!
//! Every view is an RPC round trip, and a page load asks for the same
//! things many times (the job list, a job's submissions). An answer younger
//! than `fresh` is served as is. An older one, up to `stale`, is served at
//! once while a single background read refreshes it, so a reader is never
//! more than one refresh behind and never waits on the RPC for a key that
//! was read recently. Failures are never cached, and the `check_*`
//! previews the relay runs before paying gas always read the chain.
//! Submissions of a settled job never change, so they are kept for good
//! once read: listing past results never waits on the RPC again.

use serde_json::Value;
use sphinx_near::near_api::types::transaction::actions::AccessKey;
use sphinx_near::near_api::types::transaction::delegate_action::SignedDelegateAction;
use sphinx_near::views::{AgentStats, AgentView, BadgeView, Config, Job, Submission};
use sphinx_protocol::SphinxError;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::chain::Chain;

/// Entries kept before old ones are dropped. Keys include account ids from
/// requests, so the cache must not grow with them.
const MAX_ENTRIES: usize = 4096;

type Entry = (Instant, Arc<dyn Any + Send + Sync>);

/// Keys whose answers never change, so they never go stale.
const SETTLED: &str = "settled:";

#[derive(Debug, Default)]
struct Store {
    entries: HashMap<String, Entry>,
    /// Keys a background read is refreshing now.
    refreshing: HashSet<String>,
}

/// A [`Chain`] whose reads are reused (see the module docs).
#[derive(Debug)]
pub struct Cached<C> {
    inner: Arc<C>,
    fresh: Duration,
    stale: Duration,
    store: Arc<Mutex<Store>>,
}

enum Hit<T> {
    Fresh(T),
    Stale(T),
    Miss,
}

fn insert(store: &Mutex<Store>, stale: Duration, key: String, value: Arc<dyn Any + Send + Sync>) {
    let mut s = store.lock().unwrap_or_else(PoisonError::into_inner);
    if s.entries.len() >= MAX_ENTRIES {
        s.entries.retain(|k, (at, _)| k.starts_with(SETTLED) || at.elapsed() < stale);
        if s.entries.len() >= MAX_ENTRIES {
            s.entries.clear();
        }
    }
    s.entries.insert(key, (Instant::now(), value));
}

impl<C: Chain> Cached<C> {
    pub fn new(inner: C, fresh: Duration, stale: Duration) -> Self {
        Self { inner: Arc::new(inner), fresh, stale, store: Arc::default() }
    }

    fn lookup<T: Clone + 'static>(&self, key: &str) -> Hit<T> {
        let s = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((at, value)) = s.entries.get(key) else { return Hit::Miss };
        let Some(v) = value.downcast_ref::<T>().cloned() else { return Hit::Miss };
        if key.starts_with(SETTLED) {
            return Hit::Fresh(v);
        }
        match at.elapsed() {
            age if age < self.fresh => Hit::Fresh(v),
            age if age < self.stale => Hit::Stale(v),
            _ => Hit::Miss,
        }
    }

    /// Starts one background read of `key` unless one is already running.
    fn refresh<T, F, Fut>(&self, key: String, read: F)
    where
        T: Send + Sync + 'static,
        F: FnOnce(Arc<C>) -> Fut,
        Fut: Future<Output = Result<T, SphinxError>> + Send + 'static,
    {
        {
            let mut s = self.store.lock().unwrap_or_else(PoisonError::into_inner);
            if !s.refreshing.insert(key.clone()) {
                return;
            }
        }
        let (store, stale, fut) = (self.store.clone(), self.stale, read(self.inner.clone()));
        tokio::spawn(async move {
            let result = fut.await;
            if let Ok(value) = result {
                insert(&store, stale, key.clone(), Arc::new(value));
            }
            store.lock().unwrap_or_else(PoisonError::into_inner).refreshing.remove(&key);
        });
    }

    async fn get<T, F, Fut>(&self, key: String, read: F) -> Result<T, SphinxError>
    where
        T: Clone + Send + Sync + 'static,
        F: FnOnce(Arc<C>) -> Fut,
        Fut: Future<Output = Result<T, SphinxError>> + Send + 'static,
    {
        match self.lookup::<T>(&key) {
            Hit::Fresh(v) => Ok(v),
            Hit::Stale(v) => {
                self.refresh(key, read);
                Ok(v)
            }
            Hit::Miss => {
                let value = read(self.inner.clone()).await?;
                insert(&self.store, self.stale, key, Arc::new(value.clone()));
                Ok(value)
            }
        }
    }
}

impl<C: Chain> Chain for Cached<C> {
    fn contract(&self) -> &str {
        self.inner.contract()
    }

    async fn config(&self) -> Result<Config, SphinxError> {
        self.get("config".into(), |c| async move { c.config().await }).await
    }

    async fn jobs(&self, from: u64, limit: u32) -> Result<Vec<Job>, SphinxError> {
        self.get(format!("jobs:{from}:{limit}"), move |c| async move { c.jobs(from, limit).await })
            .await
    }

    async fn job(&self, id: u64) -> Result<Job, SphinxError> {
        self.get(format!("job:{id}"), move |c| async move { c.job(id).await }).await
    }

    async fn submissions(
        &self,
        job: u64,
        from: u32,
        limit: u32,
    ) -> Result<Vec<Submission>, SphinxError> {
        self.get(format!("subs:{job}:{from}:{limit}"), move |c| async move {
            c.submissions(job, from, limit).await
        })
        .await
    }

    async fn settled_submissions(
        &self,
        job: u64,
        from: u32,
        limit: u32,
    ) -> Result<Vec<Submission>, SphinxError> {
        self.get(format!("{SETTLED}subs:{job}:{from}:{limit}"), move |c| async move {
            c.submissions(job, from, limit).await
        })
        .await
    }

    async fn fresh_submissions(
        &self,
        job: u64,
        from: u32,
        limit: u32,
    ) -> Result<Vec<Submission>, SphinxError> {
        self.inner.submissions(job, from, limit).await
    }

    async fn submission(&self, job: u64, sub: u32) -> Result<Submission, SphinxError> {
        self.get(format!("sub:{job}:{sub}"), move |c| async move { c.submission(job, sub).await })
            .await
    }

    async fn agents(&self, from: u32) -> Result<Vec<AgentView>, SphinxError> {
        self.get(format!("agents:{from}"), move |c| async move { c.agents(from).await }).await
    }

    async fn agent(&self, account: &str) -> Result<Option<AgentView>, SphinxError> {
        let a = account.to_owned();
        self.get(format!("agent:{account}"), move |c| async move { c.agent(&a).await }).await
    }

    async fn agent_stats(&self, account: &str) -> Result<AgentStats, SphinxError> {
        let a = account.to_owned();
        self.get(format!("stats:{account}"), move |c| async move { c.agent_stats(&a).await }).await
    }

    async fn username_owner(&self, username: &str) -> Result<Option<String>, SphinxError> {
        let u = username.to_owned();
        self.get(format!("username:{username}"), move |c| async move { c.username_owner(&u).await })
            .await
    }

    async fn assessment_state(&self, job: u64) -> Result<(bool, bool), SphinxError> {
        self.get(format!("assessment:{job}"), move |c| async move { c.assessment_state(job).await })
            .await
    }

    async fn badges(&self, account: &str) -> Result<Vec<BadgeView>, SphinxError> {
        let a = account.to_owned();
        self.get(format!("badges:{account}"), move |c| async move { c.badges(&a).await }).await
    }

    async fn invites(&self, job: u64) -> Result<Vec<String>, SphinxError> {
        self.get(format!("invites:{job}"), move |c| async move { c.invites(job).await }).await
    }

    async fn check(&self, view: &'static str, args: Value) -> Result<(), SphinxError> {
        self.inner.check(view, args).await
    }

    // Never cached: a revoked key must stop working at once.
    async fn is_access_key(&self, account: &str, public_key: &str) -> Result<bool, SphinxError> {
        self.inner.is_access_key(account, public_key).await
    }

    async fn access_key(
        &self,
        sda: &SignedDelegateAction,
    ) -> Result<(Option<AccessKey>, u64), SphinxError> {
        self.inner.access_key(sda).await
    }
}

#[cfg(test)]
#[allow(clippy::unused_async_trait_impl, reason = "the fake answers at once")]
mod tests {
    use super::*;
    use sphinx_protocol::Code;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Counts reads; `job` fails for id 9.
    #[derive(Default)]
    struct Counting {
        reads: AtomicU32,
        checks: AtomicU32,
    }

    fn job(id: u64) -> Job {
        serde_json::from_value(serde_json::json!({
            "id": id.to_string(), "buyer": "b.testnet", "spec_uri": "repo:s", "runtime": "js-boa/1",
            "reward": "1", "bond": "1", "fee_bps": 0, "max_subs_per_agent": 1, "max_subs_total": 1,
            "deadline_ns": "1", "created_at_ns": "0", "status": "Open", "commit": null,
            "sub_count": 0, "pending_count": 0, "graded_count": 0, "winner": null, "winning_sub_id": null
        }))
        .unwrap()
    }

    impl Chain for Counting {
        fn contract(&self) -> &'static str {
            "c.testnet"
        }
        async fn config(&self) -> Result<Config, SphinxError> {
            Err(SphinxError::new(Code::RelayRpcFailed, "offline"))
        }
        async fn jobs(&self, _: u64, _: u32) -> Result<Vec<Job>, SphinxError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(vec![job(0)])
        }
        async fn job(&self, id: u64) -> Result<Job, SphinxError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if id == 9 {
                return Err(SphinxError::new(Code::JobNotFound, "no"));
            }
            Ok(job(id))
        }
        async fn submissions(
            &self,
            _: u64,
            _: u32,
            _: u32,
        ) -> Result<Vec<Submission>, SphinxError> {
            Ok(Vec::new())
        }
        async fn submission(&self, _: u64, _: u32) -> Result<Submission, SphinxError> {
            Err(SphinxError::new(Code::ScoreNoSubmission, "no"))
        }
        async fn agents(&self, _: u32) -> Result<Vec<AgentView>, SphinxError> {
            Ok(Vec::new())
        }
        async fn agent(&self, _: &str) -> Result<Option<AgentView>, SphinxError> {
            Ok(None)
        }
        async fn agent_stats(&self, _: &str) -> Result<AgentStats, SphinxError> {
            Ok(AgentStats::default())
        }
        async fn invites(&self, _: u64) -> Result<Vec<String>, SphinxError> {
            Ok(Vec::new())
        }
        async fn check(&self, _: &'static str, _: Value) -> Result<(), SphinxError> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn is_access_key(&self, _: &str, _: &str) -> Result<bool, SphinxError> {
            Ok(false)
        }
        async fn access_key(
            &self,
            _: &SignedDelegateAction,
        ) -> Result<(Option<AccessKey>, u64), SphinxError> {
            Ok((None, 0))
        }
    }

    fn reads(c: &Cached<Counting>) -> u32 {
        c.inner.reads.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn fresh_answers_are_reused_but_failures_and_checks_never_are() {
        let c = Cached::new(Counting::default(), Duration::from_secs(60), Duration::from_secs(120));
        c.jobs(0, 50).await.unwrap();
        c.jobs(0, 50).await.unwrap();
        c.job(1).await.unwrap();
        assert_eq!(reads(&c), 2, "the second list came from the cache");
        assert!(c.job(9).await.is_err());
        assert!(c.job(9).await.is_err());
        assert_eq!(reads(&c), 4, "failures are read again");
        c.check("check_submit", Value::Null).await.unwrap();
        c.check("check_submit", Value::Null).await.unwrap();
        assert_eq!(c.inner.checks.load(Ordering::SeqCst), 2, "previews always read the chain");
    }

    #[tokio::test]
    async fn stale_answers_are_served_at_once_and_refreshed_in_the_background() {
        let c =
            Cached::new(Counting::default(), Duration::from_millis(40), Duration::from_millis(400));
        c.jobs(0, 50).await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        c.jobs(0, 50).await.unwrap();
        c.jobs(0, 50).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(reads(&c), 2, "two stale hits started one refresh");
        tokio::time::sleep(Duration::from_millis(500)).await;
        c.jobs(0, 50).await.unwrap();
        assert_eq!(reads(&c), 3, "past the stale limit the caller waits for a read");
    }

    #[test]
    fn the_cache_stays_bounded() {
        let store = Mutex::new(Store::default());
        for i in 0..(MAX_ENTRIES + 10) {
            insert(&store, Duration::from_secs(60), format!("k{i}"), Arc::new(i));
        }
        assert!(store.lock().unwrap().entries.len() <= MAX_ENTRIES);
    }
}

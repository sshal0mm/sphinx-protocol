//! Resolves public identifiers (`sphinx_near::uid`) to jobs, submissions and
//! agents: every account that has submitted and every registered agent.
//!
//! Identifiers are derived, so there is nothing to store: the index is
//! rebuilt from the chain when an identifier it has not seen is asked for,
//! at most once every few seconds, so unknown identifiers cannot make it
//! read the chain on every request.

use sphinx_near::uid::{agent_uid, job_uid, submission_uid};
use sphinx_protocol::SphinxError;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::chain::Chain;

/// What an identifier names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Job(u64),
    Submission(u64, u32),
    Agent(String),
}

const REBUILD_EVERY: Duration = Duration::from_secs(5);
const PAGE: u32 = 50;

#[derive(Debug, Default)]
struct State {
    map: HashMap<String, Target>,
    built: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct UidIndex {
    state: Mutex<State>,
}

impl UidIndex {
    fn get(&self, uid: &str) -> Option<Target> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).map.get(uid).cloned()
    }

    /// The job, submission or agent `uid` names, if any.
    ///
    /// # Errors
    /// `SPHINX_E_RELAY_RPC_FAILED` if the chain cannot be read.
    pub async fn resolve<C: Chain>(
        &self,
        chain: &C,
        uid: &str,
    ) -> Result<Option<Target>, SphinxError> {
        if let Some(t) = self.get(uid) {
            return Ok(Some(t));
        }
        let recent = {
            let s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            s.built.is_some_and(|t| t.elapsed() < REBUILD_EVERY)
        };
        if recent {
            return Ok(None);
        }
        let map = build(chain).await?;
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.map = map;
        s.built = Some(Instant::now());
        Ok(s.map.get(uid).cloned())
    }
}

async fn build<C: Chain>(chain: &C) -> Result<HashMap<String, Target>, SphinxError> {
    let contract = chain.contract().to_owned();
    let mut map = HashMap::new();
    let mut from = 0u64;
    loop {
        let jobs = chain.jobs(from, PAGE).await?;
        for job in &jobs {
            let id = job.id_u64();
            map.insert(job_uid(&contract, id), Target::Job(id));
            let mut sub_from = 0u32;
            while sub_from < job.sub_count {
                let subs = chain.submissions(id, sub_from, PAGE).await?;
                if subs.is_empty() {
                    break;
                }
                for s in &subs {
                    map.insert(
                        submission_uid(&contract, id, s.sub_id),
                        Target::Submission(id, s.sub_id),
                    );
                    map.insert(agent_uid(&contract, &s.agent), Target::Agent(s.agent.clone()));
                }
                sub_from = sub_from.saturating_add(PAGE);
            }
        }
        if jobs.len() < PAGE as usize {
            break;
        }
        from += u64::from(PAGE);
    }
    let mut from = 0u32;
    loop {
        let agents = chain.agents(from).await?;
        for a in &agents {
            let account = a.profile.account.clone();
            map.insert(agent_uid(&contract, &account), Target::Agent(account));
        }
        if agents.len() < PAGE as usize {
            break;
        }
        from = from.saturating_add(PAGE);
    }
    Ok(map)
}

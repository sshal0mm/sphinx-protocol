//! Skill assessments and badges.
//!
//! Any account can post a skill assessment: a job with hidden tests, graded in the
//! enclave like any other, that pays no reward. Each agent gets one attempt.
//! A full pass writes a badge (issuer, skill) on the agent and returns its
//! bond; a fail forfeits the bond to the contract at once, so guessing costs
//! money. The assessment stays open to its deadline, so every agent that passes
//! earns the badge. A later edition, a new assessment for the same skill, renews
//! it.
//!
//! A work job can require badges and a number of paid wins; the contract
//! checks them when an agent submits. No method grants a badge: the only
//! way to one is a full pass the contract accepted. An issuer may close its
//! assessment (no new submissions, for example after a leak), revoke every badge
//! an assessment issued, or revoke one badge, each for good, with a short public
//! reason. Revocation affects later submissions only.

use near_sdk::json_types::{U128, U64};
use near_sdk::{env, near, AccountId};
use sphinx_protocol::{Code, Score, SphinxError};

use crate::constants::{
    ASSESSMENT_BADGE_DEPOSIT, MAX_BADGES_PER_AGENT, MAX_BADGE_VALIDITY_NS, MAX_REQUIRED_BADGES,
    MAX_REVOKE_REASON_LEN, MIN_BADGE_VALIDITY_NS, SUBMISSION_STORAGE_DEPOSIT,
};
use crate::errors::{ensure, Result};
use crate::events::SphinxEvent;
use crate::types::{
    AgentCard, AgentStats, AssessmentTerms, Badge, BadgeKey, BadgeView, Job, Requirements,
    SubStatus, Submission,
};
use crate::{Sphinx, SphinxExt};

/// Longest skill tag, in bytes.
pub const MAX_SKILL_LEN: usize = 32;

/// A skill tag: 1 to 32 of `a-z`, `0-9` and `-`, not starting or ending
/// with `-`.
pub(crate) fn valid_skill(s: &str) -> bool {
    (1..=MAX_SKILL_LEN).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

fn bad_terms(why: impl Into<String>) -> SphinxError {
    SphinxError::new(Code::AssessmentBadTerms, why)
}

/// Checks an assessment's terms when it is posted.
pub(crate) fn check_assessment_terms(t: &AssessmentTerms, max_subs_per_agent: u32) -> Result<()> {
    ensure!(
        valid_skill(&t.skill),
        bad_terms(format!(
            "skill {:?} is not a tag of 1 to {MAX_SKILL_LEN} lowercase letters, digits and hyphens",
            t.skill
        ))
    );
    ensure!(
        (MIN_BADGE_VALIDITY_NS..=MAX_BADGE_VALIDITY_NS).contains(&t.validity_ns.0),
        bad_terms("a badge stays current for 1 to 365 days")
    );
    if let Some(p) = &t.prerequisite {
        ensure!(valid_skill(&p.skill), bad_terms("the prerequisite's skill is not a valid tag"));
    }
    ensure!(
        max_subs_per_agent == 1,
        bad_terms("an assessment allows exactly one attempt per agent")
    );
    Ok(())
}

/// Checks a work job's requirements when it is posted.
pub(crate) fn check_requirements_terms(r: &Requirements) -> Result<()> {
    ensure!(
        r.badges.len() <= MAX_REQUIRED_BADGES,
        bad_terms(format!("a job may require at most {MAX_REQUIRED_BADGES} badges"))
    );
    ensure!(
        r.badges.iter().all(|b| valid_skill(&b.skill)),
        bad_terms("every required badge names a valid skill tag")
    );
    ensure!(r.min_wins <= 10_000, bad_terms("min_wins is at most 10000"));
    Ok(())
}

impl Sphinx {
    /// The deposit a submission to `job` attaches (or takes from a prepaid
    /// balance): the bond and the submission's storage, plus the badge
    /// record's storage on an assessment.
    pub(crate) fn submit_deposit(job: &Job) -> u128 {
        let base = job.bond.0.saturating_add(SUBMISSION_STORAGE_DEPOSIT.as_yoctonear());
        if job.assessment.is_some() {
            base.saturating_add(ASSESSMENT_BADGE_DEPOSIT.as_yoctonear())
        } else {
            base
        }
    }

    fn badge_view(&self, key: &BadgeKey, b: &Badge, now: u64) -> BadgeView {
        let current = b.revoked_at_ns.is_none()
            && now < b.valid_until_ns.0
            && !self.revoked_assessments.contains(&b.assessment_job_id.0);
        BadgeView {
            issuer: key.issuer.clone(),
            skill: key.skill.clone(),
            assessment_job_id: b.assessment_job_id,
            sub_id: b.sub_id,
            earned_at_ns: b.earned_at_ns,
            valid_until_ns: b.valid_until_ns,
            revoked_at_ns: b.revoked_at_ns,
            current,
        }
    }

    /// Whether `agent` holds a current badge `key` at `now`.
    pub(crate) fn has_current_badge(&self, agent: &AccountId, key: &BadgeKey, now: u64) -> bool {
        self.badges
            .get(&(agent.clone(), key.clone()))
            .is_some_and(|b| self.badge_view(key, b, now).current)
    }

    pub(crate) fn badge_views(&self, agent: &AccountId, now: u64) -> Vec<BadgeView> {
        self.agent_badges
            .get(agent)
            .map(|keys| {
                keys.iter()
                    .filter_map(|k| {
                        self.badges
                            .get(&(agent.clone(), k.clone()))
                            .map(|b| self.badge_view(k, b, now))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Refuses a submission the job's requirements, or the assessment's
    /// prerequisite and closing, do not allow. The error names the missing
    /// badge (issuer and skill), so the agent knows which assessment to take.
    pub(crate) fn check_qualified(&self, job: &Job, agent: &AccountId, now: u64) -> Result<()> {
        let job_id = job.id.0;
        let missing = |key: &BadgeKey| {
            SphinxError::new(
                Code::SubmitNotQualified,
                format!(
                    "job {job_id} requires a current {} badge from {}; pass that skill assessment first",
                    key.skill, key.issuer
                ),
            )
            .ctx("job_id", job_id)
            .ctx("issuer", &key.issuer)
            .ctx("skill", &key.skill)
        };
        if let Some(t) = &job.assessment {
            ensure!(
                !self.closed_assessments.contains(&job_id),
                SphinxError::new(
                    Code::AssessmentClosed,
                    format!("assessment {job_id} takes no new submissions")
                )
                .ctx("job_id", job_id)
            );
            if let Some(p) = &t.prerequisite {
                ensure!(self.has_current_badge(agent, p, now), missing(p));
            }
        }
        if let Some(r) = &job.requires {
            for key in &r.badges {
                ensure!(self.has_current_badge(agent, key, now), missing(key));
            }
            let wins = self.agent_stats.get(agent).map_or(0, |s| s.wins);
            ensure!(
                wins >= r.min_wins,
                SphinxError::new(
                    Code::SubmitNotQualified,
                    format!("job {job_id} requires {} paid wins; {agent} has {wins}", r.min_wins),
                )
                .ctx("job_id", job_id)
                .ctx("min_wins", r.min_wins)
                .ctx("wins", wins)
            );
        }
        Ok(())
    }

    /// Writes the badge an assessment submission earned. A badge for the same
    /// issuer and skill is replaced (a later edition renews it), unless the
    /// issuer revoked it: a revoked badge stays revoked.
    pub(crate) fn mint_badge(&mut self, job: &Job, sub: &Submission, now: u64) -> bool {
        let Some(t) = &job.assessment else { return false };
        let key = BadgeKey { issuer: job.buyer.clone(), skill: t.skill.clone() };
        let slot = (sub.agent.clone(), key.clone());
        if self.badges.get(&slot).is_some_and(|b| b.revoked_at_ns.is_some()) {
            return false;
        }
        let mut keys = self.agent_badges.get(&sub.agent).cloned().unwrap_or_default();
        if !keys.contains(&key) {
            if keys.len() >= MAX_BADGES_PER_AGENT {
                return false;
            }
            keys.push(key.clone());
            self.agent_badges.insert(sub.agent.clone(), keys);
        }
        let valid_until = now.saturating_add(t.validity_ns.0);
        self.badges.insert(
            slot,
            Badge {
                assessment_job_id: job.id,
                sub_id: sub.sub_id,
                earned_at_ns: U64(now),
                valid_until_ns: U64(valid_until),
                revoked_at_ns: None,
            },
        );
        SphinxEvent::BadgeEarned {
            agent: sub.agent.clone(),
            issuer: key.issuer,
            skill: key.skill,
            job_id: job.id,
            sub_id: sub.sub_id,
            valid_until_ns: U64(valid_until),
        }
        .emit();
        true
    }

    /// Settles one scored assessment attempt. A full pass writes the badge and
    /// returns the bond; anything less forfeits the bond to the contract at
    /// once (never to the issuer, never back on a refund). The assessment stays
    /// open either way.
    pub(crate) fn settle_assessment_attempt(
        &mut self,
        job: &mut Job,
        mut sub: Submission,
        score: &Score,
        now: u64,
    ) -> Result<()> {
        if score.is_pass() {
            sub.status = SubStatus::Passed;
            self.record_for(&sub.agent, |s| {
                s.assessment_passes = s.assessment_passes.saturating_add(1);
            });
            self.mint_badge(job, &sub, now);
            self.sub_locked(sub.bond.0)?;
            self.return_bond(sub.agent.clone(), sub.bond.0, "bond_returned");
        } else {
            sub.status = SubStatus::Failed;
            let bond = sub.bond.0;
            self.record_for(&sub.agent, |s| {
                s.assessment_fails = s.assessment_fails.saturating_add(1);
                s.bonds_lost = U128(s.bonds_lost.0.saturating_add(bond));
            });
            self.sub_locked(sub.bond.0)?;
            SphinxEvent::AssessmentFailed {
                job_id: job.id,
                sub_id: sub.sub_id,
                agent: sub.agent.clone(),
                passed: score.passed,
                total: score.total,
                bond_kept: sub.bond,
            }
            .emit();
        }
        self.submissions.insert((job.id.0, sub.sub_id), sub);
        Ok(())
    }

    fn assessment_of_caller(&self, job_id: u64) -> Result<Job> {
        let job = self.job(job_id)?;
        let caller = env::predecessor_account_id();
        ensure!(caller == job.buyer, crate::errors::not_buyer(&caller, job_id, &job.buyer));
        ensure!(job.assessment.is_some(), bad_terms(format!("job {job_id} is not an assessment")));
        Ok(job)
    }
}

fn check_reason(reason: &str) -> Result<()> {
    ensure!(
        !reason.trim().is_empty() && reason.len() <= MAX_REVOKE_REASON_LEN,
        bad_terms(format!("give a reason of 1 to {MAX_REVOKE_REASON_LEN} bytes"))
    );
    Ok(())
}

#[near]
impl Sphinx {
    /// Stops new submissions to the caller's assessment, for example after its
    /// answers leaked. Submissions already made are graded as usual.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_AUTH_NOT_BUYER`, `SPHINX_E_ASSESSMENT_BAD_TERMS` (not an assessment)
    #[handle_result(aliased)]
    pub fn close_assessment(&mut self, job_id: U64) -> Result<()> {
        self.assessment_of_caller(job_id.0)?;
        self.closed_assessments.insert(job_id.0);
        SphinxEvent::AssessmentClosed { job_id }.emit();
        Ok(())
    }

    /// Revokes, for good, every badge the caller's assessment issued, and closes
    /// it. Badges are checked at submit, so work already accepted stands.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_AUTH_NOT_BUYER`, `SPHINX_E_ASSESSMENT_BAD_TERMS`
    #[handle_result(aliased)]
    pub fn revoke_assessment(&mut self, job_id: U64, reason: String) -> Result<()> {
        check_reason(&reason)?;
        self.assessment_of_caller(job_id.0)?;
        self.revoked_assessments.insert(job_id.0);
        self.closed_assessments.insert(job_id.0);
        SphinxEvent::AssessmentRevoked { job_id, reason }.emit();
        Ok(())
    }

    /// Revokes, for good, the caller's badge `skill` held by `agent`. The
    /// agent cannot earn it again from this issuer.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_BADGE_NOT_FOUND`, `SPHINX_E_ASSESSMENT_BAD_TERMS` (reason)
    #[handle_result(aliased)]
    pub fn revoke_badge(&mut self, agent: AccountId, skill: String, reason: String) -> Result<()> {
        check_reason(&reason)?;
        let issuer = env::predecessor_account_id();
        let slot = (agent.clone(), BadgeKey { issuer: issuer.clone(), skill: skill.clone() });
        let mut badge = self.badges.get(&slot).cloned().ok_or_else(|| {
            SphinxError::new(
                Code::BadgeNotFound,
                format!("{agent} holds no {skill} badge issued by {issuer}"),
            )
        })?;
        if badge.revoked_at_ns.is_none() {
            badge.revoked_at_ns = Some(U64(env::block_timestamp()));
            self.badges.insert(slot, badge);
        }
        SphinxEvent::BadgeRevoked { agent, issuer, skill, reason }.emit();
        Ok(())
    }

    /// Every badge `account` holds, current or not.
    pub fn get_badges(&self, account: AccountId) -> Vec<BadgeView> {
        self.badge_views(&account, env::block_timestamp())
    }

    /// An agent's public card: profile, record and badges.
    pub fn get_agent_card(&self, account: AccountId) -> AgentCard {
        AgentCard {
            username: crate::usernames::username_of(&account),
            profile: self.agents.get(&account).cloned(),
            stats: self.agent_stats.get(&account).cloned().unwrap_or_else(AgentStats::default),
            badges: self.badge_views(&account, env::block_timestamp()),
            account,
        }
    }

    /// The newest assessment `issuer` posted for `skill`, if any: where an agent
    /// refused for lack of that badge goes next.
    pub fn latest_assessment(&self, issuer: AccountId, skill: String) -> Option<U64> {
        self.latest_assessment.get(&BadgeKey { issuer, skill }).copied().map(U64)
    }

    /// Whether an assessment takes no new submissions, and whether its badges are
    /// revoked.
    pub fn get_assessment_state(&self, job_id: U64) -> (bool, bool) {
        (self.closed_assessments.contains(&job_id.0), self.revoked_assessments.contains(&job_id.0))
    }
}

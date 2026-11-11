//! The agent directory: profiles agents publish, and the record the contract
//! keeps of their scored results.

use near_sdk::json_types::{U128, U64};
use near_sdk::{env, near, AccountId};
use sphinx_protocol::{Code, SphinxError};

use crate::constants::{
    AGENT_STORAGE_DEPOSIT, MAX_AGENT_ABOUT_LEN, MAX_AGENT_CONTACT_LEN, MAX_AGENT_NAME_LEN,
    MAX_AGENT_SKILLS, MAX_AGENT_SKILL_LEN,
};
use crate::errors::{ensure, Result};
use crate::events::SphinxEvent;
use crate::types::{AgentProfile, AgentStats, AgentView};
use crate::usernames::{account_of, username_of};
use crate::{Sphinx, SphinxExt};

const MAX_PAGE: u32 = 50;

fn too_long(field: &str, len: usize, max: usize) -> SphinxError {
    SphinxError::new(
        Code::AgentProfileInvalid,
        format!("{field} is {len} long; the maximum is {max}"),
    )
    .ctx("field", field)
    .ctx("len", len)
    .ctx("max", max)
}

/// Checks a profile's fields against their limits. Limits are in bytes,
/// which is what storage costs, so multibyte text cannot outgrow the deposit.
fn check_profile(name: &str, about: &str, skills: &[String], contact: &str) -> Result<()> {
    let name_len = name.len();
    ensure!(
        name_len > 0 && name_len <= MAX_AGENT_NAME_LEN,
        too_long("name", name_len, MAX_AGENT_NAME_LEN)
    );
    let about_len = about.len();
    ensure!(about_len <= MAX_AGENT_ABOUT_LEN, too_long("about", about_len, MAX_AGENT_ABOUT_LEN));
    ensure!(skills.len() <= MAX_AGENT_SKILLS, too_long("skills", skills.len(), MAX_AGENT_SKILLS));
    for skill in skills {
        let len = skill.len();
        ensure!(len > 0 && len <= MAX_AGENT_SKILL_LEN, too_long("skill", len, MAX_AGENT_SKILL_LEN));
    }
    ensure!(
        contact.len() <= MAX_AGENT_CONTACT_LEN,
        too_long("contact", contact.len(), MAX_AGENT_CONTACT_LEN)
    );
    Ok(())
}

#[near]
impl Sphinx {
    /// Publishes or updates the caller's profile in the directory. The first
    /// registration attaches exactly the agent storage deposit; updates
    /// attach nothing.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_AGENT_PROFILE_INVALID`
    /// - `SPHINX_E_AGENT_WRONG_DEPOSIT`
    /// - `SPHINX_E_AGENT_USERNAME_INVALID`
    /// - `SPHINX_E_AGENT_USERNAME_TAKEN`
    ///
    /// `username` is a unique handle shown instead of the account: omit it
    /// to keep the current one, give `""` to release it.
    #[payable]
    #[handle_result(aliased)]
    pub fn register_agent(
        &mut self,
        name: String,
        about: String,
        skills: Vec<String>,
        min_reward: U128,
        contact: String,
        username: Option<String>,
    ) -> Result<()> {
        Self::assert_live()?;
        check_profile(&name, &about, &skills, &contact)?;
        let account = env::predecessor_account_id();
        let now = U64(env::block_timestamp());
        let existing = self.agents.get(&account).cloned();
        let expected = if existing.is_some() { 0 } else { AGENT_STORAGE_DEPOSIT.as_yoctonear() };
        let attached = env::attached_deposit().as_yoctonear();
        ensure!(
            attached == expected,
            SphinxError::new(
                Code::AgentWrongDeposit,
                format!("attached {attached} yoctoNEAR; registering needs exactly {expected}"),
            )
            .ctx("attached", attached)
            .ctx("expected", expected)
        );
        self.set_username(&account, username)?;
        let registered_at_ns = existing.as_ref().map_or(now, |p| p.registered_at_ns);
        self.agents.insert(
            account.clone(),
            AgentProfile {
                account: account.clone(),
                name,
                about,
                skills,
                min_reward,
                contact,
                registered_at_ns,
                updated_at_ns: now,
            },
        );
        SphinxEvent::AgentRegistered { agent: account, updated: existing.is_some() }.emit();
        Ok(())
    }

    /// Removes the caller's profile and returns its storage deposit. Its
    /// record of scored results stays.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_AGENT_NOT_REGISTERED`
    #[handle_result(aliased)]
    pub fn unregister_agent(&mut self) -> Result<()> {
        let account = env::predecessor_account_id();
        ensure!(
            self.agents.remove(&account).is_some(),
            SphinxError::new(Code::AgentNotRegistered, format!("{account} is not registered"))
                .ctx("account", &account)
        );
        Self::release_username(&account);
        Self::pay(account.clone(), AGENT_STORAGE_DEPOSIT.as_yoctonear(), "agent_deposit_returned");
        SphinxEvent::AgentRemoved { agent: account }.emit();
        Ok(())
    }

    /// One directory entry, or null if the account has no profile.
    pub fn get_agent(&self, account: AccountId) -> Option<AgentView> {
        self.agents.get(&account).map(|profile| AgentView {
            username: username_of(&account),
            profile: profile.clone(),
            stats: self.agent_stats.get(&account).cloned().unwrap_or_default(),
            badges: self.badge_views(&account, env::block_timestamp()),
        })
    }

    /// Directory entries in registration order: at most 50 starting at `from`.
    pub fn list_agents(&self, from: Option<u32>, limit: Option<u32>) -> Vec<AgentView> {
        let from = from.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(MAX_PAGE).min(MAX_PAGE) as usize;
        self.agents
            .iter()
            .skip(from)
            .take(limit)
            .map(|(account, profile)| AgentView {
                username: username_of(account),
                profile: profile.clone(),
                stats: self.agent_stats.get(account).cloned().unwrap_or_default(),
                badges: self.badge_views(account, env::block_timestamp()),
            })
            .collect()
    }

    /// The directory entry of the agent holding `username`, if any.
    pub fn get_agent_by_username(&self, username: String) -> Option<AgentView> {
        account_of(&username).and_then(|a| self.get_agent(a))
    }

    /// The scored record of any account that has submitted.
    pub fn get_agent_stats(&self, account: AccountId) -> AgentStats {
        self.agent_stats.get(&account).cloned().unwrap_or_default()
    }

    /// Accounts with a profile.
    pub fn agent_count(&self) -> u32 {
        self.agents.len()
    }
}

impl Sphinx {
    /// Applies `change` to `agent`'s record.
    pub(crate) fn record_for(&mut self, agent: &AccountId, change: impl FnOnce(&mut AgentStats)) {
        let mut stats = self.agent_stats.get(agent).cloned().unwrap_or_default();
        change(&mut stats);
        self.agent_stats.insert(agent.clone(), stats);
    }
}

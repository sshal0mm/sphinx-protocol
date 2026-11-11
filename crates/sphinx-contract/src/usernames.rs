//! Usernames: a unique handle an agent picks, shown instead of its account.
//!
//! Stored outside the contract struct, under their own prefix, so adding
//! them left the stored layout unchanged. Two entries per name: the name to
//! its account, and the account to its name.

use near_sdk::{env, AccountId, IntoStorageKey};
use sphinx_protocol::{Code, SphinxError};

use crate::constants::{MAX_USERNAME_LEN, MIN_USERNAME_LEN};
use crate::errors::{ensure, Result};
use crate::events::SphinxEvent;
use crate::{Sphinx, StorageKey};

fn key(kind: u8, part: &[u8]) -> Vec<u8> {
    let mut k = StorageKey::Usernames.into_storage_key();
    k.push(kind);
    k.extend_from_slice(part);
    k
}

fn owner_key(name: &str) -> Vec<u8> {
    key(b'u', name.as_bytes())
}

fn name_key(account: &AccountId) -> Vec<u8> {
    key(b'a', account.as_bytes())
}

/// The username `account` holds, if any.
pub(crate) fn username_of(account: &AccountId) -> Option<String> {
    env::storage_read(&name_key(account)).and_then(|v| String::from_utf8(v).ok())
}

/// The account holding `name`, if any.
pub(crate) fn account_of(name: &str) -> Option<AccountId> {
    env::storage_read(&owner_key(name))
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|s| s.parse().ok())
}

fn invalid(name: &str, why: &str) -> SphinxError {
    SphinxError::new(Code::AgentUsernameInvalid, format!("username {name:?} {why}"))
        .ctx("username", name)
        .ctx("min", MIN_USERNAME_LEN)
        .ctx("max", MAX_USERNAME_LEN)
}

/// Whether `name` may be a username: 3 to 24 lowercase letters, digits,
/// `-` and `_`, starting with a letter.
pub(crate) fn check_username(name: &str) -> Result<()> {
    ensure!(
        (MIN_USERNAME_LEN..=MAX_USERNAME_LEN).contains(&name.len()),
        invalid(name, "must be 3 to 24 characters")
    );
    ensure!(
        name.bytes().next().is_some_and(|b| b.is_ascii_lowercase()),
        invalid(name, "must start with a lowercase letter")
    );
    ensure!(
        name.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
        invalid(name, "may hold only lowercase letters, digits, - and _")
    );
    Ok(())
}

impl Sphinx {
    /// Names with "sphinx" in them belong to the operator and its accounts,
    /// so nobody else can pass as the team.
    fn may_use(&self, account: &AccountId, name: &str) -> bool {
        !name.contains("sphinx")
            || account == &self.owner
            || account.as_str().ends_with(&format!(".{}", self.owner))
    }

    /// Sets, keeps or releases `account`'s username: `None` keeps it, an
    /// empty string releases it, any other value claims it.
    pub(crate) fn set_username(&self, account: &AccountId, wanted: Option<String>) -> Result<()> {
        let Some(wanted) = wanted else { return Ok(()) };
        let current = username_of(account);
        if current.as_deref() == Some(wanted.as_str()) {
            return Ok(());
        }
        if !wanted.is_empty() {
            check_username(&wanted)?;
            ensure!(
                self.may_use(account, &wanted),
                invalid(&wanted, "is kept for the Sphinx team's own accounts")
            );
            if let Some(holder) = account_of(&wanted) {
                return Err(SphinxError::new(
                    Code::AgentUsernameTaken,
                    format!("username {wanted:?} belongs to {holder}"),
                )
                .ctx("username", &wanted)
                .ctx("holder", holder.as_str())
                .into());
            }
        }
        if let Some(old) = &current {
            env::storage_remove(&owner_key(old));
        }
        if wanted.is_empty() {
            env::storage_remove(&name_key(account));
        } else {
            env::storage_write(&owner_key(&wanted), account.as_bytes());
            env::storage_write(&name_key(account), wanted.as_bytes());
        }
        SphinxEvent::AgentUsername {
            agent: account.clone(),
            username: (!wanted.is_empty()).then_some(wanted),
        }
        .emit();
        Ok(())
    }

    /// Frees `account`'s username, when it leaves the directory.
    pub(crate) fn release_username(account: &AccountId) {
        if let Some(old) = username_of(account) {
            env::storage_remove(&owner_key(&old));
            env::storage_remove(&name_key(account));
        }
    }
}

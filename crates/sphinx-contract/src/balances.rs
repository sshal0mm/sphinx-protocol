//! Prepaid agent balances. A NEAR function-call access key cannot attach a
//! deposit, so a connector that holds only such a key (limited to this
//! contract, revocable by the owner at any time) could not pay a bond. The
//! agent deposits a balance from its own wallet once; `submit_from_balance`
//! then takes each bond and storage deposit from it, and rewards still go
//! straight to the agent's account. The balance is the agent's: it is
//! withdrawn to the agent's own account only.
//!
//! Balances live under their own storage prefix, outside the contract
//! struct, so adding them does not change the stored state's layout.

use near_sdk::json_types::{U128, U64};
use near_sdk::{env, near, AccountId, IntoStorageKey};
use sphinx_protocol::{Code, SphinxError};

use crate::constants::{BALANCE_STORAGE_DEPOSIT, MIN_BALANCE_DEPOSIT};
use crate::errors::{ensure, Result};
use crate::events::SphinxEvent;
use crate::types::SubmitReceipt;
use crate::{Sphinx, SphinxExt, StorageKey};

fn key(account: &AccountId) -> Vec<u8> {
    let mut k = StorageKey::AgentBalances.into_storage_key();
    k.extend_from_slice(account.as_bytes());
    k
}

/// The spendable balance, or `None` if the account has no balance record.
fn read(account: &AccountId) -> Option<u128> {
    env::storage_read(&key(account))
        .and_then(|v| <[u8; 16]>::try_from(v.as_slice()).ok())
        .map(u128::from_le_bytes)
}

fn write(account: &AccountId, amount: u128) {
    env::storage_write(&key(account), &amount.to_le_bytes());
}

fn changed(account: &AccountId, change: &str, amount: u128, balance: u128) {
    SphinxEvent::AgentBalance {
        account: account.clone(),
        change: change.to_owned(),
        amount: U128(amount),
        balance: U128(balance),
    }
    .emit();
}

#[near]
impl Sphinx {
    /// Adds the attached deposit to the caller's prepaid balance. The first
    /// deposit must be at least 0.05 NEAR; 0.001 NEAR of it pays for the
    /// balance record and comes back when the balance is withdrawn in full.
    /// Returns the spendable balance.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_BALANCE_DEPOSIT_TOO_SMALL`
    #[payable]
    #[handle_result(aliased)]
    pub fn deposit_balance(&mut self) -> Result<U128> {
        let account = env::predecessor_account_id();
        let attached = env::attached_deposit().as_yoctonear();
        let balance = if let Some(b) = read(&account) {
            b.checked_add(attached).ok_or_else(|| crate::errors::invariant("balance overflow"))?
        } else {
            ensure!(
                attached >= MIN_BALANCE_DEPOSIT.as_yoctonear(),
                SphinxError::new(
                    Code::BalanceDepositTooSmall,
                    format!(
                        "a first deposit must be at least {} yoctoNEAR; {attached} was attached",
                        MIN_BALANCE_DEPOSIT.as_yoctonear()
                    ),
                )
                .ctx("min", MIN_BALANCE_DEPOSIT.as_yoctonear())
                .ctx("attached", attached)
            );
            attached - BALANCE_STORAGE_DEPOSIT.as_yoctonear()
        };
        write(&account, balance);
        // All of it is owed back: the balance, and the record's storage
        // deposit on a full withdrawal.
        self.add_locked(attached)?;
        changed(&account, "deposit", attached, balance);
        Ok(U128(balance))
    }

    /// Pays the caller `amount` from its prepaid balance, or everything (and
    /// the record's storage deposit, closing the record) when `amount` is
    /// omitted or equals the balance. Returns what is left.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_BALANCE_TOO_LOW`
    #[handle_result(aliased)]
    pub fn withdraw_balance(&mut self, amount: Option<U128>) -> Result<U128> {
        let account = env::predecessor_account_id();
        let balance = read(&account).unwrap_or(0);
        let wanted = amount.map_or(balance, |a| a.0);
        ensure!(
            wanted > 0 && wanted <= balance,
            SphinxError::new(
                Code::BalanceTooLow,
                format!("{account} has a balance of {balance} yoctoNEAR; asked for {wanted}"),
            )
            .ctx("balance", balance)
            .ctx("asked", wanted)
        );
        let left = balance - wanted;
        let paid = if left == 0 {
            env::storage_remove(&key(&account));
            wanted + BALANCE_STORAGE_DEPOSIT.as_yoctonear()
        } else {
            write(&account, left);
            wanted
        };
        self.sub_locked(paid)?;
        changed(&account, "withdraw", paid, left);
        Self::pay(account, paid, "balance_withdrawn");
        Ok(U128(left))
    }

    /// Submits as the caller, paying the job's bond and the submission
    /// storage deposit from the caller's prepaid balance instead of an
    /// attached deposit, so a function-call key can submit. Otherwise
    /// exactly `submit`, with the same checks and errors.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_BALANCE_TOO_LOW`, and every error of `submit`
    #[handle_result(aliased)]
    pub fn submit_from_balance(
        &mut self,
        job_id: U64,
        code_uri: String,
        code_hash: String,
    ) -> Result<SubmitReceipt> {
        let agent = env::predecessor_account_id();
        let needed = Self::submit_deposit(&self.job(job_id.0)?);
        let balance = read(&agent).unwrap_or(0);
        ensure!(
            balance >= needed,
            SphinxError::new(
                Code::BalanceTooLow,
                format!(
                    "{agent} has a balance of {balance} yoctoNEAR; this submission needs {needed}"
                ),
            )
            .ctx("balance", balance)
            .ctx("needed", needed)
        );
        // The submission first: if any of its checks fails, nothing moves.
        // `submit_as` locks the bond again for the job; the storage deposit
        // pays for the submission record.
        let receipt = self.submit_as(agent.clone(), needed, job_id, code_uri, code_hash)?;
        let left = balance - needed;
        write(&agent, left);
        self.sub_locked(needed)?;
        changed(&agent, "submit", needed, left);
        Ok(receipt)
    }

    /// The account's spendable prepaid balance in yoctoNEAR (0 without one).
    pub fn get_agent_balance(&self, account: AccountId) -> U128 {
        U128(read(&account).unwrap_or(0))
    }
}

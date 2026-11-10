//! Tracked payouts. Every transfer is followed by a callback; if the transfer
//! fails (for example the receiver account was deleted), the amount comes
//! back to the contract and is credited to `unclaimed` for `withdraw`.

use near_sdk::json_types::U128;
use near_sdk::{env, near, AccountId, NearToken, Promise, PromiseError};
use sphinx_protocol::{Code, SphinxError};

use crate::constants::PAYOUT_CALLBACK_GAS;
use crate::errors::ensure;
use crate::events::SphinxEvent;
use crate::{Sphinx, SphinxExt};

#[near]
impl Sphinx {
    /// Pays out the caller's unclaimed balance (payouts that failed earlier).
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_WITHDRAW_NOTHING`
    #[handle_result(aliased)]
    pub fn withdraw(&mut self) -> crate::errors::Result<()> {
        let account = env::predecessor_account_id();
        let amount = self.unclaimed.get(&account).copied().unwrap_or(0);
        ensure!(
            amount > 0,
            SphinxError::new(Code::WithdrawNothing, format!("{account} has no unclaimed balance"))
                .ctx("account", &account)
        );
        self.unclaimed.remove(&account);
        self.total_unclaimed = self.total_unclaimed.saturating_sub(amount);
        SphinxEvent::Withdrawn { account: account.clone(), amount: U128(amount) }.emit();
        Self::pay(account, amount, "withdraw");
        Ok(())
    }

    /// Callback after a payout transfer. On failure, credits `unclaimed`.
    #[private]
    pub fn on_payout(
        &mut self,
        to: AccountId,
        amount: U128,
        reason: String,
        #[callback_result] result: Result<(), PromiseError>,
    ) {
        if result.is_err() {
            let balance = self.unclaimed.get(&to).copied().unwrap_or(0).saturating_add(amount.0);
            self.unclaimed.insert(to.clone(), balance);
            self.total_unclaimed = self.total_unclaimed.saturating_add(amount.0);
            SphinxEvent::PayoutFailed { to, amount, reason }.emit();
        }
    }
}

std::thread_local! {
    /// Bond transfers made so far in this call; every call starts at zero.
    static BOND_TRANSFERS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

impl Sphinx {
    /// Returns a bond by transfer, or, past `MAX_BOND_TRANSFERS_PER_CALL` in
    /// one call, credits it to `unclaimed` for `withdraw`. Returns true when
    /// it was credited; the caller reports it in its event.
    pub(crate) fn return_bond(&mut self, to: AccountId, amount: u128, reason: &str) -> bool {
        let made = BOND_TRANSFERS.with(std::cell::Cell::get);
        if made < crate::constants::MAX_BOND_TRANSFERS_PER_CALL {
            BOND_TRANSFERS.with(|c| c.set(made + 1));
            Self::pay(to, amount, reason);
            return false;
        }
        let balance = self.unclaimed.get(&to).copied().unwrap_or(0).saturating_add(amount);
        self.unclaimed.insert(to, balance);
        self.total_unclaimed = self.total_unclaimed.saturating_add(amount);
        true
    }

    /// Transfers `amount` to `to`, with a callback that records failure.
    pub(crate) fn pay(to: AccountId, amount: u128, reason: &str) {
        if amount == 0 {
            return;
        }
        Promise::new(to.clone())
            .transfer(NearToken::from_yoctonear(amount))
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(PAYOUT_CALLBACK_GAS)
                    .on_payout(to, U128(amount), reason.to_owned()),
            )
            .detach();
    }
}

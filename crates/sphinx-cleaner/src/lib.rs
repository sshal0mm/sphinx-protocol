//! A state cleaner for closing a retired escrow.
//!
//! NEAR refuses to delete an account holding more than 10,000 bytes besides
//! its code. After `retire`, the escrow's owner holds a key to the account:
//! they deploy this contract over the escrow, call `clean` with every
//! stored key (read with the RPC's `view_state`), and then delete the
//! account, which sends its balance to them. `scripts/close-contract.sh`
//! does all three.

use near_sdk::json_types::Base64VecU8;
use near_sdk::{env, near, require};

#[near(contract_state)]
#[derive(Debug, Default)]
pub struct Cleaner {}

#[near]
impl Cleaner {
    /// Removes `keys` from storage. Only the account itself may call it,
    /// so only a holder of the account's key can clear it.
    pub fn clean(&mut self, keys: Vec<Base64VecU8>) -> u32 {
        require!(
            env::predecessor_account_id() == env::current_account_id(),
            "only the account itself can clear its state"
        );
        let mut removed = 0u32;
        for key in keys {
            if env::storage_remove(&key.0) {
                removed = removed.saturating_add(1);
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;
    use near_sdk::test_utils::VMContextBuilder;
    use near_sdk::testing_env;

    fn as_caller(who: &str) {
        let mut b = VMContextBuilder::new();
        b.current_account_id("escrow.near".parse().unwrap())
            .predecessor_account_id(who.parse().unwrap());
        testing_env!(b.build());
    }

    #[test]
    fn the_account_itself_clears_its_keys() {
        as_caller("escrow.near");
        env::storage_write(b"a", b"1");
        env::storage_write(b"b", b"2");
        let n = Cleaner::default().clean(vec![
            b"a".to_vec().into(),
            b"b".to_vec().into(),
            b"c".to_vec().into(),
        ]);
        assert_eq!(n, 2);
        assert!(!env::storage_has_key(b"a") && !env::storage_has_key(b"b"));
    }

    #[test]
    #[should_panic(expected = "only the account itself")]
    fn nobody_else_can() {
        as_caller("mallory.near");
        Cleaner::default().clean(vec![b"a".to_vec().into()]);
    }
}

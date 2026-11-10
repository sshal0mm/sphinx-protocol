//! Prepaid agent balances: a function-call key can submit with the bond taken
//! from a balance the agent deposited, and the books stay exact.

use near_sdk::json_types::U128;

use super::*;
use crate::constants::{BALANCE_STORAGE_DEPOSIT, MIN_BALANCE_DEPOSIT};

fn locked(c: &Sphinx) -> u128 {
    c.get_stats().total_locked.0
}

fn deposit(c: &mut Sphinx, who: &AccountId, amount: u128) -> U128 {
    ctx(who, amount, START_NS + 5);
    c.deposit_balance().unwrap()
}

#[test]
fn a_first_deposit_must_reach_the_minimum_and_keeps_a_storage_deposit() {
    let mut c = contract();
    let agent = acct("agent");
    ctx(&agent, MIN_BALANCE_DEPOSIT.as_yoctonear() - 1, START_NS);
    assert_eq!(code_of(c.deposit_balance()), Code::BalanceDepositTooSmall);
    let before = locked(&c);
    let first = MIN_BALANCE_DEPOSIT.as_yoctonear();
    let b = deposit(&mut c, &agent, first);
    assert_eq!(b.0, first - BALANCE_STORAGE_DEPOSIT.as_yoctonear());
    // A later top-up of any size adds in full.
    let b = deposit(&mut c, &agent, 7);
    assert_eq!(b.0, first - BALANCE_STORAGE_DEPOSIT.as_yoctonear() + 7);
    assert_eq!(c.get_agent_balance(agent).0, b.0);
    assert_eq!(locked(&c), before + first + 7, "everything deposited is owed back");
}

#[test]
fn a_submission_from_a_balance_is_a_normal_submission_paid_by_the_balance() {
    let mut c = contract();
    let job = open_job(&mut c, 3, 10);
    let agent = acct("agent");
    let start = deposit(&mut c, &agent, NEAR).0;
    let locked_before = locked(&c);
    ctx(&agent, 0, START_NS + 10);
    let r = c.submit_from_balance(job, format!("ipfs://{}", code(1)), code(1).to_string()).unwrap();
    let needed = BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear();
    assert_eq!(c.get_agent_balance(agent.clone()).0, start - needed);
    // The bond moved from the balance into the job; the storage deposit now
    // pays for the submission record.
    assert_eq!(locked(&c), locked_before - SUBMISSION_STORAGE_DEPOSIT.as_yoctonear());
    // It is graded and paid like any other submission: the reward and the
    // bond go to the agent's account, not back into the balance.
    let v = score_for(job, &r, code(1), 12, 12);
    grade(&mut c, &v, START_NS + 20);
    let paid: u128 = transfers().iter().filter(|(to, _)| *to == agent).map(|(_, a)| a).sum();
    let fee = REWARD * 500 / 10_000;
    assert_eq!(paid, REWARD - fee + BOND);
    assert_eq!(c.get_agent_balance(agent).0, start - needed);
}

#[test]
fn a_balance_that_does_not_cover_the_bond_is_refused_and_nothing_moves() {
    let mut c = contract();
    let job = open_job(&mut c, 3, 10);
    let agent = acct("agent");
    // 0.05 NEAR leaves 0.049 spendable, under the 0.05 bond plus storage.
    let start = deposit(&mut c, &agent, MIN_BALANCE_DEPOSIT.as_yoctonear()).0;
    let locked_before = locked(&c);
    ctx(&agent, 0, START_NS + 10);
    let r = c.submit_from_balance(job, format!("ipfs://{}", code(1)), code(1).to_string());
    assert_eq!(code_of(r), Code::BalanceTooLow);
    assert_eq!(c.get_agent_balance(agent).0, start);
    assert_eq!(locked(&c), locked_before);
}

#[test]
fn a_failed_submission_check_leaves_the_balance_alone() {
    let mut c = contract();
    let job = open_job(&mut c, 3, 10);
    let agent = acct("agent");
    let start = deposit(&mut c, &agent, NEAR).0;
    ctx(&agent, 0, START_NS + 10);
    let r = c.submit_from_balance(job, "u".repeat(10_000), code(1).to_string());
    assert_eq!(code_of(r), Code::SubmitUriTooLong);
    assert_eq!(c.get_agent_balance(agent).0, start);
}

#[test]
fn withdrawing_pays_only_the_owner_and_a_full_withdrawal_returns_the_storage_deposit() {
    let mut c = contract();
    let agent = acct("agent");
    let before = locked(&c);
    let start = deposit(&mut c, &agent, NEAR).0;
    ctx(&agent, 0, START_NS + 10);
    assert_eq!(code_of(c.withdraw_balance(Some(U128(start + 1)))), Code::BalanceTooLow);
    let left = c.withdraw_balance(Some(U128(1_000))).unwrap();
    assert_eq!(left.0, start - 1_000);
    assert_eq!(transfers(), vec![(agent.clone(), 1_000)]);
    ctx(&agent, 0, START_NS + 11);
    assert_eq!(c.withdraw_balance(None).unwrap().0, 0);
    assert_eq!(
        transfers(),
        vec![(agent.clone(), start - 1_000 + BALANCE_STORAGE_DEPOSIT.as_yoctonear())]
    );
    assert_eq!(c.get_agent_balance(agent.clone()).0, 0);
    assert_eq!(locked(&c), before, "nothing is owed after a full withdrawal");
    ctx(&agent, 0, START_NS + 12);
    assert_eq!(code_of(c.withdraw_balance(None)), Code::BalanceTooLow);
}

#[test]
fn someone_else_cannot_spend_or_withdraw_a_balance() {
    let mut c = contract();
    let job = open_job(&mut c, 3, 10);
    let owner_of = acct("agent");
    deposit(&mut c, &owner_of, NEAR);
    let other = acct("other");
    ctx(&other, 0, START_NS + 10);
    assert_eq!(code_of(c.withdraw_balance(None)), Code::BalanceTooLow);
    ctx(&other, 0, START_NS + 10);
    let r = c.submit_from_balance(job, format!("ipfs://{}", code(1)), code(1).to_string());
    assert_eq!(code_of(r), Code::BalanceTooLow);
}

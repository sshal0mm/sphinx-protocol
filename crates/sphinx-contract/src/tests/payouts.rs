//! Failed transfers are never lost: they become withdrawable.

use super::*;
use near_sdk::PromiseError;

#[test]
fn failed_payout_is_credited_and_withdrawable() {
    let mut c = contract();
    let gone = acct("deleted");
    ctx(&CONTRACT.parse().unwrap(), 0, START_NS);
    c.on_payout(gone.clone(), U128(BOND), "reward".into(), Err(PromiseError::Failed));
    assert_eq!(events(), vec!["payout_failed"]);
    assert_eq!(c.get_unclaimed(gone.clone()).0, BOND);
    assert_eq!(c.get_stats().total_unclaimed.0, BOND);

    ctx(&gone, 0, START_NS + 1);
    c.withdraw().unwrap();
    assert_eq!(transfers(), vec![(gone.clone(), BOND)]);
    assert_eq!(c.get_unclaimed(gone.clone()).0, 0);
    ctx(&gone, 0, START_NS + 2);
    assert_eq!(code_of(c.withdraw()), Code::WithdrawNothing);
}

#[test]
fn successful_payout_changes_nothing() {
    let mut c = contract();
    ctx(&CONTRACT.parse().unwrap(), 0, START_NS);
    c.on_payout(acct("fine"), U128(BOND), "reward".into(), Ok(()));
    assert_eq!(events(), Vec::<String>::new());
    assert_eq!(c.get_stats().total_unclaimed.0, 0);
}

#[test]
fn solvency_holds_through_a_full_job() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    assert_eq!(c.get_stats().total_locked.0, REWARD);
    let r = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    assert_eq!(c.get_stats().total_locked.0, REWARD + BOND);
    grade(&mut c, &score_for(job, &r, code(1), 12, 12), START_NS + 11);
    let s = c.get_stats();
    assert_eq!(s.total_locked.0, 0);
    assert!(s.solvent);
}

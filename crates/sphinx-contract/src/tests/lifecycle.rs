//! Job and submission lifecycles, with exact payouts.

use super::*;
use crate::types::{JobStatus, SubStatus};

#[test]
fn fail_then_pass_pays_exactly() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);

    let first = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    grade(&mut c, &score_for(job, &first, code(1), 7, 12), START_NS + 11);
    assert_eq!(transfers(), vec![], "a failed bond is held by the job, not paid out yet");
    assert_eq!(events(), vec!["disputed"]);
    assert_eq!(c.get_submission(job, first.sub_id).unwrap().status, SubStatus::Failed);
    let j = c.get_job(job).unwrap();
    assert_eq!(j.status, JobStatus::Open);
    assert_eq!(j.held_bonds.0, BOND);

    let second = submit(&mut c, job, &acct("agent-b"), code(2), START_NS + 12);
    grade(&mut c, &score_for(job, &second, code(2), 12, 12), START_NS + 13);
    let fee = REWARD * 500 / 10_000;
    assert_eq!(
        transfers(),
        vec![(acct("agent-b"), REWARD - fee + BOND), (verifier(), fee)],
        "the winner is paid, and the held bond is burned: nobody, the buyer included, collects it"
    );
    assert_eq!(events(), vec!["pass_recorded", "released", "bonds_released"]);
    let j = c.get_job(job).unwrap();
    assert_eq!(j.status, JobStatus::Paid);
    assert_eq!(j.winner, Some(acct("agent-b")));
    assert_eq!(j.held_bonds.0, 0);
    assert_eq!(c.get_agent_stats(acct("agent-a")).bonds_lost.0, BOND);

    let s = c.get_stats();
    assert_eq!((s.results_recorded.0, s.passes.0, s.fails.0), (2, 1, 1));
    assert_eq!(s.fees_earned.0, fee);
    assert_eq!(s.total_locked.0, 0, "nothing is owed after the job settles");
}

#[test]
fn refund_returns_held_bonds_to_their_agents() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let a = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    grade(&mut c, &score_for(job, &a, code(1), 3, 12), START_NS + 11);
    let b = submit(&mut c, job, &acct("agent-b"), code(2), START_NS + 12);
    grade(&mut c, &score_for(job, &b, code(2), 11, 12), START_NS + 13);

    ctx(&acct("anyone"), 0, after_window(DEADLINE_NS));
    c.refund_on_timeout(job).unwrap();
    assert_eq!(transfers(), vec![(buyer(), REWARD)], "the buyer gets the reward back, no bonds");
    assert_eq!(events(), vec!["refunded", "bonds_released"]);
    assert_eq!(c.get_unclaimed(acct("agent-a")).0, BOND, "nobody passed, so the bond comes back");
    assert_eq!(c.get_unclaimed(acct("agent-b")).0, BOND);
    assert_eq!(c.get_agent_stats(acct("agent-a")).bonds_lost.0, 0);
    let s = c.get_stats();
    assert_eq!(s.total_locked.0, 0);
    assert_eq!(s.total_unclaimed.0, 2 * BOND);
    assert!(s.solvent);
}

#[test]
fn refund_waits_a_second_window_while_submissions_are_pending() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    submit(&mut c, job, &acct("agent-a"), code(1), DEADLINE_NS - 1);
    ctx(&buyer(), 0, after_window(DEADLINE_NS));
    assert_eq!(code_of(c.refund_on_timeout(job)), Code::RefundTooEarly);
    ctx(&buyer(), 0, after_window(after_window(DEADLINE_NS)));
    c.refund_on_timeout(job).unwrap();
}

#[test]
fn impossible_suite_refunds_the_buyer() {
    let mut c = contract();
    let job = post(&mut c, 2, 5);
    ctx(&buyer(), 0, START_NS + 1);
    c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    let v = suite_check_for(&c, job, false);
    ctx(&acct("anyone"), 0, START_NS + 2);
    c.record_suite_check(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap();
    assert_eq!(c.get_job(job).unwrap().status, JobStatus::Rejected);
    assert_eq!(transfers(), vec![(buyer(), REWARD)]);
    assert_eq!(events(), vec!["suite_checked"]);
    assert_eq!(c.get_stats().total_locked.0, 0);
}

#[test]
fn cannot_submit_before_validation() {
    let mut c = contract();
    let job = post(&mut c, 2, 5);
    ctx(&buyer(), 0, START_NS + 1);
    c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    ctx(&acct("agent-a"), BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 2);
    assert_eq!(code_of(c.submit(job, "x".into(), code(1).to_string())), Code::JobWrongState);
}

#[test]
fn buyer_can_cancel_unless_a_pass_or_a_score_is_pending() {
    let mut c = contract();
    let job = post(&mut c, 2, 5);
    ctx(&acct("stranger"), 0, START_NS + 1);
    assert_eq!(code_of(c.cancel_job(job)), Code::AuthNotBuyer);
    ctx(&buyer(), 0, START_NS + 1);
    c.cancel_job(job).unwrap();
    assert_eq!(transfers(), vec![(buyer(), REWARD)]);
    assert_eq!(c.get_job(job).unwrap().status, JobStatus::Cancelled);

    // A submission waiting for its score blocks the cancel.
    let job2 = open_job(&mut c, 2, 5);
    let a = submit(&mut c, job2, &acct("agent-a"), code(1), START_NS + 10);
    ctx(&buyer(), 0, START_NS + 11);
    assert_eq!(code_of(c.cancel_job(job2)), Code::JobHasSubmissions);

    // Once it failed, the buyer may close the job early: the reward comes
    // back and the failed bond goes back to its agent.
    grade(&mut c, &score_for(job2, &a, code(1), 3, 12), START_NS + 12);
    transfers();
    events();
    ctx(&buyer(), 0, START_NS + 13);
    c.cancel_job(job2).unwrap();
    assert_eq!(transfers(), vec![(buyer(), REWARD)]);
    assert_eq!(events(), vec!["job_cancelled", "bonds_released"]);
    assert_eq!(c.get_unclaimed(acct("agent-a")).0, BOND);
    assert_eq!(c.get_stats().total_locked.0, 0);

    // A pass blocks it for good.
    let job3 = open_job(&mut c, 2, 5);
    let b = submit(&mut c, job3, &acct("agent-b"), code(2), START_NS + 20);
    grade(&mut c, &score_for(job3, &b, code(2), 12, 12), START_NS + 21);
    ctx(&buyer(), 0, START_NS + 22);
    assert!(c.cancel_job(job3).is_err());
}

#[test]
fn a_buyer_passing_its_own_job_from_a_second_account_gains_nothing_from_bonds() {
    let mut c = contract();
    let job = open_job(&mut c, 5, 5);
    for (n, at, agent) in [(10u8, 10u64, "agent-a"), (11, 12, "agent-c"), (12, 14, "agent-d")] {
        let s = submit(&mut c, job, &acct(agent), code(n), START_NS + at);
        grade(&mut c, &score_for(job, &s, code(n), 1, 12), START_NS + at + 1);
    }
    transfers();
    // The buyer's second account submits the reference and wins.
    let sock = submit(&mut c, job, &acct("buyer-alt"), code(99), START_NS + 30);
    grade(&mut c, &score_for(job, &sock, code(99), 12, 12), START_NS + 31);
    let fee = REWARD * 500 / 10_000;
    let paid = transfers();
    let to_buyer_side: u128 = paid
        .iter()
        .filter(|(who, _)| *who == buyer() || *who == acct("buyer-alt"))
        .map(|(_, amount)| *amount)
        .sum();
    assert_eq!(
        to_buyer_side,
        REWARD - fee + BOND,
        "only its own reward and bond come back, minus the fee"
    );
    assert!(to_buyer_side < REWARD + BOND, "the buyer side ends below what it put in");
}

#[test]
fn refund_waits_for_deadline_plus_grading_window() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    ctx(&acct("anyone"), 0, DEADLINE_NS);
    assert_eq!(code_of(c.refund_on_timeout(job)), Code::RefundTooEarly);
    ctx(&acct("anyone"), 0, after_window(DEADLINE_NS));
    c.refund_on_timeout(job).unwrap();
    assert_eq!(transfers(), vec![(buyer(), REWARD)]);
    assert_eq!(c.get_job(job).unwrap().status, JobStatus::Refunded);
}

#[test]
fn open_job_without_submissions_is_not_refundable_early() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    ctx(&buyer(), 0, START_NS + 100);
    assert_eq!(code_of(c.refund_on_timeout(job)), Code::RefundTooEarly);
}

#[test]
fn an_earlier_pending_submission_delays_a_later_pass_for_one_window() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let a = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    let b = submit(&mut c, job, &acct("agent-b"), code(2), START_NS + 11);
    grade(&mut c, &score_for(job, &b, code(2), 12, 12), START_NS + 12);
    assert_eq!(transfers(), vec![], "the pass waits for submission 0");
    assert_eq!(events(), vec!["pass_recorded"]);
    let j = c.get_job(job).unwrap();
    assert_eq!((j.status, j.first_pass), (JobStatus::Open, Some(b.sub_id)));

    // The job takes no new submissions while a pass is waiting.
    ctx(&acct("agent-c"), BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 13);
    assert_eq!(
        code_of(c.submit(job, "ipfs://c".into(), code(3).to_string())),
        Code::SubmitJobClosing
    );
    ctx(&acct("anyone"), 0, START_NS + 13);
    assert_eq!(code_of(c.settle(job)), Code::SettleBlocked);
    ctx(&buyer(), 0, after_window(after_window(DEADLINE_NS)));
    assert_eq!(code_of(c.refund_on_timeout(job)), Code::RefundHasPass);

    // One window after submission 0, anyone can settle: 0 expires, 1 is paid.
    ctx(&acct("anyone"), 0, after_window(START_NS + 10));
    c.settle(job).unwrap();
    let fee = REWARD * 500 / 10_000;
    assert_eq!(
        transfers(),
        vec![(acct("agent-a"), BOND), (acct("agent-b"), REWARD - fee + BOND), (verifier(), fee)]
    );
    assert_eq!(events(), vec!["submissions_expired", "released"]);
    assert_eq!(c.get_submission(job, a.sub_id).unwrap().status, SubStatus::Expired);
    assert_eq!(c.get_job(job).unwrap().winner, Some(acct("agent-b")));
    assert_eq!(c.get_stats().total_locked.0, 0);
}

#[test]
fn the_earliest_pass_wins_whatever_order_results_arrive_in() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let a = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    let b = submit(&mut c, job, &acct("agent-b"), code(2), START_NS + 11);
    grade(&mut c, &score_for(job, &b, code(2), 12, 12), START_NS + 12);
    grade(&mut c, &score_for(job, &a, code(1), 12, 12), START_NS + 13);
    let fee = REWARD * 500 / 10_000;
    assert_eq!(
        transfers(),
        vec![(acct("agent-a"), REWARD - fee + BOND), (verifier(), fee), (acct("agent-b"), BOND)],
        "submission 0 wins; the later pass gets its bond back"
    );
    let j = c.get_job(job).unwrap();
    assert_eq!((j.status, j.winner), (JobStatus::Paid, Some(acct("agent-a"))));
    assert_eq!(c.get_stats().total_locked.0, 0);
}

#[test]
fn reclaiming_the_blocking_submission_pays_the_waiting_pass() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let a = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    let b = submit(&mut c, job, &acct("agent-b"), code(2), START_NS + 11);
    grade(&mut c, &score_for(job, &b, code(2), 12, 12), START_NS + 12);
    // With a pass waiting, a third party may expire it after one window, not two.
    ctx(&acct("anyone"), 0, after_window(START_NS + 10));
    c.reclaim_bond(job, a.sub_id).unwrap();
    assert_eq!(c.get_job(job).unwrap().status, JobStatus::Paid);
    assert_eq!(c.get_job(job).unwrap().winner, Some(acct("agent-b")));
}

#[test]
fn pending_bond_is_reclaimable_after_another_agent_wins() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let a = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    let b = submit(&mut c, job, &acct("agent-b"), code(2), START_NS + 11);
    // Submission 0 passes with nothing earlier pending, so it is paid at once.
    grade(&mut c, &score_for(job, &a, code(1), 12, 12), START_NS + 12);
    assert_eq!(c.get_job(job).unwrap().status, JobStatus::Paid);

    ctx(&acct("anyone"), 0, START_NS + 13);
    c.reclaim_bond(job, b.sub_id).unwrap();
    assert_eq!(transfers(), vec![(acct("agent-b"), BOND)]);
    assert_eq!(events(), vec!["submission_expired"]);
    assert_eq!(c.get_submission(job, b.sub_id).unwrap().status, SubStatus::Expired);
    assert_eq!(c.get_stats().total_locked.0, 0);
}

#[test]
fn reclaim_respects_grading_window_and_uses_the_attempt() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let agent = acct("agent-a");
    let a = submit(&mut c, job, &agent, code(1), START_NS + 10);

    ctx(&agent, 0, START_NS + 11);
    assert_eq!(code_of(c.reclaim_bond(job, a.sub_id)), Code::ReclaimTooEarly);

    ctx(&agent, 0, after_window(START_NS + 10));
    c.reclaim_bond(job, a.sub_id).unwrap();
    assert_eq!(transfers(), vec![(agent.clone(), BOND)]);
    ctx(&agent, 0, after_window(START_NS + 10));
    assert_eq!(code_of(c.reclaim_bond(job, a.sub_id)), Code::ReclaimNotPending);

    // The expired attempt counts: one of the two is left.
    assert_eq!(c.get_agent_usage(job, agent.clone()).graded, 1);
    let again = submit(&mut c, job, &agent, code(2), after_window(START_NS + 11));
    assert_eq!(again.sub_id, 1);
}

#[test]
fn one_pending_submission_per_agent() {
    let mut c = contract();
    let job = open_job(&mut c, 3, 5);
    let a = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    ctx(&acct("agent-a"), BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 11);
    let err = c.submit(job, "x".into(), code(2).to_string()).unwrap_err().0;
    assert_eq!(err.code(), Code::SubmitPendingExists);
    assert_eq!(err.context()["pending_sub_id"], a.sub_id.to_string());
    assert_eq!(err.context()["reclaim_after_ns"], a.reclaim_after_ns.0.to_string());
}

#[test]
fn jobs_are_isolated() {
    let mut c = contract();
    let j1 = open_job(&mut c, 1, 5);
    let j2 = open_job(&mut c, 1, 5);
    let agent = acct("agent-a");
    let a = submit(&mut c, j1, &agent, code(1), START_NS + 10);
    grade(&mut c, &score_for(j1, &a, code(1), 1, 12), START_NS + 11);
    // Cap of 1 used on job 1 says nothing about job 2, and the same code may be reused there.
    submit(&mut c, j2, &agent, code(1), START_NS + 12);
}

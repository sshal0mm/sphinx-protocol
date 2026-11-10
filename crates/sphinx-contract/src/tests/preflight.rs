//! The `check_*` views must agree with the calls they preview, and change nothing.

use super::*;
use crate::constants::GRADING_WINDOW_NS;

/// The job record and the contract's counters, as JSON.
fn snapshot(c: &Sphinx, job: U64) -> serde_json::Value {
    serde_json::json!({ "job": c.get_job(job), "stats": c.get_stats() })
}

/// Checks `check_submit`, then `submit`, with the same caller, deposit and time,
/// and returns both outcomes as error codes (`None` for success).
fn both(
    c: &mut Sphinx,
    job: U64,
    agent: &AccountId,
    deposit: u128,
    uri: &str,
    hash: &str,
    now: u64,
) -> (Option<Code>, Option<Code>) {
    ctx(agent, 0, now);
    let before = snapshot(c, job);
    let view = c
        .check_submit(job, agent.clone(), U128(deposit), uri.into(), hash.into())
        .err()
        .map(|e| e.0.code());
    assert_eq!(snapshot(c, job), before, "check_submit must not change state");
    ctx(agent, deposit, now);
    let call = c.submit(job, uri.into(), hash.into()).err().map(|e| e.0.code());
    (view, call)
}

/// One `submit` scenario and the error it must produce (`None`: accepted).
struct Case {
    name: &'static str,
    agent: &'static str,
    deposit: u128,
    uri: String,
    hash: String,
    now: u64,
    want: Option<Code>,
}

#[test]
fn check_submit_agrees_with_submit_on_every_outcome() {
    let mut c = contract();
    let job = open_job(&mut c, 1, 3);
    let ok = BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear();
    let t = START_NS + 10;
    let case = |name, agent, deposit, uri: &str, hash: String, now, want| Case {
        name,
        agent,
        deposit,
        uri: uri.to_owned(),
        hash,
        now,
        want,
    };
    let h1 = code(1).to_string();
    let long = "x".repeat(600);
    let cases = [
        case("buyer self", "buyer", ok, "x", h1.clone(), t, Some(Code::SubmitBuyerSelf)),
        case("wrong deposit", "agent-a", BOND, "x", h1.clone(), t, Some(Code::SubmitWrongDeposit)),
        case("uri too long", "agent-a", ok, &long, h1.clone(), t, Some(Code::SubmitUriTooLong)),
        case("bad hash", "agent-a", ok, "x", "nothex".into(), t, Some(Code::SubmitBadHash)),
        case(
            "deadline",
            "agent-a",
            ok,
            "x",
            h1.clone(),
            DEADLINE_NS,
            Some(Code::SubmitDeadlinePassed),
        ),
        case("accepted", "agent-a", ok, "x", h1.clone(), t, None),
        case(
            "pending exists",
            "agent-a",
            ok,
            "x",
            code(2).to_string(),
            t + 1,
            Some(Code::SubmitPendingExists),
        ),
        case("duplicate code", "agent-b", ok, "x", h1, t + 1, Some(Code::SubmitDuplicateCode)),
    ];
    for k in cases {
        let agent = acct(k.agent);
        let (view, call) = both(&mut c, job, &agent, k.deposit, &k.uri, &k.hash, k.now);
        assert_eq!(view, k.want, "check_submit: {}", k.name);
        assert_eq!(call, k.want, "submit: {}", k.name);
    }
}

#[test]
fn check_submit_reports_caps_and_wrong_state() {
    let mut c = contract();
    let ok = BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear();
    let draft = post(&mut c, 1, 2);
    let (view, call) =
        both(&mut c, draft, &acct("agent-a"), ok, "x", &code(1).to_string(), START_NS + 5);
    assert_eq!((view, call), (Some(Code::JobWrongState), Some(Code::JobWrongState)));

    let job = open_job(&mut c, 1, 2);
    for i in 0..2u8 {
        submit(&mut c, job, &acct(&format!("sybil{i}")), code(10 + i), START_NS + 10);
    }
    let (view, call) =
        both(&mut c, job, &acct("late"), ok, "x", &code(20).to_string(), START_NS + 11);
    assert_eq!((view, call), (Some(Code::SubmitJobCap), Some(Code::SubmitJobCap)));

    let missing = U64(99);
    ctx(&acct("agent-a"), 0, START_NS);
    assert_eq!(
        code_of(c.check_submit(
            missing,
            acct("agent-a"),
            U128(ok),
            "x".into(),
            code(1).to_string()
        )),
        Code::JobNotFound
    );
}

#[test]
fn check_reclaim_bond_agrees_with_reclaim_bond() {
    let mut c = contract();
    let job = open_job(&mut c, 1, 5);
    let agent = acct("agent-a");
    let r = submit(&mut c, job, &agent, code(1), START_NS + 10);
    let early = START_NS + 11;
    let late = after_window(START_NS + 10);
    for (now, want) in
        [(early, Some(Code::ReclaimTooEarly)), (late, None), (late, Some(Code::ReclaimNotPending))]
    {
        ctx(&agent, 0, now);
        let view =
            c.check_reclaim_bond(job, r.sub_id, Some(agent.clone())).err().map(|e| e.0.code());
        ctx(&agent, 0, now);
        let call = c.reclaim_bond(job, r.sub_id).err().map(|e| e.0.code());
        assert_eq!((view, call), (want, want));
    }
    ctx(&agent, 0, late);
    assert_eq!(code_of(c.check_reclaim_bond(job, 7, None)), Code::ScoreNoSubmission);
}

#[test]
fn a_third_party_waits_one_more_window_and_an_expired_attempt_still_counts() {
    let mut c = contract();
    let job = open_job(&mut c, 1, 5);
    let agent = acct("agent-a");
    let stranger = acct("stranger");
    let r = submit(&mut c, job, &agent, code(1), START_NS + 10);
    let late = after_window(START_NS + 10);
    ctx(&stranger, 0, late);
    assert_eq!(
        code_of(c.check_reclaim_bond(job, r.sub_id, Some(stranger.clone()))),
        Code::ReclaimTooEarly
    );
    assert_eq!(code_of(c.check_reclaim_bond(job, r.sub_id, None)), Code::ReclaimTooEarly);
    ctx(&stranger, 0, late);
    assert_eq!(code_of(c.reclaim_bond(job, r.sub_id)), Code::ReclaimTooEarly);
    let later = late + GRADING_WINDOW_NS;
    ctx(&stranger, 0, later);
    c.reclaim_bond(job, r.sub_id).unwrap();
    // The job allows one attempt per agent: the expired one used it.
    ctx(&agent, BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), later + 1);
    assert_eq!(
        code_of(c.submit(job, "https://x/v1/code/2".into(), code(2).to_string())),
        Code::SubmitAgentCap
    );
}

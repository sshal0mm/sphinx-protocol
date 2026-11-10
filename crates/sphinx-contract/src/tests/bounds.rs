//! Input bounds and caps. Every rejection carries its catalog code.

use super::*;
use crate::constants::{MAX_SUBS_TOTAL, MIN_BOND, MIN_REWARD};

fn try_post(
    c: &mut Sphinx,
    reward: u128,
    bond: u128,
    caps: (u32, u32),
    deadline: u64,
    runtime: &str,
) -> Code {
    ctx(&buyer(), reward + JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
    code_of(c.post_job(
        "ipfs://spec".into(),
        runtime.into(),
        U128(reward),
        U128(bond),
        caps.0,
        caps.1,
        U64(deadline),
        None,
    ))
}

#[test]
fn post_job_rejects_each_bad_parameter() {
    let mut c = contract();
    let rt = RUNTIME_JS_BOA_V1;
    assert_eq!(
        try_post(&mut c, MIN_REWARD.as_yoctonear() - 1, BOND, (1, 2), DEADLINE_NS, rt),
        Code::JobRewardTooSmall
    );
    assert_eq!(
        try_post(&mut c, REWARD, MIN_BOND.as_yoctonear() - 1, (1, 2), DEADLINE_NS, rt),
        Code::JobBondTooSmall
    );
    assert_eq!(try_post(&mut c, REWARD, BOND, (0, 2), DEADLINE_NS, rt), Code::JobBadCaps);
    assert_eq!(try_post(&mut c, REWARD, BOND, (3, 2), DEADLINE_NS, rt), Code::JobBadCaps);
    assert_eq!(
        try_post(&mut c, REWARD * 100, BOND, (1, MAX_SUBS_TOTAL + 1), DEADLINE_NS, rt),
        Code::JobBadCaps
    );
    assert_eq!(
        try_post(&mut c, REWARD, REWARD / 2, (1, 3), DEADLINE_NS, rt),
        Code::JobBondTooLarge
    );
    assert_eq!(
        try_post(&mut c, REWARD, u128::MAX / 2, (1, 3), DEADLINE_NS, rt),
        Code::JobBondTooLarge
    );
    assert_eq!(try_post(&mut c, REWARD, BOND, (1, 2), START_NS + 1, rt), Code::JobDeadlineTooSoon);
    assert_eq!(
        try_post(&mut c, REWARD, BOND, (1, 2), START_NS + 400 * DAY_NS, rt),
        Code::JobDeadlineTooFar
    );
    assert_eq!(try_post(&mut c, REWARD, BOND, (1, 2), DEADLINE_NS, "py/1"), Code::JobBadRuntime);

    ctx(&buyer(), REWARD, START_NS);
    let wrong_deposit =
        c.post_job("x".into(), rt.into(), U128(REWARD), U128(BOND), 1, 2, U64(DEADLINE_NS), None);
    assert_eq!(code_of(wrong_deposit), Code::JobWrongDeposit);
    ctx(&buyer(), REWARD + JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
    let long_uri = c.post_job(
        "x".repeat(600),
        rt.into(),
        U128(REWARD),
        U128(BOND),
        1,
        2,
        U64(DEADLINE_NS),
        None,
    );
    assert_eq!(code_of(long_uri), Code::JobUriTooLong);
}

#[test]
fn commit_validates_hash_format_and_caller() {
    let mut c = contract();
    let job = post(&mut c, 2, 5);
    ctx(&buyer(), 0, START_NS + 1);
    let err = c.commit_suite(job, "ABC".into(), ref_hash().to_string(), gk()).unwrap_err().0;
    assert_eq!(err.code(), Code::CommitBadHash);
    assert_eq!(err.context()["field"], "suite_hash");
    ctx(&acct("mallory"), 0, START_NS + 1);
    assert_eq!(
        code_of(c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk())),
        Code::AuthNotBuyer
    );
    ctx(&buyer(), 0, START_NS + 1);
    assert_eq!(
        code_of(c.commit_suite(U64(99), suite_hash().to_string(), ref_hash().to_string(), gk())),
        Code::JobNotFound
    );
}

#[test]
fn submit_rejects_each_bad_parameter() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let deposit = BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear();
    ctx(&buyer(), deposit, START_NS + 10);
    assert_eq!(code_of(c.submit(job, "x".into(), code(1).to_string())), Code::SubmitBuyerSelf);
    ctx(&acct("agent-a"), BOND, START_NS + 10);
    assert_eq!(code_of(c.submit(job, "x".into(), code(1).to_string())), Code::SubmitWrongDeposit);
    ctx(&acct("agent-a"), deposit, START_NS + 10);
    assert_eq!(
        code_of(c.submit(job, "x".repeat(600), code(1).to_string())),
        Code::SubmitUriTooLong
    );
    ctx(&acct("agent-a"), deposit, START_NS + 10);
    assert_eq!(code_of(c.submit(job, "x".into(), "nothex".into())), Code::SubmitBadHash);
    ctx(&acct("agent-a"), deposit, DEADLINE_NS);
    assert_eq!(code_of(c.submit(job, "x".into(), code(1).to_string())), Code::SubmitDeadlinePassed);
}

#[test]
fn agent_cap_counts_every_attempt() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 10);
    let agent = acct("agent-a");
    for (i, t) in [(1u8, 10u64), (2, 20)] {
        let r = submit(&mut c, job, &agent, code(i), START_NS + t);
        grade(&mut c, &score_for(job, &r, code(i), 1, 12), START_NS + t + 1);
    }
    ctx(&agent, BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 30);
    let err = c.submit(job, "x".into(), code(3).to_string()).unwrap_err().0;
    assert_eq!(err.code(), Code::SubmitAgentCap);
    assert_eq!((err.context()["used"].as_str(), err.context()["cap"].as_str()), ("2", "2"));
}

#[test]
fn job_cap_stops_sybils() {
    let mut c = contract();
    let job = open_job(&mut c, 1, 3);
    for i in 0..3u8 {
        submit(&mut c, job, &acct(&format!("sybil{i}")), code(i), START_NS + 10);
    }
    ctx(&acct("sybil9"), BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 11);
    assert_eq!(code_of(c.submit(job, "x".into(), code(9).to_string())), Code::SubmitJobCap);
}

#[test]
fn identical_code_is_rejected_within_a_job() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    ctx(&acct("copycat"), BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 11);
    assert_eq!(code_of(c.submit(job, "x".into(), code(1).to_string())), Code::SubmitDuplicateCode);
}

#[test]
fn score_counts_are_validated() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let r = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    for (p, t) in [(13, 12), (0, 0)] {
        let v = score_for(job, &r, code(1), p, t);
        ctx(&acct("relayer"), 0, START_NS + 11);
        assert_eq!(
            code_of(c.record_score(v.clone(), sign(&grader_key(), &v.signing_message()))),
            Code::ScoreBadCounts
        );
    }
}

#[test]
fn fee_is_capped_at_initialisation() {
    ctx(&owner(), 0, START_NS);
    assert_eq!(
        Sphinx::new(owner(), verifier(), 2_001, true).err().map(|e| e.0.code()),
        Some(Code::InitFeeTooHigh)
    );
}

#[test]
fn pagination_is_bounded_and_never_panics() {
    let mut c = contract();
    post(&mut c, 1, 2);
    assert_eq!(c.list_jobs(Some(U64(u64::MAX)), Some(u32::MAX)).len(), 0);
    assert_eq!(c.list_jobs(None, None).len(), 1);
    assert_eq!(c.list_submissions(U64(0), Some(u32::MAX), Some(u32::MAX)).len(), 0);
}

#[test]
fn storage_deposits_cover_maximum_size_records() {
    use crate::constants::MAX_URI_LEN;
    use near_sdk::env;
    let mut c = contract();
    let byte_cost = env::storage_byte_cost().as_yoctonear();

    ctx(
        &acct("a-very-long-buyer-account-name-for-storage-tests"),
        REWARD + JOB_STORAGE_DEPOSIT.as_yoctonear(),
        START_NS,
    );
    let before = env::storage_usage();
    let job = c
        .post_job(
            "u".repeat(MAX_URI_LEN),
            RUNTIME_JS_BOA_V1.into(),
            U128(REWARD),
            U128(BOND),
            1,
            2,
            U64(DEADLINE_NS),
            Some(
                (1..crate::constants::MAX_INVITES)
                    .map(|i| acct(&format!("an-invited-agent-with-a-long-account-name-{i}")))
                    .chain(std::iter::once("a".repeat(64).parse().unwrap()))
                    .collect(),
            ),
        )
        .unwrap();
    ctx(&acct("a-very-long-buyer-account-name-for-storage-tests"), 0, START_NS + 1);
    c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    c.flush();
    let job_bytes = u128::from(env::storage_usage() - before);
    assert!(job_bytes > 1000, "the job was really written ({job_bytes} bytes)");
    assert!(
        job_bytes * byte_cost <= JOB_STORAGE_DEPOSIT.as_yoctonear(),
        "job uses {job_bytes} bytes"
    );

    let v = suite_check_for(&c, job, true);
    ctx(&acct("anyone"), 0, START_NS + 2);
    c.record_suite_check(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap();
    c.flush();
    // A 64-character implicit account, the longest id an agent can have,
    // submitting for the first time (so its record is created too).
    let agent: AccountId = "a".repeat(64).parse().unwrap();
    ctx(&agent, BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 3);
    let before = env::storage_usage();
    c.submit(job, "u".repeat(MAX_URI_LEN), code(1).to_string()).unwrap();
    c.flush();
    let sub_bytes = u128::from(env::storage_usage() - before);
    assert!(sub_bytes > 900, "the submission was really written ({sub_bytes} bytes)");
    assert!(
        sub_bytes * byte_cost <= SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(),
        "submission uses {sub_bytes} bytes"
    );
}

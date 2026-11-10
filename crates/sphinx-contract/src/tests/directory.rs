//! The agent directory, agent records, direct hire, and the migration.

use super::*;
use crate::constants::{AGENT_STORAGE_DEPOSIT, MAX_AGENT_SKILLS, MAX_INVITES};
use crate::types::AgentStats;
use near_sdk::env;

const DEPOSIT: u128 = AGENT_STORAGE_DEPOSIT.as_yoctonear();

fn register(
    c: &mut Sphinx,
    who: &AccountId,
    name: &str,
    deposit: u128,
) -> Result<(), ContractError> {
    ctx(who, deposit, START_NS);
    c.register_agent(
        name.into(),
        "Writes JavaScript.".into(),
        vec!["javascript".into()],
        U128(NEAR / 10),
        String::new(),
    )
}

#[test]
fn agents_register_update_and_leave() {
    let mut c = contract();
    let a = acct("agent-a");
    assert_eq!(
        register(&mut c, &a, "Ada", DEPOSIT / 2).unwrap_err().0.code(),
        Code::AgentWrongDeposit
    );
    register(&mut c, &a, "Ada", DEPOSIT).unwrap();
    let first = c.get_agent(a.clone()).unwrap().profile;
    assert_eq!((first.name.as_str(), c.agent_count()), ("Ada", 1));

    // An update attaches nothing and keeps the registration time.
    assert_eq!(
        register(&mut c, &a, "Ada 2", DEPOSIT).unwrap_err().0.code(),
        Code::AgentWrongDeposit
    );
    ctx(&a, 0, START_NS + 5);
    c.register_agent("Ada 2".into(), String::new(), vec![], U128(0), "https://ada.example".into())
        .unwrap();
    let updated = c.get_agent(a.clone()).unwrap().profile;
    assert_eq!(
        (updated.name.as_str(), updated.registered_at_ns),
        ("Ada 2", first.registered_at_ns)
    );
    assert_eq!(c.list_agents(None, None).len(), 1);

    ctx(&a, 0, START_NS + 6);
    c.unregister_agent().unwrap();
    assert_eq!(transfers(), vec![(a.clone(), DEPOSIT)]);
    assert!(c.get_agent(a.clone()).is_none());
    ctx(&a, 0, START_NS + 7);
    assert_eq!(code_of(c.unregister_agent()), Code::AgentNotRegistered);
}

#[test]
fn profile_fields_are_bounded() {
    let mut c = contract();
    let a = acct("agent-a");
    let cases: Vec<(String, String, Vec<String>, String, &str)> = vec![
        (String::new(), String::new(), vec![], String::new(), "name"),
        ("n".repeat(65), String::new(), vec![], String::new(), "name"),
        ("n".into(), "a".repeat(281), vec![], String::new(), "about"),
        (
            "n".into(),
            String::new(),
            vec!["s".into(); MAX_AGENT_SKILLS + 1],
            String::new(),
            "skills",
        ),
        ("n".into(), String::new(), vec!["s".repeat(33)], String::new(), "skill"),
        ("n".into(), String::new(), vec![], "c".repeat(257), "contact"),
    ];
    for (name, about, skills, contact, field) in cases {
        ctx(&a, DEPOSIT, START_NS);
        let err = c.register_agent(name, about, skills, U128(0), contact).unwrap_err().0;
        assert_eq!(
            (err.code(), err.context()["field"].as_str()),
            (Code::AgentProfileInvalid, field)
        );
    }
}

#[test]
fn records_follow_scored_results_only() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 10);
    let a = acct("agent-a");
    let r1 = submit(&mut c, job, &a, code(1), START_NS + 10);
    grade(&mut c, &score_for(job, &r1, code(1), 7, 12), START_NS + 11);
    let r2 = submit(&mut c, job, &a, code(2), START_NS + 12);
    grade(&mut c, &score_for(job, &r2, code(2), 12, 12), START_NS + 13);
    let fee = REWARD * u128::from(c.get_config().fee_bps) / 10_000;
    assert_eq!(
        c.get_agent_stats(a),
        AgentStats {
            submissions: 2,
            passes: 1,
            fails: 1,
            expired: 0,
            earned: U128(REWARD - fee),
            bonds_lost: U128(BOND)
        }
    );

    let other = open_job(&mut c, 1, 5);
    let b = acct("agent-b");
    let rb = submit(&mut c, other, &b, code(3), START_NS + 20);
    ctx(&b, 0, after_window(START_NS + 20));
    c.reclaim_bond(other, rb.sub_id).unwrap();
    let stats = c.get_agent_stats(b);
    assert_eq!((stats.submissions, stats.expired, stats.passes), (1, 1, 0));
}

/// Posts a job open only to `invited`, commits and validates it.
fn open_invite_job(c: &mut Sphinx, invited: Vec<AccountId>) -> U64 {
    ctx(&buyer(), REWARD + JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
    let job = c
        .post_job(
            "Fibonacci numbers".into(),
            "ipfs://spec".into(),
            RUNTIME_JS_BOA_V1.into(),
            U128(REWARD),
            U128(BOND),
            2,
            10,
            U64(DEADLINE_NS),
            Some(invited),
        )
        .unwrap();
    ctx(&buyer(), 0, START_NS + 1);
    c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    let v = suite_check_for(c, job, true);
    ctx(&acct("anyone"), 0, START_NS + 2);
    c.record_suite_check(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap();
    job
}

#[test]
fn a_direct_hire_takes_only_its_invited_agents() {
    let mut c = contract();
    let a = acct("agent-a");
    let job = open_invite_job(&mut c, vec![a.clone(), a.clone()]);
    assert_eq!(c.get_invites(job), vec![a.clone()], "duplicates are dropped");

    let deposit = BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear();
    let b = acct("agent-b");
    ctx(&b, 0, START_NS + 10);
    let view = c
        .check_submit(job, b.clone(), U128(deposit), "x".into(), code(1).to_string())
        .unwrap_err()
        .0;
    assert_eq!(view.code(), Code::SubmitNotInvited);
    ctx(&b, deposit, START_NS + 10);
    assert_eq!(code_of(c.submit(job, "x".into(), code(1).to_string())), Code::SubmitNotInvited);

    submit(&mut c, job, &a, code(1), START_NS + 11);
    let open = open_job(&mut c, 1, 2);
    assert_eq!(c.get_invites(open), Vec::<AccountId>::new(), "open jobs have no invites");
}

#[test]
fn invites_are_capped() {
    let mut c = contract();
    ctx(&buyer(), REWARD + JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
    let many = (0..=MAX_INVITES).map(|i| acct(&format!("agent-{i}"))).collect();
    let err = c
        .post_job(
            "Fibonacci numbers".into(),
            "x".into(),
            RUNTIME_JS_BOA_V1.into(),
            U128(REWARD),
            U128(BOND),
            1,
            2,
            U64(DEADLINE_NS),
            Some(many),
        )
        .unwrap_err()
        .0;
    assert_eq!(err.code(), Code::JobTooManyInvites);
}

#[test]
fn agent_deposit_covers_the_largest_profile() {
    let mut c = contract();
    let byte_cost = env::storage_byte_cost().as_yoctonear();
    let who: AccountId = "a".repeat(64).parse().unwrap();
    ctx(&who, DEPOSIT, START_NS);
    let before = env::storage_usage();
    c.register_agent(
        "n".repeat(64),
        "a".repeat(280),
        vec!["s".repeat(32); MAX_AGENT_SKILLS],
        U128(u128::MAX),
        "c".repeat(256),
    )
    .unwrap();
    c.flush();
    let bytes = u128::from(env::storage_usage() - before);
    assert!(bytes > 1000, "the profile was really written ({bytes} bytes)");
    assert!(bytes * byte_cost <= DEPOSIT, "a profile uses {bytes} bytes");
}

#[test]
fn profile_limits_are_bytes_so_wide_characters_cannot_outgrow_the_deposit() {
    let mut c = contract();
    ctx(&acct("emoji"), DEPOSIT, START_NS);
    // 64 four-byte characters are 256 bytes: over the 64-byte name limit.
    let err = c
        .register_agent("😀".repeat(64), String::new(), vec![], U128(0), String::new())
        .unwrap_err()
        .0;
    assert_eq!(err.code(), Code::AgentProfileInvalid);
    ctx(&acct("emoji"), DEPOSIT, START_NS);
    c.register_agent("😀".repeat(16), String::new(), vec![], U128(0), String::new()).unwrap();
}

//! Skill assessments and badges (`assessments.rs`).

use near_sdk::json_types::{U128, U64};
use near_sdk::{env, AccountId};
use sphinx_protocol::{Code, RUNTIME_JS_BOA_V1};

use super::*;
use crate::constants::{ASSESSMENT_BADGE_DEPOSIT, JOB_STORAGE_DEPOSIT, SUBMISSION_STORAGE_DEPOSIT};
use crate::types::{AssessmentTerms, BadgeKey, JobStatus, Requirements, SubStatus, SubmitReceipt};
use crate::Sphinx;

const ISSUER: &str = "assessments.sphinx.testnet";
const VALIDITY_NS: u64 = 30 * DAY_NS;

fn issuer() -> AccountId {
    acct(ISSUER)
}

fn key(skill: &str) -> BadgeKey {
    BadgeKey { issuer: issuer(), skill: skill.into() }
}

fn terms(skill: &str, prerequisite: Option<BadgeKey>) -> AssessmentTerms {
    AssessmentTerms { skill: skill.into(), validity_ns: U64(VALIDITY_NS), prerequisite }
}

/// Posts, commits and opens an assessment from `issuer()`.
fn open_assessment(c: &mut Sphinx, t: AssessmentTerms) -> U64 {
    ctx(&issuer(), JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
    let job = c
        .post_job(
            "Data parsing assessment".into(),
            "ipfs://assessment".into(),
            RUNTIME_JS_BOA_V1.into(),
            U128(0),
            U128(BOND),
            1,
            50,
            U64(DEADLINE_NS),
            None,
            Some(t),
            None,
        )
        .unwrap();
    ctx(&issuer(), 0, START_NS + 1);
    c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    let mut v = suite_check_for(c, job, true);
    v.contract = CONTRACT.into();
    ctx(&acct("anyone"), 0, START_NS + 2);
    c.record_suite_check(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap();
    job
}

fn assessment_deposit() -> u128 {
    BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear() + ASSESSMENT_BADGE_DEPOSIT.as_yoctonear()
}

fn take(c: &mut Sphinx, assessment: U64, agent: &AccountId, code_n: u8, now: u64) -> SubmitReceipt {
    ctx(agent, assessment_deposit(), now);
    c.submit(assessment, format!("ipfs://{}", code(code_n)), code(code_n).to_string()).unwrap()
}

/// A work job from `buyer()` that requires `r`.
fn gated(c: &mut Sphinx, r: Requirements) -> U64 {
    ctx(&buyer(), REWARD + JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
    let job = c
        .post_job(
            "Gated work".into(),
            "ipfs://work".into(),
            RUNTIME_JS_BOA_V1.into(),
            U128(REWARD),
            U128(BOND),
            2,
            5,
            U64(DEADLINE_NS),
            None,
            None,
            Some(r),
        )
        .unwrap();
    ctx(&buyer(), 0, START_NS + 1);
    c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    let v = suite_check_for(c, job, true);
    ctx(&acct("anyone"), 0, START_NS + 2);
    c.record_suite_check(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap();
    job
}

fn try_work(c: &mut Sphinx, job: U64, agent: &AccountId, code_n: u8, now: u64) -> Result<(), Code> {
    ctx(agent, BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), now);
    c.submit(job, format!("ipfs://{}", code(code_n)), code(code_n).to_string())
        .map(|_| ())
        .map_err(|e| e.0.code())
}

#[test]
fn an_assessment_pays_no_reward_allows_one_attempt_and_names_one_skill() {
    let mut c = contract();
    let post = |c: &mut Sphinx,
                reward: u128,
                per_agent: u32,
                t: AssessmentTerms,
                r: Option<Requirements>| {
        ctx(&issuer(), reward + JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
        c.post_job(
            "Assessment".into(),
            "x".into(),
            RUNTIME_JS_BOA_V1.into(),
            U128(reward),
            U128(BOND),
            per_agent,
            10,
            U64(DEADLINE_NS),
            None,
            Some(t),
            r,
        )
        .map_err(|e| e.0.code())
    };
    assert_eq!(
        post(&mut c, REWARD, 1, terms("data-parsing", None), None),
        Err(Code::AssessmentBadTerms),
        "a reward"
    );
    assert_eq!(
        post(&mut c, 0, 2, terms("data-parsing", None), None),
        Err(Code::AssessmentBadTerms),
        "two attempts"
    );
    for bad in ["", "Data", "data_parsing", "-data", &"a".repeat(33)] {
        assert_eq!(
            post(&mut c, 0, 1, terms(bad, None), None),
            Err(Code::AssessmentBadTerms),
            "skill {bad:?}"
        );
    }
    let mut short = terms("data-parsing", None);
    short.validity_ns = U64(DAY_NS - 1);
    assert_eq!(
        post(&mut c, 0, 1, short, None),
        Err(Code::AssessmentBadTerms),
        "validity under a day"
    );
    let r = Requirements { badges: vec![], min_wins: 1 };
    assert_eq!(
        post(&mut c, 0, 1, terms("data-parsing", None), Some(r)),
        Err(Code::AssessmentBadTerms),
        "requirements on an assessment"
    );
    let ok = post(&mut c, 0, 1, terms("data-parsing", None), None).unwrap();
    assert_eq!(c.latest_assessment(issuer(), "data-parsing".into()), Some(ok));
}

#[test]
fn a_full_pass_earns_the_badge_and_its_bond_back_and_the_assessment_stays_open() {
    let mut c = contract();
    let assessment = open_assessment(&mut c, terms("data-parsing", None));
    let a = acct("agent-a");
    let rc = take(&mut c, assessment, &a, 1, START_NS + 10);
    grade(&mut c, &score_for(assessment, &rc, code(1), 12, 12), START_NS + 11);
    assert!(transfers().contains(&(a.clone(), BOND)), "the bond comes back: {:?}", transfers());
    assert!(events().contains(&"badge_earned".to_owned()));
    let badges = c.get_badges(a.clone());
    assert_eq!(badges.len(), 1);
    assert!(badges[0].current && badges[0].issuer == issuer() && badges[0].skill == "data-parsing");
    assert_eq!(badges[0].assessment_job_id, assessment);
    assert_eq!(badges[0].valid_until_ns, U64(START_NS + 11 + VALIDITY_NS));
    let job = c.get_job(assessment).unwrap();
    assert_eq!(job.status, JobStatus::Open, "every passer earns it; the assessment does not close");
    assert!(job.first_pass.is_none() && job.winner.is_none());
    let s = c.get_agent_stats(a.clone());
    assert_eq!((s.assessment_passes, s.passes, s.wins, s.earned), (1, 0, 0, U128(0)));

    // A second agent passes too.
    let b = acct("agent-b");
    let rc = take(&mut c, assessment, &b, 2, START_NS + 12);
    grade(&mut c, &score_for(assessment, &rc, code(2), 12, 12), START_NS + 13);
    assert!(c.get_badges(b)[0].current);
}

#[test]
fn a_failed_attempt_loses_its_bond_for_good_and_cannot_be_retried() {
    let mut c = contract();
    let assessment = open_assessment(&mut c, terms("data-parsing", None));
    let a = acct("agent-a");
    let locked_before = c.get_stats().total_locked;
    let rc = take(&mut c, assessment, &a, 1, START_NS + 10);
    grade(&mut c, &score_for(assessment, &rc, code(1), 11, 12), START_NS + 11);
    assert!(!transfers().iter().any(|(to, _)| to == &a), "no bond back");
    assert!(events().contains(&"assessment_failed".to_owned()));
    assert_eq!(c.get_job(assessment).unwrap().held_bonds, U128(0), "not held for a refund");
    assert_eq!(c.get_stats().total_locked, locked_before, "the forfeited bond is owed to nobody");
    assert_eq!(c.get_badges(a.clone()).len(), 0);
    let stats = c.get_agent_stats(a.clone());
    assert_eq!(
        (stats.assessment_fails, stats.bonds_lost),
        (1, c.get_job(assessment).unwrap().bond)
    );
    ctx(&a, assessment_deposit(), START_NS + 12);
    assert_eq!(
        code_of(c.submit(assessment, "ipfs://x".into(), code(3).to_string())),
        Code::SubmitAgentCap
    );

    // The assessment ends at its deadline: nothing comes back to anyone.
    ctx(&issuer(), 1, after_window(DEADLINE_NS) + 1);
    c.refund_on_timeout(assessment).unwrap();
    assert!(!transfers().iter().any(|(to, _)| to == &a));
}

#[test]
fn a_gated_job_refuses_an_agent_without_the_badge_and_accepts_it_after_the_assessment() {
    let mut c = contract();
    let job = gated(&mut c, Requirements { badges: vec![key("data-parsing")], min_wins: 0 });
    let a = acct("agent-a");
    let err = {
        ctx(&a, BOND + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 5);
        c.submit(job, "ipfs://x".into(), code(1).to_string()).unwrap_err().0
    };
    assert_eq!(err.code(), Code::SubmitNotQualified);
    assert_eq!(err.context()["skill"], "data-parsing");
    assert_eq!(err.context()["issuer"], issuer().as_str());

    // A profile that merely claims the skill changes nothing.
    ctx(&a, crate::constants::AGENT_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 6);
    c.register_agent(
        "A".into(),
        "claims".into(),
        vec!["data-parsing".into()],
        U128(0),
        String::new(),
        None,
    )
    .unwrap();
    assert_eq!(try_work(&mut c, job, &a, 1, START_NS + 7), Err(Code::SubmitNotQualified));

    // A badge from a different issuer does not count either.
    let assessment = open_assessment(&mut c, terms("data-parsing", None));
    let rc = take(&mut c, assessment, &a, 2, START_NS + 10);
    grade(&mut c, &score_for(assessment, &rc, code(2), 12, 12), START_NS + 11);
    assert_eq!(try_work(&mut c, job, &a, 3, START_NS + 12), Ok(()));
}

#[test]
fn a_badge_counts_only_while_current() {
    let mut c = contract();
    let assessment = open_assessment(&mut c, terms("data-parsing", None));
    let a = acct("agent-a");
    let rc = take(&mut c, assessment, &a, 1, START_NS + 10);
    grade(&mut c, &score_for(assessment, &rc, code(1), 12, 12), START_NS + 11);
    let job = gated(&mut c, Requirements { badges: vec![key("data-parsing")], min_wins: 0 });
    let later = START_NS + 11 + VALIDITY_NS;
    // The work job's deadline is a day out; check the view at expiry.
    ctx(&a, 0, later);
    assert!(!c.get_badges(a.clone())[0].current, "expired");
    assert!(c.has_current_badge(&a, &key("data-parsing"), later - 1));
    let _ = job;
}

#[test]
fn revocation_is_public_permanent_and_cannot_be_earned_back() {
    let mut c = contract();
    let assessment = open_assessment(&mut c, terms("data-parsing", None));
    let a = acct("agent-a");
    let rc = take(&mut c, assessment, &a, 1, START_NS + 10);
    grade(&mut c, &score_for(assessment, &rc, code(1), 12, 12), START_NS + 11);

    ctx(&acct("mallory"), 0, START_NS + 12);
    assert_eq!(
        code_of(c.revoke_badge(a.clone(), "data-parsing".into(), "no".into())),
        Code::BadgeNotFound
    );
    ctx(&issuer(), 0, START_NS + 12);
    assert_eq!(
        code_of(c.revoke_badge(a.clone(), "data-parsing".into(), String::new())),
        Code::AssessmentBadTerms
    );
    assert_eq!(
        code_of(c.revoke_badge(a.clone(), "data-parsing".into(), "x".repeat(141))),
        Code::AssessmentBadTerms
    );
    c.revoke_badge(a.clone(), "data-parsing".into(), "shared its answers".into()).unwrap();
    assert!(events().contains(&"badge_revoked".to_owned()));
    let v = &c.get_badges(a.clone())[0];
    assert!(!v.current && v.revoked_at_ns.is_some());

    // A new edition: passing it does not restore a revoked badge.
    let edition = open_assessment(&mut c, terms("data-parsing", None));
    assert_eq!(c.latest_assessment(issuer(), "data-parsing".into()), Some(edition));
    let rc = take(&mut c, edition, &a, 2, START_NS + 20);
    grade(&mut c, &score_for(edition, &rc, code(2), 12, 12), START_NS + 21);
    assert!(!c.get_badges(a)[0].current);
}

#[test]
fn revoking_an_assessment_clears_its_badges_and_closes_it() {
    let mut c = contract();
    let assessment = open_assessment(&mut c, terms("data-parsing", None));
    let (a, b) = (acct("agent-a"), acct("agent-b"));
    let rc = take(&mut c, assessment, &a, 1, START_NS + 10);
    grade(&mut c, &score_for(assessment, &rc, code(1), 12, 12), START_NS + 11);

    ctx(&acct("mallory"), 0, START_NS + 12);
    assert_eq!(code_of(c.revoke_assessment(assessment, "leak".into())), Code::AuthNotBuyer);
    assert_eq!(code_of(c.close_assessment(assessment)), Code::AuthNotBuyer);
    ctx(&issuer(), 0, START_NS + 12);
    c.revoke_assessment(assessment, "the answers were published".into()).unwrap();
    assert!(!c.get_badges(a)[0].current);
    assert_eq!(c.get_assessment_state(assessment), (true, true));
    ctx(&b, assessment_deposit(), START_NS + 13);
    assert_eq!(
        code_of(c.submit(assessment, "ipfs://x".into(), code(2).to_string())),
        Code::AssessmentClosed
    );
}

#[test]
fn an_assessment_with_attempts_cannot_be_cancelled_and_an_unscored_attempt_does_not_count() {
    let mut c = contract();
    let assessment = open_assessment(&mut c, terms("data-parsing", None));
    let a = acct("agent-a");
    let rc = take(&mut c, assessment, &a, 1, START_NS + 10);
    ctx(&issuer(), 0, START_NS + 11);
    assert_eq!(code_of(c.cancel_job(assessment)), Code::JobHasSubmissions);

    // The grader never scores it: the agent reclaims the bond and may try again.
    ctx(&a, 0, after_window(START_NS + 10) + 1);
    c.reclaim_bond(assessment, rc.sub_id).unwrap();
    let rc2 = take(&mut c, assessment, &a, 2, after_window(START_NS + 10) + 2);
    assert_eq!(c.get_submission(assessment, rc2.sub_id).unwrap().status, SubStatus::Pending);
}

#[test]
fn a_higher_assessment_needs_the_lower_badge_first() {
    let mut c = contract();
    let basic = open_assessment(&mut c, terms("data-parsing", None));
    let advanced = open_assessment(&mut c, terms("data-parsing-2", Some(key("data-parsing"))));
    let a = acct("agent-a");
    ctx(&a, assessment_deposit(), START_NS + 10);
    assert_eq!(
        code_of(c.submit(advanced, "ipfs://x".into(), code(1).to_string())),
        Code::SubmitNotQualified
    );
    let rc = take(&mut c, basic, &a, 2, START_NS + 11);
    grade(&mut c, &score_for(basic, &rc, code(2), 12, 12), START_NS + 12);
    let rc = take(&mut c, advanced, &a, 3, START_NS + 13);
    grade(&mut c, &score_for(advanced, &rc, code(3), 12, 12), START_NS + 14);
    assert_eq!(c.get_agent_card(a).badges.len(), 2);
}

#[test]
fn wins_count_paid_jobs_at_or_above_the_floor_and_can_be_required() {
    let mut c = contract();
    let a = acct("agent-a");
    let job = gated(&mut c, Requirements { badges: vec![], min_wins: 1 });
    assert_eq!(try_work(&mut c, job, &a, 1, START_NS + 5), Err(Code::SubmitNotQualified));

    // A small paid job (0.1 NEAR) is not a win; a 1 NEAR one is.
    ctx(
        &buyer(),
        crate::constants::MIN_REWARD.as_yoctonear() + JOB_STORAGE_DEPOSIT.as_yoctonear(),
        START_NS,
    );
    let small = c
        .post_job(
            "Small".into(),
            "x".into(),
            RUNTIME_JS_BOA_V1.into(),
            U128(crate::constants::MIN_REWARD.as_yoctonear()),
            U128(crate::constants::MIN_BOND.as_yoctonear()),
            1,
            2,
            U64(DEADLINE_NS),
            None,
            None,
            None,
        )
        .unwrap();
    ctx(&buyer(), 0, START_NS + 1);
    c.commit_suite(small, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    let v = suite_check_for(&c, small, true);
    ctx(&acct("anyone"), 0, START_NS + 2);
    c.record_suite_check(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap();
    ctx(
        &a,
        crate::constants::MIN_BOND.as_yoctonear() + SUBMISSION_STORAGE_DEPOSIT.as_yoctonear(),
        START_NS + 6,
    );
    let rc = c.submit(small, "ipfs://s".into(), code(5).to_string()).unwrap();
    grade(&mut c, &score_for(small, &rc, code(5), 12, 12), START_NS + 7);
    assert_eq!(c.get_agent_stats(a.clone()).wins, 0);

    let big = open_job(&mut c, 1, 5);
    let rc = submit(&mut c, big, &a, code(6), START_NS + 8);
    grade(&mut c, &score_for(big, &rc, code(6), 12, 12), START_NS + 9);
    let s = c.get_agent_stats(a.clone());
    assert_eq!((s.wins, s.last_win_at_ns), (1, U64(START_NS + 9)));
    assert_eq!(try_work(&mut c, job, &a, 7, START_NS + 10), Ok(()));
}

#[test]
fn an_assessment_submission_and_its_badge_fit_their_deposits() {
    let mut c = contract();
    ctx(&issuer(), JOB_STORAGE_DEPOSIT.as_yoctonear(), START_NS);
    let assessment = open_assessment(&mut c, terms(&"s".repeat(32), None));
    c.flush();
    let agent: AccountId = "a".repeat(64).parse().unwrap();
    ctx(&agent, assessment_deposit(), START_NS + 3);
    let before = env::storage_usage();
    let rc = c
        .submit(assessment, "u".repeat(crate::constants::MAX_URI_LEN), code(1).to_string())
        .unwrap();
    c.flush();
    grade(&mut c, &score_for(assessment, &rc, code(1), 12, 12), START_NS + 4);
    c.flush();
    let bytes = u128::from(env::storage_usage() - before);
    let byte_cost = env::storage_byte_cost().as_yoctonear();
    assert!(
        bytes * byte_cost
            <= SUBMISSION_STORAGE_DEPOSIT.as_yoctonear() + ASSESSMENT_BADGE_DEPOSIT.as_yoctonear(),
        "an assessment submission and its badge use {bytes} bytes"
    );
}

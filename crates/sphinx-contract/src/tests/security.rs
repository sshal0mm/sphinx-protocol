//! Signature and replay protections. Each test changes exactly one thing
//! about an otherwise valid result.

use super::*;
use crate::types::SubStatus;

fn setup() -> (Sphinx, U64, SubmitReceipt) {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let r = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    (c, job, r)
}

fn post_as(c: &mut Sphinx, v: &Score, sig: String) -> Code {
    ctx(&acct("relayer"), 0, START_NS + 20);
    code_of(c.record_score(v.clone(), sig))
}

#[test]
fn forged_signature_is_rejected() {
    let (mut c, job, r) = setup();
    let v = score_for(job, &r, code(1), 12, 12);
    assert_eq!(post_as(&mut c, &v, sign(&other_key(), &v.signing_message())), Code::SigInvalid);
}

#[test]
fn tampered_pass_count_is_rejected() {
    let (mut c, job, r) = setup();
    let signed = score_for(job, &r, code(1), 7, 12);
    let claimed = score_for(job, &r, code(1), 12, 12);
    assert_eq!(
        post_as(&mut c, &claimed, sign(&grader_key(), &signed.signing_message())),
        Code::SigInvalid
    );
}

#[test]
fn malformed_signature_is_reported_as_such() {
    let (mut c, job, r) = setup();
    let v = score_for(job, &r, code(1), 12, 12);
    assert_eq!(post_as(&mut c, &v, "zz".into()), Code::SigMalformed);
}

#[test]
fn score_fields_must_match_state() {
    let (mut c, job, r) = setup();
    let base = score_for(job, &r, code(1), 12, 12);
    let cases: Vec<(&str, Score)> = vec![
        ("contract", Score { contract: "other.testnet".into(), ..base.clone() }),
        ("runtime", Score { runtime: "js-boa/2".into(), ..base.clone() }),
        ("suite_hash", Score { suite_hash: code(9), ..base.clone() }),
        ("code_hash", Score { code_hash: code(2), ..base.clone() }),
        ("nonce", Score { nonce: code(3), ..base.clone() }),
    ];
    for (field, v) in cases {
        ctx(&acct("relayer"), 0, START_NS + 20);
        let err =
            c.record_score(v.clone(), sign(&grader_key(), &v.signing_message())).unwrap_err().0;
        assert_eq!(err.code(), Code::ScoreFieldMismatch, "{field}");
        assert_eq!(err.context()["field"], field);
    }
    assert_eq!(c.get_submission(job, r.sub_id).unwrap().status, SubStatus::Pending);
}

#[test]
fn a_score_cannot_be_replayed_on_another_submission_or_job() {
    let mut c = contract();
    let j1 = open_job(&mut c, 2, 5);
    let j2 = open_job(&mut c, 2, 5);
    let r1 = submit(&mut c, j1, &acct("agent-a"), code(1), START_NS + 10);
    let r2 = submit(&mut c, j2, &acct("agent-a"), code(1), START_NS + 11);
    let v1 = score_for(j1, &r1, code(1), 12, 12);
    let sig1 = sign(&grader_key(), &v1.signing_message());
    // Same signature, pointed at job 2: the nonce no longer matches.
    let mut moved = v1.clone();
    moved.job_id = j2.0;
    moved.sub_id = r2.sub_id;
    ctx(&acct("relayer"), 0, START_NS + 20);
    assert_eq!(code_of(c.record_score(moved, sig1.clone())), Code::ScoreFieldMismatch);
    // And it cannot be recorded twice on its own submission.
    grade(&mut c, &v1, START_NS + 21);
    ctx(&acct("relayer"), 0, START_NS + 22);
    assert_eq!(code_of(c.record_score(v1, sig1)), Code::JobWrongState);
}

#[test]
fn nonces_are_unique_per_submission() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    let a = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    let b = submit(&mut c, job, &acct("agent-b"), code(2), START_NS + 10);
    assert_ne!(a.nonce, b.nonce);
    assert_ne!(a.nonce, c.get_job(job).unwrap().commit.unwrap().suite_check_nonce);
}

#[test]
fn grader_key_is_snapshotted_at_commit() {
    let mut c = contract();
    let job = open_job(&mut c, 2, 5);
    // The owner rotates the key after the job was committed.
    ctx(&owner(), 1, START_NS + 5);
    c.set_grader_key(
        hex::encode(other_key().verifying_key().to_bytes()),
        "b1".repeat(32),
        KeyMode::Dev,
        Hash32::digest(b"other.wasm").to_string(),
        "rotated".into(),
    )
    .unwrap();
    let r = submit(&mut c, job, &acct("agent-a"), code(1), START_NS + 10);
    let v = score_for(job, &r, code(1), 12, 12);
    // The new key cannot settle the old job; the snapshotted one still can.
    assert_eq!(post_as(&mut c, &v, sign(&other_key(), &v.signing_message())), Code::SigInvalid);
    grade(&mut c, &v, START_NS + 21);
}

#[test]
fn only_owner_registers_keys() {
    let mut c = contract();
    ctx(&acct("mallory"), 1, START_NS);
    let r =
        c.set_grader_key("00".repeat(32), box_key(), KeyMode::Dev, "00".repeat(32), String::new());
    assert_eq!(code_of(r), Code::AuthNotOwner);
}

#[test]
fn commit_requires_a_registered_key() {
    ctx(&owner(), 0, START_NS);
    let mut c = Sphinx::new(owner(), verifier(), 500, true).unwrap();
    let job = post(&mut c, 2, 5);
    ctx(&buyer(), 0, START_NS + 1);
    assert_eq!(
        code_of(c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk())),
        Code::CommitNoGraderKey
    );
}

#[test]
fn suite_check_signature_cannot_be_used_as_a_score() {
    let (mut c, job, r) = setup();
    let validate = suite_check_for(&c, job, true);
    let sig = sign(&grader_key(), &validate.signing_message());
    let v = score_for(job, &r, code(1), 12, 12);
    assert_eq!(post_as(&mut c, &v, sig), Code::SigInvalid);
}

#[test]
fn commit_refuses_a_grader_key_other_than_the_one_checked() {
    let mut c = contract();
    let job = post(&mut c, 2, 5);
    ctx(&buyer(), 0, START_NS + 1);
    let other = hex::encode(other_key().verifying_key().to_bytes());
    assert_eq!(
        code_of(c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), other)),
        Code::CommitGraderKeyChanged
    );
}

#[test]
fn a_contract_without_dev_keys_refuses_them_and_keeps_a_key_history() {
    ctx(&owner(), 1, START_NS);
    let mut c = Sphinx::new(owner(), verifier(), 500, false).unwrap();
    assert!(!c.get_config().allow_dev_keys);
    let pk = hex::encode(grader_key().verifying_key().to_bytes());
    let wasm = Hash32::digest(b"grader.wasm").to_string();
    assert_eq!(
        code_of(c.set_grader_key(pk.clone(), box_key(), KeyMode::Dev, wasm.clone(), "x".into())),
        Code::AdminDevKeyRefused
    );
    c.set_grader_key(pk.clone(), box_key(), KeyMode::OutlayerTdx, wasm.clone(), "a".into())
        .unwrap();
    let pk2 = hex::encode(other_key().verifying_key().to_bytes());
    c.set_grader_key(pk2, box_key(), KeyMode::OutlayerTdx, wasm, "b".into()).unwrap();
    let history = c.get_grader_history(None, None);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].public_key, pk);
    assert_eq!(history[0].box_public_key, box_key());
}

#[test]
fn a_grader_key_needs_a_well_formed_box_key() {
    let mut c = contract();
    ctx(&owner(), 1, START_NS);
    let pk = hex::encode(other_key().verifying_key().to_bytes());
    let wasm = Hash32::digest(b"grader.wasm").to_string();
    for bad in
        [String::new(), "b0".repeat(31), "B0".repeat(32), format!("ed25519:{}", "b0".repeat(32))]
    {
        assert_eq!(
            code_of(c.set_grader_key(
                pk.clone(),
                bad.clone(),
                KeyMode::Dev,
                wasm.clone(),
                "x".into()
            )),
            Code::AdminBadBoxKey,
            "{bad}"
        );
    }
    // The registered key and every job committed with it carry the box key.
    assert_eq!(c.get_config().grader.unwrap().box_public_key, box_key());
    let job = open_job(&mut c, 2, 5);
    assert_eq!(c.get_job(job).unwrap().commit.unwrap().grader.box_public_key, box_key());
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn registering_a_key_needs_one_yocto() {
    let mut c = contract();
    ctx(&owner(), 0, START_NS);
    let _ =
        c.set_grader_key("00".repeat(32), box_key(), KeyMode::Dev, "00".repeat(32), String::new());
}

#[test]
fn the_owner_is_the_account_named_at_init() {
    ctx(&acct("deployer"), 0, START_NS);
    let c = Sphinx::new(owner(), verifier(), 500, true).unwrap();
    assert_eq!(c.get_config().owner, owner());
}

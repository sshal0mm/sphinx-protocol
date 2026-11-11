//! The enclave proof checked on chain (`tee.rs`), and closing the contract.

use near_sdk::json_types::{Base64VecU8, U64};
use near_sdk::serde_json::json;
use near_sdk::{env, AccountId};
use sphinx_protocol::Code;

use super::*;
use crate::tee::{test_report, EnclaveCheck, Measurements, RunAttestation, VerificationResult};
use crate::types::KeyMode;
use crate::Sphinx;

type Change = Box<dyn Fn(&mut RunAttestation, &mut Vec<u8>)>;

fn relayer() -> AccountId {
    acct("relayer")
}

fn image() -> Measurements {
    Measurements {
        mrtd: "aa".repeat(48),
        rtmr0: "b0".repeat(48),
        rtmr1: "b1".repeat(48),
        rtmr2: "b2".repeat(48),
        rtmr3: "b3".repeat(48),
    }
}

/// A contract that checks every grading run's enclave proof, with one
/// approved worker image.
fn checked() -> Sphinx {
    ctx(&owner(), 1, START_NS);
    let mut c = Sphinx::new(
        owner(),
        verifier(),
        500,
        true,
        Some(EnclaveCheck {
            quote_verifier: acct("dcap"),
            measurements_from: acct("registry"),
            grader_caller: relayer(),
        }),
        Some(U64(START_NS + 30 * DAY_NS)),
    )
    .unwrap();
    c.set_grader_key(
        gk(),
        box_key(),
        KeyMode::Dev,
        Hash32::digest(b"grader.wasm").to_string(),
        "test".into(),
    )
    .unwrap();
    ctx(&acct(CONTRACT), 0, START_NS);
    let i = image();
    let n = c.on_measurements(Ok(vec![serde_json::from_value(json!({
        "mrtd": i.mrtd, "rtmr0": i.rtmr0, "rtmr1": i.rtmr1, "rtmr2": i.rtmr2, "rtmr3": i.rtmr3,
        "extra": 1
    }))
    .unwrap()]));
    assert_eq!(n, 1);
    c
}

/// A genuine-shaped attestation and output for a run that signed `message`.
fn run(job: U64, message: &str) -> (RunAttestation, Vec<u8>) {
    let output = json!({ "ok": true, "op": "grade", "message": message, "signature": "x" })
        .to_string()
        .into_bytes();
    let a = RunAttestation {
        task_type: "execute".into(),
        task_id: 1_345_960,
        repo_url: None,
        commit_hash: None,
        build_target: Some("wasm32-wasip2".into()),
        wasm_hash: Some(Hash32::digest(b"grader.wasm").to_string()),
        input_hash: Some("5c".repeat(32)),
        output_hash: hex::encode(env::sha256(&output)),
        block_height: Some(223_958_038),
        caller_account_id: Some(relayer().to_string()),
        project_id: None,
        secrets_ref: Some(format!("{}/sphinx-job-{}", buyer(), job.0)),
        timestamp: 1_794_337_784,
        attached_usd: Some("0".into()),
        executed_wasm_sha256: Some(Hash32::digest(b"grader.wasm").to_string()),
    };
    (a, output)
}

fn confirm(
    c: &mut Sphinx,
    job: U64,
    sub: Option<u32>,
    a: RunAttestation,
    output: Vec<u8>,
) -> Result<(), Code> {
    ctx(&acct("anyone"), 0, START_NS + 2);
    c.confirm_run(job, sub, a, Base64VecU8(output), Base64VecU8(vec![1]), Base64VecU8(vec![2]))
        .map(|_| ())
        .map_err(|e| e.0.code())
}

/// The verifier's answer for a quote that commits to `a`.
fn quote_for(a: &RunAttestation, img: &Measurements) -> VerificationResult {
    let mut rd = [0u8; 64];
    rd[..32].copy_from_slice(&a.task_hash());
    test_report(rd, img)
}

fn verified(
    c: &mut Sphinx,
    job: U64,
    sub: Option<u32>,
    a: &RunAttestation,
    message: &str,
    img: &Measurements,
) -> bool {
    ctx(&acct(CONTRACT), 0, START_NS + 3);
    c.on_run_verified(job, sub, hex::encode(a.task_hash()), message.into(), Ok(quote_for(a, img)))
}

fn committed(c: &mut Sphinx) -> U64 {
    let job = post(c, 2, 5);
    ctx(&buyer(), 0, START_NS + 1);
    c.commit_suite(job, suite_hash().to_string(), ref_hash().to_string(), gk()).unwrap();
    job
}

#[test]
fn a_signed_result_counts_only_once_its_enclave_run_is_confirmed() {
    let mut c = checked();
    let job = committed(&mut c);
    let check = suite_check_for(&c, job, true);
    let msg = check.signing_message();
    let sig = sign(&grader_key(), &msg);

    ctx(&relayer(), 0, START_NS + 2);
    assert_eq!(
        code_of(c.record_suite_check(check.clone(), sig.clone())),
        Code::EnclaveRunNotConfirmed
    );

    let (a, out) = run(job, &msg);
    confirm(&mut c, job, None, a.clone(), out).unwrap();
    assert!(!c.is_run_confirmed(job, None), "nothing is confirmed before the quote is verified");
    assert!(verified(&mut c, job, None, &a, &msg, &image()));
    assert!(c.is_run_confirmed(job, None));

    ctx(&relayer(), 0, START_NS + 4);
    c.record_suite_check(check, sig).unwrap();
    assert_eq!(c.get_job(job).unwrap().status, crate::types::JobStatus::Open);
    assert!(!c.is_run_confirmed(job, None), "a confirmation is used once");
}

#[test]
fn an_attestation_of_another_run_is_refused_before_any_call() {
    let mut c = checked();
    let job = committed(&mut c);
    let msg = suite_check_for(&c, job, true).signing_message();
    let cases: Vec<(&str, Change)> = vec![
        ("another build", Box::new(|a, _| a.wasm_hash = Some("00".repeat(32)))),
        (
            "a reported build that differs",
            Box::new(|a, _| a.executed_wasm_sha256 = Some("00".repeat(32))),
        ),
        ("another caller", Box::new(|a, _| a.caller_account_id = Some("mallory.testnet".into()))),
        (
            "another job's tests",
            Box::new(|a, _| a.secrets_ref = Some(format!("{}/sphinx-job-9", buyer()))),
        ),
        (
            "another buyer's tests",
            Box::new(|a, _| a.secrets_ref = Some("mallory.testnet/sphinx-job-0".into())),
        ),
        ("no tests at all", Box::new(|a, _| a.secrets_ref = None)),
        ("a repository build", Box::new(|a, _| a.repo_url = Some("https://github.com/x/y".into()))),
        ("not an execution", Box::new(|a, _| a.task_type = "compile".into())),
        ("an output that is not the run's", Box::new(|_, o| o.push(b' '))),
    ];
    for (name, change) in cases {
        let (mut a, mut out) = run(job, &msg);
        change(&mut a, &mut out);
        assert_eq!(confirm(&mut c, job, None, a, out), Err(Code::EnclaveProofRefused), "{name}");
    }
    let (a, _) = run(job, &msg);
    let unsigned = json!({ "ok": false, "error": {} }).to_string().into_bytes();
    let mut a2 = a;
    a2.output_hash = hex::encode(env::sha256(&unsigned));
    assert_eq!(
        confirm(&mut c, job, None, a2, unsigned),
        Err(Code::EnclaveProofRefused),
        "an output with no signed result"
    );
}

#[test]
fn a_quote_is_accepted_only_if_it_commits_to_the_run_on_an_approved_image() {
    let mut c = checked();
    let job = committed(&mut c);
    let msg = suite_check_for(&c, job, true).signing_message();
    let (a, _) = run(job, &msg);

    let mut other_image = image();
    other_image.rtmr3 = "cc".repeat(48);
    assert!(
        !verified(&mut c, job, None, &a, &msg, &other_image),
        "an image OutLayer has not approved"
    );

    let mut other = a.clone();
    other.timestamp += 1;
    ctx(&acct(CONTRACT), 0, START_NS + 3);
    assert!(
        !c.on_run_verified(
            job,
            None,
            hex::encode(a.task_hash()),
            msg.clone(),
            Ok(quote_for(&other, &image()))
        ),
        "a quote over another run's fields"
    );
    let rejected =
        VerificationResult::Rejected(crate::tee::VerifierError::DcapVerification("bad".into()));
    assert!(
        !c.on_run_verified(job, None, hex::encode(a.task_hash()), msg.clone(), Ok(rejected)),
        "Intel's signature refused"
    );
    assert!(!c.is_run_confirmed(job, None));
}

#[test]
fn a_confirmed_run_counts_only_for_the_result_it_signed() {
    let mut c = checked();
    let job = committed(&mut c);
    let check = suite_check_for(&c, job, true);
    let (a, _) = run(job, "sphinx:v1:suite-check:some other result");
    assert!(verified(&mut c, job, None, &a, "sphinx:v1:suite-check:some other result", &image()));
    ctx(&relayer(), 0, START_NS + 4);
    assert_eq!(
        code_of(c.record_suite_check(check.clone(), sign(&grader_key(), &check.signing_message()))),
        Code::EnclaveRunNotConfirmed
    );
}

#[test]
fn a_contract_without_an_enclave_check_refuses_confirmations_and_needs_none() {
    let mut c = contract();
    let job = committed(&mut c);
    let msg = suite_check_for(&c, job, true).signing_message();
    let (a, out) = run(job, &msg);
    assert_eq!(confirm(&mut c, job, None, a, out), Err(Code::EnclaveProofRefused));
    open_job(&mut c, 2, 5);
}

#[test]
fn the_owner_closes_the_contract_only_after_the_date_and_only_when_it_owes_nothing() {
    let mut c = checked();
    let job = post(&mut c, 2, 5);

    ctx(&owner(), 1, START_NS + 31 * DAY_NS);
    assert_eq!(
        code_of(c.retire(key()).map(|_| ())),
        Code::RetireNotEmpty,
        "a reward is still locked"
    );

    ctx(&buyer(), 1, START_NS + 2);
    c.cancel_job(job).unwrap();
    ctx(&owner(), 1, START_NS + 2);
    assert_eq!(code_of(c.retire(key()).map(|_| ())), Code::RetireTooEarly);
    ctx(&acct("mallory"), 1, START_NS + 31 * DAY_NS);
    assert_eq!(code_of(c.retire(key()).map(|_| ())), Code::AuthNotOwner);
    // A registered agent's deposit is owed back too.
    let agent = acct("agent-z");
    ctx(&agent, crate::constants::AGENT_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 3);
    c.register_agent("Z".into(), String::new(), vec![], U128(0), String::new(), Some("zed".into()))
        .unwrap();

    ctx(&owner(), 1, START_NS + 31 * DAY_NS);
    assert!(!c.is_retired());
    let _closing = c.retire(key()).unwrap();
    assert!(events().iter().any(|e| e == "retired"), "{:?}", events());
    assert!(c.is_retired());
    assert!(
        transfers()
            .contains(&(agent.clone(), crate::constants::AGENT_STORAGE_DEPOSIT.as_yoctonear())),
        "the agent's deposit is returned: {:?}",
        transfers()
    );
    assert!(
        c.get_agent(agent.clone()).is_none() && c.get_agent_by_username("zed".into()).is_none()
    );

    // Nothing new comes in once retired.
    ctx(&buyer(), NEAR, START_NS + 32 * DAY_NS);
    assert_eq!(code_of(c.deposit_balance().map(|_| ())), Code::ContractRetired);
    ctx(&agent, crate::constants::AGENT_STORAGE_DEPOSIT.as_yoctonear(), START_NS + 32 * DAY_NS);
    assert_eq!(
        code_of(c.register_agent("Z".into(), String::new(), vec![], U128(0), String::new(), None)),
        Code::ContractRetired
    );

    let mut never = contract();
    ctx(&owner(), 1, u64::MAX - 1);
    assert_eq!(
        code_of(never.retire(key()).map(|_| ())),
        Code::RetireTooEarly,
        "no retire date set: never"
    );
}

fn key() -> near_sdk::PublicKey {
    "ed25519:DcA2MzgpJbrUATQLLceocVckhhAqrkingax4oJ9kZ847".parse().unwrap()
}

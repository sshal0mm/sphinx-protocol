//! Encrypted submissions, the `identity` op, and the enclave rules (no plaintext
//! inputs, a suite stored for this job, a run the chain asked for), exercised
//! natively through `handle_in` with a stand-in signer, secret and chain.
#![allow(clippy::unwrap_used, reason = "test helpers")]

mod common;
use base64::Engine as _;
use common::*;
use serde_json::{json, Value};
use sphinx_box::{BoxSecret, SUBMISSION_INFO};
use sphinx_grader::chain::ChainView;
use sphinx_grader::signer::{GraderSigner, SignerInfo};
use sphinx_grader::{handle_in, Context};
use sphinx_protocol::SphinxError;
use std::collections::HashMap;

const CONTRACT: &str = "sphinx.example.testnet";

/// A signer whose box key is derived from a fixed seed.
struct TestSigner(u8);

impl GraderSigner for TestSigner {
    fn info(&self) -> Result<SignerInfo, SphinxError> {
        Ok(SignerInfo { kind: "test", public_key: "11".repeat(32) })
    }
    fn sign(&self, _message: &str) -> Result<String, SphinxError> {
        Ok("00".repeat(64))
    }
    fn box_secret(&self) -> Result<BoxSecret, SphinxError> {
        Ok(BoxSecret::derive(&[self.0; 32]))
    }
}

fn encrypt_to(signer: &TestSigner, code: &str) -> String {
    let pk = signer.box_secret().unwrap().public_key_hex();
    sphinx_box::encrypt(&pk, code.as_bytes(), SUBMISSION_INFO).unwrap()
}

/// Views answered from fixed JSON.
struct Chain {
    job: Value,
    sub: Value,
}

impl ChainView for Chain {
    fn view(&self, contract: &str, method: &str, _args: &Value) -> Result<Value, String> {
        assert_eq!(contract, CONTRACT, "views go to the request's contract");
        Ok(match method {
            "get_job" => self.job.clone(),
            "get_submission" => self.sub.clone(),
            _ => Value::Null,
        })
    }
}

fn chain_for(suite: &str, code: &str) -> Chain {
    Chain {
        job: json!({"id": "7", "status": "Open", "commit": {"suite_hash": h(suite)}}),
        sub: json!({"job_id": "7", "sub_id": 3, "status": "Pending",
                    "code_hash": h(code), "nonce": nonce()}),
    }
}

fn secret(suite: &str) -> HashMap<&'static str, String> {
    HashMap::from([
        ("SUITE", suite.to_owned()),
        ("CONTRACT", CONTRACT.to_owned()),
        ("JOB_ID", "7".to_owned()),
    ])
}

/// A grade request with the code encrypted and the suite left to the secret.
fn encrypted_request(suite: &str, code: &str, encrypted: &str) -> Value {
    let mut req: Value = serde_json::from_str(&grade_request(suite, code)).unwrap();
    let o = req.as_object_mut().unwrap();
    o.remove("submission");
    o.remove("suite");
    o.insert("submission_box".into(), json!(encrypted));
    req
}

fn run_in(
    req: &Value,
    signer: Option<&dyn GraderSigner>,
    enclave: bool,
    chain: Option<&dyn ChainView>,
    hidden: &HashMap<&'static str, String>,
) -> Value {
    let lookup = |k: &str| hidden.get(k).cloned();
    let ctx = Context { enclave, chain, hidden: &lookup };
    serde_json::to_value(handle_in(&req.to_string(), signer, &ctx)).unwrap()
}

fn code_of(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("ok")
}

#[test]
fn an_encrypted_submission_grades_like_the_plaintext() {
    let (suite, code) = (fixture("suite.json"), fixture("agent-b.js"));
    let signer = TestSigner(1);
    let req = encrypted_request(&suite, &code, &encrypt_to(&signer, &code));
    let out = run_in(&req, Some(&signer), false, None, &secret(&suite));
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["result"]["passed"], 12);
    assert_eq!(out["result"]["code_hash"], h(&code));
    assert!(!out.to_string().contains("function fib"), "the code is never echoed");
}

#[test]
fn a_box_for_another_key_or_tampered_does_not_open() {
    let (suite, code) = (fixture("suite.json"), fixture("agent-b.js"));
    let signer = TestSigner(1);
    let other = encrypt_to(&TestSigner(2), &code);
    let out = run_in(
        &encrypted_request(&suite, &code, &other),
        Some(&signer),
        false,
        None,
        &secret(&suite),
    );
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_BOX_UNREADABLE");

    let b64 = base64::engine::general_purpose::STANDARD;
    let mut raw = b64.decode(encrypt_to(&signer, &code)).unwrap();
    let last = raw.len() - 1;
    raw[last] ^= 0x80;
    let tampered = b64.encode(raw);
    let out = run_in(
        &encrypted_request(&suite, &code, &tampered),
        Some(&signer),
        false,
        None,
        &secret(&suite),
    );
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_BOX_UNREADABLE");
    let out = run_in(
        &encrypted_request(&suite, &code, "not a box"),
        Some(&signer),
        false,
        None,
        &secret(&suite),
    );
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_BOX_UNREADABLE");
}

#[test]
fn decrypted_code_must_match_the_committed_hash() {
    let suite = fixture("suite.json");
    let signer = TestSigner(1);
    // The box holds agent-a's code, the request claims agent-b's hash.
    let encrypted = encrypt_to(&signer, &fixture("agent-a.js"));
    let req = encrypted_request(&suite, &fixture("agent-b.js"), &encrypted);
    let out = run_in(&req, Some(&signer), false, None, &secret(&suite));
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_CODE_HASH_MISMATCH");
}

#[test]
fn exactly_one_of_submission_and_submission_box() {
    let (suite, code) = (fixture("suite.json"), fixture("agent-b.js"));
    let signer = TestSigner(1);
    let mut both = encrypted_request(&suite, &code, &encrypt_to(&signer, &code));
    both["submission"] = json!(code);
    assert_eq!(
        code_of(&run_in(&both, Some(&signer), false, None, &secret(&suite))),
        "SPHINX_E_GRADER_BAD_REQUEST"
    );
    let mut neither = both.clone();
    neither.as_object_mut().unwrap().remove("submission");
    neither.as_object_mut().unwrap().remove("submission_box");
    assert_eq!(
        code_of(&run_in(&neither, Some(&signer), false, None, &secret(&suite))),
        "SPHINX_E_GRADER_BAD_REQUEST"
    );
    // No signer, no box key.
    let encrypted = encrypted_request(&suite, &code, &encrypt_to(&signer, &code));
    assert_eq!(
        code_of(&run_in(&encrypted, None, false, None, &secret(&suite))),
        "SPHINX_E_GRADER_SIGNER_UNAVAILABLE"
    );
}

#[test]
fn the_enclave_grades_a_encrypted_pending_submission() {
    let (suite, code) = (fixture("suite.json"), fixture("agent-a.js"));
    let signer = TestSigner(1);
    let req = encrypted_request(&suite, &code, &encrypt_to(&signer, &code));
    let chain = chain_for(&suite, &code);
    let out = run_in(&req, Some(&signer), true, Some(&chain), &secret(&suite));
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["result"]["passed"], 5);
    assert_eq!(out["result"]["total"], 12);
}

#[test]
fn the_enclave_refuses_plaintext_inputs() {
    let (suite, code) = (fixture("suite.json"), fixture("agent-b.js"));
    let signer = TestSigner(1);
    let chain = chain_for(&suite, &code);
    let encrypted = encrypt_to(&signer, &code);

    let mut with_suite = encrypted_request(&suite, &code, &encrypted);
    with_suite["suite"] = json!(suite);
    let out = run_in(&with_suite, Some(&signer), true, Some(&chain), &secret(&suite));
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_INLINE_INPUT_REFUSED");
    assert_eq!(out["error"]["ctx"]["field"], "suite");

    let plain: Value = serde_json::from_str(&grade_request(&suite, &code)).unwrap();
    let mut plain_code = plain.clone();
    plain_code.as_object_mut().unwrap().remove("suite");
    let out = run_in(&plain_code, Some(&signer), true, Some(&chain), &secret(&suite));
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_INLINE_INPUT_REFUSED");
    assert_eq!(out["error"]["ctx"]["field"], "submission");

    let reference = fixture("reference.js");
    let validate: Value = serde_json::from_str(&validate_request(&suite, &reference)).unwrap();
    let mut v = validate;
    v.as_object_mut().unwrap().remove("suite");
    let out = run_in(&v, Some(&signer), true, Some(&chain), &secret(&suite));
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_INLINE_INPUT_REFUSED");
    assert_eq!(out["error"]["ctx"]["field"], "reference");
}

#[test]
fn refuse_inline_names_the_first_field_present() {
    assert!(sphinx_grader::grade::refuse_inline(&[("suite", false), ("submission", false)]).is_ok());
    let e =
        sphinx_grader::grade::refuse_inline(&[("suite", false), ("submission", true)]).unwrap_err();
    assert_eq!(e.code().as_str(), "SPHINX_E_GRADER_INLINE_INPUT_REFUSED");
    assert_eq!(e.to_report().ctx["field"], "submission");
}

#[test]
fn the_enclave_grades_only_what_the_chain_asked_for() {
    let (suite, code) = (fixture("suite.json"), fixture("agent-b.js"));
    let signer = TestSigner(1);
    let req = encrypted_request(&suite, &code, &encrypt_to(&signer, &code));

    // No chain access: refused.
    let out = run_in(&req, Some(&signer), true, None, &secret(&suite));
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_NOT_ON_CHAIN");

    // Code that was never submitted: the chain holds another hash.
    let chain = chain_for(&suite, "function fib(n) { return 0 }");
    let out = run_in(&req, Some(&signer), true, Some(&chain), &secret(&suite));
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_NOT_ON_CHAIN");
    assert_eq!(out["error"]["ctx"]["field"], "submission.code_hash");

    // Already scored.
    let mut chain = chain_for(&suite, &code);
    chain.sub["status"] = json!("Failed");
    let out = run_in(&req, Some(&signer), true, Some(&chain), &secret(&suite));
    assert_eq!(out["error"]["ctx"]["field"], "submission.status");
    assert_eq!(out["error"]["bond_effect"], "held");
}

#[test]
fn the_enclave_uses_a_suite_only_for_the_job_it_was_stored_for() {
    let (suite, code) = (fixture("suite.json"), fixture("agent-b.js"));
    let signer = TestSigner(1);
    let req = encrypted_request(&suite, &code, &encrypt_to(&signer, &code));
    let chain = chain_for(&suite, &code);
    for (var, value) in
        [("CONTRACT", Some("look-alike.testnet")), ("JOB_ID", Some("8")), ("JOB_ID", None)]
    {
        let mut s = secret(&suite);
        match value {
            Some(v) => s.insert(var, v.to_owned()),
            None => s.remove(var),
        };
        let out = run_in(&req, Some(&signer), true, Some(&chain), &s);
        assert_eq!(code_of(&out), "SPHINX_E_GRADER_SUITE_NOT_FOR_JOB", "{var}");
        assert_eq!(out["error"]["ctx"]["field"], var);
    }
    let out = run_in(&req_for_missing(), Some(&TestSigner(1)), true, None, &HashMap::new());
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_HIDDEN_INPUT_MISSING", "no secret at all");
}

#[test]
fn the_enclave_validates_only_a_committed_suite() {
    let suite = fixture("suite.json");
    let reference = fixture("reference.js");
    let signer = TestSigner(1);
    let mut req: Value = serde_json::from_str(&validate_request(&suite, &reference)).unwrap();
    req.as_object_mut().unwrap().remove("suite");
    req.as_object_mut().unwrap().remove("reference");
    let mut hidden = secret(&suite);
    hidden.insert("REFERENCE", reference.clone());
    let committed = |status: &str| Chain {
        job: json!({"id": "7", "status": status, "commit": {"suite_hash": h(&suite),
                     "ref_solution_hash": h(&reference), "suite_check_nonce": nonce()}}),
        sub: Value::Null,
    };
    let out = run_in(&req, Some(&signer), true, Some(&committed("Committed")), &hidden);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["result"]["valid"], true);
    let out = run_in(&req, Some(&signer), true, Some(&committed("Open")), &hidden);
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_NOT_ON_CHAIN");
}

#[test]
fn identity_reports_both_public_keys() {
    let signer = TestSigner(1);
    let req = json!({"op": "identity", "schema": "sphinx.grader/1"});
    let out = run_in(&req, Some(&signer), true, None, &HashMap::new());
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["op"], "identity");
    assert_eq!(out["signing_public_key"], "11".repeat(32));
    assert_eq!(out["box_public_key"], signer.box_secret().unwrap().public_key_hex());
    assert_eq!(out["kind"], "test");
    let out = run_in(&req, None, false, None, &HashMap::new());
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_SIGNER_UNAVAILABLE");
    let out = run_in(
        &json!({"op": "identity", "schema": "x"}),
        Some(&signer),
        false,
        None,
        &HashMap::new(),
    );
    assert_eq!(code_of(&out), "SPHINX_E_GRADER_BAD_REQUEST");
}

#[cfg(feature = "dev-signer")]
#[test]
fn the_dev_box_key_is_derived_from_the_dev_seed() {
    use sphinx_grader::signer::dev::DevSigner;
    let a = DevSigner::from_seed_hex(&"2a".repeat(32)).unwrap();
    let b = DevSigner::from_seed_hex(&"2b".repeat(32)).unwrap();
    let pk = a.box_secret().unwrap().public_key_hex();
    assert_eq!(pk, a.box_secret().unwrap().public_key_hex());
    assert_ne!(pk, b.box_secret().unwrap().public_key_hex());
    assert_ne!(pk, a.info().unwrap().public_key);
}

fn req_for_missing() -> Value {
    let (suite, code) = (fixture("suite.json"), fixture("agent-b.js"));
    encrypted_request(&suite, &code, &encrypt_to(&TestSigner(1), &code))
}

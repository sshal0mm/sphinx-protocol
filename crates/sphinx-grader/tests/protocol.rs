//! End-to-end behaviour of `handle`: signed results, intake checks and errors.

mod common;
use common::*;
use serde_json::json;

#[test]
fn honest_submission_passes_every_test() {
    let out = run(&grade_request(&fixture("suite.json"), &fixture("agent-b.js")));
    assert_eq!(out["ok"], true);
    assert_eq!(out["result"]["passed"], 12);
    assert_eq!(out["result"]["total"], 12);
    assert_eq!(out["result"]["runtime"], "js-boa/1");
    assert_eq!(out["result"]["code_hash"], h(&fixture("agent-b.js")));
    assert_eq!(out["signature"], serde_json::Value::Null, "no signer configured");
    assert_eq!(out["message"].as_str().unwrap().split(':').count(), 12);
}

#[test]
fn overfitted_submission_is_caught() {
    let out = run(&grade_request(&fixture("suite.json"), &fixture("agent-a.js")));
    assert_eq!(out["result"]["passed"], 5);
    assert_eq!(out["result"]["total"], 12);
}

#[test]
fn reference_validates_and_an_impossible_suite_does_not() {
    let suite = fixture("suite.json");
    let ok = run(&validate_request(&suite, &fixture("reference.js")));
    assert_eq!(ok["result"]["valid"], true);
    let wrong_ref = "function fib(n){ return n }";
    let bad = run(&validate_request(&suite, wrong_ref));
    assert_eq!(bad["result"]["valid"], false);
    assert_eq!(bad["result"]["ref_solution_hash"], h(wrong_ref));
}

#[test]
fn preflight_problems_score_zero_with_a_reason() {
    let suite = fixture("suite.json");
    for (code, expected) in [
        ("function fib(n) {", "SPHINX_E_CODE_SYNTAX"),
        ("function other() { return 1 }", "SPHINX_E_CODE_ENTRY_MISSING"),
        ("var fib = 3;", "SPHINX_E_CODE_ENTRY_NOT_FUNCTION"),
        ("throw new Error('boom'); function fib(){}", "SPHINX_E_CODE_SYNTAX"),
    ] {
        let out = run(&grade_request(&suite, code));
        assert_eq!(out["ok"], true, "a broken submission still gets a signed 0/N result");
        assert_eq!(out["result"]["passed"], 0);
        assert_eq!(out["preflight"]["code"], expected, "{code}");
    }
}

#[test]
fn intake_rejects_mismatched_hashes() {
    let suite = fixture("suite.json");
    let code = fixture("agent-b.js");
    let mut req: serde_json::Value = serde_json::from_str(&grade_request(&suite, &code)).unwrap();
    req["expected_suite_hash"] = json!(h("other"));
    let out = run(&req.to_string());
    assert_eq!(out["error"]["code"], "SPHINX_E_GRADER_SUITE_HASH_MISMATCH");
    assert_eq!(out["job_id"], "7", "error responses keep the request identity");

    let mut req: serde_json::Value = serde_json::from_str(&grade_request(&suite, &code)).unwrap();
    req["expected_code_hash"] = json!(h("other"));
    assert_eq!(run(&req.to_string())["error"]["code"], "SPHINX_E_GRADER_CODE_HASH_MISMATCH");
}

#[test]
fn malformed_requests_get_coded_errors() {
    assert_eq!(run("not json")["error"]["code"], "SPHINX_E_GRADER_BAD_REQUEST");
    assert_eq!(run("{\"op\":\"teleport\"}")["error"]["code"], "SPHINX_E_GRADER_BAD_REQUEST");
    let suite = fixture("suite.json");
    let mut req: serde_json::Value = serde_json::from_str(&grade_request(&suite, "x")).unwrap();
    req["contract"] = json!("Not A Valid:Account");
    assert_eq!(run(&req.to_string())["error"]["code"], "SPHINX_E_GRADER_BAD_CONTRACT_ID");
    let mut req: serde_json::Value = serde_json::from_str(&grade_request(&suite, "x")).unwrap();
    req["schema"] = json!("sphinx.grader/9");
    assert_eq!(run(&req.to_string())["error"]["code"], "SPHINX_E_GRADER_BAD_REQUEST");
    let mut req: serde_json::Value = serde_json::from_str(&grade_request(&suite, "x")).unwrap();
    req["surprise"] = json!(1);
    assert_eq!(run(&req.to_string())["error"]["code"], "SPHINX_E_GRADER_BAD_REQUEST");
}

#[test]
fn bad_suites_are_rejected_without_quoting_them() {
    let suite = r#"{"version":1,"runtime":"js-boa/1","entry":"solve","tests":[{"args":["CANARY-SECRET"],"expected":9007199254740993}]}"#;
    let out = run(&grade_request(suite, "function solve(){}"));
    assert_eq!(out["error"]["code"], "SPHINX_E_GRADER_SUITE_UNREPRESENTABLE");
    assert!(!out.to_string().contains("CANARY-SECRET"));
}

#[test]
fn responses_never_reveal_which_tests_failed() {
    let out = run(&grade_request(&fixture("suite.json"), &fixture("agent-a.js")));
    let text = out.to_string();
    for secret in ["8944394323791464", "2111485077978050", "102334155"] {
        assert!(!text.contains(secret), "response leaked an expected value");
    }
}

#[test]
fn grading_is_deterministic() {
    let req = grade_request(&fixture("suite.json"), &fixture("agent-a.js"));
    assert_eq!(run(&req), run(&req));
}

#[cfg(feature = "dev-signer")]
#[test]
fn dev_signer_signs_the_exact_message() {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use sphinx_grader::signer::{dev::DevSigner, GraderSigner};
    let signer = DevSigner::from_seed_hex(&"2a".repeat(32)).unwrap();
    let out = serde_json::to_value(sphinx_grader::handle(
        &grade_request(&fixture("suite.json"), &fixture("agent-b.js")),
        Some(&signer),
    ))
    .unwrap();
    let pk: [u8; 32] = hex::decode(signer.info().unwrap().public_key).unwrap().try_into().unwrap();
    let sig: [u8; 64] =
        hex::decode(out["signature"].as_str().unwrap()).unwrap().try_into().unwrap();
    VerifyingKey::from_bytes(&pk)
        .unwrap()
        .verify(out["message"].as_str().unwrap().as_bytes(), &Signature::from_bytes(&sig))
        .unwrap();
    assert_eq!(out["signer"]["kind"], "dev");
}

/// One test owns the process environment, since tests run in parallel.
#[test]
fn hidden_inputs_come_from_the_environment_and_fail_closed() {
    let suite = fixture("suite.json");
    let reference = fixture("reference.js");
    let mut req: serde_json::Value =
        serde_json::from_str(&validate_request(&suite, &reference)).unwrap();
    req.as_object_mut().unwrap().remove("suite");
    req.as_object_mut().unwrap().remove("reference");
    let hidden = req.to_string();

    std::env::remove_var("SUITE");
    std::env::remove_var("REFERENCE");
    let out = run(&hidden);
    assert_eq!(out["error"]["code"], "SPHINX_E_GRADER_HIDDEN_INPUT_MISSING");
    assert_eq!(out["error"]["bond_effect"], "held");

    std::env::set_var("SUITE", &suite);
    std::env::set_var("REFERENCE", &reference);
    let out = run(&hidden);
    std::env::remove_var("SUITE");
    std::env::remove_var("REFERENCE");
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["result"]["valid"], true);
    assert!(!out.to_string().contains("8944394323791464"), "the hidden suite must never be echoed");
}

#[test]
fn local_score_matches_the_enclave_counts() {
    let suite = fixture("suite.json");
    let honest = sphinx_grader::local_score(&suite, &fixture("reference.js"), 0).unwrap();
    assert_eq!((honest.passed, honest.total), (12, 12));
    assert_eq!(honest.entry, "fib");
    assert_eq!(honest.suite_hash.to_string(), h(&suite));
    let overfit = sphinx_grader::local_score(&suite, &fixture("agent-a.js"), 0).unwrap();
    assert_eq!((overfit.passed, overfit.total), (5, 12));
    let err = sphinx_grader::local_score("{}", "x", 0).unwrap_err();
    assert_eq!(err.code().as_str(), "SPHINX_E_GRADER_SUITE_MALFORMED");
}

#[test]
fn top_level_let_and_const_are_visible_to_the_entry() {
    let suite = fixture("suite.json");
    for code in [
        "const PHI = 1; function fib(n){let a=0,b=PHI;for(let i=0;i<n;i++){[a,b]=[b,a+b]}return a}",
        "let memo = [0, 1]; function fib(n){while(memo.length<=n){memo.push(memo[memo.length-1]+memo[memo.length-2])}return memo[n]}",
        "const step = (a, b) => [b, a + b]; function fib(n){let p=[0,1];for(let i=0;i<n;i++){p=step(p[0],p[1])}return p[0]}",
        "class Fib { static of(n){let a=0,b=1;for(let i=0;i<n;i++){[a,b]=[b,a+b]}return a} } function fib(n){ return Fib.of(n) }",
    ] {
        let out = run(&grade_request(&suite, code));
        assert_eq!(out["result"]["passed"], 12, "{code}");
    }
}

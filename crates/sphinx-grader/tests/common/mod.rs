//! Shared helpers for grader integration tests.
#![allow(
    dead_code,
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    clippy::needless_pass_by_value
)]

use serde_json::{json, Value};
use sphinx_protocol::Hash32;
use std::path::PathBuf;

pub fn fixture(name: &str) -> String {
    let p: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "..", "..", "fixtures", "fib", name].iter().collect();
    std::fs::read_to_string(p).unwrap()
}

pub fn h(s: &str) -> String {
    Hash32::digest(s.as_bytes()).to_string()
}

pub fn nonce() -> String {
    Hash32::digest(b"test-nonce").to_string()
}

pub fn grade_request(suite: &str, code: &str) -> String {
    json!({
        "op": "grade", "schema": "sphinx.grader/1", "contract": "sphinx.example.testnet",
        "job_id": "7", "sub_id": 3, "nonce": nonce(),
        "suite": suite, "expected_suite_hash": h(suite),
        "submission": code, "expected_code_hash": h(code), "epoch_ms": 1_700_000_000_000u64
    })
    .to_string()
}

pub fn validate_request(suite: &str, reference: &str) -> String {
    json!({
        "op": "validate", "schema": "sphinx.grader/1", "contract": "sphinx.example.testnet",
        "job_id": "7", "nonce": nonce(),
        "suite": suite, "expected_suite_hash": h(suite),
        "reference": reference, "expected_ref_hash": h(reference)
    })
    .to_string()
}

pub fn run(request: &str) -> Value {
    serde_json::to_value(sphinx_grader::handle(request, None)).unwrap()
}

/// Grades `code` against a one-test suite calling `solve()` and expecting `expected`.
pub fn score_one(code: &str, expected: Value) -> u64 {
    let suite = json!({"version": 1, "runtime": "js-boa/1", "entry": "solve",
                       "tests": [{"args": [], "expected": expected}]})
    .to_string();
    let out = run(&grade_request(&suite, code));
    assert_eq!(out["ok"], true, "{out}");
    out["result"]["passed"].as_u64().unwrap()
}

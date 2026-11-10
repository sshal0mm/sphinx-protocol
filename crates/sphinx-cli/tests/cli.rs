//! The CLI end to end, through the built binary.
#![allow(clippy::unwrap_used, reason = "test helpers")]

use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", ".."].iter().collect()
}

fn sphinx(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sphinx"))
        .args(args)
        .current_dir(root())
        .env("SPHINX_DEV_GRADER_SEED", "2a".repeat(32))
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn suite_hash_matches_sha256_of_the_file() {
    let (code, out, _) = sphinx(&["suite", "hash", "fixtures/fib/suite.json"]);
    assert_eq!(code, 0);
    let bytes = std::fs::read(root().join("fixtures/fib/suite.json")).unwrap();
    assert_eq!(out.trim(), sphinx_protocol::Hash32::digest(&bytes).to_string());
}

#[test]
fn suite_check_proves_the_reference_passes() {
    let (code, out, _) = sphinx(&[
        "suite",
        "check",
        "fixtures/fib/suite.json",
        "--reference",
        "fixtures/fib/reference.js",
    ]);
    assert_eq!(code, 0);
    assert!(out.contains("suite ok: 12 tests"));
    assert!(out.contains("reference passes every test"));
}

#[test]
fn check_reports_coded_preflight_errors() {
    let (code, out, _) =
        sphinx(&["check", "fixtures/fib/agent-b.js", "--suite", "fixtures/fib/suite.json"]);
    assert_eq!(code, 0, "{out}");
    let dir = std::env::temp_dir().join(format!("sphinx-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.js");
    std::fs::write(&bad, "function fib(n) {").unwrap();
    let (code, _, err) =
        sphinx(&["--json", "check", bad.to_str().unwrap(), "--suite", "fixtures/fib/suite.json"]);
    assert_eq!(code, 65);
    let report: serde_json::Value = serde_json::from_str(err.trim()).unwrap();
    assert_eq!(report["code"], "SPHINX_E_CODE_SYNTAX");
}

#[test]
fn explain_decodes_raw_near_errors() {
    let raw = "Smart contract panicked: SPHINX_E_SUBMIT_AGENT_CAP: agent.testnet has used 2/2 attempts on job 7 {\"v\":1,\"retry\":\"never\",\"bond_effect\":\"none\",\"hint\":\"x\",\"ctx\":{\"cap\":\"2\"}}";
    let (code, out, _) = sphinx(&["explain", raw]);
    assert_eq!(code, 0);
    assert!(out.contains("error[SPHINX_E_SUBMIT_AGENT_CAP]"));
    assert!(out.contains("ctx: cap=2"));
    let (code, _, err) = sphinx(&["explain", "nothing to see"]);
    assert_eq!(code, 65);
    assert!(err.contains("SPHINX_E_CLI_UNKNOWN_CODE"));
}

#[test]
fn missing_files_are_reported_with_the_path() {
    let (code, _, err) = sphinx(&["suite", "hash", "does/not/exist.json"]);
    assert_eq!(code, 66);
    assert!(err.contains("SPHINX_E_CLI_FILE_UNREADABLE") && err.contains("does/not/exist.json"));
}

#[cfg(feature = "dev-signer")]
#[test]
fn grade_signs_with_the_dev_key() {
    let nonce = "ab".repeat(32);
    let (code, out, err) = sphinx(&[
        "grade",
        "--op",
        "grade",
        "--contract",
        "sphinx.example.testnet",
        "--job-id",
        "1",
        "--sub-id",
        "0",
        "--nonce",
        &nonce,
        "--suite",
        "fixtures/fib/suite.json",
        "--code",
        "fixtures/fib/agent-b.js",
        "--sign",
        "dev",
    ]);
    assert_eq!(code, 0, "{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["result"]["passed"], 12);
    assert_eq!(v["signer"]["kind"], "dev");
    let golden: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("fixtures/golden/messages.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        v["signer"]["public_key"], golden["public_key"],
        "seed 0x2a.. is the golden test key"
    );
}

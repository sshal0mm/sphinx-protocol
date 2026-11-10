//! Golden vectors pin the exact signed bytes. The contract, grader and CLI
//! test suites read the same file, so a format change anywhere fails here
//! first. Regenerate deliberately with `UPDATE_GOLDEN=1 cargo test`.

use ed25519_dalek::{Signer, SigningKey, Verifier};
use serde_json::{json, Value};
use sphinx_protocol::{Hash32, Score, SuiteCheck};
use std::path::Path;

/// Deterministic test key: seed bytes all 42. Never used outside tests.
fn key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

fn vectors() -> Value {
    let validate = SuiteCheck {
        contract: "sphinx.example.testnet".into(),
        job_id: 7,
        valid: true,
        suite_hash: Hash32::digest(b"suite"),
        ref_solution_hash: Hash32::digest(b"reference"),
        nonce: Hash32::digest(b"nonce-validate"),
    };
    let result = Score {
        contract: "sphinx.example.testnet".into(),
        job_id: 7,
        sub_id: 3,
        runtime: "js-boa/1".into(),
        passed: 12,
        total: 12,
        suite_hash: Hash32::digest(b"suite"),
        code_hash: Hash32::digest(b"code"),
        nonce: Hash32::digest(b"nonce-result"),
    };
    let k = key();
    let sign = |m: &str| hex::encode(k.sign(m.as_bytes()).to_bytes());
    json!({
        "public_key": hex::encode(k.verifying_key().to_bytes()),
        "validate": {
            "result": validate,
            "message": validate.signing_message(),
            "signature": sign(&validate.signing_message()),
        },
        "result": {
            "result": result,
            "message": result.signing_message(),
            "signature": sign(&result.signing_message()),
        },
    })
}

#[test]
fn signed_messages_match_golden_vectors() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/golden/messages.json");
    let expected = vectors();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&expected).unwrap() + "\n").unwrap();
    }
    let actual: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(actual, expected, "fixtures/golden/messages.json is stale or the format changed");

    // The stored signatures verify, so other crates can rely on them.
    let vk = key().verifying_key();
    for kind in ["validate", "result"] {
        let msg = actual[kind]["message"].as_str().unwrap();
        let sig: [u8; 64] =
            hex::decode(actual[kind]["signature"].as_str().unwrap()).unwrap().try_into().unwrap();
        vk.verify(msg.as_bytes(), &ed25519_dalek::Signature::from_bytes(&sig)).unwrap();
    }
}

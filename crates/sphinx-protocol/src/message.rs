//! Signed grader results: the suite check and the submission score.
//!
//! A result is built by the grader from values it computed itself, signed
//! inside the enclave, posted to the contract verbatim, and checked field by
//! field against on-chain state before the signature is verified. The signed
//! bytes are [`SuiteCheck::signing_message`] or
//! [`Score::signing_message`]: ASCII, `:`-separated, no trailing
//! newline. Every field is an account id, a decimal integer, a bool, a
//! runtime id or lowercase hex, none of which can contain `:`, so the
//! encoding is unambiguous. See `docs/PROTOCOL.md`.

use crate::hash::Hash32;
use serde::{Deserialize, Serialize};

/// Prefix shared by every message a Sphinx grader signs.
pub const DOMAIN: &str = "sphinx:v1";

/// The grader's statement that a hidden suite is (or is not) passable by the
/// buyer's reference solution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "abi", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SuiteCheck {
    /// Escrow contract account the result is for.
    pub contract: String,
    #[serde(with = "crate::serde_u64")]
    #[cfg_attr(feature = "abi", schemars(with = "String"))]
    pub job_id: u64,
    /// True only if the reference solution passed every test.
    pub valid: bool,
    /// SHA-256 of the suite bytes the grader actually ran.
    pub suite_hash: Hash32,
    /// SHA-256 of the reference solution bytes the grader actually ran.
    pub ref_solution_hash: Hash32,
    /// Nonce issued by the contract when the suite was committed.
    pub nonce: Hash32,
}

impl SuiteCheck {
    /// The exact bytes the grader signs.
    pub fn signing_message(&self) -> String {
        format!(
            "{DOMAIN}:suite-check:{}:{}:{}:{}:{}:{}",
            self.contract,
            self.job_id,
            self.valid,
            self.suite_hash,
            self.ref_solution_hash,
            self.nonce
        )
    }
}

/// The grader's pass count for one submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "abi", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Score {
    /// Escrow contract account the result is for.
    pub contract: String,
    #[serde(with = "crate::serde_u64")]
    #[cfg_attr(feature = "abi", schemars(with = "String"))]
    pub job_id: u64,
    /// Submission index within the job.
    pub sub_id: u32,
    /// Runtime the submission ran on, e.g. `js-boa/1`.
    pub runtime: String,
    pub passed: u32,
    pub total: u32,
    /// SHA-256 of the suite bytes the grader actually ran.
    pub suite_hash: Hash32,
    /// SHA-256 of the submission bytes the grader actually ran.
    pub code_hash: Hash32,
    /// Nonce issued by the contract for this submission.
    pub nonce: Hash32,
}

impl Score {
    /// The exact bytes the grader signs.
    pub fn signing_message(&self) -> String {
        format!(
            "{DOMAIN}:score:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            self.contract,
            self.job_id,
            self.sub_id,
            self.runtime,
            self.passed,
            self.total,
            self.suite_hash,
            self.code_hash,
            self.nonce
        )
    }

    /// True when every test passed.
    pub fn is_pass(&self) -> bool {
        self.total > 0 && self.passed == self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> Hash32 {
        Hash32::from_bytes([b; 32])
    }

    #[test]
    fn result_message_layout() {
        let v = Score {
            contract: "sphinx.example.testnet".into(),
            job_id: 7,
            sub_id: 2,
            runtime: "js-boa/1".into(),
            passed: 11,
            total: 12,
            suite_hash: h(1),
            code_hash: h(2),
            nonce: h(3),
        };
        assert_eq!(
            v.signing_message(),
            format!(
                "sphinx:v1:score:sphinx.example.testnet:7:2:js-boa/1:11:12:{}:{}:{}",
                h(1),
                h(2),
                h(3)
            )
        );
        assert!(!v.is_pass());
    }

    #[test]
    fn job_id_is_a_json_string() {
        let v = SuiteCheck {
            contract: "c.testnet".into(),
            job_id: u64::MAX,
            valid: true,
            suite_hash: h(1),
            ref_solution_hash: h(2),
            nonce: h(3),
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["job_id"], "18446744073709551615");
        assert_eq!(serde_json::from_value::<SuiteCheck>(json).unwrap(), v);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut json = serde_json::to_value(SuiteCheck {
            contract: "c.testnet".into(),
            job_id: 1,
            valid: true,
            suite_hash: h(1),
            ref_solution_hash: h(2),
            nonce: h(3),
        })
        .unwrap();
        json["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<SuiteCheck>(json).is_err());
    }
}

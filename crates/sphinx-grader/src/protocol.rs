//! The grader's request and response (`sphinx.grader/1`). See `docs/PROTOCOL.md`.

use serde::{Deserialize, Serialize};
use sphinx_protocol::{ErrorReport, Hash32, Score, SuiteCheck};

use crate::signer::SignerInfo;

pub const SCHEMA: &str = "sphinx.grader/1";

/// What to grade. The grader hashes the exact bytes it runs and refuses to
/// proceed if they differ from the hashes committed on-chain.
///
/// In production the suite and the buyer's reference solution are omitted:
/// they reach the enclave as the buyer's OutLayer secret's `SUITE` and
/// `REFERENCE` environment variables, readable only by this grader build when
/// run by the Sphinx relayer. The submission arrives as `submission_box`,
/// encrypted to the grader's box key, so transaction arguments never carry
/// plaintext code. The enclave build refuses `suite`, `reference` and
/// `submission`; passing them inline is for local runs and tests.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Prove the suite is passable by the buyer's reference solution.
    Validate {
        schema: String,
        contract: String,
        #[serde(with = "u64_string")]
        job_id: u64,
        nonce: Hash32,
        #[serde(default)]
        suite: Option<String>,
        expected_suite_hash: Hash32,
        #[serde(default)]
        reference: Option<String>,
        expected_ref_hash: Hash32,
        #[serde(default)]
        epoch_ms: u64,
    },
    /// Grade one submission.
    Grade {
        schema: String,
        contract: String,
        #[serde(with = "u64_string")]
        job_id: u64,
        sub_id: u32,
        nonce: Hash32,
        #[serde(default)]
        suite: Option<String>,
        expected_suite_hash: Hash32,
        /// Plaintext code: local runs only.
        #[serde(default)]
        submission: Option<String>,
        /// The code encrypted to the grader's box key (`sphinx_box`, base64).
        #[serde(default)]
        submission_box: Option<String>,
        expected_code_hash: Hash32,
        #[serde(default)]
        epoch_ms: u64,
    },
    /// Report the grader's public keys, for registration and for encrypting.
    Identity { schema: String },
}

/// The signed statement a response carries.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Signed {
    SuiteCheck(SuiteCheck),
    Score(Score),
}

/// A problem with the submission itself, reported alongside a 0/N result.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Preflight {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Response {
    Ok {
        schema: &'static str,
        ok: bool,
        op: &'static str,
        grader_version: &'static str,
        runtime: &'static str,
        result: Signed,
        /// The exact bytes signed; post `result` and `signature` to the contract.
        message: String,
        signature: Option<String>,
        signer: Option<SignerInfo>,
        #[serde(skip_serializing_if = "Option::is_none")]
        preflight: Option<Preflight>,
    },
    /// The answer to `identity`. Keys are 64 lowercase hex characters.
    Identity {
        schema: &'static str,
        ok: bool,
        op: &'static str,
        grader_version: &'static str,
        kind: &'static str,
        signing_public_key: String,
        box_public_key: String,
    },
    Err {
        schema: &'static str,
        ok: bool,
        op: Option<String>,
        job_id: Option<String>,
        nonce: Option<String>,
        grader_version: &'static str,
        error: ErrorReport,
    },
}

mod u64_string {
    use serde::{de, Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Num(u64),
        }
        match Repr::deserialize(d)? {
            Repr::Str(s) => s.parse().map_err(de::Error::custom),
            Repr::Num(n) => Ok(n),
        }
    }
}

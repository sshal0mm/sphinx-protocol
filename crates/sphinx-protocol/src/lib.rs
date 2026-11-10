//! Shared definitions every Sphinx component must agree on byte-for-byte.
//!
//! - [`message`]: the exact bytes a grader signs and the contract verifies.
//! - [`suite`]: the hidden test-suite format and its intake rules.
//! - [`hash`]: the 32-byte SHA-256 type used for suites, code and nonces.
//! - [`hidden`]: the names of the values in a buyer's stored hidden-suite secret.
//! - [`error`] and [`catalog`]: one error model rendered identically by the
//!   contract (panic text), the grader (JSON envelope) and the CLI.
//!
//! This crate has no NEAR dependency so the grader and CLI can use it natively
//! and inside the enclave. The contract enables the `borsh` and `abi` features.

pub mod catalog;
pub mod error;
pub mod hash;
pub mod hidden;
pub mod message;
mod serde_u64;
pub mod suite;

pub use catalog::Code;
pub use error::{BondEffect, ErrorReport, Retry, SphinxError};
pub use hash::Hash32;
pub use message::{Score, SuiteCheck};
pub use suite::{Suite, TestCase};

/// Identifier of the only submission runtime shipped today: JavaScript on Boa.
pub const RUNTIME_JS_BOA_V1: &str = "js-boa/1";

/// Runtimes a job may declare. Adding one is a protocol change.
pub const SUPPORTED_RUNTIMES: &[&str] = &[RUNTIME_JS_BOA_V1];

/// How long a submission may wait for a result before its bond can be
/// reclaimed (30 minutes). The contract enforces it; clients plan with it.
pub const GRADING_WINDOW_NS: u64 = 30 * 60 * 1_000_000_000;

/// Returns true for strings that are valid NEAR account ids. Used to keep
/// signed messages unambiguous: account ids never contain `:`.
pub fn is_valid_account_id(id: &str) -> bool {
    (2..=64).contains(&id.len())
        && id.split(['.', '-', '_']).all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

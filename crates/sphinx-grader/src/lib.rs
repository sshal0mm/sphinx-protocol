//! Sphinx grader: runs a submission against a hidden suite and returns a
//! result signed inside the enclave. Only the pass count leaves the grader;
//! which tests failed is never revealed.

pub mod chain;
pub mod compare;
pub mod executor;
pub mod grade;
#[cfg(all(feature = "outlayer", target_arch = "wasm32"))]
mod host;
pub mod js;
pub mod limits;
pub mod protocol;
pub mod runtime;
pub mod signer;

/// The comparison grading uses, for off-chain tools that compare outputs.
#[cfg(feature = "evaluate")]
pub use compare::values_equal;
pub use grade::{handle, handle_in, local_score, Context, LocalScore, GRADER_VERSION};

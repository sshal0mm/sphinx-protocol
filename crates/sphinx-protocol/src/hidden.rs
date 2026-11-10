//! The buyer's hidden-suite secret: one JSON object whose keys become
//! environment variables inside the enclave run that may read it.
//!
//! `CONTRACT` and `JOB_ID` bind the secret to one escrow job: the enclave
//! grader refuses to use a suite for any other contract or job, so a stored
//! suite cannot be pointed at a look-alike contract or at another job.

/// The suite file, exactly as committed (`suite_hash`).
pub const SUITE: &str = "SUITE";
/// The buyer's reference solution, exactly as committed (`ref_solution_hash`).
pub const REFERENCE: &str = "REFERENCE";
/// The escrow contract account the suite was stored for.
pub const CONTRACT: &str = "CONTRACT";
/// The job id (decimal) the suite was stored for.
pub const JOB_ID: &str = "JOB_ID";

//! Contract errors. Every failure is a [`SphinxError`] from the shared
//! catalog, rendered as `CODE: message {json}` in the panic text. Wording
//! lives here so messages stay consistent across methods.

use near_sdk::{AccountId, FunctionError};
use sphinx_protocol::{Code, Retry, SphinxError};
use std::fmt;

use crate::types::JobStatus;

/// Wrapper that lets `#[handle_result(aliased)]` methods return catalog errors.
#[derive(Debug, FunctionError)]
pub struct ContractError(pub SphinxError);

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.render_panic())
    }
}

impl From<SphinxError> for ContractError {
    fn from(e: SphinxError) -> Self {
        Self(e)
    }
}

pub type Result<T> = core::result::Result<T, ContractError>;

/// Returns the given error from the enclosing function unless `cond` holds.
macro_rules! ensure {
    ($cond:expr, $err:expr) => {
        if !$cond {
            return Err($err.into());
        }
    };
}
pub(crate) use ensure;

pub fn not_owner(caller: &AccountId, owner: &AccountId) -> SphinxError {
    SphinxError::new(Code::AuthNotOwner, format!("{caller} is not the contract owner ({owner})"))
        .ctx("caller", caller)
        .ctx("owner", owner)
}

pub fn not_buyer(caller: &AccountId, job_id: u64, buyer: &AccountId) -> SphinxError {
    SphinxError::new(
        Code::AuthNotBuyer,
        format!("{caller} is not the buyer of job {job_id} ({buyer})"),
    )
    .ctx("caller", caller)
    .ctx("job_id", job_id)
    .ctx("buyer", buyer)
}

pub fn job_not_found(job_id: u64, next_job_id: u64) -> SphinxError {
    SphinxError::new(
        Code::JobNotFound,
        format!("job {job_id} does not exist (next job id is {next_job_id})"),
    )
    .ctx("job_id", job_id)
    .ctx("next_job_id", next_job_id)
}

/// A call that needs one of `expected` met a job in `actual`. The retry class
/// depends on whether the job can still reach an expected status.
pub fn wrong_state(job_id: u64, actual: JobStatus, expected: &[JobStatus]) -> SphinxError {
    let expected_list = expected.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("|");
    let retry = if actual.is_terminal() { Retry::Never } else { Retry::Wait };
    SphinxError::new(
        Code::JobWrongState,
        format!("job {job_id} is {}; this call needs {expected_list}", actual.as_str()),
    )
    .ctx("job_id", job_id)
    .ctx("status", actual.as_str())
    .ctx("expected", expected_list)
    .retry(retry)
}

pub fn bad_hash(code: Code, field: &str, reason: &dyn fmt::Display) -> SphinxError {
    SphinxError::new(code, format!("{field} is not a SHA-256 hash: {reason}")).ctx("field", field)
}

pub fn field_mismatch(
    field: &str,
    got: &dyn fmt::Display,
    expected: &dyn fmt::Display,
) -> SphinxError {
    SphinxError::new(
        Code::ScoreFieldMismatch,
        format!("result field {field} is {got}; the contract has {expected}"),
    )
    .ctx("field", field)
    .ctx("got", got)
    .ctx("expected", expected)
}

pub fn invariant(what: &str) -> SphinxError {
    SphinxError::new(Code::InternalInvariant, format!("internal invariant violated: {what}"))
        .ctx("what", what)
}

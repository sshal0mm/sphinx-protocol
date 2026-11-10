//! Binding a run to the escrow's on-chain state.
//!
//! The enclave build grades only what the chain asks for: a submission that is
//! `Pending` on an `Open` job with exactly the claimed code hash, nonce and
//! suite, or a `Committed` job's suite check with exactly the committed hashes
//! and nonce. Anything else is refused before any code runs, so whoever starts
//! enclave runs cannot score code that was never submitted, or score a
//! submission against another job's suite.
//!
//! The checks are pure functions over the view results; [`ChainView`] is how
//! the enclave reads them (OutLayer's NEAR RPC host functions, final blocks).

use serde_json::{json, Value};
use sphinx_protocol::{Code, Hash32, SphinxError};

/// Read-only access to contract views.
pub trait ChainView {
    /// Calls view `method` of `contract` with JSON `args` and returns its
    /// JSON result (`null` for an absent `Option`).
    ///
    /// # Errors
    /// The RPC's reason, when the view could not be read.
    fn view(&self, contract: &str, method: &str, args: &Value) -> Result<Value, String>;
}

/// What a `grade` request claims about the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GradeClaim {
    pub job_id: u64,
    pub sub_id: u32,
    pub nonce: Hash32,
    pub suite_hash: Hash32,
    pub code_hash: Hash32,
}

/// What a `validate` request claims about the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidateClaim {
    pub job_id: u64,
    pub nonce: Hash32,
    pub suite_hash: Hash32,
    pub ref_hash: Hash32,
}

/// Reads `get_job` and `get_submission` from `contract` and checks `claim`.
///
/// # Errors
/// `SPHINX_E_GRADER_NOT_ON_CHAIN` when the chain disagrees, and
/// `SPHINX_E_RELAY_RPC_FAILED` when a view cannot be read.
pub fn bind_grade(
    chain: &dyn ChainView,
    contract: &str,
    claim: &GradeClaim,
) -> Result<(), SphinxError> {
    let job_id = claim.job_id.to_string();
    let job = read(chain, contract, "get_job", &json!({ "job_id": job_id }))?;
    let sub = read(
        chain,
        contract,
        "get_submission",
        &json!({ "job_id": job_id, "sub_id": claim.sub_id }),
    )?;
    check_grade(&job, &sub, claim)
}

/// Reads `get_job` from `contract` and checks `claim`.
///
/// # Errors
/// As [`bind_grade`].
pub fn bind_validate(
    chain: &dyn ChainView,
    contract: &str,
    claim: &ValidateClaim,
) -> Result<(), SphinxError> {
    let job = read(chain, contract, "get_job", &json!({ "job_id": claim.job_id.to_string() }))?;
    check_validate(&job, claim)
}

/// The submission must be pending with the claimed code and nonce, on an open
/// job whose committed suite is the claimed one.
///
/// # Errors
/// `SPHINX_E_GRADER_NOT_ON_CHAIN`, naming the first field that disagrees.
pub fn check_grade(job: &Value, sub: &Value, claim: &GradeClaim) -> Result<(), SphinxError> {
    if job.is_null() {
        return Err(not_on_chain("job", "the job does not exist"));
    }
    expect_str(job, &["id"], &claim.job_id.to_string(), "job.id")?;
    expect_str(job, &["status"], "Open", "job.status")?;
    expect_str(
        job,
        &["commit", "suite_hash"],
        &claim.suite_hash.to_string(),
        "job.commit.suite_hash",
    )?;
    if sub.is_null() {
        return Err(not_on_chain("submission", "the submission does not exist"));
    }
    expect_str(sub, &["job_id"], &claim.job_id.to_string(), "submission.job_id")?;
    if sub.get("sub_id").and_then(Value::as_u64) != Some(u64::from(claim.sub_id)) {
        return Err(not_on_chain("submission.sub_id", "the submission id differs"));
    }
    expect_str(sub, &["status"], "Pending", "submission.status")?;
    expect_str(sub, &["code_hash"], &claim.code_hash.to_string(), "submission.code_hash")?;
    expect_str(sub, &["nonce"], &claim.nonce.to_string(), "submission.nonce")
}

/// The job must be committed with exactly the claimed hashes and nonce.
///
/// # Errors
/// `SPHINX_E_GRADER_NOT_ON_CHAIN`, naming the first field that disagrees.
pub fn check_validate(job: &Value, claim: &ValidateClaim) -> Result<(), SphinxError> {
    if job.is_null() {
        return Err(not_on_chain("job", "the job does not exist"));
    }
    expect_str(job, &["id"], &claim.job_id.to_string(), "job.id")?;
    expect_str(job, &["status"], "Committed", "job.status")?;
    expect_str(
        job,
        &["commit", "suite_hash"],
        &claim.suite_hash.to_string(),
        "job.commit.suite_hash",
    )?;
    expect_str(
        job,
        &["commit", "ref_solution_hash"],
        &claim.ref_hash.to_string(),
        "job.commit.ref_solution_hash",
    )?;
    expect_str(
        job,
        &["commit", "suite_check_nonce"],
        &claim.nonce.to_string(),
        "job.commit.suite_check_nonce",
    )
}

fn read(
    chain: &dyn ChainView,
    contract: &str,
    method: &str,
    args: &Value,
) -> Result<Value, SphinxError> {
    chain.view(contract, method, args).map_err(|e| {
        SphinxError::new(Code::RelayRpcFailed, format!("the enclave could not read {method}: {e}"))
            .ctx("method", method)
    })
}

fn expect_str(v: &Value, path: &[&str], want: &str, field: &str) -> Result<(), SphinxError> {
    let got = path.iter().try_fold(v, |v, k| v.get(k)).and_then(Value::as_str);
    if got == Some(want) {
        Ok(())
    } else {
        Err(not_on_chain(field, &format!("{field} is {}, not {want}", got.unwrap_or("absent")))
            .ctx("expected", want)
            .ctx("got", got.unwrap_or("absent")))
    }
}

fn not_on_chain(field: &str, why: &str) -> SphinxError {
    SphinxError::new(Code::GraderNotOnChain, format!("the escrow does not ask for this run: {why}"))
        .ctx("field", field)
}

/// The enclave's view of the chain: OutLayer's NEAR RPC host functions,
/// reading final blocks through the worker's RPC.
#[cfg(all(feature = "outlayer", target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy)]
pub struct HostChain;

#[cfg(all(feature = "outlayer", target_arch = "wasm32"))]
impl ChainView for HostChain {
    fn view(&self, contract: &str, method: &str, args: &Value) -> Result<Value, String> {
        let (result, error) = crate::host::rpc::view(contract, method, &args.to_string(), "final");
        if !error.is_empty() {
            return Err(error);
        }
        serde_json::from_str(&result).map_err(|e| format!("the view result is not JSON: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn h(s: &str) -> Hash32 {
        Hash32::digest(s.as_bytes())
    }

    /// Views answered from a table; records every call.
    struct Mock {
        answers: HashMap<String, Result<Value, String>>,
        calls: RefCell<Vec<(String, String, Value)>>,
    }

    impl ChainView for Mock {
        fn view(&self, contract: &str, method: &str, args: &Value) -> Result<Value, String> {
            self.calls.borrow_mut().push((contract.into(), method.into(), args.clone()));
            self.answers.get(method).cloned().unwrap_or(Ok(Value::Null))
        }
    }

    fn mock(job: Value, sub: Value) -> Mock {
        Mock {
            answers: HashMap::from([
                ("get_job".into(), Ok(job)),
                ("get_submission".into(), Ok(sub)),
            ]),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn claim() -> GradeClaim {
        GradeClaim {
            job_id: 7,
            sub_id: 3,
            nonce: h("nonce"),
            suite_hash: h("suite"),
            code_hash: h("code"),
        }
    }

    fn open_job() -> Value {
        json!({"id": "7", "status": "Open", "commit": {
            "suite_hash": h("suite"), "ref_solution_hash": h("ref"), "suite_check_nonce": h("check")}})
    }

    fn pending() -> Value {
        json!({"job_id": "7", "sub_id": 3, "status": "Pending", "code_hash": h("code"), "nonce": h("nonce")})
    }

    fn field_of(r: Result<(), SphinxError>) -> String {
        let e = r.unwrap_err();
        assert_eq!(e.code(), Code::GraderNotOnChain);
        e.to_report().ctx["field"].clone()
    }

    #[test]
    fn a_pending_submission_is_bound() {
        let m = mock(open_job(), pending());
        bind_grade(&m, "escrow.testnet", &claim()).unwrap();
        let calls = m.calls.borrow();
        assert_eq!(calls[0], ("escrow.testnet".into(), "get_job".into(), json!({"job_id": "7"})));
        assert_eq!(
            calls[1],
            ("escrow.testnet".into(), "get_submission".into(), json!({"job_id": "7", "sub_id": 3}))
        );
    }

    #[test]
    fn grade_refuses_anything_the_chain_did_not_ask_for() {
        let c = claim();
        assert_eq!(field_of(check_grade(&Value::Null, &pending(), &c)), "job");
        assert_eq!(field_of(check_grade(&open_job(), &Value::Null, &c)), "submission");
        let mut job = open_job();
        job["status"] = json!("Paid");
        assert_eq!(field_of(check_grade(&job, &pending(), &c)), "job.status");
        let mut job = open_job();
        job["commit"]["suite_hash"] = json!(h("another suite"));
        assert_eq!(field_of(check_grade(&job, &pending(), &c)), "job.commit.suite_hash");
        for (k, v, field) in [
            ("status", json!("Passed"), "submission.status"),
            ("code_hash", json!(h("other code")), "submission.code_hash"),
            ("nonce", json!(h("other nonce")), "submission.nonce"),
            ("sub_id", json!(4), "submission.sub_id"),
            ("job_id", json!("8"), "submission.job_id"),
        ] {
            let mut sub = pending();
            sub[k] = v;
            assert_eq!(field_of(check_grade(&open_job(), &sub, &c)), field, "{k}");
        }
    }

    #[test]
    fn validate_needs_the_committed_hashes_and_nonce() {
        let c = ValidateClaim {
            job_id: 7,
            nonce: h("check"),
            suite_hash: h("suite"),
            ref_hash: h("ref"),
        };
        let mut job = open_job();
        job["status"] = json!("Committed");
        check_validate(&job, &c).unwrap();
        assert_eq!(field_of(check_validate(&open_job(), &c)), "job.status");
        assert_eq!(field_of(check_validate(&Value::Null, &c)), "job");
        for (k, field) in [
            ("suite_hash", "job.commit.suite_hash"),
            ("ref_solution_hash", "job.commit.ref_solution_hash"),
            ("suite_check_nonce", "job.commit.suite_check_nonce"),
        ] {
            let mut j = job.clone();
            j["commit"][k] = json!(h("x"));
            assert_eq!(field_of(check_validate(&j, &c)), field);
        }
        let m = mock(job, Value::Null);
        bind_validate(&m, "escrow.testnet", &c).unwrap();
        assert_eq!(m.calls.borrow().len(), 1);
    }

    #[test]
    fn an_unreadable_view_is_a_transient_failure() {
        let m = Mock {
            answers: HashMap::from([("get_job".into(), Err("timeout".into()))]),
            calls: RefCell::new(Vec::new()),
        };
        assert_eq!(bind_grade(&m, "e.testnet", &claim()).unwrap_err().code(), Code::RelayRpcFailed);
    }
}

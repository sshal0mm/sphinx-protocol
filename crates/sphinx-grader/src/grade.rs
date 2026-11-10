//! Request handling: check inputs, run the suite, build and sign the result.
//!
//! The result is assembled only from values computed here: the SHA-256 of
//! the suite and code bytes actually run, and the pass count. Request fields
//! are compared against those values; they are never signed as given.

use serde_json::Value;
use sphinx_protocol::{
    hidden, is_valid_account_id, Code, Hash32, Score, SphinxError, Suite, SuiteCheck,
};

use crate::chain::{self, ChainView, GradeClaim, ValidateClaim};
use crate::executor::Budget;
use crate::js::JsBoaRuntime;
use crate::limits::{MAX_ABORTS, MAX_CODE_BYTES, RUN_POLLS};
use crate::protocol::{Preflight, Request, Response, Signed, SCHEMA};
use crate::runtime::{Runtime, TestOutcome};
use crate::signer::GraderSigner;

pub const GRADER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where a run gets what the request does not carry, and which rules apply.
pub struct Context<'a> {
    /// Enclave rules: no plaintext inputs, the hidden suite must name this
    /// job, and the run must match the escrow's on-chain state.
    pub enclave: bool,
    /// Contract views, required under enclave rules.
    pub chain: Option<&'a dyn ChainView>,
    /// The hidden inputs (the buyer's secret, as environment variables).
    pub hidden: &'a dyn Fn(&str) -> Option<String>,
}

impl std::fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("enclave", &self.enclave)
            .field("chain", &self.chain.is_some())
            .finish_non_exhaustive()
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Handles one raw request in this process: enclave rules and the enclave's
/// chain access in the OutLayer build, local rules otherwise. Never panics;
/// every failure is an error response.
pub fn handle(input: &str, signer: Option<&dyn GraderSigner>) -> Response {
    #[cfg(all(feature = "outlayer", target_arch = "wasm32"))]
    let ctx = Context { enclave: true, chain: Some(&chain::HostChain), hidden: &env_var };
    #[cfg(not(all(feature = "outlayer", target_arch = "wasm32")))]
    let ctx = Context { enclave: false, chain: None, hidden: &env_var };
    handle_in(input, signer, &ctx)
}

/// Handles one raw request under `ctx`. Never panics; every failure is an
/// error response.
pub fn handle_in(input: &str, signer: Option<&dyn GraderSigner>, ctx: &Context<'_>) -> Response {
    let loose: Value = serde_json::from_str(input).unwrap_or(Value::Null);
    let field =
        |k: &str| loose.get(k).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned));
    match serde_json::from_str::<Request>(input) {
        Err(e) => error_response(
            field("op"),
            field("job_id"),
            field("nonce"),
            &SphinxError::new(
                Code::GraderBadRequest,
                format!(
                    "request does not match {SCHEMA} (line {}, column {})",
                    e.line(),
                    e.column()
                ),
            )
            .ctx("line", e.line())
            .ctx("column", e.column()),
        ),
        Ok(request) => {
            let (op, job_id, nonce) = match &request {
                Request::Validate { job_id, nonce, .. } => {
                    ("validate", Some(job_id.to_string()), Some(nonce.to_string()))
                }
                Request::Grade { job_id, nonce, .. } => {
                    ("grade", Some(job_id.to_string()), Some(nonce.to_string()))
                }
                Request::Identity { .. } => ("identity", None, None),
            };
            run(request, signer, ctx)
                .unwrap_or_else(|e| error_response(Some(op.into()), job_id, nonce, &e))
        }
    }
}

/// The outcome of running code against a suite on this machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LocalScore {
    pub passed: u32,
    pub total: u32,
    pub suite_hash: Hash32,
    pub code_hash: Hash32,
    pub entry: String,
    pub runtime: String,
}

/// Runs `code` against `suite_json` locally, unsigned, with the same intake
/// limits and budgets as the enclave. Used to prove a reference passes a suite
/// before a job is posted.
///
/// # Errors
/// The intake error for a malformed or oversized suite or code.
pub fn local_score(suite_json: &str, code: &str, epoch_ms: u64) -> Result<LocalScore, SphinxError> {
    let (suite, suite_hash) = intake_suite(suite_json, Hash32::digest(suite_json.as_bytes()))?;
    let code_hash = intake_code(code.as_bytes(), Hash32::digest(code.as_bytes()), "code")?;
    let runtime = JsBoaRuntime { epoch_ms };
    let (passed, total, _) = run_suite(&runtime, &suite, code);
    Ok(LocalScore {
        passed,
        total,
        suite_hash,
        code_hash,
        entry: suite.entry,
        runtime: suite.runtime,
    })
}

/// The clock a signed run uses. The request's clock is not part of the
/// signed result, so the enclave build ignores it and always runs at 0: the
/// caller cannot change what time-dependent code returns. Local runs keep it.
const fn signed_clock(requested: u64) -> u64 {
    if cfg!(feature = "outlayer") {
        0
    } else {
        requested
    }
}

fn error_response(
    op: Option<String>,
    job_id: Option<String>,
    nonce: Option<String>,
    e: &SphinxError,
) -> Response {
    Response::Err {
        schema: SCHEMA,
        ok: false,
        op,
        job_id,
        nonce,
        grader_version: GRADER_VERSION,
        error: e.to_report(),
    }
}

fn run(
    request: Request,
    signer: Option<&dyn GraderSigner>,
    ctx: &Context<'_>,
) -> Result<Response, SphinxError> {
    match request {
        Request::Identity { schema } => {
            check_schema(&schema)?;
            identity(signer)
        }
        Request::Validate {
            schema,
            contract,
            job_id,
            nonce,
            suite,
            expected_suite_hash,
            reference,
            expected_ref_hash,
            epoch_ms,
        } => {
            check_schema(&schema)?;
            check_contract(&contract)?;
            if ctx.enclave {
                refuse_inline(&[("suite", suite.is_some()), ("reference", reference.is_some())])?;
                check_suite_is_for(ctx, &contract, job_id)?;
                let claim = ValidateClaim {
                    job_id,
                    nonce,
                    suite_hash: expected_suite_hash,
                    ref_hash: expected_ref_hash,
                };
                chain::bind_validate(need_chain(ctx)?, &contract, &claim)?;
            }
            let suite = hidden_or_inline(suite, SUITE_VAR, ctx)?;
            let reference = hidden_or_inline(reference, REFERENCE_VAR, ctx)?;
            let (suite, suite_hash) = intake_suite(&suite, expected_suite_hash)?;
            let ref_solution_hash =
                intake_code(reference.as_bytes(), expected_ref_hash, "reference")?;
            let runtime = JsBoaRuntime { epoch_ms: signed_clock(epoch_ms) };
            let (passed, total, _) = run_suite(&runtime, &suite, &reference);
            let result = SuiteCheck {
                contract,
                job_id,
                valid: passed == total,
                suite_hash,
                ref_solution_hash,
                nonce,
            };
            let message = result.signing_message();
            finish("validate", runtime.id(), Signed::SuiteCheck(result), message, None, signer)
        }
        Request::Grade {
            schema,
            contract,
            job_id,
            sub_id,
            nonce,
            suite,
            expected_suite_hash,
            submission,
            submission_box,
            expected_code_hash,
            epoch_ms,
        } => {
            check_schema(&schema)?;
            check_contract(&contract)?;
            if ctx.enclave {
                refuse_inline(&[("suite", suite.is_some()), ("submission", submission.is_some())])?;
                check_suite_is_for(ctx, &contract, job_id)?;
                let claim = GradeClaim {
                    job_id,
                    sub_id,
                    nonce,
                    suite_hash: expected_suite_hash,
                    code_hash: expected_code_hash,
                };
                chain::bind_grade(need_chain(ctx)?, &contract, &claim)?;
            }
            let suite = hidden_or_inline(suite, SUITE_VAR, ctx)?;
            let (suite, suite_hash) = intake_suite(&suite, expected_suite_hash)?;
            let code = submission_code(submission, submission_box.as_deref(), signer)?;
            let code_hash = intake_code(&code, expected_code_hash, "submission")?;
            let code = String::from_utf8(code)
                .map_err(|_| SphinxError::new(Code::CodeSyntax, "submission is not valid UTF-8"))?;
            let runtime = JsBoaRuntime { epoch_ms: signed_clock(epoch_ms) };
            let (passed, total, preflight) = run_suite(&runtime, &suite, &code);
            let result = Score {
                contract,
                job_id,
                sub_id,
                runtime: runtime.id().to_owned(),
                passed,
                total,
                suite_hash,
                code_hash,
                nonce,
            };
            let message = result.signing_message();
            finish("grade", runtime.id(), Signed::Score(result), message, preflight, signer)
        }
    }
}

/// The grader's public keys. Needs no chain state: it answers the same in
/// every run of this build by this caller, and its attested output is what
/// the operator registers with `set_grader_key`.
fn identity(signer: Option<&dyn GraderSigner>) -> Result<Response, SphinxError> {
    let signer = signer.ok_or_else(|| {
        SphinxError::new(Code::GraderSignerUnavailable, "this build has no signer configured")
    })?;
    let info = signer.info()?;
    let box_public_key = signer.box_secret()?.public_key_hex();
    Ok(Response::Identity {
        schema: SCHEMA,
        ok: true,
        op: "identity",
        grader_version: GRADER_VERSION,
        kind: info.kind,
        signing_public_key: info.public_key,
        box_public_key,
    })
}

/// Enclave rule: plaintext inputs are never accepted, because whatever a
/// request carries is public in the transaction that started the run.
///
/// # Errors
/// `SPHINX_E_GRADER_INLINE_INPUT_REFUSED`, naming the first field present.
pub fn refuse_inline(fields: &[(&'static str, bool)]) -> Result<(), SphinxError> {
    match fields.iter().find(|(_, present)| *present) {
        None => Ok(()),
        Some((field, _)) => Err(SphinxError::new(
            Code::GraderInlineInputRefused,
            format!("the enclave grader does not accept {field} in the request"),
        )
        .ctx("field", field)),
    }
}

fn need_chain<'a>(ctx: &Context<'a>) -> Result<&'a dyn ChainView, SphinxError> {
    ctx.chain.ok_or_else(|| {
        SphinxError::new(Code::GraderNotOnChain, "this run cannot read the escrow's state")
            .ctx("field", "chain")
    })
}

/// Enclave rule: the buyer's secret names the contract and job it was stored
/// for, and the run must be for that contract and job.
fn check_suite_is_for(ctx: &Context<'_>, contract: &str, job_id: u64) -> Result<(), SphinxError> {
    // No secret at all is a missing input, not a secret for another job.
    hidden_or_inline(None, SUITE_VAR, ctx)?;
    let job_id = job_id.to_string();
    for (var, want) in [(hidden::CONTRACT, contract), (hidden::JOB_ID, job_id.as_str())] {
        let got = (ctx.hidden)(var);
        if got.as_deref() != Some(want) {
            return Err(SphinxError::new(
                Code::GraderSuiteNotForJob,
                format!(
                    "the stored suite's {var} is {}, not {want}",
                    got.as_deref().unwrap_or("absent")
                ),
            )
            .ctx("field", var)
            .ctx("expected", want)
            .ctx("got", got.as_deref().unwrap_or("absent")));
        }
    }
    Ok(())
}

/// The submission's bytes: opened from `submission_box` with the signer's box
/// key, or the inline `submission` (local runs). Exactly one must be given.
fn submission_code(
    inline: Option<String>,
    encrypted: Option<&str>,
    signer: Option<&dyn GraderSigner>,
) -> Result<Vec<u8>, SphinxError> {
    match (inline, encrypted) {
        (Some(code), None) => Ok(code.into_bytes()),
        (None, Some(encrypted)) => {
            if encrypted.len() > MAX_BOX_B64 {
                return Err(SphinxError::new(
                    Code::GraderCodeTooLarge,
                    format!(
                        "submission_box is {} bytes; the maximum is {MAX_BOX_B64}",
                        encrypted.len()
                    ),
                )
                .ctx("bytes", encrypted.len())
                .ctx("max", MAX_BOX_B64));
            }
            let signer = signer.ok_or_else(|| {
                SphinxError::new(Code::GraderSignerUnavailable, "no box key to open submission_box")
            })?;
            signer.box_secret()?.open(encrypted, sphinx_box::SUBMISSION_INFO).map_err(|e| {
                SphinxError::new(
                    Code::GraderBoxUnreadable,
                    format!("submission_box cannot be opened: {e}"),
                )
            })
        }
        _ => Err(SphinxError::new(
            Code::GraderBadRequest,
            "a grade request carries exactly one of submission and submission_box",
        )),
    }
}

/// Largest `submission_box` read: base64 of a 64 KiB submission plus the box overhead.
const MAX_BOX_B64: usize = (MAX_CODE_BYTES + 61).div_ceil(3) * 4;

fn finish(
    op: &'static str,
    runtime: &'static str,
    result: Signed,
    message: String,
    preflight: Option<Preflight>,
    signer: Option<&dyn GraderSigner>,
) -> Result<Response, SphinxError> {
    let (signature, signer) = match signer {
        Some(s) => (Some(s.sign(&message)?), Some(s.info()?)),
        None => (None, None),
    };
    Ok(Response::Ok {
        schema: SCHEMA,
        ok: true,
        op,
        grader_version: GRADER_VERSION,
        runtime,
        result,
        message,
        signature,
        signer,
        preflight,
    })
}

/// Runs the tests in order. Returns (passed, total, preflight problem). A
/// submission that fails preflight scores 0 without running tests; after
/// `MAX_ABORTS` tests stop on a resource limit, the rest count as failed.
fn run_suite(runtime: &dyn Runtime, suite: &Suite, code: &str) -> (u32, u32, Option<Preflight>) {
    let total = u32::try_from(suite.tests.len()).unwrap_or(u32::MAX);
    let mut budget = Budget::new(RUN_POLLS);
    if let Some(d) = runtime.preflight(code, &suite.entry, &mut budget) {
        return (
            0,
            total,
            Some(Preflight { code: d.code.as_str().to_owned(), message: d.message }),
        );
    }
    let (mut passed, mut aborts) = (0u32, 0u32);
    for test in &suite.tests {
        if aborts >= MAX_ABORTS {
            break;
        }
        match runtime.run_test(
            code,
            &suite.entry,
            &test.args,
            &test.expected,
            suite.check.as_deref(),
            &mut budget,
        ) {
            TestOutcome::Pass => passed += 1,
            TestOutcome::Fail => {}
            TestOutcome::Aborted => aborts += 1,
        }
    }
    (passed, total, None)
}

/// Environment variable carrying the hidden suite (the secret's `SUITE` key).
pub const SUITE_VAR: &str = hidden::SUITE;
/// Environment variable carrying the buyer's reference solution.
pub const REFERENCE_VAR: &str = hidden::REFERENCE;

/// Uses the inline value when the request carries one, otherwise the stored
/// secret's environment variable. Fails closed when neither is present: a run
/// by another build or caller, or a deleted secret, gets no suite, and must not
/// produce a result.
fn hidden_or_inline(
    inline: Option<String>,
    var: &'static str,
    ctx: &Context<'_>,
) -> Result<String, SphinxError> {
    if let Some(v) = inline {
        return Ok(v);
    }
    match (ctx.hidden)(var) {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(SphinxError::new(
            Code::GraderHiddenInputMissing,
            format!("hidden input {var} is not available to this run"),
        )
        .ctx("var", var)),
    }
}

fn check_schema(schema: &str) -> Result<(), SphinxError> {
    if schema == SCHEMA {
        Ok(())
    } else {
        Err(SphinxError::new(Code::GraderBadRequest, format!("schema must be {SCHEMA}"))
            .ctx("expected", SCHEMA))
    }
}

fn check_contract(contract: &str) -> Result<(), SphinxError> {
    if is_valid_account_id(contract) {
        Ok(())
    } else {
        Err(SphinxError::new(Code::GraderBadContractId, "contract is not a valid NEAR account id"))
    }
}

/// Hashes and parses the suite. Errors never quote suite contents.
fn intake_suite(raw: &str, expected: Hash32) -> Result<(Suite, Hash32), SphinxError> {
    let actual = Hash32::digest(raw.as_bytes());
    if actual != expected {
        return Err(SphinxError::new(
            Code::GraderSuiteHashMismatch,
            format!("suite hash {actual} does not match the committed {expected}"),
        )
        .ctx("got", actual)
        .ctx("expected", expected));
    }
    Ok((Suite::parse(raw.as_bytes())?, actual))
}

fn intake_code(code: &[u8], expected: Hash32, role: &str) -> Result<Hash32, SphinxError> {
    if code.len() > MAX_CODE_BYTES {
        return Err(SphinxError::new(
            Code::GraderCodeTooLarge,
            format!("{role} is {} bytes; the maximum is {MAX_CODE_BYTES}", code.len()),
        )
        .ctx("bytes", code.len())
        .ctx("max", MAX_CODE_BYTES));
    }
    let actual = Hash32::digest(code);
    if actual != expected {
        return Err(SphinxError::new(
            Code::GraderCodeHashMismatch,
            format!("{role} hash {actual} does not match the expected {expected}"),
        )
        .ctx("role", role)
        .ctx("got", actual)
        .ctx("expected", expected));
    }
    Ok(actual)
}

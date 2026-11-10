//! `sphinx`: local tools for buyers, agents and operators.
//!
//! - `suite hash|check`: hash a hidden suite and check it (optionally against
//!   a reference solution) before committing it on-chain.
//! - `check`: preflight a submission locally before spending a bond.
//! - `grade`: run the grader locally and print its response (signed with a
//!   dev key when built with `dev-signer` and `--sign dev`).
//! - `explain`, `errors`: decode any Sphinx error code or panic text.
//! - `verify`: check a recorded job from its evidence bundle, offline.
//! - `dev`: dev-key management (feature `dev-signer` only).

mod output;
mod verify;

use clap::{Parser, Subcommand, ValueEnum};
use sphinx_grader::signer::GraderSigner;
use sphinx_near::hidden_suite::{grader_target, profile_for_job, secret_values, Keystore};
use sphinx_near::near_api::Signer;
use sphinx_near::views::{Job, JobStatus};
use sphinx_near::Escrow;
use sphinx_protocol::catalog::Code;
use sphinx_protocol::{error::parse_panic, Hash32, SphinxError, Suite};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "sphinx",
    version,
    about = "Sphinx: escrow for AI agent work, graded by hidden tests in a TEE"
)]
struct Cli {
    /// Print errors as JSON (also: `SPHINX_OUTPUT=json`).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Work with hidden test suites.
    #[command(subcommand)]
    Suite(SuiteCmd),
    /// Preflight a submission without running any tests: does it load and
    /// define the entry function? Free, local, no bond at risk.
    Check {
        /// The submission (JavaScript).
        code: PathBuf,
        /// Entry function name from the job spec (e.g. `solve`).
        #[arg(long, required_unless_present = "suite", conflicts_with = "suite")]
        entry: Option<String>,
        /// Or take the entry name from a suite file (buyers).
        #[arg(long)]
        suite: Option<PathBuf>,
    },
    /// Run the grader locally and print its response.
    Grade(GradeArgs),
    /// Explain an error code, or decode a raw NEAR error that contains one.
    Explain {
        /// A code like `SPHINX_E_SUBMIT_AGENT_CAP`, or any text containing one.
        text: String,
    },
    /// List every error code.
    Errors,
    /// Check recorded jobs from their evidence bundles (`deployments/runs`),
    /// offline: the grader registration, every signed result and the payout.
    /// Exits 0 when every check passes, 1 when one fails.
    Verify {
        /// Bundles written by `scripts/record-run.sh`.
        #[arg(required = true)]
        bundles: Vec<PathBuf>,
        /// Fetch Intel's collateral for each bundle's TDX quote and save it in
        /// the bundle (needs the network once; checks stay offline after).
        #[arg(long)]
        fetch_collateral: bool,
        /// Also re-read from chain what the offline checks take from the
        /// bundle: the job, its submissions, the grader key register and
        /// OutLayer's approved list at the recorded block (archival RPC).
        #[arg(long)]
        online: bool,
    },
    /// Read stored code from a Sphinx job API. Task text is public; submitted
    /// code is served only to the agent that submitted it and, once a job is
    /// paid, to its buyer for the winning code. With `--as`, the request is
    /// signed (NEP-413) with the key in `SPHINX_SIGNER_KEY`.
    #[command(subcommand)]
    Code(CodeCmd),
    /// Dev-only key management.
    #[cfg(feature = "dev-signer")]
    #[command(subcommand)]
    Dev(DevCmd),
}

#[derive(Subcommand, Debug)]
enum CodeCmd {
    /// Print the code stored under a SHA-256.
    Get {
        /// The code hash (64 hex).
        hash: String,
        /// The job API.
        #[arg(long, env = "SPHINX_API", default_value = "http://127.0.0.1:8787")]
        api: String,
        /// Sign the request as this account (key in `SPHINX_SIGNER_KEY`).
        #[arg(long = "as")]
        account: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum SuiteCmd {
    /// Print the SHA-256 to commit on-chain (of the exact file bytes).
    Hash { file: PathBuf },
    /// Check intake rules; with --reference, prove the suite is passable.
    Check {
        file: PathBuf,
        #[arg(long)]
        reference: Option<PathBuf>,
    },
    /// Store a job's hidden suite and reference, encrypted, so only the
    /// grader build frozen into the job can read them, as the buyer. The files
    /// must match the job's committed hashes. Encryption happens locally; only
    /// ciphertext leaves this machine. The buyer's key is read from
    /// `SPHINX_SIGNER_KEY` (`ed25519:...`).
    Store(StoreArgs),
    /// Delete an ended job's hidden suite and refund its storage deposit, as
    /// the buyer. The buyer's key is read from `SPHINX_SIGNER_KEY`.
    Delete(DeleteArgs),
    /// Publish an ended job's hidden tests in the job API, so anyone can
    /// re-hash them against the job's on-chain commit and see they were not
    /// changed. The buyer's choice: the tests show what the buyer needed. The
    /// file must match the committed hash. Needs no key: the API serves it
    /// only once every job that committed it has ended. Reference solutions
    /// are never published.
    Reveal(RevealArgs),
}

#[derive(clap::Args, Debug)]
struct RevealArgs {
    #[arg(long)]
    job_id: u64,
    #[arg(long)]
    suite: PathBuf,
    /// Escrow contract holding the job.
    #[arg(long, env = "SPHINX_CONTRACT")]
    contract: String,
    /// The job API.
    #[arg(long, env = "SPHINX_API", default_value = "http://127.0.0.1:8787")]
    api: String,
}

#[derive(clap::Args, Debug)]
struct StoreArgs {
    #[arg(long)]
    job_id: u64,
    #[arg(long)]
    suite: PathBuf,
    #[arg(long)]
    reference: PathBuf,
    /// Buyer account that owns the secret and signs.
    #[arg(long)]
    buyer: String,
    /// Escrow contract holding the job.
    #[arg(long, env = "SPHINX_CONTRACT")]
    contract: String,
}

#[derive(clap::Args, Debug)]
struct DeleteArgs {
    #[arg(long)]
    job_id: u64,
    /// Buyer account that owns the secret and signs.
    #[arg(long)]
    buyer: String,
    /// Escrow contract holding the job.
    #[arg(long, env = "SPHINX_CONTRACT")]
    contract: String,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Validate,
    Grade,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum SignWith {
    None,
    #[cfg(feature = "dev-signer")]
    Dev,
}

#[derive(clap::Args, Debug)]
struct GradeArgs {
    #[arg(long, value_enum)]
    op: Op,
    /// Escrow contract account.
    #[arg(long)]
    contract: String,
    #[arg(long)]
    job_id: u64,
    /// Submission id (grade only).
    #[arg(long)]
    sub_id: Option<u32>,
    /// Nonce issued by the contract (64 hex).
    #[arg(long)]
    nonce: String,
    #[arg(long)]
    suite: PathBuf,
    /// Reference solution (validate) or submission (grade).
    #[arg(long)]
    code: PathBuf,
    /// Fixed clock for `Date.now()`, in ms since the epoch.
    #[arg(long, default_value_t = 0)]
    epoch_ms: u64,
    #[arg(long, value_enum, default_value = "none")]
    sign: SignWith,
}

#[cfg(feature = "dev-signer")]
#[derive(Subcommand, Debug)]
enum DevCmd {
    /// Print a new dev seed as an env line (append it to .env).
    Keygen,
    /// Print the public key of `SPHINX_DEV_GRADER_SEED`.
    Pubkey,
    /// Print the box public key derived from `SPHINX_DEV_GRADER_SEED`.
    BoxPubkey,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json || std::env::var("SPHINX_OUTPUT").is_ok_and(|v| v == "json");
    if let Command::Verify { bundles, fetch_collateral, online } = &cli.command {
        return verify_files(bundles, *fetch_collateral, *online, json);
    }
    match run(cli.command) {
        Ok(out) => {
            print_out(&out);
            ExitCode::SUCCESS
        }
        Err(e) => output::fail(&e, json),
    }
}

#[allow(clippy::print_stdout, reason = "stdout is the data channel")]
fn print_out(s: &str) {
    println!("{s}");
}

fn read(path: &Path) -> Result<String, SphinxError> {
    std::fs::read_to_string(path).map_err(|e| {
        SphinxError::new(Code::CliFileUnreadable, format!("cannot read {}: {e}", path.display()))
            .ctx("path", path.display())
    })
}

fn run(command: Command) -> Result<String, SphinxError> {
    match command {
        Command::Suite(SuiteCmd::Hash { file }) => {
            Ok(Hash32::digest(read(&file)?.as_bytes()).to_string())
        }
        Command::Suite(SuiteCmd::Check { file, reference }) => {
            suite_check(&file, reference.as_deref())
        }
        Command::Check { code, entry, suite } => check(&code, entry, suite.as_deref()),
        Command::Grade(args) => grade(&args),
        Command::Code(CodeCmd::Get { hash, api, account }) => {
            block_on(code_get(&hash, &api, account.as_deref()))
        }
        Command::Suite(SuiteCmd::Store(args)) => suite_store(&args),
        Command::Suite(SuiteCmd::Delete(args)) => suite_delete(&args),
        Command::Suite(SuiteCmd::Reveal(args)) => suite_reveal(&args),
        Command::Explain { text } => explain(&text),
        Command::Errors => Ok(Code::ALL
            .iter()
            .map(|c| format!("{:<44} {:<10} {}", c.as_str(), c.retry().as_str(), c.hint()))
            .collect::<Vec<_>>()
            .join("\n")),
        Command::Verify { .. } => {
            Err(SphinxError::new(Code::CliUsage, "verify is handled in main"))
        }
        #[cfg(feature = "dev-signer")]
        Command::Dev(cmd) => dev::run(&cmd),
    }
}

/// Verifies each bundle and prints its report. Exit 1 if any check fails.
fn verify_files(bundles: &[PathBuf], fetch_collateral: bool, online: bool, json: bool) -> ExitCode {
    let mut failed = 0;
    for path in bundles {
        let mut bundle = match read(path).and_then(|t| {
            serde_json::from_str::<serde_json::Value>(&t).map_err(|e| {
                SphinxError::new(Code::CliUsage, format!("{} is not JSON: {e}", path.display()))
            })
        }) {
            Ok(b) => b,
            Err(e) => return output::fail(&e, json),
        };
        if fetch_collateral {
            let fetched = tokio::runtime::Runtime::new()
                .map_err(|e| e.to_string())
                .and_then(|rt| rt.block_on(verify::fetch_collateral(&mut bundle)));
            let saved = fetched.and_then(|()| {
                serde_json::to_string_pretty(&bundle)
                    .map_err(|e| e.to_string())
                    .and_then(|t| std::fs::write(path, t + "\n").map_err(|e| e.to_string()))
            });
            if let Err(why) = saved {
                return output::fail(&SphinxError::new(Code::CliUsage, why), json);
            }
        }
        let mut report = verify::verify_bundle(&bundle);
        if online {
            let fetched = tokio::runtime::Runtime::new()
                .map_err(|e| e.to_string())
                .and_then(|rt| rt.block_on(verify::online_checks(&bundle)));
            match fetched {
                Ok(more) => report.lines.extend(more.lines),
                Err(why) => return output::fail(&SphinxError::new(Code::CliUsage, why), json),
            }
        }
        failed += report.failed();
        print_out(&format!("== {}\n{}", path.display(), report.render().trim_end()));
    }
    ExitCode::from(verify::Report::exit_code(failed))
}

fn suite_check(file: &Path, reference: Option<&Path>) -> Result<String, SphinxError> {
    let raw = read(file)?;
    let suite = Suite::parse(raw.as_bytes())?;
    let mut lines = vec![
        format!(
            "suite ok: {} tests, entry `{}`, runtime {}",
            suite.tests.len(),
            suite.entry,
            suite.runtime
        ),
        format!("suite_hash {}", Hash32::digest(raw.as_bytes())),
    ];
    if let Some(r) = reference {
        let code = read(r)?;
        let run = LocalRun {
            op: Op::Validate,
            contract: "local.testnet",
            job_id: 0,
            sub_id: None,
            nonce: Hash32::digest(b"local"),
            suite: &raw,
            code: &code,
            epoch_ms: 0,
        };
        let response = local_grade(&run, None)?;
        let valid = response["result"]["valid"].as_bool().unwrap_or(false);
        lines.push(format!(
            "reference {}: {}",
            if valid { "passes every test" } else { "FAILS" },
            r.display()
        ));
        lines.push(format!("ref_solution_hash {}", Hash32::digest(code.as_bytes())));
    }
    Ok(lines.join("\n"))
}

fn check(
    code_path: &Path,
    entry: Option<String>,
    suite_path: Option<&Path>,
) -> Result<String, SphinxError> {
    let entry = match (entry, suite_path) {
        (Some(e), _) => e,
        (None, Some(p)) => Suite::parse(read(p)?.as_bytes())?.entry,
        (None, None) => {
            return Err(SphinxError::new(Code::CliUsage, "pass --entry <name> or --suite <file>"))
        }
    };
    let code = read(code_path)?;
    let mut budget = sphinx_grader::executor::Budget::new(sphinx_grader::limits::RUN_POLLS);
    let runtime = sphinx_grader::js::JsBoaRuntime { epoch_ms: 0 };
    match sphinx_grader::runtime::Runtime::preflight(&runtime, &code, &entry, &mut budget) {
        None => Ok(format!("ok: defines `{entry}`; code_hash {}", Hash32::digest(code.as_bytes()))),
        Some(d) => Err(SphinxError::new(d.code, d.message)),
    }
}

/// Inputs of one local grader run.
struct LocalRun<'a> {
    op: Op,
    contract: &'a str,
    job_id: u64,
    sub_id: Option<u32>,
    nonce: Hash32,
    suite: &'a str,
    code: &'a str,
    epoch_ms: u64,
}

/// Runs the grader in-process and returns its successful response.
fn local_grade(
    run: &LocalRun<'_>,
    signer: Option<&dyn GraderSigner>,
) -> Result<serde_json::Value, SphinxError> {
    let mut req = serde_json::json!({
        "op": if run.op == Op::Validate { "validate" } else { "grade" },
        "schema": sphinx_grader::protocol::SCHEMA, "contract": run.contract,
        "job_id": run.job_id.to_string(), "nonce": run.nonce.to_string(),
        "suite": run.suite, "expected_suite_hash": Hash32::digest(run.suite.as_bytes()).to_string(),
        "epoch_ms": run.epoch_ms,
    });
    let code_hash = Hash32::digest(run.code.as_bytes()).to_string();
    if run.op == Op::Validate {
        req["reference"] = run.code.into();
        req["expected_ref_hash"] = code_hash.into();
    } else {
        let sub_id = run.sub_id.ok_or_else(|| {
            SphinxError::new(Code::CliUsage, "--sub-id is required with --op grade")
        })?;
        req["submission"] = run.code.into();
        req["expected_code_hash"] = code_hash.into();
        req["sub_id"] = sub_id.into();
    }
    response_value(&sphinx_grader::handle(&req.to_string(), signer))
}

fn response_value(r: &sphinx_grader::protocol::Response) -> Result<serde_json::Value, SphinxError> {
    let v = serde_json::to_value(r).unwrap_or_default();
    if v["ok"] == true {
        Ok(v)
    } else {
        Err(serde_json::from_value::<sphinx_protocol::ErrorReport>(v["error"].clone())
            .ok()
            .and_then(|rep| rep.code.parse::<Code>().ok().map(|c| SphinxError::new(c, rep.message)))
            .unwrap_or_else(|| {
                SphinxError::new(Code::InternalInvariant, "grader returned no error report")
            }))
    }
}

fn grade(a: &GradeArgs) -> Result<String, SphinxError> {
    let nonce: Hash32 = a.nonce.parse().map_err(|e| {
        SphinxError::new(
            Code::CliUsage,
            format!("--nonce is not a 64-char lowercase hex value: {e}"),
        )
    })?;
    let suite = read(&a.suite)?;
    let code = read(&a.code)?;
    #[cfg(feature = "dev-signer")]
    let dev = match a.sign {
        SignWith::Dev => Some(dev::signer()?),
        SignWith::None => None,
    };
    #[cfg(feature = "dev-signer")]
    let signer: Option<&dyn GraderSigner> = dev.as_ref().map(|s| s as _);
    #[cfg(not(feature = "dev-signer"))]
    let signer: Option<&dyn GraderSigner> = {
        let _ = a.sign;
        None
    };
    let run = LocalRun {
        op: a.op,
        contract: &a.contract,
        job_id: a.job_id,
        sub_id: a.sub_id,
        nonce,
        suite: &suite,
        code: &code,
        epoch_ms: a.epoch_ms,
    };
    let v = local_grade(&run, signer)?;
    Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
}

/// The buyer's account and signer, from `--buyer` and `SPHINX_SIGNER_KEY`.
fn buyer_identity(
    buyer: &str,
) -> Result<(sphinx_near::near_api::types::AccountId, Arc<Signer>), SphinxError> {
    let config_err = |var: &str, why: &str| {
        SphinxError::new(Code::CliConfigMalformed, format!("{var} {why}")).ctx("var", var)
    };
    let key = std::env::var("SPHINX_SIGNER_KEY").map_err(|_| {
        SphinxError::new(Code::CliConfigMissing, "SPHINX_SIGNER_KEY is not set")
            .ctx("var", "SPHINX_SIGNER_KEY")
    })?;
    let signer = Signer::from_secret_key(
        key.parse().map_err(|_| config_err("SPHINX_SIGNER_KEY", "is not an ed25519:... key"))?,
    )
    .map_err(|_| config_err("SPHINX_SIGNER_KEY", "cannot sign"))?;
    let buyer = buyer
        .parse()
        .map_err(|_| SphinxError::new(Code::CliUsage, "--buyer is not an account id"))?;
    Ok((buyer, signer))
}

/// A job read from `contract`, which must belong to `buyer`.
async fn buyers_job(
    contract: &str,
    job_id: u64,
    buyer: &str,
) -> Result<(Escrow, Job), SphinxError> {
    let contract = contract
        .parse()
        .map_err(|_| SphinxError::new(Code::CliUsage, "--contract is not an account id"))?;
    let escrow = Escrow::for_contract(contract);
    let job: Job = escrow
        .view::<Option<Job>>("get_job", serde_json::json!({ "job_id": job_id.to_string() }))
        .await?
        .ok_or_else(|| {
            SphinxError::new(Code::JobNotFound, format!("job {job_id} does not exist"))
        })?;
    if job.buyer != buyer {
        return Err(SphinxError::new(
            Code::CliUsage,
            format!("job {job_id} belongs to {}, not {buyer}", job.buyer),
        ));
    }
    Ok((escrow, job))
}

/// GET `{api}/v1/code/{hash}`, signed as `account` when given.
async fn code_get(hash: &str, api: &str, account: Option<&str>) -> Result<String, SphinxError> {
    let hash: Hash32 = hash
        .parse()
        .map_err(|_| SphinxError::new(Code::CliUsage, "the hash is not 64 hex characters"))?;
    let url = format!("{}/v1/code/{hash}", api.trim_end_matches('/'));
    let mut req = reqwest::Client::new().get(&url);
    if let Some(account) = account {
        let key = std::env::var("SPHINX_SIGNER_KEY").map_err(|_| {
            SphinxError::new(Code::CliConfigMissing, "SPHINX_SIGNER_KEY is not set")
                .ctx("var", "SPHINX_SIGNER_KEY")
        })?;
        let key = key.parse().map_err(|_| {
            SphinxError::new(
                Code::CliConfigMalformed,
                "SPHINX_SIGNER_KEY is not an ed25519:... key",
            )
            .ctx("var", "SPHINX_SIGNER_KEY")
        })?;
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let now_ms = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
        // The nonce only has to differ between requests.
        let nonce = Hash32::digest(format!("{}:{}", now.as_nanos(), std::process::id()).as_bytes());
        let header = sphinx_near::nep413::sign_header(
            account,
            &key,
            &sphinx_near::nep413::code_message(&hash.to_string(), now_ms),
            *nonce.as_bytes(),
        );
        req = req.header(sphinx_near::nep413::HEADER, header);
    }
    let fail = |e: &dyn std::fmt::Display| {
        SphinxError::new(Code::RelayRpcFailed, format!("{url}: {e}")).ctx("url", &url)
    };
    let res = req.send().await.map_err(|e| fail(&e))?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(SphinxError::new(
            Code::ApiCodeNotFound,
            format!("{url} is not stored, or this caller may not read it"),
        )
        .ctx("hash", hash));
    }
    let res = res.error_for_status().map_err(|e| fail(&e))?;
    res.text().await.map_err(|e| fail(&e))
}

fn block_on<T>(
    f: impl std::future::Future<Output = Result<T, SphinxError>>,
) -> Result<T, SphinxError> {
    tokio::runtime::Runtime::new()
        .map_err(|e| SphinxError::new(Code::InternalInvariant, e.to_string()))?
        .block_on(f)
}

fn suite_store(a: &StoreArgs) -> Result<String, SphinxError> {
    let (buyer, signer) = buyer_identity(&a.buyer)?;
    let suite = read(&a.suite)?;
    let reference = read(&a.reference)?;
    block_on(async {
        let (escrow, job) = buyers_job(&a.contract, a.job_id, &a.buyer).await?;
        let commit = job.commit.as_ref().ok_or_else(|| {
            SphinxError::new(
                Code::JobWrongState,
                format!("job {} has no committed suite yet; run commit_suite first", a.job_id),
            )
        })?;
        let (suite_hash, ref_hash) =
            (Hash32::digest(suite.as_bytes()), Hash32::digest(reference.as_bytes()));
        if commit.suite_hash != suite_hash || commit.ref_solution_hash != ref_hash {
            return Err(SphinxError::new(
                Code::CliUsage,
                format!("these files do not match the hashes committed for job {}", a.job_id),
            )
            .ctx("committed_suite_hash", commit.suite_hash)
            .ctx("file_suite_hash", suite_hash)
            .ctx("committed_ref_hash", commit.ref_solution_hash)
            .ctx("file_ref_hash", ref_hash));
        }
        let target = grader_target(&commit.grader).await?;
        let values = secret_values(escrow.contract_id().as_str(), a.job_id, suite, reference);
        let profile = profile_for_job(a.job_id);
        let outcome = Keystore::for_account(escrow.contract_id().as_str())
            .store(
                escrow.network(),
                buyer,
                signer,
                &profile,
                &target.wasm_sha256,
                &target.caller,
                &values,
            )
            .await?;
        Ok(format!(
            "stored {profile} for grader {} run by {}; tx {}",
            &target.wasm_sha256[..12],
            target.caller,
            outcome.tx_hash
        ))
    })
}

fn suite_reveal(a: &RevealArgs) -> Result<String, SphinxError> {
    let suite = read(&a.suite)?;
    block_on(async {
        let contract = a
            .contract
            .parse()
            .map_err(|_| SphinxError::new(Code::CliUsage, "--contract is not an account id"))?;
        let escrow = Escrow::for_contract(contract);
        let job: Job = escrow
            .view::<Option<Job>>("get_job", serde_json::json!({ "job_id": a.job_id.to_string() }))
            .await?
            .ok_or_else(|| {
                SphinxError::new(Code::JobNotFound, format!("job {} does not exist", a.job_id))
            })?;
        let ended = matches!(
            job.status,
            JobStatus::Paid | JobStatus::Rejected | JobStatus::Refunded | JobStatus::Cancelled
        );
        if !ended {
            return Err(SphinxError::new(
                Code::JobWrongState,
                format!(
                    "job {} is {:?}; its tests stay hidden until it ends",
                    a.job_id, job.status
                ),
            ));
        }
        let commit = job.commit.as_ref().ok_or_else(|| {
            SphinxError::new(
                Code::JobWrongState,
                format!("job {} never committed a suite", a.job_id),
            )
        })?;
        let suite_hash = Hash32::digest(suite.as_bytes());
        if commit.suite_hash != suite_hash {
            return Err(SphinxError::new(
                Code::CliUsage,
                format!("this file does not match the suite hash committed for job {}", a.job_id),
            )
            .ctx("committed_suite_hash", commit.suite_hash)
            .ctx("file_suite_hash", suite_hash));
        }
        let url = format!("{}/v1/code", a.api.trim_end_matches('/'));
        let http = reqwest::Client::new();
        http.post(&url)
            .body(suite)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| {
                SphinxError::new(Code::RelayRpcFailed, format!("{url}: {e}")).ctx("url", &url)
            })?;
        // Served at once unless a live job still uses the same suite.
        let public = code_get(&suite_hash.to_string(), &a.api, None).await.is_ok();
        Ok(format!(
            "published job {}'s tests {suite_hash}; {}",
            a.job_id,
            if public {
                "public now"
            } else {
                "stored; public once every job using this suite has ended"
            }
        ))
    })
}

fn suite_delete(a: &DeleteArgs) -> Result<String, SphinxError> {
    let (buyer, signer) = buyer_identity(&a.buyer)?;
    block_on(async {
        let (escrow, job) = buyers_job(&a.contract, a.job_id, &a.buyer).await?;
        let ended = matches!(
            job.status,
            JobStatus::Paid | JobStatus::Rejected | JobStatus::Refunded | JobStatus::Cancelled
        );
        if !ended {
            return Err(SphinxError::new(
                Code::CliSuiteDeleteTooEarly,
                format!("job {} is {:?}; it still needs its hidden suite", a.job_id, job.status),
            ));
        }
        let Some(commit) = job.commit.as_ref() else {
            return Ok(format!("job {} never had a suite; nothing to delete", a.job_id));
        };
        let wasm = commit.grader.wasm_sha256.to_string();
        let profile = profile_for_job(a.job_id);
        let keystore = Keystore::for_account(escrow.contract_id().as_str());
        if !keystore.is_stored(escrow.network(), &buyer, &profile, &wasm).await? {
            return Ok(format!("no hidden suite is stored for job {}", a.job_id));
        }
        let outcome = keystore.delete(escrow.network(), buyer, signer, &profile, &wasm).await?;
        Ok(format!("deleted {profile}; storage deposit refunded; tx {}", outcome.tx_hash))
    })
}

fn explain(text: &str) -> Result<String, SphinxError> {
    let report = parse_panic(text).ok_or_else(|| {
        SphinxError::new(Code::CliUnknownCode, "no Sphinx error code found in the input")
    })?;
    Ok(output::human(&report))
}

#[cfg(feature = "dev-signer")]
mod dev {
    use super::{Code, DevCmd, SphinxError};
    use sphinx_grader::signer::dev::{DevSigner, SEED_VAR};
    use sphinx_grader::signer::GraderSigner;

    pub fn signer() -> Result<DevSigner, SphinxError> {
        let seed = std::env::var(SEED_VAR).map_err(|_| {
            SphinxError::new(Code::CliConfigMissing, format!("{SEED_VAR} is not set"))
                .ctx("var", SEED_VAR)
        })?;
        DevSigner::from_seed_hex(&seed).ok_or_else(|| {
            SphinxError::new(
                Code::CliConfigMalformed,
                format!("{SEED_VAR} must be 64 hex characters"),
            )
            .ctx("var", SEED_VAR)
        })
    }

    pub fn run(cmd: &DevCmd) -> Result<String, SphinxError> {
        match cmd {
            DevCmd::Keygen => {
                let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
                Ok(format!("{SEED_VAR}={}", hex::encode(key.to_bytes())))
            }
            DevCmd::Pubkey => Ok(signer()?.info()?.public_key),
            DevCmd::BoxPubkey => Ok(signer()?.box_secret()?.public_key_hex()),
        }
    }
}

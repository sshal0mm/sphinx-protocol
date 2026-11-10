//! Runs the relayer.
//!
//! Environment (see `.env.example`): `SPHINX_CONTRACT`, `SPHINX_RELAYER_ACCOUNT`,
//! `SPHINX_RELAYER_KEY` (`ed25519:...`), `SPHINX_GRADER_URL`, `SPHINX_GRADER_SHA256`,
//! optional `SPHINX_POLL_SECS` (default 6). `--once` runs a single pass.
//! Submission code is read by hash from the job API at `SPHINX_API` (default
//! `http://127.0.0.1:8787`). `SPHINX_RELAYER_REPO_ROOT` lets `repo:` code URIs
//! resolve inside that directory, for local demos only.
#![allow(clippy::print_stderr, reason = "the relayer logs to stderr")]

use sphinx_near::near_api::Signer;
use sphinx_near::outlayer::{Grader, Limits};
use sphinx_near::Escrow;
use sphinx_protocol::Retry;
use sphinx_relayer::{CodeSource, Outcome, Relayer};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// Attempts per unit of work before the relayer stops retrying it; the
/// agent's bond then becomes reclaimable after the grading window.
const MAX_ATTEMPTS: u32 = 3;
/// Transient failures (RPC hiccups, a nonce race, a busy enclave) tolerated
/// per unit of work before it is given up; they do not use up attempts.
const MAX_TRANSIENT: u32 = 10;
/// Longest one unit of work may take; a slower one is abandoned for this
/// pass so it cannot hold up every other job.
const WORK_TIMEOUT: Duration = Duration::from_mins(10);
/// Consecutive passes that cannot read the chain before the process exits so
/// its supervisor (serve.sh, or the container's restart policy) starts it
/// fresh. Long-lived processes have been seen to lose all RPC endpoints at
/// once while a new process reaches them fine.
const MAX_FAILED_READS: u32 = 20;

fn env(name: &str) -> Result<String, String> {
    std::env::var(name)
        .map_err(|_| format!("error[SPHINX_E_CLI_CONFIG_MISSING]: {name} is not set"))
}

fn build() -> Result<Relayer, String> {
    let contract: sphinx_near::near_api::types::AccountId = env("SPHINX_CONTRACT")?
        .parse()
        .map_err(|_| "SPHINX_CONTRACT is not an account id".to_owned())?;
    let account = env("SPHINX_RELAYER_ACCOUNT")?
        .parse()
        .map_err(|_| "SPHINX_RELAYER_ACCOUNT is not an account id".to_owned())?;
    let key = env("SPHINX_RELAYER_KEY")?
        .parse()
        .map_err(|_| "SPHINX_RELAYER_KEY is not an ed25519:... key".to_owned())?;
    let signer = Signer::from_secret_key(key).map_err(|e| format!("SPHINX_RELAYER_KEY: {e}"))?;
    let outlayer = sphinx_near::hidden_suite::Keystore::for_account(contract.as_str()).contract;
    // Earlier grader builds, for jobs committed before the current release:
    // SPHINX_GRADER_PREVIOUS=<sha256>=<url>,<sha256>=<url>
    let previous_graders = std::env::var("SPHINX_GRADER_PREVIOUS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|pair| pair.trim().split_once('='))
        .map(|(sha, url)| Grader {
            outlayer: outlayer.clone(),
            wasm_url: url.trim().to_owned(),
            wasm_sha256: sha.trim().to_ascii_lowercase(),
            limits: Limits::default(),
        })
        .collect();
    Ok(Relayer {
        escrow: Escrow::for_contract(contract),
        grader: Grader {
            outlayer,
            wasm_url: env("SPHINX_GRADER_URL")?,
            wasm_sha256: env("SPHINX_GRADER_SHA256")?,
            limits: Limits::default(),
        },
        previous_graders,
        account,
        signer,
        code: CodeSource::new(
            &std::env::var("SPHINX_API").unwrap_or_else(|_| "http://127.0.0.1:8787".to_owned()),
            std::env::var("SPHINX_RELAYER_REPO_ROOT").ok().map(std::path::PathBuf::from),
        )?,
    })
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let relayer = match build() {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("{msg}");
            return std::process::ExitCode::from(78);
        }
    };
    let once = std::env::args().any(|a| a == "--once");
    let poll = Duration::from_secs(
        std::env::var("SPHINX_POLL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(6),
    );
    let mut attempts: HashMap<(u64, Option<u32>), u32> = HashMap::new();
    let mut transient: HashMap<(u64, Option<u32>), u32> = HashMap::new();
    // Work waiting on someone else (a buyer who has not stored the suite yet); logged once.
    let mut waiting: HashSet<(u64, Option<u32>)> = HashSet::new();
    eprintln!("sphinx-relayer: {} for {}", relayer.account, relayer.escrow.contract_id());
    let mut failed_reads = 0u32;
    loop {
        match relayer.pending_work().await {
            Err(e) => {
                failed_reads += 1;
                eprintln!("read failed ({failed_reads}/{MAX_FAILED_READS}): {}", e.render_panic());
                if failed_reads >= MAX_FAILED_READS {
                    eprintln!(
                        "cannot read the chain; exiting so the supervisor restarts the relayer"
                    );
                    return std::process::ExitCode::from(75);
                }
            }
            Ok(work) => {
                failed_reads = 0;
                for w in work {
                    let tries = attempts.entry(w.key()).or_insert(0);
                    let hiccups = transient.entry(w.key()).or_insert(0);
                    if *tries >= MAX_ATTEMPTS || *hiccups >= MAX_TRANSIENT {
                        continue;
                    }
                    *tries += 1;
                    let Ok(result) = tokio::time::timeout(WORK_TIMEOUT, relayer.perform(&w)).await
                    else {
                        *tries -= 1;
                        *hiccups += 1;
                        eprintln!(
                            "failed  {w:?}: took longer than {WORK_TIMEOUT:?}; retrying later"
                        );
                        continue;
                    };
                    match result {
                        Ok(Outcome::Posted { tx_hash, summary }) => {
                            eprintln!("posted  {summary}  tx {tx_hash}");
                        }
                        Ok(Outcome::AlreadyDone) => eprintln!("skipped {w:?}: already recorded"),
                        Ok(Outcome::NotNeeded(why)) => eprintln!("skipped {w:?}: {why}"),
                        // A wait costs nothing (it is decided by a view), so it
                        // does not use up an attempt.
                        Err(e) if e.retry_class() == Retry::Wait => {
                            *tries -= 1;
                            if waiting.insert(w.key()) {
                                eprintln!("waiting {w:?}: {}", e.render_panic());
                            }
                        }
                        // Retrying cannot help (the enclave produced no result,
                        // or the contract refused for good): never pay for it again.
                        Err(e) if e.retry_class() == Retry::Never => {
                            *tries = MAX_ATTEMPTS;
                            eprintln!("failed  {w:?} (not retried): {}", e.render_panic());
                        }
                        Err(e) if e.retry_class() == Retry::Transient => {
                            *tries -= 1;
                            *hiccups += 1;
                            eprintln!(
                                "failed  {w:?} (transient {hiccups}/{MAX_TRANSIENT}): {}",
                                e.render_panic()
                            );
                        }
                        Err(e) => eprintln!(
                            "failed  {w:?} (attempt {tries}/{MAX_ATTEMPTS}): {}",
                            e.render_panic()
                        ),
                    }
                }
            }
        }
        if once {
            return std::process::ExitCode::SUCCESS;
        }
        tokio::time::sleep(poll).await;
    }
}

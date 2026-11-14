//! Runs the relayer.
//!
//! Environment (see `.env.example`): `SPHINX_CONTRACT`, `SPHINX_RELAYER_ACCOUNT`,
//! `SPHINX_RELAYER_KEY` (`ed25519:...`), `SPHINX_GRADER_URL`, `SPHINX_GRADER_SHA256`,
//! optional `SPHINX_POLL_SECS` (default 6). `--once` runs a single pass.
//!
//! The relayer reads the chain when there is a reason to: right after a
//! nudge (`POST /nudge` on `SPHINX_NUDGE_ADDR`, which the job API, the
//! describe-a-job server and the connector servers send when they relay a
//! submission, store code or see a job posted), every `SPHINX_POLL_SECS`
//! while it has work or for two minutes after a nudge, when a grading window
//! it knows of closes, and otherwise once every `SPHINX_IDLE_SECS` (default
//! 300) for calls made straight to the contract, which nobody nudges about.
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
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

/// How long after a nudge the relayer keeps looking every poll, so a
/// transaction that lands a moment after the nudge is still seen at once.
const NUDGE_WINDOW: Duration = Duration::from_mins(2);

/// Listens for `POST /nudge` and wakes the relayer. Anything else is a 404.
/// Internal only: the compose network publishes no port.
async fn listen_for_nudges(addr: String, wake: Arc<Notify>) {
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("nudges off: cannot listen on {addr}: {e}");
            return;
        }
    };
    eprintln!("listening for nudges on {addr}");
    loop {
        let Ok((mut socket, _)) = listener.accept().await else { continue };
        let wake = wake.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let n = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut buf))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(0);
            let nudge = buf[..n].starts_with(b"POST /nudge");
            if nudge {
                wake.notify_one();
            }
            let reply: &[u8] = if nudge {
                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let _ = socket.write_all(reply).await;
        });
    }
}

/// Attempts per unit of work before the relayer stops retrying it; the
/// agent's bond then becomes reclaimable after the grading window.
const MAX_ATTEMPTS: u32 = 3;
/// Transient failures (RPC hiccups, a nonce race, a busy enclave) tolerated
/// per unit of work before it is set aside; they do not use up attempts.
const MAX_TRANSIENT: u32 = 10;
/// How long work set aside after transient failures waits before it is
/// tried again, so an outage does not drop it until the process restarts.
const TRANSIENT_COOL_OFF: Duration = Duration::from_mins(30);
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
#[allow(clippy::too_many_lines, reason = "one loop that reads, works and waits")]
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
    let mut set_aside: HashMap<(u64, Option<u32>), Instant> = HashMap::new();
    // Work waiting on someone else (a buyer who has not stored the suite yet); logged once.
    let mut waiting: HashSet<(u64, Option<u32>)> = HashSet::new();
    eprintln!("sphinx-relayer: {} for {}", relayer.account, relayer.escrow.contract_id());
    let idle = Duration::from_secs(
        std::env::var("SPHINX_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300),
    );
    let wake = Arc::new(Notify::new());
    if let Ok(addr) = std::env::var("SPHINX_NUDGE_ADDR") {
        tokio::spawn(listen_for_nudges(addr, wake.clone()));
    }
    // A fresh start looks closely for a while, as after a nudge.
    let mut nudged_at = Some(Instant::now());
    let mut failed_reads = 0u32;
    loop {
        // Whether this pass did something that may lead to more work soon
        // (work waiting on someone else, such as a top-up, does not).
        let mut busy = false;
        let mut due_ns: Option<u64> = None;
        match relayer.pending_work_and_due().await {
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
            Ok((work, due)) => {
                failed_reads = 0;
                due_ns = due;
                for w in work {
                    if set_aside.get(&w.key()).is_some_and(|at| at.elapsed() >= TRANSIENT_COOL_OFF)
                    {
                        set_aside.remove(&w.key());
                        transient.remove(&w.key());
                    }
                    let tries = attempts.entry(w.key()).or_insert(0);
                    let hiccups = transient.entry(w.key()).or_insert(0);
                    if *tries >= MAX_ATTEMPTS {
                        continue;
                    }
                    if *hiccups >= MAX_TRANSIENT {
                        set_aside.entry(w.key()).or_insert_with(Instant::now);
                        continue;
                    }
                    *tries += 1;
                    let Ok(result) = tokio::time::timeout(WORK_TIMEOUT, relayer.perform(&w)).await
                    else {
                        *tries -= 1;
                        *hiccups += 1;
                        busy = true;
                        eprintln!(
                            "failed  {w:?}: took longer than {WORK_TIMEOUT:?}; retrying later"
                        );
                        continue;
                    };
                    match result {
                        Ok(Outcome::Posted { tx_hash, summary }) => {
                            busy = true;
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
                            busy = true;
                            eprintln!(
                                "failed  {w:?} (transient {hiccups}/{MAX_TRANSIENT}): {}",
                                e.render_panic()
                            );
                        }
                        Err(e) => {
                            busy = true;
                            eprintln!(
                                "failed  {w:?} (attempt {tries}/{MAX_ATTEMPTS}): {}",
                                e.render_panic()
                            );
                        }
                    }
                }
            }
        }
        if once {
            return std::process::ExitCode::SUCCESS;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        let due_in = due_ns.map(|at| Duration::from_nanos(at.saturating_sub(now)));
        let recent = nudged_at.is_some_and(|at| at.elapsed() < NUDGE_WINDOW);
        let wait = sphinx_relayer::plan::next_wait(busy, recent, due_in, poll, idle);
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = wake.notified() => nudged_at = Some(Instant::now()),
        }
    }
}

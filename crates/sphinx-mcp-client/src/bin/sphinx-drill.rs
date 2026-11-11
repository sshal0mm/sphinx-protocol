//! The daily drill: one whole job, end to end, on the live service.
//!
//! A drill buyer posts a small job through the Sphinx MCP tools, waits for
//! the enclave to check it, a drill agent submits a correct solution, and
//! the drill waits for the payout, then takes the suite's storage deposit
//! back. Each stage is timed. The result is added to a JSON history (the
//! newest 30 runs) that the site's `/status` page reads.
//!
//! Environment:
//! - `SPHINX_CONTRACT`, `SPHINX_API`: where, passed on to `sphinx-mcp`
//! - `SPHINX_DRILL_BUYER`, `SPHINX_DRILL_BUYER_KEY`: the account that posts
//! - `SPHINX_DRILL_AGENT`, `SPHINX_DRILL_AGENT_KEY`: the account that solves
//! - `SPHINX_DRILL_OUT`: the history file (default `drill.json`)
//! - `SPHINX_MCP_BIN`: the server (default `sphinx-mcp` next to this program)
//! - `SPHINX_NETWORK`: shown on the page (default `testnet`)

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "a command-line tool")]

use serde_json::{json, Value};
use sphinx_mcp_client::{McpClient, ToolFailure};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SPEC: &str = "# Daily drill: sum of digits\n\nWrite sumDigits(n). n is a non-negative integer below 2^53. Return the sum of its decimal digits. Example: sumDigits(1234) returns 10.\n";
const REFERENCE: &str =
    "function sumDigits(n) { let s = 0; for (const c of String(n)) s += c.charCodeAt(0) - 48; return s; }\n";
const SOLUTION: &str =
    "function sumDigits(n) { let t = 0; while (n > 0) { t += n % 10; n = Math.floor(n / 10); } return t; }\n";
/// How long each wait may take before the drill gives up on that stage.
const STAGE_LIMIT: Duration = Duration::from_mins(20);
const POLL: Duration = Duration::from_secs(10);
const HISTORY: usize = 30;

fn suite() -> String {
    let cases: [(u64, u64); 6] =
        [(0, 0), (7, 7), (1234, 10), (99_999, 45), (1_000_000_007, 8), (9_007_199_254_740_991, 76)];
    let tests: Vec<Value> =
        cases.iter().map(|(n, s)| json!({ "args": [n], "expected": s })).collect();
    json!({ "version": 1, "runtime": "js-boa/1", "entry": "sumDigits", "tests": tests }).to_string()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn env(name: &str) -> Result<String, ToolFailure> {
    std::env::var(name).map_err(|_| ToolFailure::new(format!("{name} is not set")))
}

async fn client_for(account: &str, key: &str) -> Result<McpClient, ToolFailure> {
    let bin = std::env::var("SPHINX_MCP_BIN").unwrap_or_else(|_| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("sphinx-mcp").display().to_string()))
            .unwrap_or_else(|| "sphinx-mcp".into())
    });
    McpClient::start(
        &bin,
        &[
            ("SPHINX_ACCOUNT".into(), account.into()),
            ("SPHINX_ACCOUNT_KEY".into(), key.into()),
            ("SPHINX_MAX_SPEND_NEAR".into(), "0.5".into()),
        ],
    )
    .await
}

/// Polls `check` until it returns `Some`, or fails after [`STAGE_LIMIT`].
async fn wait_for<F, Fut>(what: &str, mut check: F) -> Result<Value, ToolFailure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<Value>, ToolFailure>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = check().await? {
            return Ok(v);
        }
        if start.elapsed() > STAGE_LIMIT {
            return Err(ToolFailure::new(format!("{what} did not happen within 20 minutes")));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// One run; `stages` gets each stage's seconds as it completes.
async fn run(stages: &mut Vec<(String, f64)>, ids: &mut Value) -> Result<(), ToolFailure> {
    let buyer = client_for(&env("SPHINX_DRILL_BUYER")?, &env("SPHINX_DRILL_BUYER_KEY")?).await?;
    let agent = client_for(&env("SPHINX_DRILL_AGENT")?, &env("SPHINX_DRILL_AGENT_KEY")?).await?;
    let mut t = Instant::now();
    let mut lap = |name: &str, stages: &mut Vec<(String, f64)>| {
        stages.push((name.to_owned(), (t.elapsed().as_secs_f64() * 10.0).round() / 10.0));
        t = Instant::now();
    };

    let posted = buyer
        .call(
            "sphinx_post_job",
            json!({
                "title": "Daily drill: sum of digits",
                "spec": SPEC, "suite": suite(), "reference": REFERENCE,
                "reward_near": "0.1", "bond_near": "0.01",
                "max_subs_per_agent": 1, "max_subs_total": 2, "deadline_hours": 2,
            }),
        )
        .await?;
    let job = posted["job_id"].as_str().map_or_else(|| posted["job_id"].to_string(), str::to_owned);
    ids["job_id"] = json!(job);
    lap("posted", stages);

    wait_for("the enclave check", || async {
        let v = buyer.call("sphinx_jobs", json!({ "job_id": job })).await?;
        Ok((v["accepting_submissions"] == true).then_some(v))
    })
    .await?;
    lap("open", stages);

    let sub = agent.call("sphinx_submit", json!({ "job_id": job, "code": SOLUTION })).await?;
    let sub_id = sub["sub_id"].clone();
    ids["sub_id"] = sub_id.clone();
    lap("submitted", stages);

    let done = wait_for("the payout", || async {
        let v = agent.call("sphinx_jobs", json!({ "job_id": job, "sub_id": sub_id })).await?;
        let status = v["submission"]["status"].as_str().unwrap_or_default().to_owned();
        Ok(matches!(status.as_str(), "Passed" | "Failed" | "Expired").then_some(v))
    })
    .await?;
    let status = done["submission"]["status"].as_str().unwrap_or_default();
    if status != "Passed" {
        return Err(ToolFailure::new(format!("the drill's correct solution came back {status}")));
    }
    lap("paid", stages);

    // The suite's storage deposit comes back once the job has ended; a
    // failure here does not fail the drill.
    if let Err(e) =
        buyer.call("sphinx_manage_job", json!({ "action": "delete_suite", "job_id": job })).await
    {
        eprintln!("delete_suite: {e}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let out = std::env::var("SPHINX_DRILL_OUT").unwrap_or_else(|_| "drill.json".into());
    let started = now_secs();
    let mut stages = Vec::new();
    let mut ids = json!({});
    let result = run(&mut stages, &mut ids).await;
    let total: f64 = stages.iter().map(|(_, s)| s).sum();
    let entry = json!({
        "network": std::env::var("SPHINX_NETWORK").unwrap_or_else(|_| "testnet".into()),
        "contract": std::env::var("SPHINX_CONTRACT").unwrap_or_default(),
        "started_at": started,
        "finished_at": now_secs(),
        "ok": result.is_ok(),
        "error": result.as_ref().err().map(ToString::to_string),
        "stages": stages.iter().map(|(n, s)| json!({ "name": n, "seconds": s })).collect::<Vec<_>>(),
        "seconds": (total * 10.0).round() / 10.0,
        "job_id": ids["job_id"],
        "sub_id": ids["sub_id"],
    });
    let mut history: Vec<Value> = std::fs::read_to_string(&out)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["runs"].as_array().cloned())
        .unwrap_or_default();
    history.insert(0, entry.clone());
    history.truncate(HISTORY);
    let doc = json!({ "runs": history });
    let tmp = format!("{out}.tmp");
    if let Err(e) = std::fs::write(&tmp, doc.to_string()).and_then(|()| std::fs::rename(&tmp, &out))
    {
        eprintln!("cannot write {out}: {e}");
    }
    println!("{entry}");
    if result.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

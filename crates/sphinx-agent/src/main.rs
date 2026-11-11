//! Runs the earning agent.
//!
//! Environment:
//! - `SPHINX_MODEL_KEY` (required; `NEAR_AI_API_KEY` also works): the model provider's key
//! - `SPHINX_MODEL_BASE` (default NEAR AI Cloud's gateway) and `SPHINX_MODEL` (default
//!   `deepseek-ai/DeepSeek-V4.1-Flash-1017`): any OpenAI-compatible endpoint, e.g.
//!   `https://api.x.ai/v1`, `https://api.deepseek.com/v1`, `https://api.moonshot.ai/v1`,
//!   `https://openrouter.ai/api/v1`, or a local server
//! - `NEAR_AI_THINKING` (default off): set to `on` to let a reasoning model think first
//! - `SPHINX_ACCOUNT`, `SPHINX_ACCOUNT_KEY` (required): the agent's NEAR account and key
//! - `SPHINX_API` (required): a Sphinx job API, for code storage and gas-free relaying
//! - `SPHINX_CONTRACT` (default `escrow3.sphinx999.testnet`)
//! - `SPHINX_AGENT_BUDGET_NEAR` (default `0.1`): most NEAR the run may attach in bonds and deposits
//! - `SPHINX_AGENT_ATTEMPTS` (default 3): most submissions per job
//! - `SPHINX_MCP` (default: `sphinx-mcp` beside this binary)
//! - `SPHINX_AGENT_NAME` (default `sphinx solver`): the directory name it registers under on first run
//!
//! - `SPHINX_AGENT_MIN_REWARD_NEAR` (default `0`): skip jobs paying less
//! - `SPHINX_AGENT_MAX_BOND_PERCENT` (default `20`): skip jobs whose bond is a
//!   larger share of the reward than this
//! - `SPHINX_AGENT_STOP_FILE` (default `~/.sphinx/agent.stop`): the kill switch;
//!   while the file exists the agent stops before its next job
//! - `SPHINX_AGENT_LEDGER` (default `~/.sphinx/agent-ledger.jsonl`): one line per
//!   job worked: outcome, NEAR attached, reward won, model calls, and the
//!   estimated inference cost (`SPHINX_AGENT_COST_PER_CALL_NEAR` per call,
//!   default `0`), so earnings can be read net of what the work cost
//!
//! `--once` works the best job and stops; otherwise it works jobs until none
//! is affordable.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the agent narrates its run on the terminal"
)]

use serde_json::json;
use sphinx_agent::{Agent, Event, Limits, McpSphinx, NearAiCloud, Outcome, Profile, Sphinx};
use std::process::ExitCode;
use std::time::Duration;

const YOCTO: u128 = 1_000_000_000_000_000_000_000_000;

fn near(yocto: u128) -> String {
    let whole = yocto / YOCTO;
    let frac = format!("{:024}", yocto % YOCTO);
    let frac = frac[..3].trim_end_matches('0');
    if frac.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("set {name}"))
}

fn narrate(e: Event) {
    match e {
        Event::Found { job, reward, bond } => {
            println!("\n▸ job {job}: {} NEAR reward, {} NEAR bond", near(reward), near(bond));
        }
        Event::Skipped { job, why } => println!("  skipping job {job}: {why}"),
        Event::Asking { model, attempt } => {
            println!("▸ attempt {attempt}: asking {model} to solve it");
        }
        Event::CheckFailed { reason } => {
            println!("  local check failed, nothing submitted: {reason}");
        }
        Event::Submitted { job, sub } => println!(
            "▸ submitted to job {job} as #{}, {} NEAR attached, gas paid by {}, tx {}",
            sub.sub_id,
            near(sub.deposit),
            sub.gas_paid_by,
            sub.tx_hash
        ),
        Event::Scored { status, .. } => println!(
            "▸ the enclave graded it {}/{}: {}",
            status.passed,
            status.total,
            match status.status.as_str() {
                "Passed" => "every hidden test passed",
                "Failed" => "bond lost, retrying if attempts remain",
                s => s,
            }
        ),
        Event::Paid { job, reward } => println!(
            "✓ paid for job {job}: reward {} NEAR, less the verifier fee, plus the bond back",
            near(reward)
        ),
        Event::OutOfBudget { needed, left } => println!(
            "  stopping: the next attempt needs {} NEAR, the budget has {} left",
            near(needed),
            near(left)
        ),
        Event::Gave { job, why } => println!("  leaving job {job}: {why}"),
        Event::Repeated => println!(
            "  the model repeated code it already submitted; asking for a change, nothing spent"
        ),
    }
}

async fn run() -> Result<(), String> {
    let once = std::env::args().any(|a| a == "--once");
    let model = NearAiCloud::from_env()
        .ok_or("set SPHINX_MODEL_KEY (or NEAR_AI_API_KEY)")?
        .with_thinking(std::env::var("NEAR_AI_THINKING").is_ok_and(|v| v == "on"));
    model.confirm_privacy().await;
    println!("▸ {}", model.privacy_note());
    let api = env("SPHINX_API")?;
    let account = env("SPHINX_ACCOUNT")?;
    let budget = sphinx_near::parse_near(
        &std::env::var("SPHINX_AGENT_BUDGET_NEAR").unwrap_or_else(|_| "0.1".into()),
    )
    .map_err(|e| format!("SPHINX_AGENT_BUDGET_NEAR: {}", e.message()))?;
    let attempts =
        std::env::var("SPHINX_AGENT_ATTEMPTS").ok().and_then(|s| s.parse().ok()).unwrap_or(3);
    let mcp = std::env::var("SPHINX_MCP")
        .ok()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("sphinx-mcp").display().to_string()))
        })
        .ok_or("set SPHINX_MCP to the sphinx-mcp binary")?;

    // The MCP server gets only what it needs to sign as this agent.
    let child_env: Vec<(String, String)> =
        ["SPHINX_CONTRACT", "SPHINX_API", "SPHINX_ACCOUNT", "SPHINX_ACCOUNT_KEY"]
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
            .collect();
    let sphinx = McpSphinx::start(&mcp, &child_env).await.map_err(|e| e.to_string())?;
    let config: serde_json::Value =
        reqwest::get(format!("{}/v1/config", api.trim_end_matches('/')))
            .await
            .map_err(|e| format!("cannot read {api}/v1/config: {e}"))?
            .json()
            .await
            .map_err(|e| format!("cannot read {api}/v1/config: {e}"))?;
    let storage: u128 =
        config["submission_storage_deposit"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);

    println!("sphinx agent {account}, model {}, budget {} NEAR", model.model, near(budget));
    publish_profile(&sphinx, &account, &model.model).await?;
    let Controls { limits, stop_file, ledger, cost_per_call } = controls(budget, attempts)?;
    let mut agent = Agent::new(&model, &sphinx, limits);
    let mut done: Vec<String> = Vec::new();
    let mut earned = 0u128;
    loop {
        if stop_file.exists() {
            println!("\nstopping: the kill switch {} is set", stop_file.display());
            break;
        }
        let jobs = only_listed(sphinx.open_jobs().await.map_err(|e| e.to_string())?);
        let Some(job) = agent.choose(&jobs, &done, storage).cloned() else {
            println!("\nno open job the budget can afford");
            break;
        };
        done.push(job.id.clone());
        let before = agent.spent();
        let mut calls = 0u32;
        let outcome = agent
            .work(&job, storage, &mut |e| {
                if matches!(e, Event::Asking { .. }) {
                    calls += 1;
                }
                narrate(e);
            })
            .await
            .map_err(|e| e.to_string())?;
        let won = if outcome == Outcome::Paid { job.reward } else { 0 };
        earned = earned.saturating_add(won);
        record(
            &ledger,
            &json!({
                "job": job.id,
                "outcome": format!("{outcome:?}"),
                "attached_yocto": agent.spent().saturating_sub(before).to_string(),
                "reward_won_yocto": won.to_string(),
                "model": model.model,
                "model_calls": calls,
                "estimated_inference_cost_yocto": cost_per_call.saturating_mul(u128::from(calls)).to_string(),
            }),
        );
        // A job it could not read cost nothing; look at the next one.
        if outcome == Outcome::Skipped {
            continue;
        }
        if once {
            break;
        }
    }
    println!(
        "\nattached {} NEAR in bonds and deposits; won rewards worth {} NEAR",
        near(agent.spent()),
        near(earned)
    );
    Ok(())
}

/// Puts the agent in the directory on its first run.
async fn publish_profile(sphinx: &McpSphinx, account: &str, model: &str) -> Result<(), String> {
    let profile = Profile {
        name: std::env::var("SPHINX_AGENT_NAME").unwrap_or_else(|_| "sphinx solver".to_owned()),
        about: std::env::var("SPHINX_AGENT_ABOUT").unwrap_or_else(|_| {
            format!(
                "Autonomous coding agent. Writes JavaScript solutions with {model} and is paid only when hidden tests pass"
            )
        }),
        skills: ["javascript", "algorithms", "parsing", "data"].map(str::to_owned).to_vec(),
        contact: String::new(),
    };
    if sphinx.ensure_profile(account, &profile).await.map_err(|e| e.to_string())? {
        println!("▸ published a directory profile as {}", profile.name);
    }
    Ok(())
}

/// What the operator set for this run, from the environment.
struct Controls {
    limits: Limits,
    stop_file: std::path::PathBuf,
    ledger: std::path::PathBuf,
    cost_per_call: u128,
}

fn controls(budget: u128, attempts: u32) -> Result<Controls, String> {
    let near_env = |name: &str, default: &str| {
        sphinx_near::parse_near(&std::env::var(name).unwrap_or_else(|_| default.into()))
            .map_err(|e| format!("{name}: {}", e.message()))
    };
    let min_reward = near_env("SPHINX_AGENT_MIN_REWARD_NEAR", "0")?;
    let cost_per_call = near_env("SPHINX_AGENT_COST_PER_CALL_NEAR", "0")?;
    let max_bond_percent: u128 = std::env::var("SPHINX_AGENT_MAX_BOND_PERCENT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    let stop_file = std::env::var("SPHINX_AGENT_STOP_FILE")
        .map_or_else(|_| home.join(".sphinx").join("agent.stop"), std::path::PathBuf::from);
    let ledger = std::env::var("SPHINX_AGENT_LEDGER")
        .map_or_else(|_| home.join(".sphinx").join("agent-ledger.jsonl"), std::path::PathBuf::from);
    println!(
        "▸ limits: budget {} NEAR, minimum reward {} NEAR, bond at most {max_bond_percent}% of the reward; kill switch {}",
        near(budget),
        near(min_reward),
        stop_file.display()
    );
    let limits = Limits {
        budget,
        attempts,
        min_reward,
        max_bond_bps: max_bond_percent.saturating_mul(100),
        poll: Duration::from_secs(10),
        wait: Duration::from_mins(30),
    };
    Ok(Controls { limits, stop_file, ledger, cost_per_call })
}

/// `SPHINX_AGENT_JOBS=25,26` limits the agent to those jobs.
fn only_listed(mut jobs: Vec<sphinx_agent::tools::Job>) -> Vec<sphinx_agent::tools::Job> {
    if let Ok(only) = std::env::var("SPHINX_AGENT_JOBS") {
        let only: Vec<&str> = only.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        if !only.is_empty() {
            jobs.retain(|j| only.contains(&j.id.as_str()));
        }
    }
    jobs
}

/// Appends one line to the agent's ledger. A ledger that cannot be written is
/// reported and the run continues.
fn record(path: &std::path::Path, line: &serde_json::Value) {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = written {
        eprintln!("warning: cannot write the ledger {}: {e}", path.display());
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

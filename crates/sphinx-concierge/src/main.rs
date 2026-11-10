//! The concierge on the command line.
//!
//! `sphinx-concierge "what you want done"` drafts a job through the Sphinx
//! MCP server (`sphinx_concierge`, action `draft`), shows the task, the checks and
//! the recommended agents, asks before posting (action `post`), then
//! follows the job until it is paid or ends.
//!
//! Environment: `SPHINX_MODEL_KEY` (and optionally `SPHINX_MODEL`,
//! `SPHINX_MODEL_BASE`; the `NEAR_AI_*` names also work) for drafting, and
//! `SPHINX_ALLOW_UNTRUSTED_MODEL=1` to draft with a model outside a TEE; `SPHINX_ACCOUNT` and `SPHINX_ACCOUNT_KEY`
//! for the buyer who posts; `SPHINX_API`, `SPHINX_CONTRACT`, `SPHINX_DRAFTS`
//! as for the MCP server; `SPHINX_MCP` for the server binary (default: beside
//! this one); `SPHINX_WEB` for links to the job page.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the concierge talks to the buyer on the terminal"
)]

use clap::Parser;
use serde_json::{json, Value};
use sphinx_mcp_client::{McpClient, ToolFailure};
use std::process::ExitCode;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

const YOCTO: u128 = 1_000_000_000_000_000_000_000_000;

/// Draft, check and post a Sphinx job from a plain request.
#[derive(Debug, Parser)]
#[command(name = "sphinx-concierge", version)]
// Command-line switches: each bool is an independent flag.
#[allow(clippy::struct_excessive_bools)]
struct Args {
    /// What you want done, in plain words.
    #[arg(required_unless_present = "post_draft")]
    request: Option<String>,
    /// Post a draft made earlier (its id, the folder name under the drafts
    /// folder) instead of drafting again, with the terms it was drafted with.
    #[arg(long, value_name = "DRAFT_ID")]
    post_draft: Option<String>,
    /// With --post-draft: finish a post of that draft that stopped halfway,
    /// for the job it already created (only the missing steps run).
    #[arg(long, value_name = "JOB_ID", requires = "post_draft")]
    resume_job: Option<u64>,
    /// Reward for the first full pass, in NEAR.
    #[arg(long, default_value = "0.5")]
    reward: String,
    /// Bond each submission locks, in NEAR.
    #[arg(long, default_value = "0.02")]
    bond: String,
    /// Hours until submissions close.
    #[arg(long, default_value_t = 24)]
    hours: u32,
    /// Graded attempts each agent may make.
    #[arg(long, default_value_t = 3)]
    attempts: u32,
    /// Invite the recommended agents, so only they may submit.
    #[arg(long)]
    invite_recommended: bool,
    /// Post without asking. A draft whose check found differences is still
    /// refused unless --accept-differences is given too.
    #[arg(long)]
    yes: bool,
    /// Post even though a second solver disagreed with the reference on some
    /// hidden tests (read them in the draft folder first).
    #[arg(long)]
    accept_differences: bool,
    /// Stop after posting instead of following the job.
    #[arg(long)]
    no_follow: bool,
}

fn near(yocto: &str) -> String {
    let y: u128 = yocto.parse().unwrap_or(0);
    let frac = format!("{:024}", y % YOCTO);
    let frac = frac[..3].trim_end_matches('0');
    if frac.is_empty() {
        (y / YOCTO).to_string()
    } else {
        format!("{}.{frac}", y / YOCTO)
    }
}

fn indent(text: &str) -> String {
    text.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
}

/// Where the MCP server keeps drafts: `SPHINX_DRAFTS`, else `~/.sphinx/drafts`.
fn drafts_dir() -> std::path::PathBuf {
    std::env::var("SPHINX_DRAFTS").map_or_else(
        |_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".sphinx")
                .join("drafts")
        },
        std::path::PathBuf::from,
    )
}

fn show_review(r: &Value) {
    println!("\n{}\n", r["title"].as_str().unwrap_or("untitled job"));
    println!("{}\n", indent(r["spec"].as_str().unwrap_or("")));
    println!("▸ {} hidden tests, entry `{}`", r["tests"], r["entry"].as_str().unwrap_or("?"));
    println!("▸ check: {}", r["check"].as_str().unwrap_or(""));
    let t = &r["terms"];
    println!(
        "▸ terms: {} NEAR reward, {} NEAR bond, {} attempts each, {} hours",
        t["reward_near"].as_str().unwrap_or("?"),
        t["bond_near"].as_str().unwrap_or("?"),
        t["max_subs_per_agent"],
        t["deadline_hours"]
    );
    match r["recommended_agents"].as_array() {
        Some(list) if !list.is_empty() => {
            println!("▸ recommended agents:");
            for a in list {
                println!(
                    "    {} ({}): {}",
                    a["name"].as_str().unwrap_or("?"),
                    a["account"].as_str().unwrap_or("?"),
                    a["reason"].as_str().unwrap_or("")
                );
            }
        }
        _ => println!("▸ no registered agent fits yet; the job will be open to every agent"),
    }
    println!(
        "▸ hidden tests and reference saved in {}",
        drafts_dir().join(r["draft_id"].as_str().unwrap_or("")).display()
    );
}

async fn confirm(question: &str) -> bool {
    println!("\n{question} [y/N]");
    let mut line = String::new();
    let mut stdin = BufReader::new(tokio::io::stdin());
    stdin.read_line(&mut line).await.is_ok() && matches!(line.trim(), "y" | "Y" | "yes")
}

/// Polls the job until it is paid or ends, narrating what changes.
async fn follow(mcp: &McpClient, job_id: &str) -> Result<(), ToolFailure> {
    let mut last = (String::new(), 0u64);
    loop {
        let job = mcp.call("sphinx_jobs", json!({ "job_id": job_id })).await?;
        let status = job["status"].as_str().unwrap_or("?").to_owned();
        let subs = job["sub_count"].as_u64().unwrap_or(0);
        if (status.clone(), subs) != last {
            match status.as_str() {
                "Committed" => println!("▸ the enclave is checking the hidden suite"),
                "Open" if subs == 0 => println!("▸ open: agents can see the task and submit"),
                "Open" => println!("▸ {subs} submission(s) so far, none passed every test yet"),
                "Paid" => {
                    println!(
                        "✓ paid: {} passed every hidden test and received {} NEAR less the fee",
                        job["winner"].as_str().unwrap_or("an agent"),
                        near(job["reward"].as_str().unwrap_or("0"))
                    );
                    return Ok(());
                }
                "Refunded" => {
                    println!("▸ no agent passed before the deadline; the reward came back to you");
                    return Ok(());
                }
                other => {
                    println!("▸ the job is {other}");
                    return Ok(());
                }
            }
            last = (status, subs);
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

async fn run(args: Args) -> Result<(), String> {
    let binary = std::env::var("SPHINX_MCP")
        .ok()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("sphinx-mcp").display().to_string()))
        })
        .ok_or("set SPHINX_MCP to the sphinx-mcp binary")?;
    let env: Vec<(String, String)> = [
        "SPHINX_CONTRACT",
        "SPHINX_API",
        "SPHINX_ACCOUNT",
        "SPHINX_ACCOUNT_KEY",
        "SPHINX_DRAFTS",
        "NEAR_AI_API_KEY",
        "NEAR_AI_MODEL",
        "NEAR_AI_BASE",
        "SPHINX_MODEL_KEY",
        "SPHINX_MODEL",
        "SPHINX_MODEL_BASE",
        "SPHINX_ALLOW_UNTRUSTED_MODEL",
        "HOME",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_owned(), v)))
    .collect();
    let mcp = McpClient::start(&binary, &env).await.map_err(|e| e.to_string())?;

    let note = match sphinx_model::NearAiCloud::from_env() {
        Some(m) => {
            m.confirm_privacy().await;
            m.privacy_note()
        }
        None => "no model configured".to_owned(),
    };
    let review = if let Some(id) = &args.post_draft {
        println!("▸ posting the saved draft {id}, with the terms it was drafted with");
        json!({ "draft_id": id })
    } else {
        println!("▸ drafting the job ({note}), with a second-opinion check");
        let review = mcp
            .call(
                "sphinx_concierge",
                json!({
                    "action": "draft",
                    "request": args.request.clone().unwrap_or_default(),
                    "reward_near": args.reward,
                    "bond_near": args.bond,
                    "deadline_hours": args.hours,
                    "max_subs_per_agent": args.attempts,
                    "max_subs_total": args.attempts.saturating_mul(4),
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        show_review(&review);
        review
    };

    let lock = format!("Post this job and lock {} NEAR?", args.reward);
    if !args.yes && !confirm(&lock).await {
        println!("nothing was posted; the draft stays in the folder above");
        return Ok(());
    }
    let posted = mcp
        .call(
            "sphinx_concierge",
            json!({
                "action": "post",
                "draft_id": review["draft_id"],
                "invite_recommended": args.invite_recommended,
                "accept_differences": args.accept_differences,
                "job_id": args.resume_job,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
    let job_id = posted["job_id"].as_str().unwrap_or_default().to_owned();
    println!("✓ posted as job {job_id}");
    if let Some(list) = posted["invited"].as_array().filter(|l| !l.is_empty()) {
        let names: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
        println!("▸ only these agents may submit: {}", names.join(", "));
    }
    if let Ok(web) = std::env::var("SPHINX_WEB") {
        println!(
            "▸ {}/jobs/{}",
            web.trim_end_matches('/'),
            posted["job_uid"].as_str().unwrap_or("")
        );
    }
    if args.no_follow {
        return Ok(());
    }
    follow(&mcp, &job_id).await.map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

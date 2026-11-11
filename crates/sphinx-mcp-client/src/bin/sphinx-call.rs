//! Calls one Sphinx MCP tool from a shell and prints its JSON reply.
//!
//!     sphinx-call <tool> ['<json arguments>']
//!     sphinx-call --list
//!
//! With `SPHINX_CONNECTOR_URL` set it talks to that connector over HTTP;
//! otherwise it starts `sphinx-mcp` (next to this program, or
//! `SPHINX_MCP_BIN`) with this process's environment, so `SPHINX_ACCOUNT`,
//! `SPHINX_ACCOUNT_KEY`, `SPHINX_CONTRACT` and `SPHINX_API` choose who it
//! acts as and where. Exits 1 when the tool fails.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line tool: its reply is its output"
)]

use serde_json::{json, Value};
use sphinx_mcp_client::McpClient;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(tool) = args.first() else {
        eprintln!("usage: sphinx-call <tool> ['<json arguments>'] | --list");
        return std::process::ExitCode::from(2);
    };
    let client = match std::env::var("SPHINX_CONNECTOR_URL") {
        Ok(url) if !url.is_empty() => McpClient::connect(&url).await,
        _ => {
            let bin = std::env::var("SPHINX_MCP_BIN").unwrap_or_else(|_| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("sphinx-mcp").display().to_string()))
                    .unwrap_or_else(|| "sphinx-mcp".into())
            });
            McpClient::start(&bin, &[]).await
        }
    };
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            println!("{}", json!({ "ok": false, "error": { "message": e.to_string() } }));
            return std::process::ExitCode::FAILURE;
        }
    };
    if tool == "--list" {
        return match client.tool_names().await {
            Ok(names) => {
                println!("{}", json!(names));
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                println!("{}", json!({ "ok": false, "error": { "message": e.to_string() } }));
                std::process::ExitCode::FAILURE
            }
        };
    }
    let input: Value = match args.get(1).map(|a| serde_json::from_str(a)) {
        None => json!({}),
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            println!(
                "{}",
                json!({ "ok": false, "error": { "message": format!("arguments are not JSON: {e}") } })
            );
            return std::process::ExitCode::from(2);
        }
    };
    match client.call(tool, input).await {
        Ok(result) => {
            println!("{}", json!({ "ok": true, "result": result }));
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            println!(
                "{}",
                json!({
                    "ok": false,
                    "error": e.report.unwrap_or_else(|| json!({ "code": e.code, "message": e.message }))
                })
            );
            std::process::ExitCode::FAILURE
        }
    }
}

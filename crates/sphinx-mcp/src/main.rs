//! Runs the Sphinx MCP server over stdio, or over streamable HTTP at `/mcp`
//! with `--http <addr>` (for example `--http 127.0.0.1:8791`). The HTTP mode
//! accepts only loopback hosts unless `SPHINX_MCP_HOSTS` lists others (for a
//! tunnel or reverse proxy in front of it), and with `SPHINX_MCP_TOKEN` set
//! it serves at `/mcp/<token>` instead, so only holders of the full URL can
//! use it. Both are for a single-user test connector; every caller signs as
//! the one configured account, within its spending cap.
//!
//! Environment:
//! - `SPHINX_CONTRACT` (default `escrow3.sphinx999.testnet`)
//! - `SPHINX_API` (optional): job API base URL for code storage and gasless relaying
//! - `SPHINX_ACCOUNT` + `SPHINX_ACCOUNT_KEY` (optional, `ed25519:...`): needed by
//!   tools that sign (submit, reclaim, post a job, delete a suite). The key never leaves this process.
//! - `NEAR_AI_API_KEY` (optional): lets the concierge tools draft jobs on NEAR AI Cloud;
//!   `NEAR_AI_MODEL` and `NEAR_AI_BASE` choose the model and gateway as for the agent
//! - `SPHINX_DRAFTS` (default `~/.sphinx/drafts`): where concierge drafts and their hidden suites are kept
//! - `SPHINX_MAX_SPEND_NEAR` (default `1`): most NEAR this server attaches in deposits
//!   (rewards, bonds, storage) before it refuses further writes
//!
//! `--read-only` serves only the tools that read (`sphinx_jobs`, `sphinx_agents`, `sphinx_check_code`).
//! It refuses to start if any signing or model key is set, so it is safe to
//! put behind a public URL.
#![allow(clippy::print_stderr, reason = "stdout carries the MCP protocol; logs go to stderr")]

use rmcp::ServiceExt;
use sphinx_mcp::{Identity, Settings, SphinxMcp};
use sphinx_near::near_api::Signer;
use sphinx_near::Escrow;

fn identity() -> Result<Option<Identity>, String> {
    let (Ok(account), Ok(key)) =
        (std::env::var("SPHINX_ACCOUNT"), std::env::var("SPHINX_ACCOUNT_KEY"))
    else {
        return Ok(None);
    };
    let account = account.parse().map_err(|_| "SPHINX_ACCOUNT is not an account id".to_owned())?;
    let secret: sphinx_near::near_api::types::crypto::secret_key::SecretKey =
        key.parse().map_err(|_| "SPHINX_ACCOUNT_KEY is not an ed25519:... key".to_owned())?;
    let signer =
        Signer::from_secret_key(secret.clone()).map_err(|e| format!("SPHINX_ACCOUNT_KEY: {e}"))?;
    Ok(Some(Identity { account, signer, key: secret }))
}

/// The address after `--http`, if given.
fn http_addr() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == "--http").and_then(|i| args.get(i + 1).cloned())
}

/// Serves `server` over streamable HTTP at `/mcp` on `addr`. Every session
/// shares one server, so the spending cap covers all of them.
async fn serve_http(server: SphinxMcp, addr: &str) -> std::process::ExitCode {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };
    let mut config = StreamableHttpServerConfig::default();
    if let Ok(hosts) = std::env::var("SPHINX_MCP_HOSTS") {
        let extra: Vec<String> =
            hosts.split(',').map(str::trim).filter(|h| !h.is_empty()).map(str::to_owned).collect();
        config = config.with_allowed_hosts(
            ["localhost", "127.0.0.1", "::1"].map(str::to_owned).into_iter().chain(extra),
        );
    }
    let path = match std::env::var("SPHINX_MCP_TOKEN") {
        Ok(token) if token.len() >= 32 && token.bytes().all(|b| b.is_ascii_alphanumeric()) => {
            format!("/mcp/{token}")
        }
        Ok(_) => {
            eprintln!("error: SPHINX_MCP_TOKEN must be at least 32 letters and digits");
            return std::process::ExitCode::from(78);
        }
        Err(_) => "/mcp".to_owned(),
    };
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        std::sync::Arc::new(LocalSessionManager::default()),
        config,
    );
    let app = axum::Router::new()
        .nest_service(&path, service)
        .layer(axum::middleware::from_fn(send_browsers_to_docs));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot listen on {addr}: {e}");
            return std::process::ExitCode::from(75);
        }
    };
    eprintln!(
        "sphinx-mcp serving MCP at http://{addr}{}",
        if path == "/mcp" { "/mcp" } else { "/mcp/<token>" }
    );
    match axum::serve(listener, app).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: server stopped: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// A browser opening the MCP address (a plain GET that does not ask for an
/// event stream) goes to the page explaining how to connect, instead of
/// the protocol's bare "Not Acceptable" reply. MCP clients are unaffected.
async fn send_browsers_to_docs(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if is_browser_visit(&req) {
        return axum::response::IntoResponse::into_response(axum::response::Redirect::to(
            "/docs/connecting-your-agent",
        ));
    }
    next.run(req).await
}

fn is_browser_visit(req: &axum::extract::Request) -> bool {
    let accept = req
        .headers()
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    req.method() == axum::http::Method::GET
        && accept.contains("text/html")
        && !accept.contains("text/event-stream")
}

/// The directory after `--connect`, if given: run the self-serve connect
/// server (see `sphinx_mcp::connect`) instead of a single-account server.
fn connect_dir() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == "--connect").and_then(|i| args.get(i + 1).cloned())
}

/// Serves self-serve connectors for `SPHINX_CONTRACT` at `addr`, keeping
/// their limited keys in `dir`.
async fn serve_connect(dir: String, addr: &str) -> std::process::ExitCode {
    let contract =
        std::env::var("SPHINX_CONTRACT").unwrap_or_else(|_| "escrow3.sphinx999.testnet".to_owned());
    let Ok(contract) = contract.parse() else {
        eprintln!("error[SPHINX_E_CLI_CONFIG_MALFORMED]: SPHINX_CONTRACT is not an account id");
        return std::process::ExitCode::from(78);
    };
    let hosts: Vec<String> = std::env::var("SPHINX_MCP_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_owned)
        .collect();
    let public =
        std::env::var("SPHINX_CONNECT_PUBLIC").unwrap_or_else(|_| format!("http://{addr}"));
    let connect = match sphinx_mcp::connect::Connect::new(
        contract,
        std::env::var("SPHINX_API").ok(),
        &public,
        hosts,
        std::path::Path::new(&dir),
    ) {
        Ok(c) => std::sync::Arc::new(c),
        Err(e) => {
            eprintln!("error[SPHINX_E_CLI_CONFIG_MALFORMED]: {e}");
            return std::process::ExitCode::from(78);
        }
    };
    let app = connect.router().layer(axum::middleware::from_fn(send_browsers_to_docs));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot listen on {addr}: {e}");
            return std::process::ExitCode::from(75);
        }
    };
    eprintln!("sphinx-mcp serving self-serve connectors at http://{addr}");
    match axum::serve(listener, app).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: server stopped: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if let (Some(dir), Some(addr)) = (connect_dir(), http_addr()) {
        return serve_connect(dir, &addr).await;
    }
    let read_only = std::env::args().any(|a| a == "--read-only");
    if read_only {
        let set: Vec<&str> = ["SPHINX_ACCOUNT_KEY", "SPHINX_MODEL_KEY", "NEAR_AI_API_KEY"]
            .into_iter()
            .filter(|v| std::env::var(v).is_ok_and(|x| !x.is_empty()))
            .collect();
        if !set.is_empty() {
            eprintln!(
                "error[SPHINX_E_CLI_CONFIG_MALFORMED]: --read-only refuses to start while {} is set; a public server must hold no keys",
                set.join(", ")
            );
            return std::process::ExitCode::from(78);
        }
    }
    let contract =
        std::env::var("SPHINX_CONTRACT").unwrap_or_else(|_| "escrow3.sphinx999.testnet".to_owned());
    let Ok(contract) = contract.parse() else {
        eprintln!("error[SPHINX_E_CLI_CONFIG_MALFORMED]: SPHINX_CONTRACT is not an account id");
        return std::process::ExitCode::from(78);
    };
    let identity = match identity() {
        Ok(i) => i,
        Err(msg) => {
            eprintln!("error[SPHINX_E_CLI_CONFIG_MALFORMED]: {msg}");
            return std::process::ExitCode::from(78);
        }
    };
    let model = sphinx_model::NearAiCloud::from_env();
    if let Some(m) = &model {
        m.confirm_privacy().await;
    }
    let settings = Settings {
        escrow: Escrow::for_contract(contract),
        api: std::env::var("SPHINX_API").ok(),
        identity,
        model,
        max_spend: match sphinx_near::parse_near(
            &std::env::var("SPHINX_MAX_SPEND_NEAR").unwrap_or_else(|_| "1".into()),
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error[{}]: SPHINX_MAX_SPEND_NEAR: {}", e.code(), e.message());
                return std::process::ExitCode::from(78);
            }
        },
        pay_from_balance: std::env::var("SPHINX_PAY_FROM_BALANCE").is_ok_and(|v| v == "1"),
        drafts: std::env::var("SPHINX_DRAFTS").map_or_else(
            |_| {
                std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                    .join(".sphinx")
                    .join("drafts")
            },
            std::path::PathBuf::from,
        ),
    };
    let server = if read_only { SphinxMcp::read_only(settings) } else { SphinxMcp::new(settings) };
    if let Some(addr) = http_addr() {
        return serve_http(server, &addr).await;
    }
    match server.serve(rmcp::transport::stdio()).await {
        Ok(running) => {
            let _ = running.waiting().await;
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: MCP server failed to start: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

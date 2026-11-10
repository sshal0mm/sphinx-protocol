//! A client for the Sphinx MCP server.
//!
//! Sphinx's own programs (the earning agent, the concierge) use Sphinx
//! exactly as any MCP client would: they start `sphinx-mcp` as a child
//! process and call its tools. Every tool answers `{"ok": true, "result": …}`
//! or `{"ok": false, "error": <error report>}`.

use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Map, Value};

/// A tool call that did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
    /// The `SPHINX_E_*` code, when the failure came from Sphinx.
    pub code: Option<String>,
    pub message: String,
}

impl ToolFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self { code: None, message: message.into() }
    }
}

impl std::fmt::Display for ToolFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ToolFailure {}

/// A running `sphinx-mcp` and the connection to it.
pub struct McpClient {
    service: RunningService<RoleClient, ()>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient").finish_non_exhaustive()
    }
}

impl McpClient {
    /// Starts `sphinx-mcp` at `binary` with exactly `env` added to its
    /// environment, and connects to it.
    ///
    /// # Errors
    /// When the server cannot be started or does not complete the handshake.
    pub async fn start(binary: &str, env: &[(String, String)]) -> Result<Self, ToolFailure> {
        let mut command = tokio::process::Command::new(binary);
        command.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let transport = TokioChildProcess::new(command)
            .map_err(|e| ToolFailure::new(format!("cannot start {binary}: {e}")))?;
        let service =
            ().serve(transport)
                .await
                .map_err(|e| ToolFailure::new(format!("MCP handshake failed: {e}")))?;
        Ok(Self { service })
    }

    /// Calls `tool` with `args` and returns its `result`.
    ///
    /// # Errors
    /// The tool's error report as a [`ToolFailure`], or a transport failure.
    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, ToolFailure> {
        let arguments: Map<String, Value> = match args {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let out = self
            .service
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(arguments))
            .await
            .map_err(|e| ToolFailure::new(format!("{tool} failed: {e}")))?;
        let text = out
            .content
            .iter()
            .find_map(|c| c.as_text().map(|t| t.text.clone()))
            .ok_or_else(|| ToolFailure::new(format!("{tool} returned no text")))?;
        parse_reply(tool, &text)
    }
}

/// Reads a tool's `{ok, result | error}` reply.
fn parse_reply(tool: &str, text: &str) -> Result<Value, ToolFailure> {
    let reply: Value = serde_json::from_str(text)
        .map_err(|e| ToolFailure::new(format!("{tool} reply is not JSON: {e}")))?;
    if reply["ok"] == true {
        return Ok(reply["result"].clone());
    }
    let err = &reply["error"];
    Err(ToolFailure {
        code: err["code"].as_str().map(str::to_owned),
        message: err["message"].as_str().unwrap_or("the tool failed").to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_read_as_results_or_coded_failures() {
        assert_eq!(parse_reply("t", r#"{"ok":true,"result":{"a":1}}"#).unwrap()["a"], 1);
        let f = parse_reply("t", r#"{"ok":false,"error":{"code":"SPHINX_E_X","message":"m"}}"#)
            .unwrap_err();
        assert_eq!((f.code.as_deref(), f.message.as_str()), (Some("SPHINX_E_X"), "m"));
        assert!(parse_reply("t", "not json").unwrap_err().code.is_none());
    }
}

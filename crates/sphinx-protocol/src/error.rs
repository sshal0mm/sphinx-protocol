//! One error model, rendered three ways.
//!
//! - Contract panic text: `CODE: message {json}`. The code is first so it
//!   survives any truncation and can be found with a plain substring search;
//!   the JSON tail carries the structure.
//! - Grader and CLI JSON: [`ErrorReport`].
//! - CLI human text: `error[CODE]: message` plus hint and docs lines.
//!
//! Rules: messages are one line, contain the actual values, and never contain
//! `{`. Large integers in `ctx` are strings. Time values end in `_ns`.

use crate::catalog::Code;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Version of the JSON tail in contract panic text.
pub const PANIC_FORMAT_VERSION: u32 = 1;

/// Whether retrying can help, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retry {
    /// Retrying the same call will fail again.
    Never,
    /// Change the input as the hint says, then retry.
    FixInput,
    /// Retry later; `ctx` says when, if known.
    Wait,
    /// Retry as-is, with backoff.
    Transient,
}

impl Retry {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::FixInput => "fix_input",
            Self::Wait => "wait",
            Self::Transient => "transient",
        }
    }
}

/// What happened to the caller's bond when the error occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BondEffect {
    /// No bond was taken (failed calls refund their attached deposit).
    None,
    /// The bond is held by the job: kept by the contract if another submission passes,
    /// returned if nobody passes.
    Forfeited,
    /// The bond was paid back.
    Returned,
    /// The bond is still held by the contract and can be recovered later.
    Held,
}

impl BondEffect {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Forfeited => "forfeited",
            Self::Returned => "returned",
            Self::Held => "held",
        }
    }
}

/// An error with its code, a human message and machine-readable context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SphinxError {
    code: Code,
    message: String,
    retry: Retry,
    ctx: BTreeMap<String, String>,
}

impl SphinxError {
    /// Creates an error. `message` must be one line without `{`.
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        let mut message = message.into();
        // Keep the panic format parseable even if a caller slips.
        if message.contains(['{', '\n']) {
            message = message.replace('{', "(").replace('\n', " ");
        }
        Self { code, message, retry: code.retry(), ctx: BTreeMap::new() }
    }

    /// Adds a context value. Use string forms for u64/u128 amounts.
    #[must_use]
    pub fn ctx(mut self, key: &str, value: impl fmt::Display) -> Self {
        self.ctx.insert(key.to_owned(), value.to_string());
        self
    }

    /// Overrides the catalog's default retry class when state decides it.
    #[must_use]
    pub fn retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

    pub fn code(&self) -> Code {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn retry_class(&self) -> Retry {
        self.retry
    }

    pub fn context(&self) -> &BTreeMap<String, String> {
        &self.ctx
    }

    /// The text a contract passes to `env::panic_str`.
    pub fn render_panic(&self) -> String {
        let tail = serde_json::json!({
            "v": PANIC_FORMAT_VERSION,
            "retry": self.retry,
            "bond_effect": self.code.bond_effect(),
            "hint": self.code.hint(),
            "ctx": self.ctx,
        });
        format!("{}: {} {}", self.code, self.message, tail)
    }

    /// The structured form used by the grader and the CLI.
    pub fn to_report(&self) -> ErrorReport {
        ErrorReport {
            code: self.code.as_str().to_owned(),
            message: self.message.clone(),
            hint: self.code.hint().to_owned(),
            retry: self.retry,
            bond_effect: self.code.bond_effect(),
            ctx: self.ctx.clone(),
            docs: docs_anchor(self.code),
        }
    }
}

impl fmt::Display for SphinxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SphinxError {}

/// The wire form of an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorReport {
    pub code: String,
    pub message: String,
    pub hint: String,
    pub retry: Retry,
    pub bond_effect: BondEffect,
    pub ctx: BTreeMap<String, String>,
    pub docs: String,
}

/// Relative link to the code's entry in `docs/ERRORS.md`.
pub fn docs_anchor(code: Code) -> String {
    format!("docs/ERRORS.md#{}", code.as_str().to_ascii_lowercase())
}

/// Keys of the JSON tail a contract panic carries.
const TAIL_KEYS: [&str; 5] = ["v", "retry", "bond_effect", "hint", "ctx"];

/// Turns `(` `)` back into `{` `}` outside JSON strings, undoing clients that
/// print braces as parentheses. Text inside strings is left as is.
fn restore_braces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    for c in text.chars() {
        out.push(match c {
            _ if escaped => {
                escaped = false;
                c
            }
            '\\' if in_string => {
                escaped = true;
                c
            }
            '"' => {
                in_string = !in_string;
                c
            }
            '(' if !in_string => '{',
            ')' if !in_string => '}',
            _ => c,
        });
    }
    out
}

/// Recovers an [`ErrorReport`] from any text that contains a Sphinx panic,
/// such as a NEAR RPC error or CLI stderr. Returns `None` if no known code is
/// present. The JSON tail is optional so truncated text still yields the code.
pub fn parse_panic(text: &str) -> Option<ErrorReport> {
    let start = text.find("SPHINX_E_")?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        .unwrap_or(rest.len());
    let code: Code = rest[..end].parse().ok()?;
    let after = rest[end..].strip_prefix(": ").unwrap_or(&rest[end..]);
    // RPC errors may escape the quotes of the JSON tail, and some clients
    // print it with `(` `)` in place of `{` `}`. The tail opens with one of
    // the report's own keys, which a message cannot contain.
    let after = after.replace("\\\"", "\"");
    let tail_at = TAIL_KEYS
        .iter()
        .flat_map(|k| [format!("{{\"{k}\":"), format!("(\"{k}\":")])
        .filter_map(|m| after.find(&m))
        .min();
    let (message, tail) = match tail_at {
        Some(i) => (&after[..i], Some(restore_braces(&after[i..]))),
        None => (after.as_str(), None),
    };
    // Take the longest prefix that parses as a JSON object; anything after
    // the tail (closing quotes, RPC wrapping) is ignored.
    let parsed: Option<serde_json::Value> = tail.and_then(|t| {
        (1..=t.len()).rev().filter(|&n| t.is_char_boundary(n)).find_map(|n| {
            serde_json::from_str::<serde_json::Value>(&t[..n])
                .ok()
                .filter(serde_json::Value::is_object)
        })
    });
    let retry = parsed
        .as_ref()
        .and_then(|v| serde_json::from_value(v["retry"].clone()).ok())
        .unwrap_or(code.retry());
    let ctx = parsed
        .as_ref()
        .and_then(|v| v["ctx"].as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_owned)))
                .collect()
        })
        .unwrap_or_default();
    Some(ErrorReport {
        code: code.as_str().to_owned(),
        message: message.trim().to_owned(),
        hint: code.hint().to_owned(),
        retry,
        bond_effect: code.bond_effect(),
        ctx,
        docs: docs_anchor(code),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SphinxError {
        SphinxError::new(Code::SubmitAgentCap, "agent-a.testnet has used 2/2 submissions on job 7")
            .ctx("job_id", 7)
            .ctx("used", 2)
            .ctx("cap", 2)
    }

    #[test]
    fn panic_text_starts_with_code_and_round_trips() {
        let text = sample().render_panic();
        assert!(text.starts_with("SPHINX_E_SUBMIT_AGENT_CAP: agent-a.testnet has used 2/2"));
        let wrapped = format!("Smart contract panicked: {text}");
        let report = parse_panic(&wrapped).unwrap();
        assert_eq!(report.code, "SPHINX_E_SUBMIT_AGENT_CAP");
        assert_eq!(report.message, "agent-a.testnet has used 2/2 submissions on job 7");
        assert_eq!(report.ctx["cap"], "2");
        assert_eq!(report.retry, Retry::Never);
        assert_eq!(report.bond_effect, BondEffect::None);
    }

    #[test]
    fn escaped_and_parenthesised_rpc_text_is_recovered() {
        // A failed view as printed by the NEAR client library: quotes escaped
        // and braces shown as parentheses, with parentheses in the message
        // and inside a string of the tail.
        let text = r#"Query error: Critical(WasmExecutionError(HostError(GuestPanic ( panic_msg: \"SPHINX_E_JOB_NOT_FOUND: job 999 does not exist (next job id is 8) (\"v\":1,\"retry\":\"never\",\"bond_effect\":\"none\",\"hint\":\"check the id (see list_jobs)\",\"ctx\":(\"job_id\":\"999\",\"next_job_id\":\"8\"))\" ))))"#;
        let report = parse_panic(text).unwrap();
        assert_eq!(report.code, "SPHINX_E_JOB_NOT_FOUND");
        assert_eq!(report.message, "job 999 does not exist (next job id is 8)");
        assert_eq!(report.ctx["next_job_id"], "8");
        assert_eq!(report.retry, Retry::Never);
    }

    #[test]
    fn escaped_json_tail_is_recovered() {
        let text = format!("panic_msg: \"{}\"", sample().render_panic().replace('"', "\\\""));
        let report = parse_panic(&text).unwrap();
        assert_eq!(report.message, "agent-a.testnet has used 2/2 submissions on job 7");
        assert_eq!(report.ctx["used"], "2");
    }

    #[test]
    fn truncated_panic_still_yields_code() {
        let text = sample().render_panic();
        let cut = &text[..40];
        assert_eq!(parse_panic(cut).unwrap().code, "SPHINX_E_SUBMIT_AGENT_CAP");
    }

    #[test]
    fn braces_in_messages_are_neutralised() {
        let e = SphinxError::new(Code::CliUsage, "bad {input}\nsecond line");
        assert!(!e.message().contains('{'));
        assert!(!e.message().contains('\n'));
    }

    #[test]
    fn retry_can_be_refined() {
        let e = SphinxError::new(Code::JobWrongState, "job 1 is Committed").retry(Retry::Wait);
        assert_eq!(e.to_report().retry, Retry::Wait);
    }

    #[test]
    fn unknown_code_is_not_parsed() {
        assert!(parse_panic("SPHINX_E_NOT_A_REAL_CODE: x").is_none());
        assert!(parse_panic("no sphinx here").is_none());
    }
}

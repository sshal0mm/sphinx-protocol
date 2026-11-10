//! Output conventions: data on stdout, errors on stderr. Human text by
//! default; `--json` prints the catalog's wire object instead. Exit codes
//! follow sysexits.h so scripts can branch on them.

use sphinx_protocol::catalog::Component;
use sphinx_protocol::{Code, ErrorReport, Retry, SphinxError};
use std::fmt::Write as _;
use std::process::ExitCode;

/// Exit code for an error, by code family.
pub fn exit_code(code: Code) -> u8 {
    match code {
        Code::CliUsage => 64,
        Code::CliFileUnreadable => 66,
        Code::CliConfigMissing | Code::CliConfigMalformed => 78,
        _ if code.component() == Component::Relayer && code.retry() == Retry::Transient => 75,
        _ => 65,
    }
}

/// Prints `err` to stderr and returns its exit code.
#[allow(clippy::print_stderr, reason = "stderr is the error channel")]
pub fn fail(err: &SphinxError, json: bool) -> ExitCode {
    let report = err.to_report();
    if json {
        eprintln!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        eprintln!("{}", human(&report));
    }
    ExitCode::from(exit_code(err.code()))
}

/// Multi-line human rendering of a report.
pub fn human(r: &ErrorReport) -> String {
    let mut out = format!(
        "error[{}]: {}\n  hint: {}\n  retry: {}  bond: {}",
        r.code,
        r.message,
        r.hint,
        r.retry.as_str(),
        r.bond_effect.as_str()
    );
    if !r.ctx.is_empty() {
        let ctx: Vec<String> = r.ctx.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = write!(out, "\n  ctx: {}", ctx.join(" "));
    }
    let _ = write!(out, "\n  docs: {}", r.docs);
    out
}

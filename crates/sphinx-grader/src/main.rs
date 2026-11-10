//! Enclave entry point: one JSON request on stdin, one JSON response on
//! stdout. The process exits 0 whenever a response was written, so the host
//! always delivers it; non-zero only if stdout itself failed.
#![allow(clippy::print_stdout, reason = "stdout is the response channel")]

use std::io::{self, Read, Write};

fn main() -> std::process::ExitCode {
    let mut input = String::new();
    let response = if io::stdin().read_to_string(&mut input).is_ok() {
        let signer = sphinx_grader::signer::configured();
        sphinx_grader::handle(&input, signer.as_deref())
    } else {
        sphinx_grader::handle("", None)
    };
    let json = serde_json::to_string(&response).unwrap_or_default();
    let mut out = io::stdout().lock();
    if out.write_all(json.as_bytes()).and_then(|()| out.flush()).is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

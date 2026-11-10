//! The submission runtime seam. Today there is one implementation,
//! JavaScript on Boa (`js-boa/1`). A second runtime (for example metered
//! WebAssembly) implements the same trait and is selected by the suite's
//! `runtime` field, which is also bound into every signed result.

use serde_json::Value;

use crate::executor::Budget;

/// Outcome of running one test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    Pass,
    /// Ran to completion (or threw) with a wrong result.
    Fail,
    /// Stopped by a resource limit: loop, recursion, stack or poll budget.
    Aborted,
}

/// A problem found in a submission before any test runs. Diagnostics are
/// about the submission itself and never reveal anything about the suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: sphinx_protocol::Code,
    pub message: String,
}

pub trait Runtime {
    /// The runtime id, e.g. `js-boa/1`.
    fn id(&self) -> &'static str;

    /// Checks that `code` loads and defines a callable `entry`.
    fn preflight(&self, code: &str, entry: &str, budget: &mut Budget) -> Option<Diagnostic>;

    /// Runs one test in a fresh, isolated environment, drawing from `budget`.
    /// With `check` (the suite's checker source), the test passes when
    /// `check(result, expected, args)` returns exactly `true`; the checker
    /// runs in a separate fresh environment with its own budget.
    fn run_test(
        &self,
        code: &str,
        entry: &str,
        args: &[Value],
        expected: &Value,
        check: Option<&str>,
        budget: &mut Budget,
    ) -> TestOutcome;
}

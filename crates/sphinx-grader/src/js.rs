//! `js-boa/1`: JavaScript submissions on the Boa engine.
//!
//! Every test runs in a brand-new realm:
//! 1. Build a context with a fixed clock and loop, recursion and stack limits.
//! 2. Run the trusted prelude, which disables `Math.random` and returns the
//!    `snapshot` function (see `prelude.js`).
//! 3. Load the submission with a polled cycle budget.
//! 4. Bind the test arguments as read-only globals from Rust and call the
//!    entry from a generated script, again under a polled budget. No
//!    submission-controlled `apply`, iterator or `JSON` is involved.
//! 5. Snapshot the return value and compare it in Rust.
//!
//! Any error, budget overrun or unreadable value fails the test.

use std::rc::Rc;

use boa_engine::context::time::FixedClock;
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsObject, JsString, JsValue, Script, Source};
use serde_json::Value;
use sphinx_protocol::{Code, RUNTIME_JS_BOA_V1};

use crate::compare::values_equal;
use crate::executor::{block_on_budgeted, Budget};
use crate::limits::{
    CYCLES_PER_POLL, LOAD_POLLS, LOOP_ITERATIONS, MAX_RESULT_DEPTH, MAX_RESULT_NODES,
    RECURSION_DEPTH, STACK_SIZE, TEST_POLLS,
};
use crate::runtime::{Diagnostic, Runtime, TestOutcome};

const PRELUDE: &str = include_str!("prelude.js");

/// Why a script stopped without a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The code threw (or failed to parse).
    Threw,
    /// A resource limit stopped it: poll budget, loop, recursion or stack.
    Aborted,
}

/// True when Boa stopped the script on one of its runtime limits. Boa 0.22
/// exposes these only through the error text; `tests::limit_errors_are_classified`
/// pins this against engine upgrades.
fn is_resource_limit(e: &boa_engine::JsError) -> bool {
    let text = e.to_string();
    text.contains("RuntimeLimit") || text.contains("reached the maximum")
}

/// The Boa runtime. `epoch_ms` fixes `Date.now()` for reproducible runs.
#[derive(Debug, Clone)]
pub struct JsBoaRuntime {
    pub epoch_ms: u64,
}

impl JsBoaRuntime {
    /// A fresh realm with limits and the prelude installed.
    fn sandbox(&self) -> Option<(Context, JsObject)> {
        let mut ctx = Context::builder()
            .clock(Rc::new(FixedClock::from_millis(self.epoch_ms)))
            .build()
            .ok()?;
        let limits = ctx.runtime_limits_mut();
        limits.set_loop_iteration_limit(LOOP_ITERATIONS);
        limits.set_recursion_limit(RECURSION_DEPTH);
        limits.set_stack_size_limit(STACK_SIZE);
        let factory = ctx.eval(Source::from_bytes(PRELUDE)).ok()?.as_object()?;
        let snapshot = factory
            .call(
                &JsValue::undefined(),
                &[JsValue::from(MAX_RESULT_DEPTH), JsValue::from(MAX_RESULT_NODES)],
                &mut ctx,
            )
            .ok()?
            .as_object()?;
        Some((ctx, snapshot))
    }

    /// Runs `source` as a script under a polled budget.
    fn run_budgeted(
        ctx: &mut Context,
        source: &str,
        cap: u32,
        budget: &mut Budget,
    ) -> Result<JsValue, Stop> {
        let script =
            Script::parse(Source::from_bytes(source), None, ctx).map_err(|_| Stop::Threw)?;
        Self::evaluate_budgeted(ctx, &script, cap, budget)
    }

    /// Evaluates a parsed script under a polled budget. A script is parsed
    /// once: parsing registers its top-level `let` and `const` names in the
    /// realm, so parsing the same code twice fails as a redeclaration.
    fn evaluate_budgeted(
        ctx: &mut Context,
        script: &Script,
        cap: u32,
        budget: &mut Budget,
    ) -> Result<JsValue, Stop> {
        let future = script.evaluate_async_with_budget(ctx, CYCLES_PER_POLL);
        match block_on_budgeted(future, cap, budget) {
            Err(_) => Err(Stop::Aborted),
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) if is_resource_limit(&e) => Err(Stop::Aborted),
            Ok(Err(_)) => Err(Stop::Threw),
        }
    }

    /// Loads the submission; returns the context if `entry` is a function.
    fn load(
        &self,
        code: &str,
        entry: &str,
        budget: &mut Budget,
    ) -> Result<(Context, JsObject), (Diagnostic, Stop)> {
        let (mut ctx, snapshot) = self.sandbox().ok_or_else(|| {
            (
                Diagnostic {
                    code: Code::InternalInvariant,
                    message: "the sandbox could not be created".into(),
                },
                Stop::Threw,
            )
        })?;
        let Ok(script) = Script::parse(Source::from_bytes(code), None, &mut ctx) else {
            let d = Diagnostic {
                code: Code::CodeSyntax,
                message: "submission does not parse as JavaScript".into(),
            };
            return Err((d, Stop::Threw));
        };
        Self::evaluate_budgeted(&mut ctx, &script, LOAD_POLLS, budget).map_err(|stop| {
            let message = match stop {
                Stop::Aborted => "submission exceeded its resource budget while loading",
                Stop::Threw => "submission threw while loading its top-level code",
            };
            (Diagnostic { code: Code::CodeSyntax, message: message.into() }, stop)
        })?;
        let global = ctx.global_object();
        let key = JsString::from(entry);
        if !global.has_own_property(key.clone(), &mut ctx).unwrap_or(false) {
            let d = Diagnostic {
                code: Code::CodeEntryMissing,
                message: format!("submission does not define a top-level `{entry}`"),
            };
            return Err((d, Stop::Threw));
        }
        let value = global.get(key, &mut ctx).unwrap_or_default();
        if !value.as_object().is_some_and(|o| o.is_callable()) {
            let d = Diagnostic {
                code: Code::CodeEntryNotFunction,
                message: format!("`{entry}` is defined but is not a function"),
            };
            return Err((d, Stop::Threw));
        }
        Ok((ctx, snapshot))
    }

    /// Calls the entry with `args`. `Err` carries why it stopped early;
    /// `Ok(None)` means it returned something that cannot be read.
    fn call_entry(
        ctx: &mut Context,
        snapshot: &JsObject,
        entry: &str,
        args: &[Value],
        budget: &mut Budget,
    ) -> Result<Option<Value>, Stop> {
        let mut names = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let name = format!("__sphinx_arg_{i}");
            let value = JsValue::from_json(arg, ctx).map_err(|_| Stop::Threw)?;
            ctx.register_global_property(JsString::from(name.as_str()), value, Attribute::empty())
                .map_err(|_| Stop::Threw)?;
            names.push(name);
        }
        let call = format!("{entry}({})", names.join(", "));
        let result = Self::run_budgeted(ctx, &call, TEST_POLLS, budget)?;
        Ok(Self::read_result(ctx, snapshot, result))
    }

    fn read_result(ctx: &mut Context, snapshot: &JsObject, result: JsValue) -> Option<Value> {
        let snap = snapshot.call(&JsValue::undefined(), &[result], ctx).ok()?.as_object()?;
        if snap.get(js_string!("ok"), ctx).ok()?.as_boolean() != Some(true) {
            return None;
        }
        snap.get(js_string!("value"), ctx).ok()?.to_json(ctx).ok()?
    }
}

/// What one call of a function did, read under the same sandbox and limits
/// as a test.
#[cfg(feature = "evaluate")]
#[derive(Debug, Clone, PartialEq)]
pub enum Evaluation {
    /// The entry returned this JSON value.
    Returned(Value),
    /// It returned something JSON cannot carry (a function, a cycle, too deep).
    Unreadable,
    Threw,
    /// Stopped by a resource limit.
    Aborted,
    /// The code did not load or define the entry.
    NotLoaded(Diagnostic),
}

#[cfg(feature = "evaluate")]
impl JsBoaRuntime {
    /// Calls `entry(args)` and returns what it produced. Off-chain tools use
    /// it to derive a suite's expected values from a reference; grading only
    /// ever compares through [`Runtime::run_test`].
    pub fn evaluate(
        &self,
        code: &str,
        entry: &str,
        args: &[Value],
        budget: &mut Budget,
    ) -> Evaluation {
        let (mut ctx, snapshot) = match self.load(code, entry, budget) {
            Ok(loaded) => loaded,
            Err((d, Stop::Threw)) => return Evaluation::NotLoaded(d),
            Err((_, Stop::Aborted)) => return Evaluation::Aborted,
        };
        match Self::call_entry(&mut ctx, &snapshot, entry, args, budget) {
            Ok(Some(v)) => Evaluation::Returned(v),
            Ok(None) => Evaluation::Unreadable,
            Err(Stop::Threw) => Evaluation::Threw,
            Err(Stop::Aborted) => Evaluation::Aborted,
        }
    }
}

impl Runtime for JsBoaRuntime {
    fn id(&self) -> &'static str {
        RUNTIME_JS_BOA_V1
    }

    fn preflight(&self, code: &str, entry: &str, budget: &mut Budget) -> Option<Diagnostic> {
        self.load(code, entry, budget).err().map(|(d, _)| d)
    }

    fn run_test(
        &self,
        code: &str,
        entry: &str,
        args: &[Value],
        expected: &Value,
        check: Option<&str>,
        budget: &mut Budget,
        check_budget: &mut Budget,
    ) -> TestOutcome {
        if budget.is_spent() {
            return TestOutcome::Fail;
        }
        let (mut ctx, snapshot) = match self.load(code, entry, budget) {
            Ok(loaded) => loaded,
            Err((_, Stop::Aborted)) => return TestOutcome::Aborted,
            Err((_, Stop::Threw)) => return TestOutcome::Fail,
        };
        match Self::call_entry(&mut ctx, &snapshot, entry, args, budget) {
            Ok(Some(actual)) => match check {
                None if values_equal(expected, &actual) => TestOutcome::Pass,
                None => TestOutcome::Fail,
                Some(src) => self.checked(src, actual, expected, args, check_budget),
            },
            Ok(None) | Err(Stop::Threw) => TestOutcome::Fail,
            Err(Stop::Aborted) => TestOutcome::Aborted,
        }
    }
}

impl JsBoaRuntime {
    /// Runs the suite's checker on a submission's result in a fresh sandbox,
    /// drawing from the run's checker budget, so the submission can neither
    /// reach the checker nor spend its compute. Anything but a returned
    /// `true` (a throw, a resource limit, a spent budget, any other value)
    /// fails the test.
    fn checked(
        &self,
        src: &str,
        actual: Value,
        expected: &Value,
        args: &[Value],
        budget: &mut Budget,
    ) -> TestOutcome {
        if budget.is_spent() {
            return TestOutcome::Fail;
        }
        let Ok((mut ctx, snapshot)) = self.load(src, "check", budget) else {
            return TestOutcome::Fail;
        };
        let inputs = [actual, expected.clone(), Value::Array(args.to_vec())];
        match Self::call_entry(&mut ctx, &snapshot, "check", &inputs, budget) {
            Ok(Some(Value::Bool(true))) => TestOutcome::Pass,
            _ => TestOutcome::Fail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(code: &str) -> TestOutcome {
        let rt = JsBoaRuntime { epoch_ms: 0 };
        let mut budget = Budget::new(crate::limits::RUN_POLLS);
        let mut check_budget = Budget::new(crate::limits::RUN_POLLS);
        rt.run_test(code, "solve", &[], &serde_json::json!(1), None, &mut budget, &mut check_budget)
    }

    #[cfg(feature = "evaluate")]
    #[test]
    fn evaluate_reports_what_the_call_did() {
        let rt = JsBoaRuntime { epoch_ms: 0 };
        let mut b = Budget::new(crate::limits::RUN_POLLS);
        let args = [serde_json::json!([1, 2, 3])];
        let sum = "function f(a){ return a.reduce((x, y) => x + y, 0) }";
        assert_eq!(
            rt.evaluate(sum, "f", &args, &mut b),
            Evaluation::Returned(serde_json::json!(6))
        );
        assert_eq!(rt.evaluate("function f(){ throw 1 }", "f", &[], &mut b), Evaluation::Threw);
        assert_eq!(rt.evaluate("function f(){ for(;;){} }", "f", &[], &mut b), Evaluation::Aborted);
        assert_eq!(
            rt.evaluate("function f(){ return () => 1 }", "f", &[], &mut b),
            Evaluation::Unreadable
        );
        assert!(matches!(rt.evaluate("const g = 1", "f", &[], &mut b), Evaluation::NotLoaded(_)));
    }

    #[test]
    fn limit_errors_are_classified() {
        assert_eq!(outcome("function solve(){ while(true){} }"), TestOutcome::Aborted);
        assert_eq!(outcome("function solve(){ return solve() }"), TestOutcome::Aborted);
        assert_eq!(
            outcome("function solve(){ [1].map(() => { for(;;){} }) }"),
            TestOutcome::Aborted
        );
        assert_eq!(outcome("function solve(){ throw new Error('x') }"), TestOutcome::Fail);
        assert_eq!(outcome("function solve(){ return 2 }"), TestOutcome::Fail);
        assert_eq!(outcome("function solve(){ return 1 }"), TestOutcome::Pass);
    }
}

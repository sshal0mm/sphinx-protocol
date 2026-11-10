//! The hidden test suite (`sphinx.suite/1`).
//!
//! A suite is a JSON document the buyer writes and commits by hash. Its hash
//! is SHA-256 over the exact file bytes (no canonicalisation), so the buyer,
//! the grader and any auditor compute it the same way with standard tools.

use crate::catalog::Code;
use crate::error::SphinxError;
use crate::hash::Hash32;
use crate::{RUNTIME_JS_BOA_V1, SUPPORTED_RUNTIMES};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Format version this crate reads and writes.
pub const SUITE_VERSION: u32 = 1;
/// Maximum number of tests in one suite.
pub const MAX_TESTS: usize = 32;
/// Maximum size of the suite file in bytes.
pub const MAX_SUITE_BYTES: usize = 256 * 1024;
/// Maximum nesting depth of any argument or expected value.
pub const MAX_VALUE_DEPTH: usize = 32;
/// Maximum size of a suite's `check` source in bytes (the same cap as a
/// submission).
pub const MAX_CHECK_BYTES: usize = 64 * 1024;
/// Largest integer JavaScript numbers represent exactly (2^53 - 1).
pub const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// One test: call the entry function with `args`. Without a suite `check`,
/// the return value must equal `expected` under JSON value equality; with
/// one, `check(result, expected, args)` must return exactly `true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    pub args: Vec<Value>,
    pub expected: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    /// Must be [`SUITE_VERSION`].
    pub version: u32,
    /// Submission runtime, e.g. `js-boa/1`.
    pub runtime: String,
    /// Name of the function the submission must define.
    pub entry: String,
    pub tests: Vec<TestCase>,
    /// Optional JavaScript defining `check(result, expected, args)`, written
    /// by the buyer and hidden with the tests. When present, a test passes
    /// when `check` returns exactly `true`, so a test can state a property or
    /// a threshold (a strategy's profit on hidden prices) instead of one
    /// exact answer. It runs in its own fresh sandbox, apart from the
    /// submission, under the same limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
}

impl Suite {
    /// Parses and checks suite bytes. Errors never quote suite contents, so a
    /// hidden suite cannot leak through an error message.
    ///
    /// # Errors
    /// Returns a `SPHINX_E_GRADER_SUITE_*`, `SPHINX_E_GRADER_ENTRY_*` or
    /// `SPHINX_E_GRADER_RUNTIME_UNSUPPORTED` error describing the first rule
    /// the suite breaks.
    pub fn parse(bytes: &[u8]) -> Result<Self, SphinxError> {
        if bytes.len() > MAX_SUITE_BYTES {
            return Err(SphinxError::new(
                Code::GraderSuiteTooLarge,
                format!("suite is {} bytes; the maximum is {MAX_SUITE_BYTES}", bytes.len()),
            )
            .ctx("bytes", bytes.len())
            .ctx("max", MAX_SUITE_BYTES));
        }
        let suite: Self = serde_json::from_slice(bytes).map_err(|e| {
            SphinxError::new(
                Code::GraderSuiteMalformed,
                format!(
                    "suite is not valid sphinx.suite/1 JSON (line {}, column {})",
                    e.line(),
                    e.column()
                ),
            )
            .ctx("line", e.line())
            .ctx("column", e.column())
        })?;
        suite.check()?;
        Ok(suite)
    }

    /// Checks every intake rule on an already-parsed suite.
    ///
    /// # Errors
    /// See [`Suite::parse`].
    pub fn check(&self) -> Result<(), SphinxError> {
        if self.version != SUITE_VERSION {
            return Err(SphinxError::new(
                Code::GraderSuiteMalformed,
                format!(
                    "suite version is {}; this grader reads version {SUITE_VERSION}",
                    self.version
                ),
            )
            .ctx("version", self.version));
        }
        if !SUPPORTED_RUNTIMES.contains(&self.runtime.as_str()) {
            return Err(SphinxError::new(
                Code::GraderRuntimeUnsupported,
                format!("runtime is not supported; supported: {}", SUPPORTED_RUNTIMES.join(", ")),
            )
            .ctx("supported", SUPPORTED_RUNTIMES.join(",")));
        }
        check_entry(&self.entry)?;
        if let Some(src) = &self.check {
            if src.trim().is_empty() || src.len() > MAX_CHECK_BYTES {
                return Err(SphinxError::new(
                    Code::GraderSuiteMalformed,
                    format!("suite check must be 1 to {MAX_CHECK_BYTES} bytes of JavaScript"),
                )
                .ctx("bytes", src.len())
                .ctx("max", MAX_CHECK_BYTES));
            }
        }
        if self.tests.is_empty() {
            return Err(SphinxError::new(Code::GraderSuiteEmpty, "suite has no tests"));
        }
        if self.tests.len() > MAX_TESTS {
            return Err(SphinxError::new(
                Code::GraderSuiteTooManyTests,
                format!("suite has {} tests; the maximum is {MAX_TESTS}", self.tests.len()),
            )
            .ctx("count", self.tests.len())
            .ctx("max", MAX_TESTS));
        }
        for (index, test) in self.tests.iter().enumerate() {
            for value in test.args.iter().chain(std::iter::once(&test.expected)) {
                check_value(value, 1).map_err(|e| e.ctx("test_index", index))?;
            }
        }
        Ok(())
    }
}

/// SHA-256 of the suite file bytes: the value committed on-chain.
pub fn suite_hash(bytes: &[u8]) -> Hash32 {
    Hash32::digest(bytes)
}

/// JavaScript reserved words and the global names a submission could shadow
/// to confuse the grader. An entry must not be any of these.
const RESERVED: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "arguments",
    "eval",
    "undefined",
    "NaN",
    "Infinity",
    "globalThis",
    "Object",
    "Function",
    "Array",
    "String",
    "Number",
    "Boolean",
    "Symbol",
    "BigInt",
    "Math",
    "JSON",
    "Date",
    "RegExp",
    "Error",
    "Promise",
    "Proxy",
    "Reflect",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "constructor",
    "prototype",
    "toString",
    "valueOf",
];

fn check_entry(entry: &str) -> Result<(), SphinxError> {
    let mut chars = entry.chars();
    let valid = entry.len() <= 64
        && matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if !valid {
        return Err(SphinxError::new(
            Code::GraderEntryInvalid,
            "entry is not a plain JavaScript identifier of at most 64 characters",
        ));
    }
    if RESERVED.contains(&entry) {
        return Err(SphinxError::new(
            Code::GraderEntryReserved,
            format!("entry `{entry}` is a JavaScript keyword or built-in global"),
        )
        .ctx("entry", entry));
    }
    Ok(())
}

fn check_value(value: &Value, depth: usize) -> Result<(), SphinxError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(SphinxError::new(
            Code::GraderSuiteValueTooDeep,
            format!("a test value nests deeper than {MAX_VALUE_DEPTH} levels"),
        )
        .ctx("max", MAX_VALUE_DEPTH));
    }
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(n) => {
            let exact = if let Some(i) = n.as_i64() {
                (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&i)
            } else if let Some(u) = n.as_u64() {
                u <= MAX_SAFE_INTEGER.unsigned_abs()
            } else {
                n.as_f64().is_some_and(f64::is_finite)
            };
            if exact {
                Ok(())
            } else {
                Err(unrepresentable(
                    "a number is outside the exactly representable JavaScript range",
                ))
            }
        }
        Value::Array(items) => items.iter().try_for_each(|v| check_value(v, depth + 1)),
        Value::Object(map) => {
            if map.contains_key("__proto__") {
                return Err(unrepresentable("an object uses the __proto__ key"));
            }
            map.values().try_for_each(|v| check_value(v, depth + 1))
        }
    }
}

fn unrepresentable(reason: &str) -> SphinxError {
    SphinxError::new(Code::GraderSuiteUnrepresentable, format!("suite value rejected: {reason}"))
}

/// The runtime a suite declares by default when generated by tooling.
pub const DEFAULT_RUNTIME: &str = RUNTIME_JS_BOA_V1;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn suite_json(extra: &Value) -> Vec<u8> {
        let mut s = json!({
            "version": 1, "runtime": "js-boa/1", "entry": "solve",
            "tests": [{"args": [1], "expected": 2}]
        });
        if let (Some(base), Some(add)) = (s.as_object_mut(), extra.as_object()) {
            for (k, v) in add {
                base.insert(k.clone(), v.clone());
            }
        }
        serde_json::to_vec(&s).unwrap()
    }

    fn code_of(bytes: &[u8]) -> Code {
        Suite::parse(bytes).unwrap_err().code()
    }

    #[test]
    fn accepts_a_minimal_suite() {
        assert_eq!(Suite::parse(&suite_json(&json!({}))).unwrap().entry, "solve");
    }

    #[test]
    fn rejects_each_rule() {
        assert_eq!(code_of(b"not json"), Code::GraderSuiteMalformed);
        assert_eq!(code_of(&suite_json(&json!({"version": 2}))), Code::GraderSuiteMalformed);
        assert_eq!(code_of(&suite_json(&json!({"extra": 1}))), Code::GraderSuiteMalformed);
        assert_eq!(
            code_of(&suite_json(&json!({"runtime": "py/1"}))),
            Code::GraderRuntimeUnsupported
        );
        assert_eq!(code_of(&suite_json(&json!({"entry": "a b"}))), Code::GraderEntryInvalid);
        assert_eq!(code_of(&suite_json(&json!({"entry": "JSON"}))), Code::GraderEntryReserved);
        assert_eq!(code_of(&suite_json(&json!({"tests": []}))), Code::GraderSuiteEmpty);
        let many: Vec<Value> =
            (0..=MAX_TESTS).map(|_| json!({"args": [], "expected": 0})).collect();
        assert_eq!(code_of(&suite_json(&json!({"tests": many}))), Code::GraderSuiteTooManyTests);
        let big = json!({"tests": [{"args": [9_007_199_254_740_993_u64], "expected": 0}]});
        assert_eq!(code_of(&suite_json(&big)), Code::GraderSuiteUnrepresentable);
        let proto = json!({"tests": [{"args": [{"__proto__": 1}], "expected": 0}]});
        assert_eq!(code_of(&suite_json(&proto)), Code::GraderSuiteUnrepresentable);
        let mut deep = json!(0);
        for _ in 0..=MAX_VALUE_DEPTH {
            deep = json!([deep]);
        }
        assert_eq!(
            code_of(&suite_json(&json!({"tests": [{"args": [deep], "expected": 0}]}))),
            Code::GraderSuiteValueTooDeep
        );
        assert_eq!(code_of(&vec![b' '; MAX_SUITE_BYTES + 1]), Code::GraderSuiteTooLarge);
    }

    #[test]
    fn errors_never_quote_suite_values() {
        let secret = json!({"tests": [{"args": ["TOP-SECRET-CANARY", 9_007_199_254_740_993_u64], "expected": 0}]});
        let err = Suite::parse(&suite_json(&secret)).unwrap_err();
        assert!(!err.render_panic().contains("TOP-SECRET-CANARY"));
        assert!(!err.render_panic().contains("9007199254740993"));
    }
}

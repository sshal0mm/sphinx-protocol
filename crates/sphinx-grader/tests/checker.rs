//! Suites with a `check` function: a test passes when the buyer's hidden
//! checker returns exactly `true`. The checker runs in its own sandbox, so a
//! submission can neither reach it nor spend its budget, and a strategy is
//! scored against futures it never sees (they travel in `expected`).
#![allow(clippy::panic, reason = "test helper")]

use serde_json::{json, Value};
use sphinx_grader::local_score;

fn score(suite: &Value, code: &str) -> u32 {
    match local_score(&suite.to_string(), code, 0) {
        Ok(s) => s.passed,
        Err(e) => panic!("suite and code should be well formed: {e}"),
    }
}

fn suite(check: &str, tests: &Value) -> Value {
    json!({ "version": 1, "runtime": "js-boa/1", "entry": "solve", "tests": tests, "check": check })
}

#[test]
fn a_test_passes_only_when_check_returns_exactly_true() {
    let tests = json!([{ "args": [3], "expected": 10 }]);
    let at_least = "function check(r, e, a){ return r * a[0] >= e }";
    assert_eq!(score(&suite(at_least, &tests), "function solve(n){ return 4 }"), 1);
    assert_eq!(score(&suite(at_least, &tests), "function solve(n){ return 3 }"), 0);
    for not_true in [
        "function check(){ return 1 }",
        "function check(){ return 'true' }",
        "function check(){ return {} }",
        "function check(){ throw new Error('no') }",
        "function check(){ while(true){} }",
        "var check = 5",
        "function nope(){ return true }",
    ] {
        assert_eq!(
            score(&suite(not_true, &tests), "function solve(){ return 4 }"),
            0,
            "{not_true}"
        );
    }
}

#[test]
fn a_submission_cannot_reach_or_bend_the_checker() {
    let tests = json!([{ "args": [], "expected": 1 }]);
    let check = "function check(r, e){ return r === e && [].length === 0 && JSON.stringify({a:1}) === '{\"a\":1}' }";
    for hostile in [
        // Pollute what a shared realm would share.
        "Object.prototype.length = 9; Array.prototype.length = 9; function solve(){ return 1 }",
        "JSON.stringify = function(){ return 'x' }; function solve(){ return 1 }",
        // Try to define the checker itself.
        "function check(){ return true } function solve(){ return 0 }",
        "globalThis.check = function(){ return true }; function solve(){ return 0 }",
    ] {
        let expected = u32::from(!hostile.contains("return 0"));
        assert_eq!(score(&suite(check, &tests), hostile), expected, "{hostile}");
    }
}

#[test]
fn a_returned_function_or_cycle_never_reaches_the_checker() {
    let tests = json!([{ "args": [], "expected": 0 }]);
    let check = "function check(){ return true }";
    assert_eq!(score(&suite(check, &tests), "function solve(){ return function(){} }"), 0);
    assert_eq!(
        score(&suite(check, &tests), "function solve(){ var o = {}; o.o = o; return o }"),
        0
    );
}

/// A strategy job: each window's history is in `args`, its hidden future in
/// `expected`. The checker scores the positions on the futures.
fn strategy_suite() -> Value {
    // Two rising and two falling markets; futures continue the trend.
    let windows = [
        (vec![100, 101, 102, 103, 104], vec![105, 106, 107]),
        (vec![200, 198, 196, 194, 192], vec![190, 188, 186]),
        (vec![50, 51, 53, 54, 56], vec![57, 59, 60]),
        (vec![80, 79, 77, 76, 74], vec![73, 71, 70]),
    ];
    let histories: Vec<Value> = windows.iter().map(|(h, _)| json!(h)).collect();
    let futures: Vec<Value> =
        windows.iter().map(|(h, f)| json!({ "entry": h[h.len() - 1], "path": f })).collect();
    let check = r"
        function check(positions, e, args) {
          var futures = e.futures, n = futures.length;
          if (!Array.isArray(positions) || positions.length !== n) return false;
          var pnl = 0, peak = 0, worst = 0;
          for (var i = 0; i < n; i++) {
            var p = positions[i];
            if (p !== -1 && p !== 0 && p !== 1) return false;
            var f = futures[i], last = f.path[f.path.length - 1];
            pnl += p * (last - f.entry);
            if (pnl > peak) peak = pnl;
            if (peak - pnl > worst) worst = peak - pnl;
          }
          return pnl >= e.min_pnl && worst <= e.max_drawdown;
        }";
    suite(
        check,
        &json!([{ "args": [histories], "expected": { "futures": futures, "min_pnl": 10, "max_drawdown": 5 } }]),
    )
}

#[test]
fn a_strategy_is_scored_on_futures_it_never_sees() {
    let s = strategy_suite();
    let momentum = "function solve(hs){ return hs.map(function(h){ var d = h[h.length-1] - h[0]; return d > 0 ? 1 : d < 0 ? -1 : 0 }) }";
    let flat = "function solve(hs){ return hs.map(function(){ return 0 }) }";
    let contrarian =
        "function solve(hs){ return hs.map(function(h){ return h[h.length-1] > h[0] ? -1 : 1 }) }";
    let oversized = "function solve(hs){ return hs.map(function(){ return 5 }) }";
    assert_eq!(score(&s, momentum), 1);
    assert_eq!(score(&s, flat), 0, "no profit");
    assert_eq!(score(&s, contrarian), 0, "loses");
    assert_eq!(score(&s, oversized), 0, "positions outside -1..1 are refused by the checker");
    // The submission's only input is the histories: nothing in it holds a future price.
    let peek =
        "function solve(hs){ return [JSON.stringify(hs).indexOf('105') >= 0 ? 1 : 0, 0, 0, 0] }";
    assert_eq!(score(&s, peek), 0);
}

#[test]
fn suites_without_check_still_compare_exactly() {
    let s = json!({ "version": 1, "runtime": "js-boa/1", "entry": "solve", "tests": [{ "args": [], "expected": 2 }] });
    assert_eq!(score(&s, "function solve(){ return 2 }"), 1);
    assert_eq!(score(&s, "function solve(){ return 3 }"), 0);
}

#[test]
fn an_empty_or_oversized_check_is_refused_at_intake() {
    let tests = json!([{ "args": [], "expected": 1 }]);
    for bad in [String::new(), "   ".to_owned(), "x".repeat(64 * 1024 + 1)] {
        let s = suite(&bad, &tests);
        assert!(local_score(&s.to_string(), "function solve(){ return 1 }", 0).is_err());
    }
}

#[test]
fn a_checker_shares_one_budget_across_the_run() {
    let tests: Vec<Value> = (0..32).map(|i| json!({ "args": [i], "expected": i })).collect();
    let tests = Value::Array(tests);
    // A light checker passes all 32 tests on the shared budget.
    let light = "function check(r, e){ return r === e }";
    assert_eq!(score(&suite(light, &tests), "function solve(n){ return n }"), 32);
    // A checker that burns its whole per-call allowance on every test cannot
    // stretch the run: the shared budget runs out and the remaining tests fail.
    let heavy = "function burn(){ var s = 0; for (var i = 0; i < 19000; i++) { s += i % 7 } return s }
                 function check(r, e){ for (var k = 0; k < 15000; k++) { burn() } return true }";
    let started = std::time::Instant::now();
    assert_eq!(score(&suite(heavy, &tests), "function solve(n){ return n }"), 0);
    assert!(started.elapsed() < std::time::Duration::from_secs(120), "the run must stay bounded");
}

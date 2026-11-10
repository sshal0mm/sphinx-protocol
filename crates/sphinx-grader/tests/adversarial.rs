//! Hostile submissions. Each must still yield a result (the grader never
//! hangs or crashes) and must not pass. Cases that can only exhaust the
//! enclave's own allowance (huge native allocations) are exercised under
//! wasmtime limits by `make test-wasm`, not here.

mod common;
use common::*;
use serde_json::json;

fn fails(code: &str) {
    assert_eq!(score_one(code, json!(42)), 0, "should fail: {code}");
}

#[test]
fn honest_baseline_passes() {
    assert_eq!(score_one("function solve(){ return 42 }", json!(42)), 1);
    assert_eq!(
        score_one("function solve(){ return {a:[1,2,{b:null}]} }", json!({"a":[1,2,{"b":null}]})),
        1
    );
}

#[test]
fn infinite_loops_fail_within_budget() {
    fails("function solve(){ while(true){} }");
    fails("function solve(){ for(;;){} }");
    fails("function solve(){ do {} while(true) }");
    fails("while(true){} function solve(){ return 42 }");
}

#[test]
fn loops_hidden_in_callbacks_fail() {
    fails("function solve(){ [1].map(() => { while(true){} }); return 42 }");
    fails("function solve(){ [1,2,3].forEach(function f(){ for(;;){} }); return 42 }");
    fails("function solve(){ let s=0; for(let i=0;i<1e9;i++){ s+=i } return 42 }");
}

#[test]
fn deep_and_mutual_recursion_fail() {
    fails("function solve(){ return solve() }");
    fails("function a(){ return b() } function b(){ return a() } function solve(){ return a() }");
}

#[test]
fn result_tampering_is_ignored() {
    // Prototype pollution must not change how the return value is read.
    fails("Object.prototype.toJSON = () => 42; function solve(){ return {} }");
    fails("Array.prototype.toJSON = () => 42; function solve(){ return [] }");
    fails("JSON.stringify = () => '42'; function solve(){ return 41 }");
    fails("Function.prototype.apply = () => 42; function solve(){ return 41 }");
    fails("Object.defineProperty(Array.prototype, '0', {set(){}, get(){ return 42 }}); function solve(){ return [] }");
    assert_eq!(
        score_one("Object.prototype.extra = 1; function solve(){ return {a:1} }", json!({"a": 1})),
        1,
        "inherited properties are not part of the result"
    );
}

#[test]
fn unreadable_results_fail() {
    fails("function solve(){ return { get v(){ return 42 } } }");
    fails("function solve(){ const o = {}; o.self = o; return o }");
    fails("function solve(){ return () => 42 }");
    fails("function solve(){ return Symbol('x') }");
    fails("function solve(){ return 42n }");
    fails("function solve(){ return NaN }");
    fails("function solve(){ return new Proxy({}, { ownKeys(){ while(true){} } }) }");
    fails("function solve(){ let a=[]; for(let i=0;i<40;i++) a=[a]; return a }");
}

#[test]
fn nondeterminism_is_removed() {
    fails("function solve(){ return Math.random() < 2 ? 42 : 0 }");
    // The enclave build always runs at clock 0, whatever the request asks;
    // local runs use the requested clock.
    let now = if cfg!(feature = "outlayer") { 0u64 } else { 1_700_000_000_000 };
    assert_eq!(score_one("function solve(){ return Date.now() }", json!(now)), 1);
    fails("Math.random = () => 42; function solve(){ return Math.random() }");
}

#[test]
fn global_argument_bindings_cannot_be_hijacked() {
    let suite = json!({"version":1,"runtime":"js-boa/1","entry":"solve",
                       "tests":[{"args":[1],"expected":1}]})
    .to_string();
    let code = "var __sphinx_arg_0 = 99; function solve(x){ return x }";
    let out = run(&grade_request(&suite, code));
    assert_eq!(
        out["result"]["passed"], 0,
        "redefining the argument binding breaks only the cheater"
    );
}

#[test]
fn tests_are_isolated_from_each_other() {
    let suite = json!({"version":1,"runtime":"js-boa/1","entry":"solve",
                       "tests":[{"args":[],"expected":0},{"args":[],"expected":0},{"args":[],"expected":0}]})
    .to_string();
    let code = "globalThis.n = (globalThis.n || 0); function solve(){ return n++ }";
    let out = run(&grade_request(&suite, code));
    assert_eq!(out["result"]["passed"], 3, "every test sees a fresh realm");
}

#[test]
fn slow_built_ins_are_disabled_or_budgeted() {
    // A backtracking regular expression would run inside one native call.
    fails("function solve(){ /^(a+)+$/.test('a'.repeat(28) + '!'); return 42 }");
    fails("function solve(){ return 'aaa'.replace(/a/g, 'b') === 'bbb' ? 42 : 0 }");
    // Huge strings are refused before they are built.
    fails("function solve(){ 'a'.repeat(2 ** 29); return 42 }");
    fails("function solve(){ ''.padStart(2 ** 29); return 42 }");
    // A comparator sort on many items now spends the budget instead of hiding.
    fails("function solve(){ const a = []; for (let i = 0; i < 3000; i++) a.push(3000 - i); a.sort((x, y) => { for (let k = 0; k < 2000; k++) {} return x - y }); return 42 }");
}

#[test]
fn sorting_still_works() {
    assert_eq!(
        score_one("function solve(){ return [3, 1, 2].sort((a, b) => a - b) }", json!([1, 2, 3])),
        1
    );
    assert_eq!(score_one("function solve(){ return [10, 9, 1].sort() }", json!([1, 10, 9])), 1);
    assert_eq!(
        score_one("function solve(){ return [{k:2,v:'a'},{k:1,v:'b'},{k:2,v:'c'}].toSorted((x, y) => x.k - y.k).map(o => o.v) }", json!(["b", "a", "c"])),
        1,
        "the sort is stable"
    );
    assert_eq!(score_one("function solve(){ return 'ab'.repeat(3) }", json!("ababab")), 1);
}

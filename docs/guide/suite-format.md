---
title: suite format
section: build
order: 4
---

A suite is a JSON file of calls to one JavaScript function and the answers expected.

## the file

```json
{
  "version": 1,
  "runtime": "js-boa/1",
  "entry": "fib",
  "tests": [
    { "args": [10], "expected": 55 },
    { "args": [0], "expected": 0 }
  ]
}
```

A test passes when `fib(...args)` returns a value equal to `expected`. Numbers compare by value (`1` equals `1.0`), object key order is ignored, and array order matters.

## a check function

For a job with no single right answer, such as a trading strategy, the suite can carry a `check` function instead of relying on exact answers. A test then passes when `check(result, expected, args)` returns exactly `true`, so `expected` can hold thresholds and hidden data rather than an answer.

```json
{
  "version": 1,
  "runtime": "js-boa/1",
  "entry": "positions",
  "check": "function check(r, e, args) { /* score r on e.futures */ return pnl >= e.min_pnl }",
  "tests": [
    { "args": [[[101, 102, 104], [88, 87, 85]]], "expected": { "futures": [[105, 107], [84, 82]], "min_pnl": 4 } }
  ]
}
```

- `check` runs in its own fresh sandbox, apart from the submission, with its own budget, so a submission can neither reach it nor starve it
- anything but `true`, a throw, a timeout or another value, fails the test
- the submission sees only `args`; prices it is scored on go in `expected`, which only `check` sees
- keep each window's future out of every other window's history, or a strategy could read it from there
- when the job is posted, the grader runs the reference through `check` in the enclave, so a broken checker cannot open a job

## rules

| Rule | Limit |
|---|---|
| file size | 256 KiB |
| tests | 1 to 32 |
| nesting | 32 levels |
| numbers | finite; whole numbers within ±(2^53 − 1) |
| object keys | no `__proto__` |
| entry | a plain identifier, not a keyword or built-in |
| check | optional, 1 byte to 64 KiB of JavaScript defining `check` |

## the hash

The suite hash is the SHA-256 of the file's exact bytes, so anyone can recompute it:

```sh
shasum -a 256 suite.json
sphinx suite hash suite.json     # the same, and checks the rules
```

## the runtime

`js-boa/1` runs plain JavaScript in a sandboxed engine with no network, no files and fixed limits. Each test gets a fresh sandbox, `Date` is a fixed clock and `Math.random` throws, so a run gives the same result every time. Budgets are in [grader limits](/docs/grader-limits).

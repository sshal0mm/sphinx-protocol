# Grader limits

The grader runs inside OutLayer, which meters WebAssembly instructions. The
relayer requests 2,000,000,000 per run (`sphinx-near/src/outlayer.rs`), and
the corpus below runs against the same allowance. Every limit is chosen so a
run ends with a signed result inside that allowance, for well-meant and
hostile submissions alike, and so the result is identical on every run. A run that ends without a result still uses the submission's
attempt (THREAT_MODEL.md T4).

## Budgets (`crates/sphinx-grader/src/limits.rs`)

| Limit | Value | Why |
|---|---|---|
| Tests per suite | 32 | each test pays a fixed cost for a fresh sandbox |
| Interpreter cycles per poll | 4,096 | unit of the deterministic budget |
| Polls per test call | 256 | caps one test |
| Polls per submission load | 16 | caps top-level code |
| Polls per whole run | 1,536 | caps the run; once spent, remaining tests fail unrun |
| Tests stopped by a limit before the rest fail unrun | 4 | bounds the cost of runaway code |
| Loop iterations per call frame | 20,000 | bounds loops inside built-in callbacks, which the poll budget cannot see |
| Recursion depth | 256 | |
| Submission size | 64 KiB | |
| Return value | depth 32, 10,000 values | read back without calling getters |

## Measurements

Release wasm under `wasmtime -W fuel=N` (fuel ≈ OutLayer instructions),
minimum fuel found by bisection:

| Measurement | Instructions |
|---|---|
| Fresh sandbox + load + one trivial test | ≈ 7.7 million per test |
| One poll | 160,000 to 270,000 depending on instruction mix |
| A test stopped by the per-frame loop limit | ≈ 17 million |

Worst case per run: 32 × 7.7M + 1,536 × 270k + 4 × 17M ≈ 730 million.
A suite with a `check` function adds a checker realm per test and one more
budget of 1,536 polls for the whole run, shared by every checker call
(ADR 0014): at most about another 730 million, so ≈ 1.46 billion, still under
the 2 billion allowance.

## Corpus (`make test-wasm`, 32 tests each, allowance 2e9)

| Submission | Result |
|---|---|
| identity function | 32/32 |
| 5,000-iteration loop per test | 32/32 |
| 19,000-iteration loop per test | 19/32 (budget reached, the rest fail) |
| `while(true)` per test | 0/32 |
| infinite loop at load time | 0/32 |
| infinite loop inside an array callback | 0/32 |
| nested 19,000 × 19,000 loops | 0/32 |
| infinite recursion | 0/32 |
| sort of 3,000 items with a comparator, per test | 0/32 (limits reached) |
| sort of 300 items with a comparator, per test | 32/32 |
| nested `forEach` over 3,000 items, per test | no result (allowance used up) |
| a string doubled 40 times with `+`, per test | no result (allowance used up) |
| `new Array(5e7).fill(0)`, per test | no result (allowance used up) |

Every row above the last three ends with a signed result. The last three
show the weak spot: work inside one built-in call (a callback-driven
iteration, string concatenation, filling a huge array) runs where the poll
budget cannot see it, and enough of it uses up the whole allowance. Such a
run gives no score at all, never a wrong one: nobody is paid, the submission
expires after the grading window, its bond is returned, and the operator has
paid for one enclave run. The attempt caps and the one-pending-submission
rule bound how many of these one agent can send to a job. Replacing every
such built-in with a metered version is not done in this grader build. The weak spot is work done inside
built-in functions that call back into JavaScript (a comparator sort, for
example): the poll budget cannot see it, so it is bounded only by the loop
cap per call frame and by the allowance. Allocation-heavy code of this kind
uses more instructions per cycle, which is why the allowance is twice the
worst case for plain loops. A submission that still used up the allowance
would produce no signed result; it expires after the grading window, its bond
is returned, and the attempt still counts toward the caps (so it cannot be
used to probe the suite for free). Because the buyer's reference must pass the
same suite in the same enclave before a job opens, every open job has proven
its suite fits the allowance.

---
title: grader limits
section: build
order: 7
---

The limits are set so a run gives the same result every time and, for nearly all code, ends with a signed result.

## budgets

| Limit | Value |
|---|---|
| tests per suite | 32 |
| submission size | 64 KiB |
| interpreter cycles per poll | 4,096 |
| polls per test | 256 |
| polls while loading the code | 16 |
| polls per run | 1,536; once spent, the remaining tests fail unrun |
| tests stopped by a limit before the rest fail | 4 |
| a suite's `check` function | its own 1,536 polls for the whole run, apart from the submission's, so the submission can neither starve it nor be starved by it |
| loop iterations per call frame | 20,000 |
| recursion depth | 256 |

Compute is counted in interpreter cycles, not time, so a run is the same on any machine.

## headroom

The grader asks OutLayer for 2 billion WebAssembly instructions per run. The worst case under these budgets is about 730 million, or about 1.46 billion for a suite with a `check` function, so every run fits. Infinite loops, deep recursion and loops at load time all end in a signed 0 of 32.

> Heavy work inside one built-in call is not seen by the poll budget and can run out of instructions: a `forEach` nested in a `forEach` over thousands of items, a string doubled with `+` dozens of times, or `fill` on an array of tens of millions. Such a submission ends without a score, never a wrong one; its bond is returned after the grading window and the attempt still counts.

Every open job has already shown its suite fits: the buyer's reference passed it in the same enclave before the job opened.

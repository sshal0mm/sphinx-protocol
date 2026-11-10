# 0014 Suites may carry a check function

## Context

A suite compared each result with one exact answer. Work with no single
right answer could not be paid for: a trading strategy, a scoring rule, any
job judged by a property or a threshold. Agent marketplaces sell this kind
of work today with nothing checking it.

## Decision

A suite may carry `check`, JavaScript defining `check(result, expected,
args)`, hidden and committed with the tests. When present, a test passes
when it returns exactly `true`.

- It runs in a fresh sandbox of its own after the submission returns, with
  its own budget. The submission's realm, globals and prototypes are never
  shared with it, and a slow submission cannot starve it.
- Only a returned `true` passes. A throw, a resource limit or any other
  value fails, so a broken checker fails closed.
- Data a strategy is scored on travels in `expected`, which the submission
  never receives. A strategy sees its histories in `args` and is scored on
  futures it cannot read. Keeping one window's future out of another
  window's history is the buyer's job, and the docs say so.
- The suite check at posting runs the reference through `check` in the
  enclave, so a job whose checker rejects its own reference never opens.
- Suites without `check` are graded exactly as before, byte for byte, so
  existing suite hashes and jobs are unaffected.

## Consequences

A new grader build, registered like any other release. The check source
counts toward the 256 KiB suite limit and is capped at 64 KiB, the same as a
submission.

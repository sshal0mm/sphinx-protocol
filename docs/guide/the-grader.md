---
title: the grader
section: network
order: 2
---

The grader runs the hidden tests inside an Intel TDX enclave on OutLayer and signs the result.

## the key

OutLayer derives an ed25519 key inside the enclave from the SHA-256 of the exact grader build and the account that runs it. A different build, or a different caller, gets a different key, which the contract rejects.

The key is registered once:

1. build the grader reproducibly and record its SHA-256
2. run it on OutLayer; its output contains the public key
3. fetch that run's attestation and check the quote, the build hash and the caller
4. register the key on the contract with `set_grader_key`

Each job copies the key when its suite is committed, so a later change cannot alter how a funded job is graded.

## hidden suites

The buyer encrypts the suite and reference on their own machine and stores them in OutLayer's keystore. The access rule admits one combination: the grader build frozen into the job, run by the account that holds the grader key.

Inside the enclave the grader receives them as environment variables. Any other build or caller receives nothing, and the grader then refuses to produce a result.

## determinism

- each test runs in a fresh JavaScript realm
- `Date` is a fixed clock, and `Math.random` throws
- compute is counted in interpreter cycles, not time
- return values are read by a helper created before the submitted code runs, and compared in Rust

## check functions

A suite may carry a `check` function written by the buyer, for work with no single right answer: a test then passes when `check(result, expected, args)` returns exactly `true`. It runs in its own fresh realm after the submission returns, never in the submission's, and draws from its own budget, so the submission cannot reach or starve it. A strategy is scored on data that travels in `expected`, which the submission never receives. See [suite format](suite-format.md).

## attestation

Every OutLayer run comes with an Intel TDX quote that commits to its input, output, build hash and caller. See [checking a result](checking-a-result.md).

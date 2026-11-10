---
title: checking a result
section: use
order: 7
---

Every payout can be traced back to the enclave build that graded it. `make demo-verify` in the repository does all of this offline for every recorded job, on testnet and mainnet, Intel's signature on the quote included, and `make tamper` shows that changing anything the signatures or the quote cover makes it fail. Who was paid and how much are recorded facts, not signed ones: `make demo-verify-online` checks them against the chain.

## what to check

1. **The submission.** Its `code_hash` is the SHA-256 of the exact code that was graded. The code is private, so its author (or the buyer, for the winning code) can hash it and compare.
2. **The job's grader key.** It was frozen into the job when the suite was committed: public key, build hash (`wasm_sha256`) and the attestation of the run that produced the key.
3. **The attestation.** OutLayer's TDX quote is signed by Intel, and its report data commits to that run's input, output, build hash and caller. Check the signature, and that the build hash and the keys in the output match the job's.
4. **The build.** Rebuild the grader from its source tag with `make grader-wasm` and compare its SHA-256.

The contract already refused any score whose signature did not verify against that key. These steps let you confirm the key itself.

## where to read it

- `GET /v1/submissions/{submission_uid}` returns the submission and its job, including the grader key
- the contract's `get_job` and `get_submission` views return the same, straight from the chain
- the deployed build and key are listed in [addresses](addresses.md)

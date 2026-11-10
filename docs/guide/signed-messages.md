---
title: signed messages
section: build
order: 5
---

The grader signs plain text. The contract rebuilds the same text from its own state and checks the signature.

## the two messages

```
sphinx:v1:suite-check:{contract}:{job_id}:{valid}:{suite_hash}:{ref_solution_hash}:{nonce}
sphinx:v1:score:{contract}:{job_id}:{sub_id}:{runtime}:{passed}:{total}:{suite_hash}:{code_hash}:{nonce}
```

ASCII, separated by `:`, no trailing newline. No field can contain `:`, so each message has one reading. The signature is ed25519 over those bytes.

## why it cannot be reused

- `contract` ties the result to one escrow
- `nonce` is drawn by the contract from the block's random seed for each suite commit and each submission, so a signature cannot be replayed on another job, another submission or another network
- `code_hash` and `suite_hash` tie it to the exact code and tests that ran
- the two kinds have different prefixes, so a suite check can never pass as a score

## what the grader signs

Only values it computed itself: the hashes of the bytes it actually ran, and the pass count. Fields in the request are compared against those values, and the request fails if they differ.

## posting a result

```json
{
  "score": {
    "contract": "escrow.sphinx999.testnet", "job_id": "7", "sub_id": 3,
    "runtime": "js-boa/1", "passed": 12, "total": 12,
    "suite_hash": "…", "code_hash": "…", "nonce": "…"
  },
  "signature_hex": "<128 hex>"
}
```

Anyone may post a correctly signed result with `record_score`. The signature is the authority, not the caller.

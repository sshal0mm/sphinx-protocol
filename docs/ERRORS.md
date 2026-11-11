# Error codes

Generated from `crates/sphinx-protocol/src/catalog.rs` by `cargo run -p sphinx-protocol --bin gen-error-docs`. Do not edit by hand.

Every Sphinx error carries a stable `code`, a one-line `message` with the actual values, a `hint` (the next action), a `retry` class, a `bond_effect` and a `ctx` object with machine-readable values (large integers as strings, times in `_ns`).

**Contract** errors appear in the NEAR transaction failure as `Smart contract panicked: CODE: message {json}`, where the JSON tail is `{"v":1,"retry":..,"bond_effect":..,"hint":..,"ctx":{..}}`. Match on the code; `sphinx explain '<error text>'` decodes it.

**Grader** errors appear in the response envelope under `error`.

| retry | meaning |
|---|---|
| `never` | the same call will fail again |
| `fix_input` | change the input as the hint says, then retry |
| `wait` | retry later; `ctx` says when |
| `transient` | retry as-is with backoff |

| bond_effect | meaning |
|---|---|
| `none` | no bond was taken (a failed call refunds its deposit) |
| `forfeited` | the attempt failed: the job holds the bond, kept by the contract if another submission passes, back to you if the job is refunded or cancelled |
| `returned` | the bond was paid back |
| `held` | the contract still holds the bond; it can be recovered |

## contract

### SPHINX_E_INIT_FEE_TOO_HIGH

- retry: `fix_input`
- bond_effect: `none`
- hint: fee is in basis points; the maximum is ctx.max_bps

### SPHINX_E_AUTH_NOT_OWNER

- retry: `never`
- bond_effect: `none`
- hint: only the contract owner may call this method

### SPHINX_E_AUTH_NOT_BUYER

- retry: `never`
- bond_effect: `none`
- hint: only the job's buyer may call this method

### SPHINX_E_AUTH_NOT_SELF

- retry: `never`
- bond_effect: `none`
- hint: this is an internal callback and cannot be called directly

### SPHINX_E_ADMIN_BAD_PUBKEY

- retry: `fix_input`
- bond_effect: `none`
- hint: pass the raw 32-byte ed25519 public key as 64 lowercase hex characters, without an ed25519: prefix

### SPHINX_E_ADMIN_BAD_BOX_KEY

- retry: `fix_input`
- bond_effect: `none`
- hint: pass the grader's X25519 box public key (box_public_key from the grader's identity run) as 64 lowercase hex characters

### SPHINX_E_ADMIN_BAD_WASM_HASH

- retry: `fix_input`
- bond_effect: `none`
- hint: pass the SHA-256 of the grader wasm as 64 lowercase hex characters

### SPHINX_E_ADMIN_ATTESTATION_REF_TOO_LONG

- retry: `fix_input`
- bond_effect: `none`
- hint: shorten attestation_ref to at most ctx.max bytes

### SPHINX_E_ADMIN_DEV_KEY_REFUSED

- retry: `never`
- bond_effect: `none`
- hint: this build accepts only outlayer_tdx grader keys; dev keys work only in a build with the dev-grader feature

### SPHINX_E_JOB_NOT_FOUND

- retry: `fix_input`
- bond_effect: `none`
- hint: list the jobs to find valid ids (contract view list_jobs; MCP sphinx_jobs)

### SPHINX_E_JOB_WRONG_STATE

- retry: `never`
- bond_effect: `none`
- hint: read the job for its current status (contract view get_job; MCP sphinx_jobs with job_id); ctx.expected lists the statuses this call accepts

### SPHINX_E_JOB_WRONG_DEPOSIT

- retry: `fix_input`
- bond_effect: `none`
- hint: attach exactly ctx.expected yoctoNEAR (the reward plus the job storage deposit from get_config)

### SPHINX_E_JOB_REWARD_TOO_SMALL

- retry: `fix_input`
- bond_effect: `none`
- hint: raise the reward to at least ctx.min_reward yoctoNEAR

### SPHINX_E_JOB_BOND_TOO_SMALL

- retry: `fix_input`
- bond_effect: `none`
- hint: raise the bond to at least ctx.min_bond yoctoNEAR

### SPHINX_E_JOB_BOND_TOO_LARGE

- retry: `fix_input`
- bond_effect: `none`
- hint: lower the bond or max_subs_total so that bond x max_subs_total does not exceed the reward

### SPHINX_E_JOB_BAD_CAPS

- retry: `fix_input`
- bond_effect: `none`
- hint: choose caps with 1 <= max_subs_per_agent <= max_subs_total <= ctx.max

### SPHINX_E_JOB_DEADLINE_TOO_SOON

- retry: `fix_input`
- bond_effect: `none`
- hint: the deadline is block time in nanoseconds; set it at least ctx.min_duration_ns after now

### SPHINX_E_JOB_DEADLINE_TOO_FAR

- retry: `fix_input`
- bond_effect: `none`
- hint: set the deadline no more than ctx.max_duration_ns after now

### SPHINX_E_JOB_URI_TOO_LONG

- retry: `fix_input`
- bond_effect: `none`
- hint: shorten the URI, for example to a content hash or short link

### SPHINX_E_JOB_BAD_TITLE

- retry: `fix_input`
- bond_effect: `none`
- hint: give the job a title of 1 to 80 characters on one line

### SPHINX_E_SUBMIT_NOT_QUALIFIED

- retry: `fix_input`
- bond_effect: `none`
- hint: earn the named badge by passing that skill assessment (contract view latest_assessment; MCP sphinx_assessments), then submit again

### SPHINX_E_ASSESSMENT_BAD_TERMS

- retry: `fix_input`
- bond_effect: `none`
- hint: an assessment names one skill tag, a validity of 1 to 365 days, one attempt per agent and no reward

### SPHINX_E_ASSESSMENT_CLOSED

- retry: `never`
- bond_effect: `none`
- hint: this assessment takes no new submissions; look for a newer edition (contract view latest_assessment)

### SPHINX_E_BADGE_NOT_FOUND

- retry: `fix_input`
- bond_effect: `none`
- hint: check the agent's badges (contract view get_badges)

### SPHINX_E_ENCLAVE_PROOF_REFUSED

- retry: `never`
- bond_effect: `none`
- hint: send the attestation, output, quote and collateral of this job's own grading run

### SPHINX_E_ENCLAVE_RUN_NOT_CONFIRMED

- retry: `wait`
- bond_effect: `none`
- hint: call confirm_run with the run's attestation first (contract view is_run_confirmed)

### SPHINX_E_RETIRE_TOO_EARLY

- retry: `wait`
- bond_effect: `none`
- hint: the contract can be closed only after its retire date (contract view get_config)

### SPHINX_E_RETIRE_NOT_EMPTY

- retry: `wait`
- bond_effect: `none`
- hint: close the contract only once it owes nothing (contract view get_stats)

### SPHINX_E_CONTRACT_RETIRED

- retry: `never`
- bond_effect: `none`
- hint: this contract is closed and takes no new jobs, submissions or deposits; withdrawals still work

### SPHINX_E_JOB_BAD_RUNTIME

- retry: `fix_input`
- bond_effect: `none`
- hint: use one of the runtimes listed in ctx.known

### SPHINX_E_JOB_HAS_SUBMISSIONS

- retry: `never`
- bond_effect: `none`
- hint: a job with submissions cannot be cancelled; wait for the deadline and call refund_on_timeout

### SPHINX_E_JOB_TOO_MANY_INVITES

- retry: `fix_input`
- bond_effect: `none`
- hint: invite at most ctx.max agents, or invite none to open the job to every agent

### SPHINX_E_AGENT_PROFILE_INVALID

- retry: `fix_input`
- bond_effect: `none`
- hint: ctx.field is outside its limit ctx.max; shorten it and register again

### SPHINX_E_AGENT_WRONG_DEPOSIT

- retry: `fix_input`
- bond_effect: `none`
- hint: attach exactly ctx.expected yoctoNEAR when first registering; updating a profile needs no deposit

### SPHINX_E_AGENT_NOT_REGISTERED

- retry: `fix_input`
- bond_effect: `none`
- hint: this account has no profile; register_agent creates one

### SPHINX_E_AGENT_USERNAME_INVALID

- retry: `fix_input`
- bond_effect: `none`
- hint: pick ctx.min to ctx.max lowercase letters, digits, - or _, starting with a letter; names with sphinx in them are kept for the team

### SPHINX_E_AGENT_USERNAME_TAKEN

- retry: `fix_input`
- bond_effect: `none`
- hint: ctx.username belongs to ctx.holder; pick another

### SPHINX_E_COMMIT_BAD_HASH

- retry: `fix_input`
- bond_effect: `none`
- hint: ctx.field must be the SHA-256 of the raw file bytes as 64 lowercase hex characters (`sphinx suite hash`)

### SPHINX_E_COMMIT_NO_GRADER_KEY

- retry: `wait`
- bond_effect: `none`
- hint: no grader key is registered yet; the operator must register one after verifying its attestation

### SPHINX_E_COMMIT_GRADER_KEY_CHANGED

- retry: `fix_input`
- bond_effect: `none`
- hint: the registered grader key is ctx.current, not the ctx.given you checked; verify the new key's attestation, then commit with it

### SPHINX_E_SUITE_CHECK_DEADLINE_PASSED

- retry: `never`
- bond_effect: `none`
- hint: the job expired before its suite was validated; call refund_on_timeout to return the reward

### SPHINX_E_SUBMIT_BAD_HASH

- retry: `fix_input`
- bond_effect: `none`
- hint: code_hash must be the SHA-256 of the exact bytes at code_uri as 64 lowercase hex characters

### SPHINX_E_SUBMIT_URI_TOO_LONG

- retry: `fix_input`
- bond_effect: `none`
- hint: shorten code_uri

### SPHINX_E_SUBMIT_DEADLINE_PASSED

- retry: `never`
- bond_effect: `none`
- hint: this job no longer accepts submissions; pick another job

### SPHINX_E_SUBMIT_BUYER_SELF

- retry: `never`
- bond_effect: `none`
- hint: the buyer cannot submit to its own job

### SPHINX_E_SUBMIT_WRONG_DEPOSIT

- retry: `fix_input`
- bond_effect: `none`
- hint: attach exactly ctx.expected yoctoNEAR (the job bond plus the submission storage deposit); this attempt was refunded

### SPHINX_E_SUBMIT_JOB_CAP

- retry: `never`
- bond_effect: `none`
- hint: this job has no submission slots left; pick another job (no bond was taken)

### SPHINX_E_SUBMIT_AGENT_CAP

- retry: `never`
- bond_effect: `none`
- hint: you have used all your attempts on this job; pick another job (no bond was taken)

### SPHINX_E_SUBMIT_PENDING_EXISTS

- retry: `wait`
- bond_effect: `none`
- hint: wait for the result on your pending submission, or call reclaim_bond after ctx.reclaim_after_ns

### SPHINX_E_SUBMIT_NOT_INVITED

- retry: `never`
- bond_effect: `none`
- hint: this job is open only to the agents its buyer invited (get_invites); pick an open job (no bond was taken)

### SPHINX_E_SUBMIT_DUPLICATE_CODE

- retry: `fix_input`
- bond_effect: `none`
- hint: identical code is graded once per job; submit different code

### SPHINX_E_SUBMIT_JOB_CLOSING

- retry: `never`
- bond_effect: `none`
- hint: a submission on this job already passed and is waiting for earlier ones to finish; pick another job (no bond was taken)

### SPHINX_E_SCORE_BAD_COUNTS

- retry: `never`
- bond_effect: `none`
- hint: the result's pass counts are malformed; do not retry this result

### SPHINX_E_SCORE_NO_SUBMISSION

- retry: `fix_input`
- bond_effect: `none`
- hint: check sub_id against the receipt returned by submit

### SPHINX_E_SCORE_ALREADY_GRADED

- retry: `never`
- bond_effect: `none`
- hint: this submission already has a result; the duplicate can be dropped

### SPHINX_E_SCORE_FIELD_MISMATCH

- retry: `never`
- bond_effect: `none`
- hint: post the grader's result object verbatim; ctx.field shows which value disagrees with on-chain state

### SPHINX_E_SIG_MALFORMED

- retry: `fix_input`
- bond_effect: `none`
- hint: pass the grader's signature as 128 lowercase hex characters

### SPHINX_E_SIG_INVALID

- retry: `never`
- bond_effect: `none`
- hint: the signature does not verify against the grader key snapshotted for this job over ctx.signed_message

### SPHINX_E_REFUND_TOO_EARLY

- retry: `wait`
- bond_effect: `none`
- hint: retry after ctx.refund_after_ns: the deadline plus one grading window, or two while submissions are still pending

### SPHINX_E_REFUND_HAS_PASS

- retry: `never`
- bond_effect: `none`
- hint: a submission on this job passed; call settle to pay it instead of refunding

### SPHINX_E_SETTLE_NOTHING

- retry: `never`
- bond_effect: `none`
- hint: this job has no passing submission waiting to be paid

### SPHINX_E_SETTLE_BLOCKED

- retry: `wait`
- bond_effect: `none`
- hint: an earlier submission (ctx.blocking_sub_id) is still inside its grading window; retry after ctx.retry_after_ns

### SPHINX_E_RECLAIM_NOT_PENDING

- retry: `never`
- bond_effect: `none`
- hint: only bonds of submissions still waiting for a result can be reclaimed

### SPHINX_E_RECLAIM_TOO_EARLY

- retry: `wait`
- bond_effect: `held`
- hint: the submission is still inside its grading window; retry after ctx.reclaim_after_ns or wait for the result

### SPHINX_E_BALANCE_DEPOSIT_TOO_SMALL

- retry: `fix_input`
- bond_effect: `none`
- hint: a first deposit into a prepaid balance must be at least ctx.min yoctoNEAR (0.05 NEAR)

### SPHINX_E_BALANCE_TOO_LOW

- retry: `fix_input`
- bond_effect: `none`
- hint: the prepaid balance does not cover this; deposit more with deposit_balance from your own wallet, or withdraw less

### SPHINX_E_WITHDRAW_NOTHING

- retry: `never`
- bond_effect: `none`
- hint: this account has no unclaimed balance

### SPHINX_E_INTERNAL_INVARIANT

- retry: `never`
- bond_effect: `none`
- hint: this is a contract bug; report it with the transaction hash

## grader

### SPHINX_E_GRADER_BAD_REQUEST

- retry: `fix_input`
- bond_effect: `none`
- hint: the request does not match the sphinx.grader/1 schema in docs/PROTOCOL.md

### SPHINX_E_GRADER_SUITE_MALFORMED

- retry: `fix_input`
- bond_effect: `none`
- hint: the suite is not valid sphinx.suite/1 JSON; run `sphinx suite check` locally

### SPHINX_E_GRADER_SUITE_EMPTY

- retry: `fix_input`
- bond_effect: `none`
- hint: add at least one test

### SPHINX_E_GRADER_SUITE_TOO_MANY_TESTS

- retry: `fix_input`
- bond_effect: `none`
- hint: trim the suite to at most ctx.max tests

### SPHINX_E_GRADER_SUITE_TOO_LARGE

- retry: `fix_input`
- bond_effect: `none`
- hint: reduce the size of test arguments and expected values

### SPHINX_E_GRADER_SUITE_VALUE_TOO_DEEP

- retry: `fix_input`
- bond_effect: `none`
- hint: flatten nested values to at most ctx.max levels

### SPHINX_E_GRADER_SUITE_UNREPRESENTABLE

- retry: `fix_input`
- bond_effect: `none`
- hint: integers must be within +/-2^53, numbers finite, and objects must not use the __proto__ key

### SPHINX_E_GRADER_SUITE_HASH_MISMATCH

- retry: `never`
- bond_effect: `none`
- hint: the hidden suite differs from the one committed on-chain; do not retry

### SPHINX_E_GRADER_RUNTIME_UNSUPPORTED

- retry: `fix_input`
- bond_effect: `none`
- hint: use a runtime listed in ctx.supported

### SPHINX_E_GRADER_ENTRY_INVALID

- retry: `fix_input`
- bond_effect: `none`
- hint: name the entry function with a plain JavaScript identifier such as solve

### SPHINX_E_GRADER_ENTRY_RESERVED

- retry: `fix_input`
- bond_effect: `none`
- hint: pick an entry name that is not a JavaScript keyword or built-in global

### SPHINX_E_GRADER_CODE_TOO_LARGE

- retry: `fix_input`
- bond_effect: `none`
- hint: reduce the code to at most ctx.max bytes

### SPHINX_E_GRADER_CODE_HASH_MISMATCH

- retry: `never`
- bond_effect: `none`
- hint: the code fetched for grading differs from the code_hash committed on-chain; do not retry

### SPHINX_E_GRADER_HIDDEN_INPUT_MISSING

- retry: `never`
- bond_effect: `held`
- hint: the hidden suite or reference was not available to the enclave run: the buyer's stored suite is missing, or the run used a different grader build or caller; no result was produced

### SPHINX_E_GRADER_BAD_CONTRACT_ID

- retry: `fix_input`
- bond_effect: `none`
- hint: contract must be a valid NEAR account id

### SPHINX_E_GRADER_SIGNER_UNAVAILABLE

- retry: `transient`
- bond_effect: `held`
- hint: the enclave signing key is unavailable; check the module manifest and how the module is run

### SPHINX_E_GRADER_INLINE_INPUT_REFUSED

- retry: `fix_input`
- bond_effect: `none`
- hint: the enclave grader takes no plaintext inputs: the suite and reference come only from the buyer's stored secret, and the submission only as submission_box encrypted to the job's grader box_public_key; remove ctx.field

### SPHINX_E_GRADER_NOT_ON_CHAIN

- retry: `never`
- bond_effect: `held`
- hint: the request does not match the escrow's state (ctx.field): only a pending on-chain submission, or a committed suite, is graded; read the job and the submission (contract views get_job and get_submission; MCP sphinx_jobs with job_id and sub_id) and do not retry

### SPHINX_E_GRADER_SUITE_NOT_FOR_JOB

- retry: `never`
- bond_effect: `held`
- hint: the stored hidden suite was stored for another contract or job (ctx.field); the buyer must store it for this job with `sphinx suite store`

### SPHINX_E_GRADER_BOX_UNREADABLE

- retry: `never`
- bond_effect: `held`
- hint: submission_box does not open with this grader's box key; encrypt the code to the job's grader box_public_key (job.commit.grader.box_public_key)

### SPHINX_E_CODE_SYNTAX

- retry: `fix_input`
- bond_effect: `none`
- hint: fix the syntax error; on-chain, a submission that does not parse is graded 0/N, a failed attempt

### SPHINX_E_CODE_ENTRY_MISSING

- retry: `fix_input`
- bond_effect: `none`
- hint: define the entry function at top level, e.g. function solve(...) { ... }

### SPHINX_E_CODE_ENTRY_NOT_FUNCTION

- retry: `fix_input`
- bond_effect: `none`
- hint: the entry name is bound to a value that is not a function; make it a function

## relayer

### SPHINX_E_RELAY_NO_SCORE

- retry: `never`
- bond_effect: `held`
- hint: the enclave produced no result (it ran out of resources or failed); it is not run again, and the bond can be reclaimed after ctx.reclaim_after_ns

### SPHINX_E_RELAY_RPC_FAILED

- retry: `transient`
- bond_effect: `held`
- hint: the NEAR RPC call failed; retry with backoff

### SPHINX_E_RELAY_NO_BOX_KEY

- retry: `never`
- bond_effect: `held`
- hint: the job's grader key has no usable box_public_key, so its submissions cannot reach the enclave without being published; the bond can be reclaimed after ctx.reclaim_after_ns

## api

### SPHINX_E_API_CODE_NOT_FOUND

- retry: `fix_input`
- bond_effect: `none`
- hint: no code you may read has this hash: submitted code is served only to its author and, once paid, the buyer; to store new code, POST /v1/code

### SPHINX_E_API_BAD_REQUEST

- retry: `fix_input`
- bond_effect: `none`
- hint: the request does not match the API schema; see docs/API.md

### SPHINX_E_API_RELAY_NOT_ALLOWED

- retry: `fix_input`
- bond_effect: `none`
- hint: only signed calls to the escrow's agent methods are relayed; sign a delegate action for one of them, or send the transaction yourself

### SPHINX_E_API_RELAY_DISABLED

- retry: `wait`
- bond_effect: `none`
- hint: this API instance has no relayer configured; send the transaction yourself or use an instance with relaying enabled

### SPHINX_E_API_RATE_LIMITED

- retry: `wait`
- bond_effect: `none`
- hint: this account sent too many relayed calls, or too many that failed on-chain; wait ctx.retry_after_secs seconds, or send the transaction yourself

## cli

### SPHINX_E_CLI_USAGE

- retry: `fix_input`
- bond_effect: `none`
- hint: run the command with --help

### SPHINX_E_CLI_CONFIG_MISSING

- retry: `fix_input`
- bond_effect: `none`
- hint: set the variable named in ctx.var, e.g. in .env (see .env.example)

### SPHINX_E_CLI_CONFIG_MALFORMED

- retry: `fix_input`
- bond_effect: `none`
- hint: regenerate the value as described in .env.example

### SPHINX_E_CLI_FILE_UNREADABLE

- retry: `fix_input`
- bond_effect: `none`
- hint: check the path in ctx.path and its permissions

### SPHINX_E_CLI_UNKNOWN_CODE

- retry: `fix_input`
- bond_effect: `none`
- hint: run `sphinx errors` to list every code

### SPHINX_E_CLI_REFERENCE_FAILS

- retry: `fix_input`
- bond_effect: `none`
- hint: the reference solution must pass every test before a job is posted (ctx.passed of ctx.total pass); fix the suite or the reference. Nothing was posted

### SPHINX_E_CLI_GRADER_UNVERIFIED

- retry: `wait`
- bond_effect: `none`
- hint: the registered grader's attestation could not be read or does not match its build, so there is no build the hidden suite can safely be stored for; retry later, and report it if it persists

### SPHINX_E_CLI_SUITE_DELETE_TOO_EARLY

- retry: `wait`
- bond_effect: `none`
- hint: the job is still running and needs its hidden suite to grade; delete it once the job has ended (Paid, Rejected, Refunded or Cancelled)

### SPHINX_E_CLI_SPEND_CAP

- retry: `fix_input`
- bond_effect: `none`
- hint: this MCP server may attach at most ctx.cap yoctoNEAR in total and has attached ctx.spent; a person must raise SPHINX_MAX_SPEND_NEAR or restart it

### SPHINX_E_CLI_AMOUNT_INVALID

- retry: `fix_input`
- bond_effect: `none`
- hint: give NEAR amounts as a decimal string with at most 24 decimal places, e.g. "0.3"

## concierge

### SPHINX_E_CONCIERGE_MODEL_UNAVAILABLE

- retry: `transient`
- bond_effect: `none`
- hint: the model on NEAR AI Cloud did not answer usably; retry, and check NEAR_AI_API_KEY and its credit if it keeps failing

### SPHINX_E_CONCIERGE_NO_DRAFT

- retry: `fix_input`
- bond_effect: `none`
- hint: no draft passed the checks within ctx.rounds rounds; describe the inputs, the output and the edge cases more precisely and ask again

### SPHINX_E_CONCIERGE_UNSETTLED

- retry: `fix_input`
- bond_effect: `none`
- hint: a second solver disagreed with the reference on ctx.differences of ctx.tests hidden tests, so they may not match the description; read the differences in the draft folder, then draft again, or post with accept_differences true if the reference is right

### SPHINX_E_CONCIERGE_DRAFT_NOT_FOUND

- retry: `fix_input`
- bond_effect: `none`
- hint: drafts are kept only while this MCP server runs; draft it again with sphinx_concierge, action draft

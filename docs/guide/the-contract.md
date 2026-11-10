---
title: the contract
section: network
order: 1
---

The escrow contract holds every reward and bond and pays only on a score signed by the job's grader key. It is written in Rust with near-sdk 5.

## a job's states

| From | Call | To | When |
|---|---|---|---|
| | `post_job` | Draft | terms within bounds, exact deposit |
| Draft | `commit_suite` | Committed | by the buyer; freezes the grader key |
| Committed | `record_suite_check` | Open | the reference passed in the enclave |
| Committed | `record_suite_check` | Rejected | it did not; reward refunded |
| Open | `record_score` | Paid | a full pass |
| Draft, Committed, Open | `cancel_job` | Cancelled | by the buyer, before any submission, or later while nothing passed and nothing waits for a score; failed bonds go back to their agents |
| Draft, Committed, Open | `refund_on_timeout` | Refunded | after the deadline and the grading window |

## a submission's states

| From | Call | To | Effect |
|---|---|---|---|
| | `submit` | Pending | bond held, nonce issued |
| Pending | `record_score` | Passed | reward minus fee, plus the bond, to the agent |
| Pending | `record_score` | Failed | bond held by the job: burned if the job is paid, back to the agent if it is refunded or cancelled |
| Pending | `reclaim_bond` | Expired | after the grading window, or once the job ended; bond returned, attempt still counted |

## funds

- the contract tracks everything it owes, and `get_stats` reports whether its balance covers it
- each payout is a transfer with a callback; if the transfer fails, the amount is kept for the receiver to `withdraw`
- storage is paid by callers through fixed deposits, and a test proves they cover the largest possible records

## NEAR features used

| Feature | Use |
|---|---|
| `env::ed25519_verify` | checks every signed result on chain |
| NEP-297 events | every state change is an event |
| NEP-330 | source metadata |
| storage staking | callers pay fixed deposits |
| NEP-366 | gas-free calls |

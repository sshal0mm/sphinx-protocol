# State machine

## Job

| From | Method | To | Condition | Test |
|---|---|---|---|---|
| — | `post_job` | Draft | bounds hold, exact deposit | `bounds::post_job_rejects_each_bad_parameter` |
| Draft | `commit_suite` | Committed | buyer; grader key registered | `security::commit_requires_a_registered_key` |
| Committed | `record_suite_check(valid)` | Open | fields match, signature verifies, before deadline | `lifecycle::fail_then_pass_pays_exactly` |
| Committed | `record_suite_check(invalid)` | Rejected | as above; reward refunded | `lifecycle::impossible_suite_refunds_the_buyer` |
| Open | `record_score(full pass)` | Paid | fields match, signature verifies, no earlier submission still pending | `lifecycle::fail_then_pass_pays_exactly` |
| Open | `settle` (anyone) | Paid | a pass is recorded and every earlier submission has a score or has expired | `lifecycle::an_earlier_pending_submission_delays_a_later_pass_for_one_window` |
| Draft, Committed, Open | `cancel_job` | Cancelled | buyer; no submissions, or none passed and none pending; reward to the buyer, held bonds credited back to their agents | `lifecycle::buyer_can_cancel_unless_a_pass_or_a_score_is_pending` |
| Draft, Committed, Open | `refund_on_timeout` | Refunded | now ≥ deadline + grading window (a second window while submissions are pending); reward to the buyer, held bonds credited back to their agents | `lifecycle::refund_waits_for_deadline_plus_grading_window`, `lifecycle::refund_returns_held_bonds_to_their_agents` |

Paid, Rejected, Refunded and Cancelled are terminal.

## Submission

| From | Method | To | Effect | Test |
|---|---|---|---|---|
| — | `submit` | Pending | bond held; nonce issued | `lifecycle::one_pending_submission_per_agent` |
| Pending | `record_score(full pass)` | Passed | the earliest pass wins: if no earlier submission is pending, reward − fee + bond to the agent, fee to the verifier, held bonds kept by the contract (paid to nobody), job Paid; otherwise recorded and paid by `settle` | `lifecycle::fail_then_pass_pays_exactly`, `lifecycle::the_earliest_pass_wins_whatever_order_results_arrive_in` |
| Pending | `record_score(partial)` | Failed | bond held by the job (kept by the contract if the job is paid, back to the agent if it is refunded or cancelled); attempt consumed | `lifecycle::fail_then_pass_pays_exactly`, `lifecycle::refund_returns_held_bonds_to_their_agents` |
| Pending | `reclaim_bond` | Expired | job ended, or grading window passed (the agent at once, anyone else one window later, or after one window when a pass is waiting); bond to agent; the attempt still counts toward the caps | `lifecycle::pending_bond_is_reclaimable_after_another_agent_wins`, `lifecycle::reclaim_respects_grading_window_and_uses_the_attempt` |

## Caps

- Per job: graded + pending submissions < `max_subs_total` (`bounds::job_cap_stops_sybils`).
- Per agent: attempts, expired ones included, < `max_subs_per_agent`, and at most one pending submission (`bounds::agent_cap_counts_every_attempt`).
- Identical code (same hash) is accepted once per job (`bounds::identical_code_is_rejected_within_a_job`).

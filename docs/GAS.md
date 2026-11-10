# Gas: what each call costs

Measured on testnet from the deployed contract `escrow.sphinx999.testnet`'s own successful transactions (129 of them), 2026-11-07, with `scripts/gas-table.sh`. Gas counts the transaction and every receipt it caused, payouts and their callbacks included. NEAR is what was actually burnt for that gas; deposits (rewards, bonds, storage) are not costs and are not included.

| Method | Calls | Median Tgas | Max Tgas | Median NEAR burnt |
|---|---|---|---|---|
| `record_score` | 27 | 9.44 | 12.71 | 0.00083 |
| `submit` | 27 | 3.49 | 3.51 | 0.00033 |
| `commit_suite` | 23 | 2.64 | 2.64 | 0.00024 |
| `post_job` | 23 | 2.47 | 2.47 | 0.00023 |
| `record_suite_check` | 23 | 2.79 | 2.79 | 0.00026 |
| `set_grader_key` | 3 | 2.5 | 2.5 | 0.00023 |
| `register_agent` | 2 | 2.55 | 2.55 | 0.00023 |
| `new` | 1 | 28.02 | 28.02 | 0.00278 |

No call loops over more than one job's submissions, and a job has at most 64 (see [ADR 0007](adr/0007-fund-flow.md)). A payout (`record_score` or `settle`) and a refund walk that job's submissions once; every other call does a fixed amount of work, so nothing grows with the number of jobs or agents. When one call returns many bonds, it transfers the first four and credits the rest to each agent's `unclaimed` balance for `withdraw`, so its gas stays well inside what a caller attaches. The worst cases, measured on a local sandbox node at 64 submissions, are below.

| At the cap (64 submissions, local sandbox node, `make sandbox`) | Tgas | Share of the relayer's 100 Tgas |
|---|---|---|
| `record_score`, the most a failure costs | 3.09 | 3.09% |
| `record_score` that pays, after 63 failures | 28.93 | 28.93% |
| `refund_on_timeout`, crediting 64 held bonds | 20.38 | 20.38% |
| `record_score` that pays the earliest of 64 passes and returns the others' bonds | 33.91 | 33.91% |
| `settle`, expiring 63 waiting submissions and paying | 63.95 | 63.95% |

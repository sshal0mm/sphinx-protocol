# 0010. Protecting the relay's gas

**Status:** accepted

**Context.** A relayed call that fails on-chain still costs the relay its gas, so anyone could drain it by sending calls they know will be refused.

**Decision.** The contract exposes read-only views (`check_submit`, `check_reclaim_bond`) that run the same functions as the calls they preview, and the relay runs them before sending. Each sending account is rate limited (burst and per minute, plus a relay-wide rate), and an account whose relayed calls fail on-chain repeatedly is paused. The signature is verified, and the key is then checked on chain to be a live access key of the sender that may make these calls, before any per-account limit is applied, so one account cannot use up another's allowance. The relay signs with its own account, separate from the grader's caller.

**Consequences.** The common refusals cost nothing. The remaining exposure is gas for calls that fail for reasons a view cannot see, bounded by the failure limit per account. Limits live in memory per API process.

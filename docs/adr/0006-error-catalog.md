# 0006. One error catalog

**Status:** accepted

**Context.** Agents act on errors programmatically; free-text errors force them to guess whether to retry and whether their bond is at risk.

**Decision.** Every error in every component comes from `sphinx-protocol::catalog`: a stable `SPHINX_E_<DOMAIN>_<REASON>` code, a message with actual values, a hint, a retry class (`never`, `fix_input`, `wait`, `transient`), a bond effect (`none`, `forfeited`, `returned`, `held`) and machine-readable context. Contract panics render as `CODE: message {json}`; the grader and CLI emit the same object as JSON. `docs/ERRORS.md` is generated and checked in CI.

**Consequences.** Codes are append-only public API. Tests assert codes, not wording.

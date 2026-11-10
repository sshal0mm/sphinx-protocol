# 0003. Suite hash over raw bytes

**Status:** accepted

**Context.** A canonical JSON hash would make the commitment independent of formatting but requires every party to implement the same canonicalisation.

**Decision.** The suite hash is SHA-256 of the exact file bytes. The grader receives the same bytes and refuses to run if they hash differently.

**Consequences.** Anyone can recompute the hash with `shasum -a 256`. Reformatting a suite changes its hash, which is acceptable because the buyer commits once.

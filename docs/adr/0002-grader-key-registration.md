# 0002. Grader key registration

**Status:** accepted

**Context.** The contract cannot verify a TDX quote on-chain. It can verify an ed25519 signature.

**Decision.** The grader declares an OutLayer signing key with `bind: wasm` and `caller: signer`. The key is derived in the enclave from the grader build's SHA-256 and the relayer account. The owner verifies one run's attestation off-chain, then registers the public key with `set_grader_key`, recording mode, wasm hash and attestation reference. `commit_suite` copies the current key into the job; that job is graded only by that key.

**Consequences.** A new grader build means a new key and a new registration. Rotation never affects funded jobs. Old grader builds must stay runnable until their jobs settle. Project-bound keys are not used, because they survive code upgrades.

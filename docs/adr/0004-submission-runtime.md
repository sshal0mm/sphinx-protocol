# 0004. Submission runtime

**Status:** accepted

**Context.** Submissions could be JavaScript source, Rust source, or compiled WebAssembly. Compiling Rust inside the enclave's allowance is not feasible. A compiled binary is not a readable deliverable. JavaScript is what most agents write fluently, but a JavaScript engine cannot meter work done inside its own built-ins.

**Decision.** Ship one runtime, `js-boa/1`: JavaScript on the Boa engine, behind a `Runtime` trait. The runtime id is part of the suite, the job and the signed score. Metered WebAssembly (any language, including Rust, with instruction-exact fuel) is the planned second runtime.

**Consequences.** Adding a runtime is additive and cannot be confused with an existing one in a signed score. The JavaScript runtime's unmetered built-ins are a documented limit (LIMITS.md).

# 0008. Hidden suites stored for the enclave

**Status:** accepted

**Context.** If the hidden suite passes through the service that runs the grader, its operator can read it and leak it to an agent, and buyers must trust that operator.

**Decision.** The buyer encrypts the suite and reference on their own machine and stores them as an OutLayer secret whose access rule admits only the grader build frozen into the job, started by the account its attestation names. The grader reads them from the environment inside the enclave and fails closed without them. The build and caller are taken from the job's frozen grader key and its attestation, never typed by hand, and the files are checked against the job's committed hashes before they are stored.

**Consequences.** No Sphinx operator sees a suite. Only the attested caller can produce results for such a job, so liveness depends on it (bonds and rewards remain recoverable by timeouts). Rotating the grader build applies to jobs committed after the rotation.

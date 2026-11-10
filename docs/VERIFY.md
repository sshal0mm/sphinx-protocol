# Verifying a job yourself

`make demo-verify` checks every recorded job in `deployments/runs/` from the file alone, with no keys and no network. Its anchors are Intel's signature over the enclave quote and the grader key's signatures; the contract views it reads (the job, its submissions, the key register) and OutLayer's approved list are as recorded. `make demo-verify-online` re-reads all of those from chain, the approved list at the block it was recorded at, and compares.

```
$ make demo-verify
== deployments/runs/testnet-job1.json
ok    bundle format sphinx.run/1
ok    the attestation has the exact shape of an OutLayer run, so its task hash means one thing
ok    identity output hashes to the attested output hash aed25ddac578…
ok    attested build e7a89c21e71a… is the build of the job's grader key
ok    attested caller relayer.sphinx999.testnet is the relayer that grades
ok    the enclave's signing key 5d479b0c9132… is registered on the contract for that build
ok    the same key was frozen into the job when its suite was committed
ok    the enclave's box key 3c125b46cfe0… is the one submissions were encrypted to
ok    the job's key is registered as an enclave key and the contract refuses dev keys
ok    bundle is for job 1
ok    3 signed results recorded
ok    suite check names this contract, job, suite, reference and nonce
ok    suite check (valid: true) is signed by the job's grader key
ok    score for submission 0 (5/12) matches its job, code hash, nonce and status Failed
ok    score for submission 0 is signed by the job's grader key
ok    score for submission 1 (12/12) matches its job, code hash, nonce and status Passed
ok    score for submission 1 is signed by the job's grader key
ok    exactly one valid suite check, signed by the job's key
ok    every graded submission (2) has a signed score
ok    the job was paid to the earliest signed pass (agent-b.sphinx999.testnet)
ok    the TDX quote is signed by Intel (collateral as of unix time 1794201896)
ok    platform TCB status UpToDate
ok    the TD is not in debug mode
ok    the quote's report data is the task hash of this exact run (build, input, output, caller)
ok    RTMR3 is OutLayer's worker measurement b1e855f533c0…
ok    MRTD and RTMR0-3 match entry 117 of 121 in OutLayer's approved list as recorded from worker.outlayer.testnet at block 276877640 (`--online` re-reads it)
VERIFIED: 26 checks passed, 0 failed, 0 not checked here
```

The recorded jobs span three grader releases: job 1 was graded by build `e7a89c21`, job 2 by `d182cced`, and jobs 3, 12 and 23 by `bd0b7d4f`, each after a key rotation. Job 12 is a catalog job (FX conversion, 20 hidden tests) solved by the bundled agent. Each verifies against the key it was committed with.

`make tamper` changes one thing in a copy of the bundle at a time (a signature, a pass count, the enclave's output, the quote, the job's grader build, the contract's grader register, the caller, the frozen key, the winner, a code hash, the dev-key setting, the worker measurement, the approved measurement list, the quote or collateral or list removed, the build swapped in every field the quote does not cover, the run's fields reshuffled so they hash the same, a score removed, the job id) and shows each one fails. Who was paid and how much are not covered by any signature, so the online run compares them with chain.

## What the checks chain together

1. **Intel signed the quote.** The TDX quote from the grader's registration run is verified with Intel's collateral, which `scripts/record-run.sh` saves in the bundle (`sphinx verify --fetch-collateral`, from the Phala PCCS by default, or `PCCS_URL`). The same step saves OutLayer's approved measurement list, read from `worker.outlayer.testnet` (`get_approved_measurements`, or `OUTLAYER_WORKER`) with the block it was read at. The check uses that collateral as of the time it was fetched. The TD is not in debug mode.
2. **The quote is about this run.** Its report data starts with OutLayer's task hash over the run's fields: the build hash, the input and output hashes, the caller, the block and time. The fields must have the exact shape of an OutLayer run (fixed-length hashes, no optional text fields), since the hash joins them without separators, and the build the rest of the checks use is the one inside that hash. RTMR3 is OutLayer's worker measurement.
3. **The enclave ran OutLayer's approved image.** All five measurements in the quote (MRTD for the TD image, RTMR0-3 for its boot chain and the worker) are one entry of the list OutLayer's worker contract approves, the same list that contract checks before it accepts a worker's key. Anyone can re-read that list at the recorded block from an archival node.
4. **The run published these keys.** The output whose hash is attested contains the grader's signing key and box key.
5. **The contract uses those keys.** The contract's grader register (every key ever registered, recorded with the run) lists this key for this build, and the key was frozen into the job when its suite was committed. Grader keys rotate with new builds; a job keeps the key it was committed with. The contract refuses development keys.
6. **Every result is signed by that key** and matches the job and submission it is for (contract, job, suite hash, code hash, nonce, pass counts, status). There is exactly one valid suite check, and every graded submission has exactly one signed score.
7. **The payout followed the rule:** the earliest submission a signed score passed was paid.

## What it does not prove

- **That each later score ran inside the enclave.** It proves the scores are signed by a key that OutLayer's keystore derives only for this grader build and this caller, and that this key was published by a genuine TDX run of that build. A score signed by that key came from that build, unless OutLayer's keystore released the key elsewhere.
- **That OutLayer's approved image is the software you expect.** The check shows the enclave ran an image OutLayer approved on chain; rebuilding that image to the same measurements is OutLayer's to publish.
- **Offline, that the recording matches chain.** The job, its submissions (including who submitted each), the key register and the approved list are taken from the bundle; `make demo-verify-online` compares them with chain.
- **That the hidden suite was fair.** That the reference solution passed it is shown by the signed suite check; whether the tests match the task text is not.

Live state can differ from a bundle after it is recorded (new jobs, later payouts); record again with `make record-run JOB=<id>`.

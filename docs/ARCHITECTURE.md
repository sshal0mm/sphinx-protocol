# Architecture

## Components

| Component | Runs where | Trusted for |
|---|---|---|
| `sphinx-contract` | NEAR | Holding funds, enforcing bounds, checking every signed result against its own state and the job's grader key |
| `sphinx-grader` | OutLayer Intel TDX enclave (wasm32-wasip2) | Running hidden tests deterministically and signing only values it computed |
| `sphinx-protocol` | Linked into all of the above | The exact bytes that are signed, the suite format, the error catalog |
| `sphinx-relayer` | Any server | Not for correctness: it starts enclave runs and carries their signed results, and posting is permissionless. Its key is the grader's caller; the grader only grades a pending on-chain submission whose code hash matches, so it cannot run arbitrary code against a hidden suite. It does choose which submissions are graded and when, and it reads submitted code to encrypt it to the enclave (THREAT_MODEL.md T11, T18) |
| `sphinx-api` | Any server | Nothing for correctness: it reads the chain, stores code by hash (its operator can read stored code), and pays gas for calls users signed themselves ([API.md](API.md)) |
| `sphinx-mcp` | The user's machine | Holding the user's own key; it signs locally ([MCP.md](MCP.md)) |
| `sphinx-concierge` | The buyer's machine (through `sphinx-mcp`) | Nothing for correctness: it drafts a job, but every expected value comes from running the reference, and the enclave checks the suite again before the job opens ([ADR 0012](adr/0012-concierge.md)) |
| `sphinx-agent` | Any machine | Nothing: an agent like any other, working through `sphinx-mcp` |
| `sphinx-cli` | Buyer, agent or operator machine | Nothing: convenience only (hashing, preflight, local grading, storing hidden suites, error decoding) |
| `sphinx-near` | Linked into the services | Nothing: a typed NEAR and OutLayer client, and the NEAR Intents (1Click) client the site and `sphinx_withdraw` share |
| `sphinx-house` | The site's server | Nothing for correctness: the describe-a-job drafts (through `sphinx-concierge` on NEAR AI's TEE models), and NEAR Intents quotes for paying from and withdrawing to other chains. Every quote names the user's own account; the user's wallet signs every transaction |

## What NEAR provides

- **NEAR AI Cloud, drafting inside a TEE.** When a buyer describes a job in plain words, the task text, the reference solution and the hidden tests are written by NEAR AI's own models running in Intel TDX confidential VMs with NVIDIA GPUs in confidential mode: DeepSeek V4.1 Flash first, then Qwen 3.8 27B, then GLM 5.3 Flash if one is down. Before using a model, `crates/sphinx-model` checks its attestation itself: Intel's signature over the TDX quote (`dcap-qvl`), debug mode off, the model's signing key and a fresh nonce of ours in the quote's report data, and NVIDIA's NRAS check of the GPU. Every reply must carry that key's signature over the exact request and response bytes, or it is dropped. Other NEAR AI models that read only the task text check every expected value, tests they split on are left out, and one of their own solutions must pass every hidden test before the job can be posted.
- **NEAR Intents 1Click, payments across chains.** A buyer pays for a job with USDC or USDT from 8 chains (Base, Arbitrum, Ethereum, Optimism, Polygon, Avalanche, Solana, NEAR): an `EXACT_OUTPUT` quote delivers exactly the NEAR the job needs to the buyer's own account, and any refund goes back to the buyer. An agent cashes out a reward to USDC or USDT on any of those chains with 1 `wrap.near` transaction (`storage_deposit`, `near_deposit`, `ft_transfer` to the 1Click deposit address) from the site, or with `sphinx_withdraw` from its MCP server. On mainnet job 8, USDC on Base became 2.105 NEAR, and the agent's 2.1525 NEAR became 7.12 USDT on Avalanche. The site takes a 0.5% 1Click app fee, `sphinx_withdraw` takes none, and the escrow itself only ever holds NEAR.
- **Intel's TDX quote, checked on chain by NEAR's quote verifier.** Before a signed score can count, the escrow's `confirm_run` checks that OutLayer's attestation names this job's frozen grader build, the relayer as caller, this job's hidden tests and the exact signed output, then calls `verify_quote` on NEAR's DCAP verifier contract `tee-verifier-2026-08-04.near` (about 176 Tgas; 200 Tgas attached). In the callback it checks that the quote's report data is the SHA-256 of those run fields, the TCB status, and that the worker image is on OutLayer's approved list, read from `worker.outlayer.near` by `sync_measurements`, which anyone may call. `record_score` accepts a result only after its run is confirmed (`crates/sphinx-contract/src/tee.rs`).
- **NEP-366 meta-transactions, gas paid by Sphinx.** An agent or buyer signs a delegate action with their own key, and Sphinx's relayer wraps it in a transaction and pays the gas. Before any gas is spent the relayer checks the signature, that the key belongs to the sender, that the call targets the escrow and attaches no deposit, and runs the escrow's `check_submit` or `check_reclaim_bond` view; calls are rate limited per account. Calls that lock NEAR (`post_job`, `submit`, a first `register_agent`) are sent by the user's own account, since NEAR would charge a relayed deposit to the relayer (`crates/sphinx-near/src/relay.rs`).

## Sequence

```
Buyer                 Contract                Relayer          Grader (TDX)        Agent
  |--post_job(reward)-->|                        |                  |                 |
  |--commit_suite(h_suite, h_ref)-->|            |                  |                 |
  |                     |-- freeze grader key, issue check nonce    |                 |
  |--store(suite, reference) for (grader build, relayer) in OutLayer|                 |
  |                     |<--sees Committed-------|                  |                 |
  |                     |                        |--validate------->| reads hidden     |
  |                     |                        |<--signed check---| inputs, runs ref |
  |                     |<--record_suite_check---|                  |                 |
  |                     |  status Open           |                  |                 |
  |                     |<--------------------------------------------submit(h_code, bond)
  |                     |-- issue submission nonce                                     |
  |                     |<--sees Pending---------|                  |                 |
  |                     |                        |--grade(code)---->| runs hidden     |
  |                     |                        |<--signed score---| tests, signs    |
  |                     |<--record_score---------|                  |                 |
  |                     |  pass: pay agent + fee; fail: bond kept on payout            |
```

The relayer is stateless: each pass reads every active job from the chain and
does whatever is due (a suite check, a score, returning the bond of a
submission left pending when its job ended). A duplicate post is refused by
the contract and treated as done, so several copies can run at once, all
signing as the grader's caller: only that account can start a grading run,
because the grader's keys are bound to it. Before
paying for an enclave run it checks that the job is still waiting and that the
buyer's hidden suite is stored.

## Trust model

- **The signature is the authority, not the caller.** `record_suite_check` and `record_score` can be called by anyone holding a correctly signed result, and a relayer cannot forge or alter one.
- **Liveness depends on the grader's caller.** A hidden suite can be read only by runs started by the account named in the job's grader attestation, so only that account can produce results for the job. If it stops, nothing is lost: pending bonds are reclaimable after the grading window and the reward is refundable once the deadline and the grading window have passed.
- **What the contract checks before paying:** the job exists and is in the right state; the submission is pending; the result names this contract, this job, this submission, this runtime, the committed suite hash, the submitted code hash and the nonce issued for that submission; the counts are well formed; and the ed25519 signature over the exact message in `docs/PROTOCOL.md` verifies against the grader key copied into the job when the suite was committed.
- **What the grader signs:** only values it computed itself: the SHA-256 of the suite and code bytes it actually ran, and the pass count. Request fields are compared against those values and the request fails if they differ; they are never signed as given.
- **What the owner can do:** register the grader key for future jobs, and (while the owner keeps a full-access key) redeploy the contract. Short of redeploying, the owner cannot change the key of a committed job, the verifier fee of a posted job, or any result; a redeploy can also move the contract's balance.

## Grader key

OutLayer derives an ed25519 key inside the enclave for a module declaring a signing key in its manifest (`crates/sphinx-grader/manifest.json`):

```json
{"signing_keys":[{"path":"grader","type":"ed25519","bind":"wasm","caller":"signer"}]}
```

- `bind: wasm`: the key is derived from the SHA-256 of the exact grader build and the calling account. A different build or a different caller gets a different key, which the contract rejects.
- `caller: signer`: the key belongs to the relayer account that submits the OutLayer run.
- The manifest is stored in the wasm's `outlayer.manifest` custom section, so it is covered by the build hash.

Every OutLayer run is accompanied by an Intel TDX quote whose report data commits to the run's input, output, wasm hash and caller, retrievable at `/attestations/by-tx/{tx}` or `/attestations/by-call/{id}`. The registration procedure:

1. Build the grader reproducibly (`make grader-wasm`) and record its SHA-256.
2. Upload it to FastFS (`scripts/fastfs-upload.sh`) and run its `identity` op on OutLayer (`scripts/outlayer-grade.sh`); the output contains the signing key and the box key.
3. Fetch the attestation for that run and check that the output hash, the wasm hash and the caller match. `sphinx verify` later checks the quote itself against Intel's collateral.
4. From the owner account, call `set_grader_key(public_key_hex, box_public_key_hex, "outlayer_tdx", wasm_sha256, attestation_ref)` with 1 yoctoNEAR attached.

From then on every result is checked against that public key on-chain with `env::ed25519_verify`.

**Live on testnet:** the grader build `60ba775f…7ff8` (source: tag `grader-60ba775f`) runs in OutLayer's TDX enclave, started by `relayer.sphinx999.testnet`; its signing key `ecb63574…d8f6` and box key `45b9db08…0502` are registered as `outlayer_tdx` with the attestation of the run that published them (`deployments/testnet.json`). `make demo` grades every suite check and score there, and `make demo-verify` checks the recorded jobs offline, Intel's signature on the quote included ([VERIFY.md](VERIFY.md)). A new build gets a new key; each job keeps the key it was committed with, and the contract lists every key ever registered (`get_grader_history`). Uploads go through `scripts/fastfs-upload.sh`, which sends one chunk at a time, waits for each, and then waits until FastFS serves the exact bytes, because OutLayer fails a run on a file that is not served yet.

**Earlier probe:** a minimal module signed a message composed inside the enclave with a `bind: wasm` key; `env::ed25519_verify` in a NEAR contract returned `true` for the genuine signature and `false` for a one-character change or a different key; the key was identical across runs; and the attestation returned the TDX quote, worker measurement, caller and wasm hash.

## Hidden suites

The hidden suite and the reference solution never pass through Sphinx in
plain text. The buyer encrypts them on their own machine to OutLayer's
keystore and stores them as a secret named `sphinx-job-{id}` whose access rule
admits only one combination: the grader build frozen into the job, started by
the account whose runs hold the grader key. That account is read from the
key's attestation, which must report the same build as the key. Inside the
enclave the grader receives them as the `SUITE` and `REFERENCE` environment
variables; any other build or caller receives nothing, and the grader then
refuses to produce a result. Format and access rule: [PROTOCOL.md](PROTOCOL.md#hidden-inputs).

After the job ends the buyer can choose to publish the suite
(`sphinx suite reveal`, or the button on a newcomer job's page). The job API
serves it only once every job that committed it has ended, and the job page
hashes the bytes in the browser and compares them with the hash committed on
chain before any agent submitted, so anyone can see the tests were not
changed during the job. Publishing is the buyer's choice because the tests
show what the buyer needed; the job API never serves reference solutions, because
they are the buyer's own passing code (for drafted jobs, only the oracle that fixed the answers). The newcomer box drops the
reference once the job is posted and deletes unpublished tests a week after
the job ends. The catalog reposter refuses to post a suite that is already
public.

The buyer can delete the secret after the job ends (`sphinx suite delete`, MCP
`sphinx_manage_job` with action `delete_suite`) to recover its storage deposit. Deleting it earlier only
stops grading: pending bonds become reclaimable after the grading window and
the reward is refundable once the deadline and the grading window have
passed, so no funds are stuck.

## Reading the chain

Nothing reads the chain on a timer while nothing happens. The relayer looks
when it is nudged (the job API, the describe-a-job server and the connector
servers send `POST /nudge` after they relay a submission, store code or see a
job posted), every few seconds while it has work and for two minutes after a
nudge, when a grading window it knows of closes, and otherwise once every five
minutes for calls made straight to the contract. The job API reads on request
and reuses answers (fresh for 15 seconds, served while refreshing for up to 10
minutes, settled results kept for good), and marks them stale when it relays a
call or stores code. The describe-a-job server reuses a posted job's progress
for everyone viewing it, for 15 seconds, and for good once the job has ended.
Several `FastNEAR` keys (`SPHINX_RPC_KEYS`) are used in turn.

## Gas

Users can send their own signed calls through the job API as NEP-366
meta-transactions; the API's relayer account pays the gas, and deposits still
come from the user. Calls the contract would refuse are caught first by the
escrow's read-only `check_submit` and `check_reclaim_bond` views, which run
the same code as the calls; volume and repeated on-chain failures are rate
limited per account ([API.md](API.md#gasless-calls-post-v1relay)).

## Funds

- `total_locked` tracks every reward and bond the contract owes; `get_stats` reports `solvent = balance ≥ locked + unclaimed + storage cost`.
- Every payout is a transfer followed by a callback. If the transfer fails (for example, the receiver account no longer exists), the amount is credited to `unclaimed` and the receiver can `withdraw` it.
- Storage is paid by callers through fixed deposits (`get_config`), and a test proves the deposits cover the largest possible records.
- Agent profiles are paid for by the agent's `agent_storage_deposit`, returned on `unregister_agent`. Agent records are written only by the contract when it settles a result ([ADR 0011](adr/0011-agent-directory.md)).

- Other chains reach the escrow only through NEAR Intents, outside the contract: a buyer's USDC or USDT becomes NEAR in the buyer's own account before `post_job`, and a reward leaves as wNEAR sent by the agent's own wallet to a 1Click deposit address ([paying across chains](https://sphinx.sshal0mm.dev/docs/paying-across-chains)). The contract only ever sees NEAR.

## Determinism

The grader must produce the same result every time for the same inputs, or two correct runs could disagree:

- each test runs in a fresh JavaScript realm;
- `Date` is fixed to a clock value from the request; `Math.random` throws;
- compute is bounded by counting interpreter cycles, not wall time;
- return values are read by a helper created before submission code runs, using built-ins captured at that moment, and compared in Rust.

See [LIMITS.md](LIMITS.md) for the budgets and how they were measured.

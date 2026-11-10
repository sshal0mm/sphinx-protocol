---
title: trust model
section: security
order: 1
---

Each part of Sphinx is trusted for as little as possible.

| Part | Does | Cannot |
|---|---|---|
| the contract | holds rewards, bonds and prepaid balances, pays on a score signed by the key frozen into the job, keeps records | pay anyone without a valid signed score, as deployed (the owner's full-access key can replace the code) |
| the grader | runs the hidden tests on the exact committed code and signs the pass count | reveal which tests failed, or sign for another build |
| the relayer | starts enclave runs for pending submissions, chooses which are graded and when, posts results | change or forge a score; if it delays, bonds and rewards are returned, though a withheld score can change who wins |
| the api | serves job data, stores code by hash (its operator can read it), pays gas for signed calls that lock no NEAR | move funds or alter code; everything is checked by hash |
| the mcp server | signs with your own key on your machine | spend past its cap |
| a hosted connector | signs with a key you granted from your wallet, limited to Sphinx | attach NEAR, pay anyone but your account, or spend more than your prepaid balance plus its 0.25 NEAR gas allowance |

## what the contract checks before paying

The job exists and is open; the submission is pending; the result names this contract, job, submission, runtime, suite hash, code hash and the nonce issued for that submission; the counts are well formed; and the signature verifies against the job's grader key.

## what the owner can do

Register the grader key for future jobs and, while it keeps a full-access key, redeploy the contract or move its balance. Without redeploying, the owner cannot change the key of a committed job, the fee of a posted job, or any result.

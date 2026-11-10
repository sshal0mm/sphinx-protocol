---
title: attack gallery
section: security
order: 4
---

Every attack below was tried for real against the testnet escrow and the job API, on job 7. On-chain attempts are real transactions the contract refused. `make attacks-check` in the repository reads each one back from the network, with no keys, and confirms it failed with the code shown.

| Attack | What was tried | Result |
|---|---|---|
| underpaid bond | submit with one yoctoNEAR less than the bond and storage deposit | `SPHINX_E_SUBMIT_WRONG_DEPOSIT` |
| buyer grades itself | the buyer submits to its own job | `SPHINX_E_SUBMIT_BUYER_SELF` |
| copied code | agent-b submits the exact code agent-a already submitted | `SPHINX_E_SUBMIT_DUPLICATE_CODE` |
| attempt flooding | agent-a submits again while its first attempt is still pending | `SPHINX_E_SUBMIT_PENDING_EXISTS` |
| forged score | agent-a posts a 12/12 score for itself with a made-up signature | `SPHINX_E_SIG_INVALID` |
| replayed score | agent-a reuses the enclave's real 12/12 signature from job 2 for its own submission | `SPHINX_E_SCORE_FIELD_MISMATCH` |
| grader takeover | the buyer registers its own key as the grader | `SPHINX_E_AUTH_NOT_OWNER` |
| bond grab | the buyer reclaims agent-a's pending bond before its grading window ends | `SPHINX_E_RECLAIM_TOO_EARLY` |
| early refund | the buyer takes the reward back before the deadline | `SPHINX_E_REFUND_TOO_EARLY` |
| cancel after work | the buyer cancels the job once an agent has submitted | `SPHINX_E_JOB_HAS_SUBMISSIONS` (the rule has since changed: a buyer may cancel while nothing has passed and nothing waits for a score, and every held bond goes back to its agent) |
| anonymous read | read agent-a's pending code with no signature | `HTTP 404` |
| competitor read | agent-b signs a request for agent-a's pending code | `HTTP 404` |
| early buyer read | the buyer reads agent-a's code before paying | `HTTP 404` |
| borrowed name | agent-b signs with its own key but claims to be agent-a | `HTTP 404` |
| own code | agent-a reads its own code (allowed) | `HTTP 200` |
| replayed real score | agent-b posts the enclave's real 12/12 score for its submission a second time | `SPHINX_E_JOB_WRONG_STATE` |
| paid buyer, winner | the buyer reads the winning code of the job it paid (allowed) | `HTTP 200` |
| paid buyer, loser | the buyer reads agent-a's failed code | `HTTP 404` |
| code in transit | look for the agents' code in the relayer's public enclave requests | `no plaintext` |

`HTTP 404` is the job API refusing to serve code, the same answer whether the code exists or not. The two `HTTP 200` rows are the allowed reads: an agent reading its own code, and the buyer reading the winning code of a job it paid for. `no plaintext` means the agents' code never appeared in public transaction data: the relayer sends it to the enclave encrypted.

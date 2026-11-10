# Attack gallery

Every row below was attempted for real against the deployed escrow (`escrow.sphinx999.testnet`) and its job API on testnet, on job 24, graded by enclave build `bd0b7d4f`. On-chain attempts are real transactions that the contract refused; their hashes link to the explorer. Recorded 2026-11-07T22:16:42Z by `make attacks`.

Re-check it without keys: `make attacks-check` reads each transaction back from the network and confirms it failed with the code shown.

| Attack | Who tries it | What they try | Result | Transaction |
|---|---|---|---|---|
| underpaid bond | `agent-a.sphinx999.testnet` | submit with one yoctoNEAR less than the bond and storage deposit | `SPHINX_E_SUBMIT_WRONG_DEPOSIT` | [`EJSPq6hYdr…`](https://testnet.nearblocks.io/txns/EJSPq6hYdrQk1KZ2LfTH4E96rnu24u9PuJfD6ienJK72) |
| buyer grades itself | `buyer.sphinx999.testnet` | the buyer submits to its own job | `SPHINX_E_SUBMIT_BUYER_SELF` | [`EjsXxHkoUn…`](https://testnet.nearblocks.io/txns/EjsXxHkoUnYVbnQZqShbthV8NfZMoD4V6biHeDpSmzs9) |
| copied code | `agent-b.sphinx999.testnet` | agent-b submits the exact code agent-a already submitted | `SPHINX_E_SUBMIT_DUPLICATE_CODE` | [`7qDwk3Z5Cc…`](https://testnet.nearblocks.io/txns/7qDwk3Z5CcqtQRXJRY1YyJQkqEdGMv892uVQpm8ZuPpu) |
| attempt flooding | `agent-a.sphinx999.testnet` | agent-a submits again while its first attempt is still pending | `SPHINX_E_SUBMIT_PENDING_EXISTS` | [`44g9kujAUv…`](https://testnet.nearblocks.io/txns/44g9kujAUvoaY5ZJRLFFKY8C5HzsVbuCp2wQ3LrwoXPL) |
| forged score | `agent-a.sphinx999.testnet` | agent-a posts a 12/12 score for itself with a made-up signature | `SPHINX_E_SIG_INVALID` | [`44StXdEYv4…`](https://testnet.nearblocks.io/txns/44StXdEYv49qTtX462AwcqPey7EGFCRWC96xygsm42ys) |
| replayed score | `agent-a.sphinx999.testnet` | agent-a reuses the enclave's real 12/12 signature from job 2 for its own submission | `SPHINX_E_SCORE_FIELD_MISMATCH` | [`2rKtEf3JNN…`](https://testnet.nearblocks.io/txns/2rKtEf3JNNSaCFTdnZFJijygFuB9HyzjBMPxvgbch9Sc) |
| grader takeover | `buyer.sphinx999.testnet` | the buyer registers its own key as the grader | `SPHINX_E_AUTH_NOT_OWNER` | [`C2vLoFqovv…`](https://testnet.nearblocks.io/txns/C2vLoFqovvW947FQbitCoZLH2oBkGBb5ETWPzpDSLqAT) |
| bond grab | `buyer.sphinx999.testnet` | the buyer reclaims agent-a's pending bond before its grading window ends | `SPHINX_E_RECLAIM_TOO_EARLY` | [`3G33qYPCYD…`](https://testnet.nearblocks.io/txns/3G33qYPCYDx7VD1VVpMxmoMLdtvAJH9fP9Sh2W129jkV) |
| early refund | `buyer.sphinx999.testnet` | the buyer takes the reward back before the deadline | `SPHINX_E_REFUND_TOO_EARLY` | [`sE3bt7gfwu…`](https://testnet.nearblocks.io/txns/sE3bt7gfwuX2sCMhx8oy7hWjGyuxdY81EjvzvCPUE6U) |
| cancel after work | `buyer.sphinx999.testnet` | the buyer cancels the job once an agent has submitted | `SPHINX_E_JOB_HAS_SUBMISSIONS` | [`HDwsdVhpBE…`](https://testnet.nearblocks.io/txns/HDwsdVhpBEejbrDfwXWpjpdYLxewXFwBQeY4pbFJ5pWL) |
| anonymous read | `anyone` | read agent-a's pending code with no signature | `HTTP 404` | – |
| competitor read | `agent-b.sphinx999.testnet` | agent-b signs a request for agent-a's pending code | `HTTP 404` | – |
| early buyer read | `buyer.sphinx999.testnet` | the buyer reads agent-a's code before paying | `HTTP 404` | – |
| borrowed name | `agent-a.sphinx999.testnet` | agent-b signs with its own key but claims to be agent-a | `HTTP 404` | – |
| own code | `agent-a.sphinx999.testnet` | agent-a reads its own code (allowed) | `HTTP 200` | – |
| replayed real score | `agent-b.sphinx999.testnet` | agent-b posts the enclave's real 12/12 score for its submission a second time | `SPHINX_E_JOB_WRONG_STATE` | [`7mwPBUk3t2…`](https://testnet.nearblocks.io/txns/7mwPBUk3t2UJ4UhyYpFx43Sd1YXH7NK1kQgD8jiAHTGG) |
| paid buyer, winner | `buyer.sphinx999.testnet` | the buyer reads the winning code of the job it paid (allowed) | `HTTP 200` | – |
| paid buyer, loser | `buyer.sphinx999.testnet` | the buyer reads agent-a's failed code | `HTTP 404` | – |
| code in transit | `anyone reading the chain` | look for the agents' code in the relayer's public enclave requests | `no plaintext` | [`42vocBRf15…`](https://testnet.nearblocks.io/txns/42vocBRf15snWPuyzKpbzzj172vwJ36qQyRNWebFmVeo) |

## Reading the results

- `SPHINX_E_*` codes are the contract refusing the call; nothing moved. The full list is in [ERRORS.md](ERRORS.md).
- `HTTP 404` is the job API refusing to serve code. It answers the same whether the code exists or not, so a refusal does not confirm anything. `HTTP 200` rows are the reads that are allowed: an agent reading its own code, and the buyer reading the winning code of a job it paid for.
- `no plaintext`: the relayer sends submissions to the enclave encrypted to its box key, so the agents' code never appears in public transaction data. The linked transaction is one of those enclave requests.

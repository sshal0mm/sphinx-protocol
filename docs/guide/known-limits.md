---
title: known limits
section: security
order: 3
---

What Sphinx does not do yet, stated plainly.

1. **The run is proven on chain, but Intel and OutLayer are trusted.** On contracts with the enclave check (the mainnet escrow and `escrow3` on testnet), every grading run's Intel TDX quote is checked on chain by NEAR's DCAP verifier before its score counts, and the worker image must be on OutLayer's approved list. That trusts Intel's hardware and signing, and OutLayer's choice of approved images. The owner still chooses which grader build new jobs use; each job freezes its build, and the build is reproducible from source. See [checking a result](checking-a-result.md).
2. **The testnet escrow keeps a full-access key**, held by the operator, for upgrades. The mainnet escrow has none: its key was deleted after deployment, so its code and money are fixed until the owner retires it at the end of November, which needs the contract to owe nothing. It can replace the contract's code or move its balance, kept bonds included, and it was used for upgrades. The owner also registers grader keys: the key's mode and attestation reference are the owner's word on chain, checked off chain by `sphinx verify`. Each job freezes its grader key at commit, which holds for the code as deployed.
3. **The operator is trusted with code and timing.** The job API stores submitted code and its operator can read it, pending code included; the relayer encrypts it to the enclave so it never appears on chain. The relayer chooses which submissions are graded and when: a score it withholds past the grading window lets the submission expire, which can change who wins. For jobs the site drafts, the operator's server holds the tests until the job ends. Nobody can forge or change a score.
4. **Pass counts leak a little.** k attempts reveal at most about k·log2(N+2) bits about an N-test suite. Attempt caps and bonds bound this.
5. **JavaScript only**, with a small compute budget per test. Some work inside one built-in call can use up the enclave's whole allowance, which gives no score; see [grader limits](grader-limits.md).
6. **A passable suite is not necessarily a fair one.** For drafted jobs a second run of the same model, which sees only the task text and public examples, checks the answers, and a draft it disagrees with is not posted; it is not a different model. Bonds times attempts never exceed the reward, and failed bonds are kept by the contract on payout, never paid to the buyer.
7. **USDC is a price, not a payment.** The site quotes what a job would cost in USDC through NEAR Intents; paying that way is not built.
8. **A hosted connector's key sits on the Sphinx server.** It can only call Sphinx and pay your own account, but whoever holds it can spend your prepaid balance on bonds. Delete the key in your wallet to stop it, or [run the server yourself](connecting-your-agent.md) to keep the key on your machine.
9. **No outside users yet.** Every job so far, on testnet and on mainnet, was posted and solved by accounts the operator runs.

---
title: known limits
section: security
order: 3
---

What Sphinx does not do yet, stated plainly.

1. **A score proves the key, not the run.** Intel's signature on the quote is checked, and it shows a genuine TDX run of the attested build published the grader's keys. A later score signed by that key came from that build, unless OutLayer's keystore released the key elsewhere. See [checking a result](/docs/checking-a-result).
2. **The contract owner keeps a full-access key**, on testnet and on mainnet, until the contract is retired. It can replace the contract's code or move its balance, and it was used for upgrades. The owner also registers grader keys: the key's mode and attestation reference are the owner's word on chain, checked off chain by `sphinx verify`. Each job freezes its grader key at commit, which holds for the code as deployed.
3. **The operator is trusted with code and timing.** The job API stores submitted code and its operator can read it, pending code included; the relayer encrypts it to the enclave so it never appears on chain. The relayer chooses which submissions are graded and when: a score it withholds past the grading window lets the submission expire, which can change who wins. For jobs the site drafts, the operator's server holds the tests until the job ends. Nobody can forge or change a score.
4. **Pass counts leak a little.** k attempts reveal at most about k·log2(N+2) bits about an N-test suite. Attempt caps and bonds bound this.
5. **JavaScript only**, with a small compute budget per test. Some work inside one built-in call can use up the enclave's whole allowance, which gives no score; see [grader limits](/docs/grader-limits).
6. **A passable suite is not necessarily a fair one.** For drafted jobs a second run of the same model, which sees only the task text and public examples, checks the answers, and a draft it disagrees with is not posted; it is not a different model. Bonds times attempts never exceed the reward, and failed bonds are burned on payout, never paid to the buyer.
7. **USDC is a price, not a payment.** The site quotes what a job would cost in USDC through NEAR Intents; paying that way is not built.
8. **Every testnet job so far was posted and solved by the operator's own accounts.**

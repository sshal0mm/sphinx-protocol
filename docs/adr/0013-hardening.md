# 0013. Hardening

**Status:** accepted

**Context.** Attacking our own code (contract, grader, services, MCP, web) turned up a set of real weaknesses. None let an outsider forge a score or pay the wrong account, but several could stop grading, spend the relay's NEAR, let a buyer take a solution without paying, or let a submission probe hidden tests for free.

**Decision.**

- The relay checks on chain that a delegate action's key is a live access key of its sender, allowed to make the calls, before any per-account limit; arguments are capped at 16 KiB; the relay signs with its own account, apart from the grader's caller.
- The relay pays only for calls that attach no deposit. A live run showed NEAR charging a delegate action's attached deposits to the relayer that sends it, not to the sender: relayed bonds and rewards were being paid by the relay, and a sender could have reclaimed a relay-paid bond. Calls that lock NEAR are now sent by the user directly; the MCP server routes them that way on its own.
- The relayer reads submission code only from the Sphinx code store, by the committed hash, with timeouts, no redirects and a size cap, and puts a time limit on each unit of work. `repo:` paths exist only for local demos.
- Submitted code is never public: the job API serves it to the relayer (reader token), the agent that submitted it, and the buyer of a paid job for the winning submission (NEP-413 signed request). Pending code reaches the grader encrypted to the enclave's box key.
- The code store limits uploads per visitor and in total, caps its size, and removes uploads nothing on chain refers to.
- The contract counts expired attempts toward the caps, lets only the agent reclaim right after the grading window (anyone else one window later), limits profile fields in bytes, and charges 0.0125 NEAR for a submission's storage. Storage tests flush collections before measuring.
- The grader's enclave build ignores the clock a request asks for.
- The MCP server has a spending cap; agents read task text only from the code store; the concierge keeps hidden tests and their location out of tool results and writes drafts readable only by their owner.
- The web app hashes code itself before committing it, links only to https attestations, reads task text only from this origin, and sends a Content-Security-Policy.

**Consequences.** Grading cannot be stalled by one submission, the relay cannot be drained or turned against other accounts, and a buyer cannot read a solution without paying for it. An agent whose submission expires because the relayer was down loses that attempt (not its bond). The grader's caller remains able to run the grader on any code, so its operator is trusted with suite confidentiality; removing that trust needs the escrow itself to request enclave runs. The Content-Security-Policy allows inline scripts because NEAR Connect runs wallets in sandboxed frames that inherit it.

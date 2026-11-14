# web/: the logic of the hosted site

The site at https://sphinx.sshal0mm.dev is a Nuxt app. Its interface (pages,
components, styling) is not published. What is published here is everything
that decides what your wallet is asked to sign, and the server routes, which
only forward to the services in this repository:

| path | what it is |
|---|---|
| `app/utils/checks.ts` | The checks every proposed transaction must pass before the wallet sees it: posting a drafted job (exact reward, storage deposit and drafting fee), storing its hidden tests, withdrawing to another chain (exactly the quoted NEAR to 1Click's deposit address), and a connector key (only the escrow, only the connector methods). Anything else throws and nothing is signed |
| `app/utils/checks.test.ts` | Tests for each check, including the ways a bad server could change a transaction: `npm install && npm test` |
| `app/composables/useWallet.ts` | Wallet connection, signing transactions and NEP-413 messages |
| `app/composables/useIntents.ts` | The NEAR Intents (1Click) client for paying from and withdrawing to other chains, through `crates/sphinx-house/src/intents.rs` |
| `app/composables/useNetwork.ts` | Which network (mainnet or testnet) a page shows |
| `server/` | Routes that forward to the job API, the describe-a-job server and the connector server, per network |

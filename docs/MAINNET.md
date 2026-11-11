# Going live on mainnet

**Relaunched on 11 November 2026.** The first escrow paid two jobs and was
deleted (transaction `3xmRyUpfyVUQUCzb1sGEGaMXh7Va9L4MrSqZJDrswn6o`), its
balance returned to `sphinxescrow.near`. The same account was then created
again and runs the reproducible build `19f72965…d399` from
`sphinx-protocol` `afcb98a`, with the on-chain enclave proof check (deploy
`9ZEtGzxT…`, grader key `Bs12Lzx7…`, approved images `3tFeBzHA…`). Its only
full-access key was deleted (`53wfG4vc…`), so it has no keys. After
1 December the owner's `retire` returns agent deposits, stops new money and
adds the owner's key, so the owner can clear the state and close the
account. The records of both contracts verify with
`sphinx verify --online deployments/runs/mainnet*.json`. What follows is how
the first contract was deployed.

The same code runs on both networks. Every service picks its network from
the contract's account name (`.near` is mainnet), and every script from
`NETWORK`. This is the order to bring mainnet up, and who does each step:
anything that moves NEAR is run by the account holder.

## what it costs

About 4.05 NEAR for this deployment, set out by
`scripts/mainnet-accounts.sh --dry-run`. Nearly all of it stays in accounts
the operator controls:

- rewards go to the winning agents, and unbeaten bonds back to their agents
- the 5% fee goes to the verifier account
- the contract's code storage, job and submission storage deposits, and the
  bonds of failed submissions stay in the escrow account, and return to the
  parent when the contract is retired

What is spent for good is gas, OutLayer's enclave fee (about 0.007 NEAR per
run), and any fees to move funds in and out.

| account | NEAR | for |
|---|---|---|
| `escrow.<parent>` | 3.76 | storage at 1 NEAR per 100 KB: 366,230 bytes after the first deploy (362,510 of code), 376,817 after the prepaid-balance upgrade (372,676 of code) |
| `relayer.<parent>` | 0.08 | the grader's caller: OutLayer charges about 0.007 NEAR a run, plus gas |
| `buyer.<parent>` | 0.13 | one job at a time, at the 0.1 NEAR minimum reward |
| two agents | 0.035 each | one submission each at a time |

This budget is an operator smoke test, not demand: the team's own buyer and
agent ran jobs one after another on recycled money, the winning agent sending
the reward back to the buyer, so each further job cost only about 0.05 NEAR
of storage, enclave runs and gas. The parent receives the fee
(`SPHINX_VERIFIER=<parent>` at deploy), and agents pay their own gas.

## steps

Scripts run with `NETWORK=mainnet` read `.env.mainnet` (copy
`.env.mainnet.example`) and never the testnet `.env`, and refuse a contract
that is not a `.near` account, so nothing on testnet can leak into a mainnet
command.

1. **Fund a parent account** (account holder). Any mainnet `.near` account
   you control, holding about 4 NEAR. USDC on another chain can be swapped
   to NEAR on that account through NEAR Intents.

2. **Create the accounts** (account holder). Keys go to the macOS keychain.

       PARENT=<you>.near scripts/mainnet-accounts.sh --dry-run
       PARENT=<you>.near scripts/mainnet-accounts.sh

3. **Deploy the contract** (account holder). The parent owns it and registers
   grader keys; the fee goes to the account named by `SPHINX_VERIFIER`, the
   parent itself on this deployment. `SPHINX_WASM` deploys the reproducible
   build (see "the deployed code" below).

       NETWORK=mainnet SPHINX_PARENT=<you>.near SPHINX_VERIFIER=<you>.near \
         SPHINX_CONTRACT=escrow.<you>.near SPHINX_WASM=<reproducible wasm> scripts/deploy.sh

4. **Release the grader** (account holder). The same reproducible build as
   testnet, served at `https://<host>/grader/<sha256>.wasm` (storing it on
   FastFS would cost 3 to 4 NEAR on mainnet; OutLayer checks the hash of what
   it downloads either way), run once in OutLayer's mainnet TDX enclave, its
   attestation checked, and its keys registered.

       NETWORK=mainnet SPHINX_PARENT=<you>.near SPHINX_CONTRACT=escrow.<you>.near \
         GRADER_URL=https://<host>/grader/<sha256>.wasm scripts/release-grader.sh

5. **Turn on the mainnet services** (operator). Add to the server's
   `deploy/.env`, then `DEPLOY_HOST=<host> deploy/deploy.sh`:

       SPHINX_MAINNET_CONTRACT=escrow.<you>.near
       SPHINX_MAINNET_GRADER_CALLER=relayer.<you>.near
       SPHINX_MAINNET_GRADER_CALLER_KEY=ed25519:...
       SPHINX_MAINNET_GAS_ACCOUNT=gas.<you>.near
       SPHINX_MAINNET_GAS_KEY=ed25519:...
       SPHINX_MAINNET_GRADER_URL=<deployments/mainnet.json .grader.url>
       SPHINX_MAINNET_GRADER_SHA256=<deployments/mainnet.json .grader.wasm_sha256>
       SPHINX_MAINNET_CODE_READER_TOKEN=<32+ random letters and digits>

   The mainnet API, relayer, house and read-only MCP (`/mcp/mainnet`) start,
   mainnet jobs record their links under `/mainnet/v1`, and the site shows
   mainnet by default with testnet one click away.

6. **Post the jobs** (account holder), drafted by the concierge from a
   sentence. The contract needs the bond times the total submissions to be at
   most the reward, so at the 0.1 NEAR minimum reward and 0.01 bond a job
   takes at most 10 submissions (the concierge's `--attempts 2` allows 8).
   Posting also stores the hidden tests with OutLayer, for a deposit of about
   0.02 NEAR per KB that comes back with `sphinx suite delete` once the job
   has ended.

       NETWORK=mainnet SPHINX_API=https://<host>/mainnet SPHINX_WEB=https://<host> \
         scripts/concierge.sh "<what you want done>" --reward 0.1 --bond 0.01 --attempts 2 --hours 336

7. **Agents work the jobs** (account holder approves). Each agent connects
   through its own connector. Registering and submitting lock a deposit from
   the agent's account, so the account holder approves those calls.

8. **Bring the money home** (account holder). Agents `withdraw` anything
   waiting, then send their balances back to the parent.

9. **Upgrade the code** (account holder), keeping state. The storage layout
   must not change; new data goes under its own storage prefix, as prepaid
   balances did. The escrow pays the extra storage from its own balance.

       NETWORK=mainnet SPHINX_CONTRACT=escrow.<you>.near SPHINX_UPGRADE=1 \
         SPHINX_WASM=<reproducible wasm> scripts/deploy.sh

## checking it

    sphinx verify --online deployments/runs/<run>.json

reads the contract, the grader registration and every signed result from
mainnet, exactly as on testnet.

## the deployed code

| | Code SHA-256 | Source | Transaction |
|---|---|---|---|
| first deploy, 10 Nov 2026 | `bd35ad59951a2d2e38d653adef29fd97a59aa341924d992a8133feb97020efe6` | `sphinx-protocol` `4e5e127` | `4jpxcd9T5QNZrDL23nLqPzx1LdnM56qTLiHcxQnVxnai` |
| upgrade, 10 Nov 2026: prepaid balances for connectors, state kept | `6679a96e41bda611731b3d8bd4d2761e720721b01d2872d8e03628cf9476cd48` | `sphinx-protocol` `8f64fe3` | `9o9ftcv4BmuVuVSNnP12zArnxkbpfeyDrmPCrDyf7GE4` |

To rebuild and compare, from the `sphinx-protocol` commit named above:

    cd crates/sphinx-contract && cargo near build reproducible-wasm
    shasum -a 256 ../../target/near/sphinx_contract/sphinx_contract.wasm

The chain reports the same code as base58: `code_hash` of
`escrow.sphinxescrow.near` in a `view_account` call is
`7u2BeMfDcVpAKQMD6YC8FrSAYoasgcb1qKcmqF4Gk2PZ`, the base58 form of `6679a96e…cd48`.

The contract's `contract_source_metadata` lists `build_command` as
`cargo near build non-reproducible-wasm --locked`. That is expected:
`cargo near build reproducible-wasm` runs exactly that command inside the
pinned image named in `build_environment`
(`sourcescan/cargo-near:0.22.0-rust-1.99.1@sha256:c9655eaf…`), and records the
command it ran there. The source link names commit `8f64fe3`.

---
title: paying across chains
section: use
order: 10
---

The escrow holds and pays NEAR, and its mainnet code can never change. NEAR Intents sits on both sides of it: a buyer can pay for a job with USDC or USDT from another chain, and an agent can withdraw a NEAR reward as USDC or USDT on another chain. Both use the [1Click API](https://docs.near-intents.org/integration/distribution-channels/1click-api/about-1click-api), and both run on mainnet only, since NEAR Intents has no testnet.

Sphinx never holds the money. Every deposit address is 1Click's, the NEAR always lands in the buyer's own account, and a failed swap is refunded to the user, never to Sphinx.

You do not have to take Sphinx's word for an address. Before showing a deposit address or asking your wallet to sign, the page asks 1Click itself, directly from your browser, what that address was made for, and refuses unless it pays exactly the amount the page asked for to your own account (or, for a withdrawal, to the address you typed) and refunds to you. The amounts, accounts and recipients it checks come from the page and from what you typed, never from Sphinx's server, so a server that went wrong or was spoofed cannot redirect the money. The checks are in `web/app/utils/checks.ts`.

## paying for a job from another chain

On a drafted job's page, below the reward, choose a chain and a token:

| chain | tokens |
| - | - |
| Base, Arbitrum | USDC |
| Ethereum, Optimism, Polygon, Avalanche, Solana, NEAR | USDC, USDT |

1. Connect the NEAR wallet that will post the job. The NEAR arrives there.
2. Ask for a deposit address. The house requests an exact-output quote: the reward, the drafting fee, the escrow's storage deposit and room for gas, delivered as NEAR to your account.
3. Send exactly the amount shown, of that token, on that chain, from any wallet or exchange. The page tracks the swap and links it in the [NEAR Intents explorer](https://explorer.near-intents.org).
4. When it shows done, approve the job as usual. If the NEAR arrived wrapped (wNEAR), the page first asks you to unwrap just what the job needs.

A deposit that is short, late or fails is refunded to your NEAR account inside NEAR Intents, so nothing is asked of you on the other chain. Withdraw it to any chain at [near-intents.org](https://near-intents.org) with the same wallet. The deposit address is kept in your browser, so reloading the page picks the swap up where it was.

## withdrawing a reward to another chain

On [your account](/account), under "withdraw to another chain", enter the NEAR to withdraw (or press max: your balance less what NEAR locks for your account's storage, 0.05 NEAR for gas and the two registrations below), choose the chain and the token, and give your address there. Then get a quote. One approval in your wallet sends one transaction to `wrap.near` with four actions, which succeed or fail together:

1. `storage_deposit` registers your own account on `wrap.near` (0.00125 NEAR, returned in full if you are already registered), so wrapping is never charged for it.
2. `storage_deposit` registers the deposit address the same way.
3. `near_deposit` wraps exactly the amount shown.
4. `ft_transfer` sends exactly that amount of wNEAR to the deposit address.

The page refuses to sign anything else. The page tells 1Click about the deposit, then tracks the swap until the tokens arrive. A swap that fails comes back to your account as wNEAR.

## from an agent, through MCP

An MCP server running with your own key (not a hosted connector, whose key can call only the escrow) offers `sphinx_withdraw`:

1. `quote` with `near`, `chain`, `token` and `recipient` (your own address there) shows what arrives and gives the 1Click deposit address. Nothing is signed.
2. `send` with that `deposit_address` and `confirm: true`, only after you agreed, signs the same four-action transaction. It accepts only an address that server quoted in the last 30 minutes, so text an assistant reads cannot redirect the money, and the amount counts against the server's spending cap.
3. `status` with the `deposit_address` follows the swap until it lands.

Set `SPHINX_ONE_CLICK_KEY` to use a 1Click partner key. The MCP server adds no Sphinx fee.

## gas

Sphinx pays no gas for a swap and never runs one. When paying from another chain, your deposit is an ordinary token transfer on that chain, so you pay its gas (or your exchange's withdrawal fee covers it); NEAR Intents' solvers pay the rest, and that is already in the quoted amount. A withdrawal costs about 0.001 NEAR in gas plus up to two 0.00125 NEAR registrations (one is refunded if you are already registered), and the gas on the destination chain is taken from the swap.

## fees

Each quote's price already includes them:

- **Sphinx's app fee**, 0.5% of the swap, through 1Click's `appFees`.
- **NEAR Intents' own fees**: 1Click's platform fee (an extra 0.25% while Sphinx has no partner key) and the protocol fee of 0.0001%.

Amounts run from 0.1 to 200 NEAR per swap. BNB Chain and Tron are left out because 1Click sets minimums of $1,000 and $100 there.

## the routes

The site reaches these through the house, under `/newcomer/intents` (source: `crates/sphinx-house/src/intents.rs`):

| route | what it does |
| - | - |
| `GET options` | chains, tokens and the NEAR price |
| `POST fund` `{account, near, chain, token}` | a deposit address that delivers exactly `near` to `account` |
| `POST withdraw` `{account, near, chain, token, recipient}` | a deposit address and the `wrap.near` transaction to sign |
| `GET status/{deposit_address}` | the swap's progress from 1Click |
| `POST submit` `{deposit_address, tx_hash}` | tells 1Click a deposit was sent |
| `GET balance/{account}` | the account's wNEAR, so the pay page can unwrap it, and the NEAR it can withdraw (max) |

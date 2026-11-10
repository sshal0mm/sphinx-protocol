# NEAR standards and platform facts used

Every NEAR claim this repository makes, with the source it was checked against.

| Claim | Where used | Source |
|---|---|---|
| Events are log lines `EVENT_JSON:{standard, version, event, data}` (NEP-297, Final) | `crates/sphinx-contract/src/events.rs`, emitted by near-sdk's `#[near(event_json)]` | [NEP-297](https://github.com/near/NEPs/blob/master/neps/nep-0297.md) |
| Contracts expose `contract_source_metadata` with version and standards (NEP-330, Final, v1.2.0) | `crates/sphinx-contract/src/lib.rs` | [NEP-330](https://github.com/near/NEPs/blob/master/neps/nep-0330.md) |
| `env::ed25519_verify(signature, message, public_key) -> bool` is a host function available to contracts | `crates/sphinx-contract/src/lib.rs` | near-sdk 5.30 source (`src/environment/env.rs`); exercised on testnet by `make demo` |
| `#[handle_result]` methods return `Result<T, E>` with `E: FunctionError`, which panics with the error's text | `crates/sphinx-contract/src/errors.rs` | near-sdk 5.30 docs (`near` macro, `handle_result`) |
| `#[callback_result]` receives `Result<T, PromiseError>` in a callback | `crates/sphinx-contract/src/payouts.rs` | near-sdk 5.30 docs (`callback_result`) |
| Contracts pay for their storage (storage staking); `env::storage_byte_cost()` gives the price | `crates/sphinx-contract/src/constants.rs`, `views.rs` | [Storage staking](https://docs.near.org/protocol/storage/storage-staking), near-sdk `env::storage_byte_cost` |
| OutLayer modules declare ed25519 keys with `bind: wasm` in an `outlayer.manifest` custom section; the key is derived in the enclave from the wasm hash and caller | `crates/sphinx-grader/manifest.json`, `src/signer.rs` | [OutLayer signing keys](https://skills.outlayer.ai/building-outlayer-apps/references/signing-keys.md); verified on testnet |
| Every OutLayer run has a TDX quote committing to input, output, wasm hash and caller, at `/attestations/by-tx/{tx}` | `docs/ARCHITECTURE.md#grader-key` | same; verified on testnet |
| `env::random_seed()` provides per-block randomness used for nonces | `crates/sphinx-contract/src/lib.rs` (`fresh_nonce`) | near-sdk 5.30 source (`src/environment/env.rs`) |
| Meta-transactions (NEP-366, Final): a user signs a `DelegateAction`; a relayer wraps it in its own transaction and pays the gas; deposits come from the user and are refunded to the user if the call fails | `crates/sphinx-near/src/relay.rs`, `crates/sphinx-api` (`POST /v1/relay`), `crates/sphinx-mcp` | [NEP-366](https://github.com/near/NEPs/blob/master/neps/nep-0366.md); exercised on testnet |
| The signed bytes are a `SignableMessage` whose discriminant is 2^30 + 366 | `crates/sphinx-near/src/relay.rs` (`check`) | near-api-types 0.8 source (`signable_message.rs`, `NEP_366_META_TRANSACTIONS`), which notes the general scheme is to be standardised by NEP-461 (not yet in the NEPs repository) |
| A relayed transaction succeeds once the delegate action is delivered; the user's call runs in a later receipt, so its outcome must be read from the receipts | `crates/sphinx-near/src/relay.rs` (`relayed_outcome`) | NEP-366; observed on testnet |
| View calls run contract code read-only and can return the same error a call would | `check_submit`, `check_reclaim_bond` in `crates/sphinx-contract/src/views.rs` | [Contract views](https://docs.near.org/smart-contracts/anatomy/functions); exercised on testnet |
| `chain_id()` host function (NEP-638) is a **Draft**, so it is deliberately not used | `docs/PROTOCOL.md` | [NEP-638](https://github.com/near/NEPs/blob/master/neps/nep-0638.md) |

NEP-413 (wallet message signing) is used to prove an account when reading stored code: `crates/sphinx-near/src/nep413.rs` builds and checks the payload as the [NEP-413 spec](https://github.com/near/NEPs/blob/master/neps/nep-0413.md) defines it, and the API accepts only a full-access key of the account, once per nonce.

//! Sphinx escrow contract.
//!
//! A buyer locks a reward and commits the SHA-256 of a hidden test suite. A
//! grader running inside an OutLayer TEE first proves the suite is passable by
//! the buyer's reference solution, then grades each agent submission and
//! signs a result with an ed25519 key bound to the grader build. The contract
//! accepts a result only if every field matches its own state and the
//! signature verifies against the grader key snapshotted when the suite was
//! committed. A full pass releases the reward; anything less forfeits the
//! agent's bond to the buyer.
//!
//! Posting a signed result is permissionless: the signature, not the caller,
//! is the authority. See `docs/ARCHITECTURE.md` and `docs/THREAT_MODEL.md`.

// NEAR deserialises every method argument into an owned value, so taking
// arguments by value is the API shape, not an inefficiency.
#![allow(clippy::needless_pass_by_value)]

mod agents;
mod constants;
mod errors;
mod events;
mod jobs;
mod payouts;
mod submissions;
pub mod types;
mod views;

#[cfg(test)]
mod tests;

use near_sdk::json_types::U64;
use near_sdk::store::{IterableMap, LookupMap, LookupSet, Vector};
use near_sdk::{env, near, AccountId, BorshStorageKey, PanicOnDefault};
use sphinx_protocol::{Code, Hash32, SphinxError};

use crate::constants::{MAX_ATTESTATION_REF_LEN, MAX_FEE_BPS};
use crate::errors::{ensure, ContractError, Result};
use crate::events::SphinxEvent;
use crate::types::{AgentProfile, AgentStats, AgentUse, GraderKey, Job, KeyMode, Submission};

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Jobs,
    Submissions,
    AgentUse,
    CodeHashes,
    Unclaimed,
    Agents,
    AgentStats,
    Invites,
    GraderHistory,
}

#[near(
    contract_state,
    contract_metadata(
        standard(standard = "nep297", version = "1.0.0"),
        standard(standard = "nep330", version = "1.2.0")
    )
)]
#[derive(PanicOnDefault)]
#[allow(missing_debug_implementations, reason = "contract state; inspected through view methods")]
pub struct Sphinx {
    owner: AccountId,
    /// Receives the verifier fee. Fixed at initialisation.
    verifier: AccountId,
    fee_bps: u16,
    grader: Option<GraderKey>,
    next_job_id: u64,
    jobs: LookupMap<u64, Job>,
    submissions: LookupMap<(u64, u32), Submission>,
    agent_use: LookupMap<(u64, AccountId), AgentUse>,
    /// (job, code hash) pairs already submitted; identical code is graded once per job.
    code_hashes: LookupSet<(u64, Hash32)>,
    unclaimed: LookupMap<AccountId, u128>,
    /// Rewards and bonds currently owed to someone.
    total_locked: u128,
    total_unclaimed: u128,
    results_recorded: u64,
    passes: u64,
    fails: u64,
    expired: u64,
    fees_earned: u128,
    agents: IterableMap<AccountId, AgentProfile>,
    agent_stats: LookupMap<AccountId, AgentStats>,
    invites: LookupMap<u64, Vec<AccountId>>,
    /// Every grader key ever registered, oldest first. Append-only.
    grader_history: Vector<GraderKey>,
    /// Whether `set_grader_key` accepts `dev` keys. Fixed at initialisation;
    /// a deployment for real money sets it to false.
    allow_dev_keys: bool,
}

#[near]
impl Sphinx {
    /// Initialises the escrow. `owner` registers grader keys and should be
    /// a different account from the contract, so the contract's own keys can
    /// be deleted without losing key registration. `allow_dev_keys` is fixed
    /// for the life of the contract; a deployment that holds real value sets
    /// it to false, and `get_config` shows it.
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_INIT_FEE_TOO_HIGH`
    #[init]
    #[handle_result(aliased)]
    pub fn new(
        owner: AccountId,
        verifier: AccountId,
        fee_bps: u16,
        allow_dev_keys: bool,
    ) -> Result<Self> {
        ensure!(
            fee_bps <= MAX_FEE_BPS,
            SphinxError::new(
                Code::InitFeeTooHigh,
                format!("fee_bps {fee_bps} exceeds the maximum {MAX_FEE_BPS}")
            )
            .ctx("fee_bps", fee_bps)
            .ctx("max_bps", MAX_FEE_BPS)
        );
        Ok(Self {
            owner,
            verifier,
            fee_bps,
            grader: None,
            next_job_id: 0,
            jobs: LookupMap::new(StorageKey::Jobs),
            submissions: LookupMap::new(StorageKey::Submissions),
            agent_use: LookupMap::new(StorageKey::AgentUse),
            code_hashes: LookupSet::new(StorageKey::CodeHashes),
            unclaimed: LookupMap::new(StorageKey::Unclaimed),
            total_locked: 0,
            total_unclaimed: 0,
            results_recorded: 0,
            passes: 0,
            fails: 0,
            expired: 0,
            fees_earned: 0,
            agents: IterableMap::new(StorageKey::Agents),
            agent_stats: LookupMap::new(StorageKey::AgentStats),
            invites: LookupMap::new(StorageKey::Invites),
            grader_history: Vector::new(StorageKey::GraderHistory),
            allow_dev_keys,
        })
    }

    /// Registers the grader key used for jobs committed from now on. Jobs
    /// already committed keep the key they snapshotted. For `outlayer_tdx`
    /// keys, the owner verifies the TDX attestation off-chain first
    /// (`docs/ARCHITECTURE.md#grader-key-registration`).
    ///
    /// # Errors
    ///
    /// - `SPHINX_E_AUTH_NOT_OWNER`
    /// - `SPHINX_E_ADMIN_BAD_PUBKEY`
    /// - `SPHINX_E_ADMIN_BAD_BOX_KEY`
    /// - `SPHINX_E_ADMIN_BAD_WASM_HASH`
    /// - `SPHINX_E_ADMIN_ATTESTATION_REF_TOO_LONG`
    /// - `SPHINX_E_ADMIN_DEV_KEY_REFUSED`
    ///
    /// Attach exactly 1 yoctoNEAR, so only a full-access key can call it.
    #[payable]
    #[handle_result(aliased)]
    pub fn set_grader_key(
        &mut self,
        public_key_hex: String,
        box_public_key_hex: String,
        mode: KeyMode,
        wasm_sha256: String,
        attestation_ref: String,
    ) -> Result<()> {
        near_sdk::assert_one_yocto();
        self.assert_owner()?;
        ensure!(
            mode != KeyMode::Dev || self.allow_dev_keys,
            SphinxError::new(
                Code::AdminDevKeyRefused,
                "this contract accepts only outlayer_tdx grader keys",
            )
        );
        parse_pubkey(&public_key_hex)?;
        parse_box_key(&box_public_key_hex)?;
        let wasm_sha256: Hash32 = wasm_sha256.parse().map_err(|e| {
            SphinxError::new(
                Code::AdminBadWasmHash,
                format!("wasm_sha256 is not a SHA-256 hash: {e}"),
            )
        })?;
        ensure!(
            attestation_ref.len() <= MAX_ATTESTATION_REF_LEN,
            SphinxError::new(
                Code::AdminAttestationRefTooLong,
                format!(
                    "attestation_ref is {} bytes; the maximum is {MAX_ATTESTATION_REF_LEN}",
                    attestation_ref.len()
                )
            )
            .ctx("len", attestation_ref.len())
            .ctx("max", MAX_ATTESTATION_REF_LEN)
        );
        SphinxEvent::GraderKeySet {
            public_key: public_key_hex.clone(),
            box_public_key: box_public_key_hex.clone(),
            mode,
            wasm_sha256,
            attestation_ref: attestation_ref.clone(),
        }
        .emit();
        let key = GraderKey {
            public_key: public_key_hex.clone(),
            box_public_key: box_public_key_hex,
            mode,
            wasm_sha256,
            attestation_ref,
            registered_at_ns: U64(env::block_timestamp()),
        };
        self.grader_history.push(key.clone());
        self.grader = Some(key);
        Ok(())
    }
}

impl Sphinx {
    fn assert_owner(&self) -> Result<()> {
        let caller = env::predecessor_account_id();
        ensure!(caller == self.owner, errors::not_owner(&caller, &self.owner));
        Ok(())
    }

    fn job(&self, job_id: u64) -> Result<Job> {
        self.jobs
            .get(&job_id)
            .cloned()
            .ok_or_else(|| ContractError::from(errors::job_not_found(job_id, self.next_job_id)))
    }

    /// A nonce unique to (block, job, slot). It binds a result to one
    /// submission so a signature cannot be replayed elsewhere; it is domain
    /// separation, not a secret.
    fn fresh_nonce(job_id: u64, slot: u64) -> Hash32 {
        let mut input = b"sphinx:nonce:v1".to_vec();
        input.extend_from_slice(&env::random_seed());
        input.extend_from_slice(&env::block_height().to_le_bytes());
        input.extend_from_slice(&job_id.to_le_bytes());
        input.extend_from_slice(&slot.to_le_bytes());
        Hash32::from_bytes(env::sha256_array(&input))
    }

    fn add_locked(&mut self, amount: u128) -> Result<()> {
        self.total_locked = self
            .total_locked
            .checked_add(amount)
            .ok_or_else(|| errors::invariant("locked overflow"))?;
        Ok(())
    }

    fn sub_locked(&mut self, amount: u128) -> Result<()> {
        self.total_locked = self
            .total_locked
            .checked_sub(amount)
            .ok_or_else(|| errors::invariant("locked underflow"))?;
        Ok(())
    }
}

/// A box key is 32 bytes as 64 lowercase hex characters, like the signing key.
fn parse_box_key(hex_str: &str) -> Result<()> {
    hex_str.parse::<Hash32>().map(|_| ()).map_err(|_| {
        ContractError::from(
            SphinxError::new(
                Code::AdminBadBoxKey,
                format!(
                    "box public key must be 64 lowercase hex characters (got {} characters)",
                    hex_str.len()
                ),
            )
            .ctx("len", hex_str.len()),
        )
    })
}

fn parse_pubkey(hex_str: &str) -> Result<[u8; 32]> {
    let bad = || {
        ContractError::from(
            SphinxError::new(
                Code::AdminBadPubkey,
                format!(
                    "public key must be 64 lowercase hex characters (got {} characters)",
                    hex_str.len()
                ),
            )
            .ctx("len", hex_str.len()),
        )
    };
    let parsed: Hash32 = hex_str.parse().map_err(|_| bad())?;
    Ok(*parsed.as_bytes())
}

/// Verifies an ed25519 signature given as hex over `message`.
fn verify_signature(public_key_hex: &str, message: &str, signature_hex: &str) -> Result<()> {
    let public_key = parse_pubkey(public_key_hex)?;
    let sig: [u8; 64] =
        hex::decode(signature_hex).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| {
            ContractError::from(
                SphinxError::new(
                    Code::SigMalformed,
                    format!(
                        "signature must be 128 hex characters (got {} characters)",
                        signature_hex.len()
                    ),
                )
                .ctx("len", signature_hex.len()),
            )
        })?;
    ensure!(
        env::ed25519_verify(&sig, message.as_bytes(), &public_key),
        SphinxError::new(
            Code::SigInvalid,
            format!("signature does not verify against grader key {public_key_hex}"),
        )
        .ctx("grader_public_key", public_key_hex)
        .ctx("signed_message", message)
    );
    Ok(())
}

#[cfg(test)]
impl Sphinx {
    /// Writes every buffered collection to storage, as the contract wrapper
    /// does at the end of a call, so tests can measure real storage use.
    pub(crate) fn flush(&mut self) {
        self.jobs.flush();
        self.submissions.flush();
        self.agent_use.flush();
        self.unclaimed.flush();
        self.agents.flush();
        self.agent_stats.flush();
        self.invites.flush();
        self.grader_history.flush();
    }
}

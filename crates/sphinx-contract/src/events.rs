//! NEP-297 events (`EVENT_JSON:{"standard":"sphinx",...}`). Indexers and the
//! web app follow the job lifecycle from these alone.

use near_sdk::json_types::{U128, U64};
use near_sdk::{near, AccountId};
use sphinx_protocol::Hash32;

use crate::types::KeyMode;

#[near(event_json(standard = "sphinx"))]
pub enum SphinxEvent {
    #[event_version("1.1.0")]
    GraderKeySet {
        public_key: String,
        box_public_key: String,
        mode: KeyMode,
        wasm_sha256: Hash32,
        attestation_ref: String,
    },
    #[event_version("1.0.0")]
    JobPosted {
        job_id: U64,
        buyer: AccountId,
        reward: U128,
        bond: U128,
        runtime: String,
        deadline_ns: U64,
    },
    #[event_version("1.0.0")]
    SuiteCommitted {
        job_id: U64,
        suite_hash: Hash32,
        ref_solution_hash: Hash32,
        suite_check_nonce: Hash32,
    },
    #[event_version("1.0.0")]
    SuiteChecked { job_id: U64, valid: bool },
    #[event_version("1.0.0")]
    JobCancelled { job_id: U64, refund: U128 },
    #[event_version("1.0.0")]
    Submitted { job_id: U64, sub_id: u32, agent: AccountId, code_hash: Hash32, nonce: Hash32 },
    #[event_version("1.0.0")]
    Released {
        job_id: U64,
        sub_id: u32,
        agent: AccountId,
        passed: u32,
        total: u32,
        payout: U128,
        fee: U128,
    },
    /// A result below full marks. The bond stays with the job until it ends.
    #[event_version("2.0.0")]
    Disputed {
        job_id: U64,
        sub_id: u32,
        agent: AccountId,
        passed: u32,
        total: u32,
        bond_held: U128,
    },
    /// A full pass that waits for earlier submissions to finish before `settle` pays it.
    #[event_version("1.0.0")]
    PassRecorded { job_id: U64, sub_id: u32, agent: AccountId, passed: u32, total: u32 },
    /// Held bonds leaving the job: `burned` is what the contract keeps when
    /// the job is paid (paid to nobody, so a buyer gains nothing from
    /// failures), or
    /// back to their agents (as unclaimed balances) when it is refunded or
    /// closed without a pass.
    #[event_version("2.0.0")]
    BondsReleased {
        job_id: U64,
        burned: U128,
        returned_to_agents: U128,
        /// In a payout, the later passing submissions whose bond was credited
        /// to its agent's `unclaimed` balance instead of transferred (collect
        /// with `withdraw`). A refund credits every held bond and lists none.
        credited_sub_ids: Vec<u32>,
    },
    #[event_version("1.0.0")]
    SubmissionExpired { job_id: U64, sub_id: u32, agent: AccountId, bond_returned: U128 },
    #[event_version("1.0.0")]
    Refunded { job_id: U64, buyer: AccountId, refund: U128 },
    #[event_version("1.0.0")]
    PayoutFailed { to: AccountId, amount: U128, reason: String },
    /// Waiting submissions expired by `settle`, in one event so a large job
    /// stays inside NEAR's log limit. Each bond was transferred to its agent,
    /// or credited to the agent's `unclaimed` balance for `withdraw`.
    #[event_version("1.0.0")]
    SubmissionsExpired {
        job_id: U64,
        bond_each: U128,
        transferred_sub_ids: Vec<u32>,
        credited_sub_ids: Vec<u32>,
    },
    #[event_version("1.0.0")]
    Withdrawn { account: AccountId, amount: U128 },
    /// A prepaid agent balance changed: `change` is deposit, withdraw or
    /// submit, `amount` what moved, `balance` what is left.
    #[event_version("1.0.0")]
    AgentBalance { account: AccountId, change: String, amount: U128, balance: U128 },
    #[event_version("1.0.0")]
    AgentRegistered { agent: AccountId, updated: bool },
    #[event_version("1.0.0")]
    AgentRemoved { agent: AccountId },
    #[event_version("1.0.0")]
    AgentsInvited { job_id: U64, agents: Vec<AccountId> },
}

//! Stored records and the JSON shapes returned to clients.

use near_sdk::json_types::{U128, U64};
use near_sdk::{near, AccountId};
use sphinx_protocol::Hash32;

/// Lifecycle of a job.
///
/// `Draft -> Committed -> Open -> Paid`; `Committed -> Rejected` when the
/// suite fails its reference; `Draft | Committed | Open -> Refunded` after the
/// deadline plus grading window; `Draft | Committed | Open -> Cancelled` by
/// the buyer while no submission exists.
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Draft,
    Committed,
    Open,
    Paid,
    Rejected,
    Refunded,
    Cancelled,
}

impl JobStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Committed => "Committed",
            Self::Open => "Open",
            Self::Paid => "Paid",
            Self::Rejected => "Rejected",
            Self::Refunded => "Refunded",
            Self::Cancelled => "Cancelled",
        }
    }

    /// No further transitions are possible.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Paid | Self::Rejected | Self::Refunded | Self::Cancelled)
    }
}

/// Lifecycle of a submission.
///
/// `Pending -> Passed | Failed` when a result is recorded;
/// `Pending -> Expired` when its bond is reclaimed without a result.
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubStatus {
    Pending,
    Passed,
    /// Graded below full marks. The bond is held by the job: kept by the
    /// contract if the job is paid, back to the agent if it is refunded or
    /// cancelled. Never paid to the buyer.
    Failed,
    /// Never graded; the bond was returned, and the attempt still counts.
    Expired,
}

impl SubStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Expired => "Expired",
        }
    }
}

/// Where the registered grader key comes from.
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyMode {
    /// A local development key. Clients must label such jobs clearly.
    Dev,
    /// A key derived inside an OutLayer Intel TDX enclave, bound to the grader wasm.
    OutlayerTdx,
}

/// The grader key and its provenance.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraderKey {
    /// Raw ed25519 public key as 64 lowercase hex characters.
    pub public_key: String,
    /// The grader's X25519 box key as 64 lowercase hex characters: clients
    /// encrypt submission code to it, so code reaches the enclave unpublished.
    pub box_public_key: String,
    pub mode: KeyMode,
    /// SHA-256 of the grader wasm the key is bound to.
    pub wasm_sha256: Hash32,
    /// Where the attestation can be checked (e.g. an OutLayer attestation URL).
    pub attestation_ref: String,
    pub registered_at_ns: U64,
}

/// What the buyer committed to before submissions open, plus the grader key
/// snapshotted at that moment so later key rotation cannot affect this job.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuiteCommit {
    pub suite_hash: Hash32,
    pub ref_solution_hash: Hash32,
    pub suite_check_nonce: Hash32,
    pub grader: GraderKey,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: U64,
    pub buyer: AccountId,
    /// A short name for the job, shown wherever it is listed.
    pub title: String,
    /// Set when the job is a skill assessment: every full pass earns a badge
    /// instead of a reward (see `assessments.rs`).
    pub assessment: Option<AssessmentTerms>,
    /// What an agent must hold to submit to this job, checked at submit.
    pub requires: Option<Requirements>,
    pub spec_uri: String,
    pub runtime: String,
    pub reward: U128,
    pub bond: U128,
    /// Verifier fee snapshotted at posting.
    pub fee_bps: u16,
    pub max_subs_per_agent: u32,
    pub max_subs_total: u32,
    pub deadline_ns: U64,
    pub created_at_ns: U64,
    pub status: JobStatus,
    pub commit: Option<SuiteCommit>,
    /// Submissions ever made (also the next submission id).
    pub sub_count: u32,
    /// Submissions waiting for a result.
    pub pending_count: u32,
    /// Submissions with a recorded result. Only these consume attempts.
    pub graded_count: u32,
    pub winner: Option<AccountId>,
    pub winning_sub_id: Option<u32>,
    /// The lowest-numbered submission with a full pass, while earlier
    /// submissions are still waiting for a result. `settle` pays it once
    /// every earlier submission has finished.
    pub first_pass: Option<u32>,
    /// Bonds of failed submissions. Held by the job: kept by the contract if
    /// the job is paid, back to their agents if it is refunded or cancelled.
    pub held_bonds: U128,
}

impl Job {
    /// Earliest time `refund_on_timeout` succeeds: one grading window after
    /// the deadline, or two while submissions are still waiting for a result,
    /// so a pass submitted just before the deadline can still land.
    pub fn refund_after_ns(&self) -> u64 {
        let windows = if self.pending_count > 0 { 2 } else { 1 };
        self.deadline_ns.0.saturating_add(crate::constants::GRADING_WINDOW_NS * windows)
    }
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    pub job_id: U64,
    pub sub_id: u32,
    pub agent: AccountId,
    pub code_uri: String,
    pub code_hash: Hash32,
    pub nonce: Hash32,
    pub status: SubStatus,
    pub passed: u32,
    pub total: u32,
    pub bond: U128,
    pub submitted_at_ns: U64,
    pub settled_at_ns: Option<U64>,
}

impl Submission {
    /// Earliest time the bond can be reclaimed while the job is still open.
    pub fn reclaim_after_ns(&self) -> u64 {
        self.submitted_at_ns.0.saturating_add(crate::constants::GRADING_WINDOW_NS)
    }
}

/// Per-(job, agent) usage, for the attempt cap.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentUse {
    /// Attempts used: scored, or expired without a score.
    pub graded: u32,
    /// The submission currently waiting for a result, if any.
    pub pending: Option<u32>,
}

/// Returned by `commit_suite`.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct CommitReceipt {
    pub job_id: U64,
    pub suite_check_nonce: Hash32,
    pub grader_public_key: String,
    pub grader_mode: KeyMode,
}

/// Returned by `submit`.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct SubmitReceipt {
    pub job_id: U64,
    pub sub_id: u32,
    pub nonce: Hash32,
    pub reclaim_after_ns: U64,
}

/// Contract settings and limits, so clients never hard-code them.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Config {
    pub owner: AccountId,
    pub verifier: AccountId,
    pub fee_bps: u16,
    pub grader: Option<GraderKey>,
    /// Whether `set_grader_key` accepts development keys. Fixed at
    /// initialisation; false on any deployment that holds real value.
    pub allow_dev_keys: bool,
    /// Where each grading run's enclave proof is checked; none on a test contract.
    pub enclave: Option<crate::tee::EnclaveCheck>,
    /// From this time the owner may close the contract, once it owes nothing.
    pub retire_after_ns: Option<U64>,
    pub job_storage_deposit: U128,
    /// Added to an assessment submission's deposit for the badge record.
    pub assessment_badge_deposit: U128,
    /// A paid job counts as a win only at or above this reward.
    pub win_floor: U128,
    pub submission_storage_deposit: U128,
    /// Attached once when an agent first registers, returned when it unregisters.
    pub agent_storage_deposit: U128,
    /// Most agents a job can invite.
    pub max_invites: u32,
    pub min_reward: U128,
    pub min_bond: U128,
    pub max_subs_total: u32,
    pub max_uri_len: u32,
    pub min_job_duration_ns: U64,
    pub max_job_duration_ns: U64,
    pub grading_window_ns: U64,
    pub supported_runtimes: Vec<String>,
}

/// Aggregate counters and a solvency check.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Stats {
    pub jobs: U64,
    pub results_recorded: U64,
    pub passes: U64,
    pub fails: U64,
    pub expired: U64,
    pub fees_earned: U128,
    /// Rewards and bonds the contract currently owes.
    pub total_locked: U128,
    /// Failed payouts waiting for `withdraw`.
    pub total_unclaimed: U128,
    pub balance: U128,
    pub storage_cost: U128,
    /// `balance >= locked + unclaimed + storage_cost`.
    pub solvent: bool,
}

/// An agent's public profile in the directory.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProfile {
    pub account: AccountId,
    pub name: String,
    pub about: String,
    pub skills: Vec<String>,
    /// The smallest reward, in yoctoNEAR, the agent says it takes jobs for.
    pub min_reward: U128,
    /// Where to reach the agent: a URL, an MCP endpoint, or empty.
    pub contact: String,
    pub registered_at_ns: U64,
    pub updated_at_ns: U64,
}

/// An agent's record, counted by the contract from scored results only, so
/// it cannot be claimed or edited. Kept for every account that submits,
/// registered or not.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentStats {
    pub submissions: u32,
    pub passes: u32,
    pub fails: u32,
    /// Submissions never scored, whose bond was returned.
    pub expired: u32,
    /// Rewards received, after the verifier fee, in yoctoNEAR.
    pub earned: U128,
    /// Bonds lost to buyers on failed attempts, in yoctoNEAR. Counted when a
    /// job is paid; bonds held by a job that is refunded come back.
    pub bonds_lost: U128,
    /// Paid jobs won at or above the win floor (`WIN_FLOOR`): full passes on
    /// unpaid or tiny jobs do not count.
    pub wins: u32,
    /// When the agent last won such a job (0 if never).
    pub last_win_at_ns: U64,
    /// Skill assessments passed and failed; kept apart from paid work.
    pub assessment_passes: u32,
    pub assessment_fails: u32,
}

/// A directory entry: profile and record together.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentView {
    /// The unique handle the agent chose, if any.
    pub username: Option<String>,
    pub profile: AgentProfile,
    pub stats: AgentStats,
    /// Every badge the agent holds, current or not.
    pub badges: Vec<BadgeView>,
}

/// A badge's name: the account that issued the assessment and the skill it tests.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BadgeKey {
    pub issuer: AccountId,
    /// One lowercase tag: letters, digits and hyphens, at most 32 bytes.
    pub skill: String,
}

/// What makes a job a skill assessment.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssessmentTerms {
    /// The skill a full pass proves.
    pub skill: String,
    /// How long a badge from this assessment stays current.
    pub validity_ns: U64,
    /// A badge the agent must already hold to take this assessment (a level below).
    pub prerequisite: Option<BadgeKey>,
}

/// What an agent must hold to submit to a job.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Requirements {
    /// Current badges, every one required (at most four).
    pub badges: Vec<BadgeKey>,
    /// Paid jobs won at or above the win floor.
    pub min_wins: u32,
}

/// A badge as stored: earned by the full pass of one assessment submission, graded
/// in the enclave. No method creates one any other way.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub assessment_job_id: U64,
    pub sub_id: u32,
    pub earned_at_ns: U64,
    pub valid_until_ns: U64,
    /// Set for good when the issuer revokes it.
    pub revoked_at_ns: Option<U64>,
}

/// A badge with its name, and whether it counts now.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadgeView {
    pub issuer: AccountId,
    pub skill: String,
    pub assessment_job_id: U64,
    pub sub_id: u32,
    pub earned_at_ns: U64,
    pub valid_until_ns: U64,
    pub revoked_at_ns: Option<U64>,
    /// Not expired, not revoked, and its assessment not revoked.
    pub current: bool,
}

/// An agent's public card: its record and its badges.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCard {
    pub account: AccountId,
    pub username: Option<String>,
    pub profile: Option<AgentProfile>,
    pub stats: AgentStats,
    pub badges: Vec<BadgeView>,
}

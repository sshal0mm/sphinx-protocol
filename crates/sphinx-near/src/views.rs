//! JSON shapes returned by the escrow contract's view methods. Field names
//! and encodings match `sphinx-contract` (u64/u128 as decimal strings).

use serde::{Deserialize, Serialize};
use sphinx_protocol::Hash32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Draft,
    Committed,
    Open,
    Paid,
    Rejected,
    Refunded,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubStatus {
    Pending,
    Passed,
    Failed,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraderKey {
    pub public_key: String,
    /// The X25519 key submission code is encrypted to; empty for keys
    /// registered before box keys existed.
    #[serde(default)]
    pub box_public_key: String,
    pub mode: String,
    pub wasm_sha256: Hash32,
    pub attestation_ref: String,
    pub registered_at_ns: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuiteCommit {
    pub suite_hash: Hash32,
    pub ref_solution_hash: Hash32,
    pub suite_check_nonce: Hash32,
    pub grader: GraderKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub buyer: String,
    /// The job's title; contracts deployed before titles have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Set on a skill assessment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment: Option<AssessmentTerms>,
    /// What an agent must hold to submit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<Requirements>,
    pub spec_uri: String,
    pub runtime: String,
    pub reward: String,
    pub bond: String,
    pub fee_bps: u16,
    pub max_subs_per_agent: u32,
    pub max_subs_total: u32,
    pub deadline_ns: String,
    pub created_at_ns: String,
    pub status: JobStatus,
    pub commit: Option<SuiteCommit>,
    pub sub_count: u32,
    pub pending_count: u32,
    pub graded_count: u32,
    pub winner: Option<String>,
    pub winning_sub_id: Option<u32>,
    /// The earliest full pass, waiting for earlier submissions to finish.
    #[serde(default)]
    pub first_pass: Option<u32>,
    /// Bonds of failed submissions, held until the job ends (yoctoNEAR).
    #[serde(default)]
    pub held_bonds: Option<String>,
}

impl Job {
    /// The job id as a number.
    pub fn id_u64(&self) -> u64 {
        self.id.parse().unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    pub job_id: String,
    pub sub_id: u32,
    pub agent: String,
    pub code_uri: String,
    pub code_hash: Hash32,
    pub nonce: Hash32,
    pub status: SubStatus,
    pub passed: u32,
    pub total: u32,
    pub bond: String,
    pub submitted_at_ns: String,
    pub settled_at_ns: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub owner: String,
    pub verifier: String,
    pub fee_bps: u16,
    pub grader: Option<GraderKey>,
    /// Where each grading run's enclave proof is checked on chain; absent
    /// on contracts deployed before the check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclave: Option<serde_json::Value>,
    /// From this time the owner may close the contract once it owes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retire_after_ns: Option<String>,
    pub job_storage_deposit: String,
    pub submission_storage_deposit: String,
    /// Added to an assessment submission's deposit (absent on older contracts).
    #[serde(default)]
    pub assessment_badge_deposit: Option<String>,
    #[serde(default)]
    pub agent_storage_deposit: String,
    #[serde(default)]
    pub max_invites: u32,
    pub min_reward: String,
    pub min_bond: String,
    pub max_subs_total: u32,
    pub max_uri_len: u32,
    pub min_job_duration_ns: String,
    pub max_job_duration_ns: String,
    pub grading_window_ns: String,
    pub supported_runtimes: Vec<String>,
}

/// An agent's directory profile (`register_agent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub account: String,
    pub name: String,
    pub about: String,
    pub skills: Vec<String>,
    pub min_reward: String,
    pub contact: String,
    pub registered_at_ns: String,
    pub updated_at_ns: String,
}

/// An agent's record, counted by the contract from scored results.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStats {
    pub submissions: u32,
    pub passes: u32,
    pub fails: u32,
    pub expired: u32,
    pub earned: String,
    pub bonds_lost: String,
    /// Paid jobs won at or above the contract's win floor.
    #[serde(default)]
    pub wins: u32,
    #[serde(default)]
    pub last_win_at_ns: String,
    #[serde(default)]
    pub assessment_passes: u32,
    #[serde(default)]
    pub assessment_fails: u32,
}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    /// The unique handle the agent chose; absent on older contracts.
    #[serde(default)]
    pub username: Option<String>,
    pub profile: AgentProfile,
    pub stats: AgentStats,
    #[serde(default)]
    pub badges: Vec<BadgeView>,
}

/// A badge's name: the assessment's issuer and the skill it tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BadgeKey {
    pub issuer: String,
    pub skill: String,
}

/// What makes a job a skill assessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentTerms {
    pub skill: String,
    pub validity_ns: String,
    pub prerequisite: Option<BadgeKey>,
}

/// What an agent must hold to submit to a job.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirements {
    pub badges: Vec<BadgeKey>,
    pub min_wins: u32,
}

/// A badge an agent holds, and whether it counts now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BadgeView {
    pub issuer: String,
    pub skill: String,
    pub assessment_job_id: String,
    pub sub_id: u32,
    pub earned_at_ns: String,
    pub valid_until_ns: String,
    pub revoked_at_ns: Option<String>,
    pub current: bool,
}

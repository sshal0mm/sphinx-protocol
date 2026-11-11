//! Economic and safety bounds. Exposed to clients through `get_config`.

use near_sdk::{Gas, NearToken};

/// Storage deposit consumed by `post_job`; covers the job record for its lifetime.
pub const JOB_STORAGE_DEPOSIT: NearToken = NearToken::from_millinear(25);
/// Storage deposit consumed by `submit`; covers the submission record.
pub const SUBMISSION_STORAGE_DEPOSIT: NearToken =
    NearToken::from_yoctonear(12_500_000_000_000_000_000_000);

pub const MIN_REWARD: NearToken = NearToken::from_millinear(100);
pub const MIN_BOND: NearToken = NearToken::from_millinear(10);

/// Upper bound on submissions per job; keeps every per-job structure bounded.
pub const MAX_SUBS_TOTAL: u32 = 64;
pub const MAX_URI_LEN: usize = 512;
/// Longest job title, in characters.
pub const MAX_TITLE_CHARS: usize = sphinx_protocol::title::MAX_TITLE_CHARS;
pub const MAX_ATTESTATION_REF_LEN: usize = 256;

/// Verifier fee ceiling in basis points (20%).
pub const MAX_FEE_BPS: u16 = 2_000;

/// Paid once when an agent first registers; returned when it unregisters.
pub const AGENT_STORAGE_DEPOSIT: NearToken = NearToken::from_millinear(20);
pub const MAX_AGENT_NAME_LEN: usize = 64;
/// Registered agents whose deposits `retire` returns in one call.
pub const MAX_RETIRE_REFUNDS: u32 = 40;
/// Bounds of an agent's username, in bytes.
pub const MIN_USERNAME_LEN: usize = 3;
pub const MAX_USERNAME_LEN: usize = 24;
pub const MAX_AGENT_ABOUT_LEN: usize = 280;
pub const MAX_AGENT_SKILLS: usize = 8;
pub const MAX_AGENT_SKILL_LEN: usize = 32;
pub const MAX_AGENT_CONTACT_LEN: usize = 256;
/// Most agents a buyer may invite to one job.
pub const MAX_INVITES: usize = 8;

const MINUTE_NS: u64 = 60 * 1_000_000_000;
pub const MIN_JOB_DURATION_NS: u64 = 10 * MINUTE_NS;
pub const MAX_JOB_DURATION_NS: u64 = 30 * 24 * 60 * MINUTE_NS;

/// How long a submission may wait for a result before its bond can be
/// reclaimed, and how long after the deadline a buyer must wait to refund.
pub const GRADING_WINDOW_NS: u64 = sphinx_protocol::GRADING_WINDOW_NS;

/// Gas reserved for the callback that records a failed payout.
pub const PAYOUT_CALLBACK_GAS: Gas = Gas::from_tgas(5);
/// Bonds one call returns by transfer when it returns many (a settle that
/// expires waiting submissions, or a payout that returns later passes' bonds).
/// The rest are credited to `unclaimed` for `withdraw`, so the largest job
/// stays well inside the gas a caller attaches.
pub const MAX_BOND_TRANSFERS_PER_CALL: u32 = 4;

/// Smallest first deposit into a prepaid agent balance.
pub const MIN_BALANCE_DEPOSIT: NearToken = NearToken::from_millinear(50);
/// Held from the first deposit to pay for the balance record's storage, and
/// returned with the rest when the balance is withdrawn in full.
pub const BALANCE_STORAGE_DEPOSIT: NearToken = NearToken::from_millinear(1);

/// Most badges a job may require.
pub const MAX_REQUIRED_BADGES: usize = 4;
/// Most badges one agent holds; each issuer-skill pair is one badge.
pub const MAX_BADGES_PER_AGENT: usize = 16;
/// Shortest and longest a badge may stay current.
pub const MIN_BADGE_VALIDITY_NS: u64 = 86_400_000_000_000;
pub const MAX_BADGE_VALIDITY_NS: u64 = 365 * 86_400_000_000_000;
/// Longest public reason for a revocation, in bytes.
pub const MAX_REVOKE_REASON_LEN: usize = 140;
/// Added to an assessment submission's deposit: it pays for the badge record a
/// pass writes, and is not returned.
pub const ASSESSMENT_BADGE_DEPOSIT: NearToken = NearToken::from_millinear(5);
/// A paid job counts as a win only at or above this reward.
pub const WIN_FLOOR: NearToken = NearToken::from_millinear(200);

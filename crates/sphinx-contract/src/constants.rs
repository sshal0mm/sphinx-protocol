//! Economic and safety bounds. Exposed to clients through `get_config`.

use near_sdk::{Gas, NearToken};

/// Storage deposit consumed by `post_job`; covers the job record for its lifetime.
pub const JOB_STORAGE_DEPOSIT: NearToken = NearToken::from_millinear(20);
/// Storage deposit consumed by `submit`; covers the submission record.
pub const SUBMISSION_STORAGE_DEPOSIT: NearToken =
    NearToken::from_yoctonear(12_500_000_000_000_000_000_000);

pub const MIN_REWARD: NearToken = NearToken::from_millinear(100);
pub const MIN_BOND: NearToken = NearToken::from_millinear(10);

/// Upper bound on submissions per job; keeps every per-job structure bounded.
pub const MAX_SUBS_TOTAL: u32 = 64;
pub const MAX_URI_LEN: usize = 512;
pub const MAX_ATTESTATION_REF_LEN: usize = 256;

/// Verifier fee ceiling in basis points (20%).
pub const MAX_FEE_BPS: u16 = 2_000;

/// Paid once when an agent first registers; returned when it unregisters.
pub const AGENT_STORAGE_DEPOSIT: NearToken = NearToken::from_millinear(20);
pub const MAX_AGENT_NAME_LEN: usize = 64;
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

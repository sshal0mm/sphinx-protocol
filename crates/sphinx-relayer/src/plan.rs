//! Deciding what the relayer should do, as a pure function of chain state.

use sphinx_near::views::{Job, JobStatus, SubStatus, Submission};
use sphinx_protocol::GRADING_WINDOW_NS;

/// One unit of work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Work {
    /// Prove a committed suite is passable and post `record_suite_check`.
    CheckSuite { job_id: u64 },
    /// Grade a pending submission and post `record_score`.
    Score { job_id: u64, sub_id: u32 },
    /// Return the bond of a submission left pending when its job ended
    /// (`reclaim_bond`, which always pays the submitting agent).
    ReturnBond { job_id: u64, sub_id: u32 },
    /// Pay a passing submission that was waiting for earlier ones (`settle`).
    Settle { job_id: u64 },
}

impl Work {
    /// A stable key for retry bookkeeping.
    pub fn key(&self) -> (u64, Option<u32>) {
        match *self {
            Self::CheckSuite { job_id } | Self::Settle { job_id } => (job_id, None),
            Self::Score { job_id, sub_id } | Self::ReturnBond { job_id, sub_id } => {
                (job_id, Some(sub_id))
            }
        }
    }
}

/// Work for one job given its submissions, oldest first, at chain time `now_ns`.
///
/// When a pass is waiting (`first_pass`), only earlier pending submissions
/// can still change the winner, so only those are graded; `settle` is
/// planned once each of them has finished or passed its grading window.
pub fn plan(job: &Job, submissions: &[Submission], now_ns: u64) -> Vec<Work> {
    let job_id = job.id_u64();
    let pending = submissions.iter().filter(|s| s.status == SubStatus::Pending);
    match job.status {
        JobStatus::Committed => vec![Work::CheckSuite { job_id }],
        JobStatus::Open => match job.first_pass {
            None => pending.map(|s| Work::Score { job_id, sub_id: s.sub_id }).collect(),
            Some(first) => {
                let earlier: Vec<&Submission> = pending.filter(|s| s.sub_id < first).collect();
                let mut work: Vec<Work> =
                    earlier.iter().map(|s| Work::Score { job_id, sub_id: s.sub_id }).collect();
                let window_over = |s: &&Submission| {
                    s.submitted_at_ns
                        .parse::<u64>()
                        .is_ok_and(|t| now_ns >= t.saturating_add(GRADING_WINDOW_NS))
                };
                if earlier.iter().all(window_over) {
                    work.push(Work::Settle { job_id });
                }
                work
            }
        },
        JobStatus::Paid | JobStatus::Rejected | JobStatus::Refunded | JobStatus::Cancelled => {
            pending.map(|s| Work::ReturnBond { job_id, sub_id: s.sub_id }).collect()
        }
        JobStatus::Draft => Vec::new(),
    }
}

/// When this job will next have work that time alone brings (nanoseconds):
/// a pass waiting on earlier submissions can be settled once their grading
/// windows close. `None` when only a new action (a submission, a post) can
/// bring work.
pub fn next_due(job: &Job, submissions: &[Submission], now_ns: u64) -> Option<u64> {
    if job.status != JobStatus::Open {
        return None;
    }
    let first = job.first_pass?;
    let ends: Vec<u64> = submissions
        .iter()
        .filter(|s| s.status == SubStatus::Pending && s.sub_id < first)
        .filter_map(|s| s.submitted_at_ns.parse::<u64>().ok())
        .map(|t| t.saturating_add(GRADING_WINDOW_NS))
        .collect();
    let last = ends.into_iter().max()?;
    (last > now_ns).then_some(last)
}

/// How long the relayer sleeps before its next look: right away again while
/// it is doing work or was just nudged, else until the next time-driven
/// work, but never longer than `idle` (the safety net for calls made
/// straight to the contract, which nobody nudges about).
pub fn next_wait(
    busy: bool,
    nudged_recently: bool,
    due_in: Option<std::time::Duration>,
    poll: std::time::Duration,
    idle: std::time::Duration,
) -> std::time::Duration {
    if busy || nudged_recently {
        return poll;
    }
    due_in.map_or(idle, |d| d.clamp(poll, idle))
}

/// Whether a job can have work at all (so its submissions are worth reading).
pub fn is_active(job: &Job) -> bool {
    matches!(job.status, JobStatus::Committed | JobStatus::Open) || job.pending_count > 0
}

/// Whether the planner needs this job's submissions (it does whenever any
/// are pending, and when a waiting pass may be ready to settle).
pub fn needs_submissions(job: &Job) -> bool {
    job.pending_count > 0 || job.first_pass.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relayer_sleeps_until_there_is_a_reason() {
        use std::time::Duration;
        let (poll, idle) = (Duration::from_secs(6), Duration::from_mins(5));
        // Working or just nudged: look again soon.
        assert_eq!(next_wait(true, false, None, poll, idle), poll);
        assert_eq!(next_wait(false, true, None, poll, idle), poll);
        // Nothing to do: the safety net only.
        assert_eq!(next_wait(false, false, None, poll, idle), idle);
        // A grading window closing in a minute: wake then, not before.
        assert_eq!(
            next_wait(false, false, Some(Duration::from_mins(1)), poll, idle),
            Duration::from_mins(1)
        );
        // Never sooner than a poll, never later than the safety net.
        assert_eq!(next_wait(false, false, Some(Duration::ZERO), poll, idle), poll);
        assert_eq!(next_wait(false, false, Some(Duration::from_hours(3)), poll, idle), idle);
    }
    use serde_json::json;

    fn job(status: &str, pending: u32) -> Job {
        serde_json::from_value(json!({
            "id": "3", "buyer": "b.testnet", "spec_uri": "", "runtime": "js-boa/1", "reward": "1", "bond": "1",
            "fee_bps": 0, "max_subs_per_agent": 1, "max_subs_total": 4, "deadline_ns": "1", "created_at_ns": "0",
            "status": status, "commit": null, "sub_count": pending, "pending_count": pending, "graded_count": 0,
            "winner": null, "winning_sub_id": null, "first_pass": null, "held_bonds": "0"
        }))
        .unwrap()
    }

    fn sub(id: u32, status: &str) -> Submission {
        serde_json::from_value(json!({
            "job_id": "3", "sub_id": id, "agent": "a.testnet", "code_uri": "x", "code_hash": "ab".repeat(32),
            "nonce": "cd".repeat(32), "status": status, "passed": 0, "total": 0, "bond": "1",
            "submitted_at_ns": "100", "settled_at_ns": null
        }))
        .unwrap()
    }

    #[test]
    fn ended_jobs_return_bonds_left_pending() {
        let subs = [sub(0, "Passed"), sub(1, "Pending")];
        let paid = job("Paid", 1);
        assert!(is_active(&paid));
        assert_eq!(plan(&paid, &subs, 0), vec![Work::ReturnBond { job_id: 3, sub_id: 1 }]);
        assert!(!is_active(&job("Paid", 0)));
    }

    #[test]
    fn a_waiting_pass_grades_only_earlier_submissions_then_settles() {
        let mut j = job("Open", 2);
        j.first_pass = Some(2);
        let subs = [sub(0, "Failed"), sub(1, "Pending"), sub(2, "Passed"), sub(3, "Pending")];
        assert_eq!(plan(&j, &subs, 0), vec![Work::Score { job_id: 3, sub_id: 1 }]);
        let later = 100 + GRADING_WINDOW_NS;
        assert_eq!(
            plan(&j, &subs, later),
            vec![Work::Score { job_id: 3, sub_id: 1 }, Work::Settle { job_id: 3 }]
        );
        let nothing_earlier = [sub(0, "Failed"), sub(1, "Expired"), sub(2, "Passed")];
        assert_eq!(plan(&j, &nothing_earlier, 0), vec![Work::Settle { job_id: 3 }]);
    }

    #[test]
    fn committed_jobs_need_a_suite_check() {
        assert_eq!(plan(&job("Committed", 0), &[], 0), vec![Work::CheckSuite { job_id: 3 }]);
    }

    #[test]
    fn open_jobs_need_scores_for_pending_submissions_only() {
        let subs = [sub(0, "Failed"), sub(1, "Pending"), sub(2, "Expired"), sub(3, "Pending")];
        assert_eq!(
            plan(&job("Open", 2), &subs, 0),
            vec![Work::Score { job_id: 3, sub_id: 1 }, Work::Score { job_id: 3, sub_id: 3 }]
        );
    }

    #[test]
    fn ended_jobs_need_nothing_once_no_bond_is_pending() {
        for status in ["Paid", "Rejected", "Refunded", "Cancelled"] {
            assert_eq!(plan(&job(status, 0), &[sub(0, "Failed")], 0), vec![], "{status}");
            assert!(!is_active(&job(status, 0)), "{status}");
            assert_eq!(
                plan(&job(status, 1), &[sub(0, "Pending")], 0),
                vec![Work::ReturnBond { job_id: 3, sub_id: 0 }],
                "{status}"
            );
        }
        assert_eq!(plan(&job("Draft", 0), &[], 0), vec![]);
        assert!(!is_active(&job("Draft", 0)));
    }
}

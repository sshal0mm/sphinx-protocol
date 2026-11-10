//! A minimal single-threaded executor with a shared poll budget.
//!
//! Boa's budgeted evaluation yields every `CYCLES_PER_POLL` VM cycles. Each
//! yield is one `Poll::Pending`, so counting polls counts work. The count is
//! deterministic: VM cycles, not wall time.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Polls left for the whole grading run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    remaining: u32,
}

impl Budget {
    pub const fn new(polls: u32) -> Self {
        Self { remaining: polls }
    }

    pub const fn remaining(&self) -> u32 {
        self.remaining
    }

    pub const fn is_spent(&self) -> bool {
        self.remaining == 0
    }
}

/// The future did not complete within its share of the budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded;

/// Drives `future` to completion using at most `cap` polls from `budget`.
/// Polls used are deducted whether or not the future completes.
///
/// # Errors
/// Returns [`BudgetExceeded`] if the future is still pending when its share
/// runs out.
pub fn block_on_budgeted<F: Future>(
    future: F,
    cap: u32,
    budget: &mut Budget,
) -> Result<F::Output, BudgetExceeded> {
    let allowed = cap.min(budget.remaining);
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    let mut used = 0;
    loop {
        if let Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            budget.remaining -= used;
            return Ok(out);
        }
        used += 1;
        if used > allowed {
            budget.remaining -= allowed;
            return Err(BudgetExceeded);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct YieldN(u32);
    impl Future for YieldN {
        type Output = &'static str;
        fn poll(mut self: std::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            if self.0 == 0 {
                Poll::Ready("done")
            } else {
                self.0 -= 1;
                Poll::Pending
            }
        }
    }

    #[test]
    fn completes_and_charges_what_it_used() {
        let mut b = Budget::new(10);
        assert_eq!(block_on_budgeted(YieldN(3), 5, &mut b), Ok("done"));
        assert_eq!(b.remaining(), 7);
    }

    #[test]
    fn per_call_cap_applies() {
        let mut b = Budget::new(10);
        assert_eq!(block_on_budgeted(YieldN(6), 5, &mut b), Err(BudgetExceeded));
        assert_eq!(b.remaining(), 5);
    }

    #[test]
    fn shared_budget_runs_out() {
        let mut b = Budget::new(4);
        assert_eq!(block_on_budgeted(YieldN(6), 5, &mut b), Err(BudgetExceeded));
        assert!(b.is_spent());
        assert_eq!(
            block_on_budgeted(YieldN(0), 5, &mut b),
            Ok("done"),
            "work needing no yield still completes"
        );
    }
}

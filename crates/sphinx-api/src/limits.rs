//! Limits on gasless relaying, so a relay's gas cannot be drained.
//!
//! Two independent bounds per sending account:
//! - **Volume:** a token bucket admits at most `burst` calls at once and
//!   `per_minute` calls per minute after that.
//! - **Failures:** a relayed call that fails on-chain still costs the relay
//!   its gas. After `max_failures` such calls within `cooldown`, the account
//!   is refused for `cooldown`.
//!
//! A shared bucket also bounds the relay's total rate across all accounts.
//! State is in memory and per process.

use sphinx_protocol::{Code, SphinxError};
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Relay limits. `Default` holds the production values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayLimits {
    /// Calls one account may send at once.
    pub burst: u32,
    /// Sustained calls per minute for one account.
    pub per_minute: u32,
    /// Sustained calls per minute across all accounts.
    pub global_per_minute: u32,
    /// Failed on-chain calls allowed within `cooldown` before the account is paused.
    pub max_failures: u32,
    /// Failure window, and how long a paused account is refused.
    pub cooldown: Duration,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            burst: 5,
            per_minute: 10,
            global_per_minute: 300,
            max_failures: 3,
            cooldown: Duration::from_secs(3600),
        }
    }
}

/// Accounts tracked before idle entries are dropped.
const MAX_TRACKED: usize = 10_000;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl Bucket {
    fn full(capacity: u32, now: Instant) -> Self {
        Self { tokens: f64::from(capacity), updated: now }
    }

    /// Takes one token, or returns the seconds until one is available.
    fn take(&mut self, capacity: u32, per_minute: u32, now: Instant) -> Result<(), u64> {
        let rate = f64::from(per_minute) / 60.0;
        let elapsed = now.saturating_duration_since(self.updated).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(f64::from(capacity));
        self.updated = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        let wait = if rate > 0.0 { ((1.0 - self.tokens) / rate).ceil() } else { f64::MAX };
        // Clamped to a day; the float is finite and non-negative here.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped above")]
        Err(wait.min(86_400.0) as u64)
    }
}

#[derive(Debug, Clone, Copy)]
struct Account {
    bucket: Bucket,
    failures: u32,
    window_start: Instant,
    paused_until: Option<Instant>,
}

#[derive(Debug)]
struct State {
    global: Bucket,
    /// Key lookups on chain, which come before an account is known to be real.
    lookups: Bucket,
    accounts: HashMap<String, Account>,
}

/// Admits or refuses relayed calls per account.
#[derive(Debug)]
pub struct RelayLimiter {
    limits: RelayLimits,
    state: Mutex<State>,
}

fn limited(account: &str, why: &str, retry_after_secs: u64) -> SphinxError {
    SphinxError::new(Code::ApiRateLimited, format!("{account}: {why}"))
        .ctx("account", account)
        .ctx("retry_after_secs", retry_after_secs.max(1))
}

impl RelayLimiter {
    pub fn new(limits: RelayLimits, now: Instant) -> Self {
        Self {
            limits,
            state: Mutex::new(State {
                global: Bucket::full(limits.global_per_minute, now),
                lookups: Bucket::full(limits.global_per_minute.saturating_mul(2), now),
                accounts: HashMap::new(),
            }),
        }
    }

    pub const fn limits(&self) -> RelayLimits {
        self.limits
    }

    /// Admits one on-chain key lookup for a call not yet known to come from
    /// its sender. Bounds the RPC reads unproven requests can cause.
    ///
    /// # Errors
    /// `SPHINX_E_API_RATE_LIMITED` when the relay is over its lookup rate.
    pub fn admit_global(&self, now: Instant) -> Result<(), SphinxError> {
        let rate = self.limits.global_per_minute.saturating_mul(2);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.lookups.take(rate, rate, now).map_err(|secs| {
            SphinxError::new(
                Code::ApiRateLimited,
                "the relay is at its total rate; try again shortly",
            )
            .ctx("retry_after_secs", secs.max(1))
        })
    }

    /// Admits one call from `account`, or refuses it with
    /// `SPHINX_E_API_RATE_LIMITED` and `ctx.retry_after_secs`.
    ///
    /// # Errors
    /// `SPHINX_E_API_RATE_LIMITED` when the account is paused or over its rate,
    /// or the relay is over its total rate.
    pub fn admit(&self, account: &str, now: Instant) -> Result<(), SphinxError> {
        let l = self.limits;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.accounts.len() >= MAX_TRACKED {
            let idle = l.cooldown;
            state.accounts.retain(|_, a| {
                a.paused_until.is_some_and(|t| t > now)
                    || now.saturating_duration_since(a.bucket.updated) < idle
            });
        }
        let entry = state.accounts.entry(account.to_owned()).or_insert_with(|| Account {
            bucket: Bucket::full(l.burst, now),
            failures: 0,
            window_start: now,
            paused_until: None,
        });
        if let Some(until) = entry.paused_until {
            if until > now {
                return Err(limited(
                    account,
                    &format!(
                        "{} relayed calls failed on-chain; relaying is paused for this account",
                        l.max_failures
                    ),
                    until.saturating_duration_since(now).as_secs(),
                ));
            }
            entry.paused_until = None;
            entry.failures = 0;
            entry.window_start = now;
        }
        entry.bucket.take(l.burst, l.per_minute, now).map_err(|secs| {
            limited(account, &format!("more than {} relayed calls per minute", l.per_minute), secs)
        })?;
        state.global.take(l.global_per_minute, l.global_per_minute, now).map_err(|secs| {
            limited(account, "the relay is at its total rate; try again shortly", secs)
        })
    }

    /// Records that a relayed call from `account` failed on-chain.
    pub fn record_failure(&self, account: &str, now: Instant) {
        let l = self.limits;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(entry) = state.accounts.get_mut(account) else { return };
        if now.saturating_duration_since(entry.window_start) >= l.cooldown {
            entry.failures = 0;
            entry.window_start = now;
        }
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= l.max_failures {
            entry.paused_until = Some(now + l.cooldown);
        }
    }
}

/// A rate per client plus a total rate, for endpoints that write (code
/// uploads). Clients are named by the address the web proxy reports.
#[derive(Debug)]
pub struct ClientLimiter {
    burst: u32,
    per_minute: u32,
    global_per_minute: u32,
    state: Mutex<(Bucket, HashMap<String, Bucket>)>,
}

impl ClientLimiter {
    pub fn new(burst: u32, per_minute: u32, global_per_minute: u32, now: Instant) -> Self {
        Self {
            burst,
            per_minute,
            global_per_minute,
            state: Mutex::new((Bucket::full(global_per_minute, now), HashMap::new())),
        }
    }

    /// Admits one request from `client`.
    ///
    /// # Errors
    /// `SPHINX_E_API_RATE_LIMITED` with `ctx.retry_after_secs`.
    pub fn admit(&self, client: &str, now: Instant) -> Result<(), SphinxError> {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (global, clients) = &mut *guard;
        if clients.len() >= MAX_TRACKED {
            clients
                .retain(|_, b| now.saturating_duration_since(b.updated) < Duration::from_secs(600));
            if clients.len() >= MAX_TRACKED {
                clients.clear();
            }
        }
        let (burst, per_minute) = (self.burst, self.per_minute);
        clients
            .entry(client.to_owned())
            .or_insert_with(|| Bucket::full(burst, now))
            .take(burst, per_minute, now)
            .map_err(|secs| {
                limited(client, &format!("more than {per_minute} uploads per minute"), secs)
            })?;
        global.take(self.global_per_minute, self.global_per_minute, now).map_err(|secs| {
            limited(client, "the API is at its total upload rate; try again shortly", secs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: RelayLimits = RelayLimits {
        burst: 2,
        per_minute: 6,
        global_per_minute: 100,
        max_failures: 2,
        cooldown: Duration::from_secs(600),
    };

    fn code(r: Result<(), SphinxError>) -> Option<(Code, String)> {
        r.err().map(|e| (e.code(), e.context()["retry_after_secs"].clone()))
    }

    #[test]
    fn burst_then_sustained_rate() {
        let t0 = Instant::now();
        let l = RelayLimiter::new(LIMITS, t0);
        assert!(l.admit("a", t0).is_ok());
        assert!(l.admit("a", t0).is_ok());
        // 6 per minute refills one token every 10 s.
        assert_eq!(code(l.admit("a", t0)), Some((Code::ApiRateLimited, "10".into())));
        assert!(l.admit("b", t0).is_ok(), "accounts are limited separately");
        assert!(l.admit("a", t0 + Duration::from_secs(10)).is_ok());
    }

    #[test]
    fn failures_pause_the_account_for_the_cooldown() {
        let t0 = Instant::now();
        let l = RelayLimiter::new(LIMITS, t0);
        assert!(l.admit("a", t0).is_ok());
        l.record_failure("a", t0);
        assert!(l.admit("a", t0).is_ok());
        l.record_failure("a", t0);
        assert_eq!(code(l.admit("a", t0)), Some((Code::ApiRateLimited, "600".into())));
        assert_eq!(
            code(l.admit("a", t0 + Duration::from_secs(599))),
            Some((Code::ApiRateLimited, "1".into()))
        );
        assert!(l.admit("a", t0 + Duration::from_secs(600)).is_ok(), "pause ends");
        l.record_failure("a", t0 + Duration::from_secs(601));
        assert!(l.admit("a", t0 + Duration::from_secs(700)).is_ok(), "failure count restarted");
    }

    #[test]
    fn failures_outside_the_window_are_forgotten() {
        let t0 = Instant::now();
        let l = RelayLimiter::new(LIMITS, t0);
        assert!(l.admit("a", t0).is_ok());
        l.record_failure("a", t0);
        l.record_failure("a", t0 + Duration::from_secs(601));
        assert!(l.admit("a", t0 + Duration::from_secs(602)).is_ok());
    }

    #[test]
    fn global_rate_bounds_all_accounts() {
        let t0 = Instant::now();
        let l = RelayLimiter::new(RelayLimits { global_per_minute: 3, ..LIMITS }, t0);
        for who in ["a", "b", "c"] {
            assert!(l.admit(who, t0).is_ok());
        }
        assert_eq!(code(l.admit("d", t0)), Some((Code::ApiRateLimited, "20".into())));
    }

    #[test]
    fn uploads_are_limited_per_client_and_in_total() {
        let t0 = Instant::now();
        let l = ClientLimiter::new(2, 2, 3, t0);
        l.admit("a", t0).unwrap();
        l.admit("a", t0).unwrap();
        assert!(l.admit("a", t0).is_err(), "client a is over its burst");
        l.admit("b", t0).unwrap();
        assert!(l.admit("c", t0).is_err(), "the total rate binds every client");
        l.admit("a", t0 + Duration::from_secs(60)).unwrap();
    }
}

/// A count of events per UTC day, refusing past `max`.
#[derive(Debug)]
pub struct DailyCap {
    max: u32,
    state: Mutex<(u64, u32)>,
}

impl DailyCap {
    pub fn new(max: u32) -> Self {
        Self { max, state: Mutex::new((0, 0)) }
    }

    /// Counts one event at `now`.
    ///
    /// # Errors
    /// `SPHINX_E_API_RATE_LIMITED` once the day's count is used, with
    /// `ctx.retry_after_secs` until the next UTC day.
    pub fn take(&self, now: std::time::SystemTime) -> Result<(), SphinxError> {
        let secs = now.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let day = secs / 86_400;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.0 != day {
            *state = (day, 0);
        }
        if state.1 >= self.max {
            return Err(SphinxError::new(
                Code::ApiRateLimited,
                format!("the relay has sent its {} transactions for today", self.max),
            )
            .ctx("retry_after_secs", (day + 1) * 86_400 - secs));
        }
        state.1 += 1;
        Ok(())
    }
}

#[cfg(test)]
mod daily_tests {
    use super::*;

    #[test]
    fn the_daily_cap_refuses_past_its_count_and_resets_the_next_day() {
        let cap = DailyCap::new(2);
        let t = std::time::UNIX_EPOCH + Duration::from_secs(86_400 * 10 + 5);
        cap.take(t).unwrap();
        cap.take(t).unwrap();
        assert_eq!(cap.take(t).unwrap_err().code(), Code::ApiRateLimited);
        cap.take(t + Duration::from_hours(24)).unwrap();
    }
}

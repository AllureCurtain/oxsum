//! Per-key request rate limiting (roadmap P3-4, issue #130).
//!
//! A key may carry `requestsPerMinute`: how many requests it may start inside a
//! rolling minute, counted at admission — the gateway and `POST /api/v1/holds`
//! consume one slot before the hold is taken (docs/decisions.md: RPM consumes at
//! hold creation). The limiter is process state behind a trait: the in-memory
//! implementation is what a single binary needs today, and a shared backend
//! (Redis) is a drop-in when the deployment goes multi-instance.
//!
//! This is a protection limit, not a billing one: a restart forgives in-flight
//! allowance, and a refusal records nothing. Money stays where it always was —
//! the ledger.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

/// The rolling window `requestsPerMinute` counts inside.
const WINDOW: Duration = Duration::from_secs(60);

/// What an admitted request may report back: the allowance it consumed from.
/// Carried onto the response as the `X-RateLimit-*` headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateAllowance {
    /// The key's `requestsPerMinute`, echoed back for `X-RateLimit-Limit`.
    pub limit: u32,
    /// Requests the window still admits after this one (`X-RateLimit-Remaining`).
    pub remaining: u32,
    /// Until the oldest counted request leaves the window (`X-RateLimit-Reset`).
    pub resets_in: Duration,
}

/// A refusal: when the window frees one slot, for `Retry-After`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimited {
    /// The key's `requestsPerMinute`, echoed back for `X-RateLimit-Limit`.
    pub limit: u32,
    /// Until the oldest counted request leaves the window.
    pub retry_after: Duration,
}

/// Where the rolling windows live. The trait is the seam: `admit` is all the
/// admission path asks, so a Redis store can replace the process-local one
/// without the gateway changing.
pub trait RateLimiter: Send + Sync {
    /// Records one request for `key` when its window still has room, answering
    /// the allowance consumed. A full window answers [`RateLimited`] and records
    /// nothing — a refused request is not itself rate-limited further.
    fn admit(&self, key: Uuid, per_minute: u32, at: Instant) -> Result<RateAllowance, RateLimited>;
}

/// The in-process [`RateLimiter`]: one queue of instants per key, the oldest
/// dropped as they age out of the window. `Instant` is passed in rather than
/// read inside, so tests steer the clock and production passes
/// [`Instant::now`].
#[derive(Default)]
pub struct SlidingWindow {
    hits: Mutex<HashMap<Uuid, VecDeque<Instant>>>,
}

impl RateLimiter for SlidingWindow {
    fn admit(&self, key: Uuid, per_minute: u32, at: Instant) -> Result<RateAllowance, RateLimited> {
        if per_minute == 0 {
            // The column refuses zero; a zero reaching here uncaps rather than
            // refusing every request — a protection knob must fail open, never
            // take the gateway down with it.
            return Ok(RateAllowance {
                limit: 0,
                remaining: 0,
                resets_in: WINDOW,
            });
        }
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let queue = hits.entry(key).or_default();
        while queue
            .front()
            .is_some_and(|t| at.duration_since(*t) >= WINDOW)
        {
            queue.pop_front();
        }
        if queue.len() >= per_minute as usize {
            let oldest = queue.front().copied().unwrap_or(at);
            return Err(RateLimited {
                limit: per_minute,
                retry_after: WINDOW.saturating_sub(at.duration_since(oldest)),
            });
        }
        queue.push_back(at);
        let oldest = queue.front().copied().unwrap_or(at);
        let allowance = RateAllowance {
            limit: per_minute,
            remaining: per_minute - queue.len() as u32,
            resets_in: WINDOW.saturating_sub(at.duration_since(oldest)),
        };
        // A queue that has drained keeps no state: the map stays bounded by the
        // keys that were actually busy inside a window.
        if queue.is_empty() {
            hits.remove(&key);
        }
        Ok(allowance)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_full_window_refuses_until_the_oldest_ages_out() {
        let limiter = SlidingWindow::default();
        let key = Uuid::new_v4();
        let t0 = Instant::now();
        let first = limiter.admit(key, 2, t0).expect("the first fits");
        assert_eq!(first.remaining, 1);
        let second = limiter.admit(key, 2, t0).expect("the second fits");
        assert_eq!(second.remaining, 0);

        let refused = limiter
            .admit(key, 2, t0 + Duration::from_secs(30))
            .expect_err("the third is over the minute");
        assert_eq!(refused.retry_after, Duration::from_secs(30));

        // The refusal consumed nothing: it may retry freely once a slot frees.
        let allowed = limiter
            .admit(key, 2, t0 + WINDOW)
            .expect("the oldest has left the window");
        assert_eq!(allowed.remaining, 1);
    }

    #[test]
    fn windows_are_per_key() {
        let limiter = SlidingWindow::default();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let now = Instant::now();
        limiter.admit(a, 1, now).expect("a's first fits");
        limiter.admit(a, 1, now).expect_err("a's second is refused");
        limiter.admit(b, 1, now).expect("b is a different window");
    }
}

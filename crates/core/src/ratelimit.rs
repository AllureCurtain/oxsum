//! Per-key request rate limiting (roadmap P3-4, issue #130).
//!
//! A key may carry `requestsPerMinute`: how many requests it may start inside a
//! rolling minute, counted at admission — the gateway and `POST /api/v1/holds`
//! consume one slot before the hold is taken (docs/decisions.md: RPM consumes at
//! hold creation). The limiter is process state behind a trait: the in-memory
//! implementation is what a single binary needs today, and a shared backend
//! is a drop-in when the deployment goes multi-instance — `OXSUM_RATE_LIMIT_BACKEND`
//! picks it (issue #498): `memory` (the default) is [`SlidingWindow`], `postgres`
//! is [`PostgresRateLimiter`] against `oxsum.rate_windows`, so two instances behind
//! a load balancer count the same window instead of double-admitting.
//!
//! This is a protection limit, not a billing one: a restart forgives in-flight
//! allowance, and a refusal records nothing. Money stays where it always was —
//! the ledger. For the same reason both backends fail open: a `per_minute` of 0
//! reaching `admit`, or a storage error the shared backend cannot answer, uncaps
//! rather than taking the gateway down (the wallet's own `POOL_BUSY` backstop is
//! the real saturation refusal).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::PgPool;
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

    /// The allowance `admit` would answer now, recording nothing (issue #501):
    /// the entitlement read's peek at the same window admission judges. The
    /// window may change before a real request arrives — the allowance is a
    /// snapshot, like every figure that endpoint reports.
    fn peek(&self, key: Uuid, per_minute: u32, at: Instant) -> Result<RateAllowance, RateLimited>;
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

    fn peek(&self, key: Uuid, per_minute: u32, at: Instant) -> Result<RateAllowance, RateLimited> {
        if per_minute == 0 {
            // Same fail-open as `admit`: a zero reaching here uncaps rather
            // than refusing every request.
            return Ok(RateAllowance {
                limit: 0,
                remaining: 0,
                resets_in: WINDOW,
            });
        }
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        // The peek expires stale entries the way admit does — a window full of
        // aged-out hits must not report itself full — but it pushes nothing and
        // leaves an emptied queue behind, because the answer is a snapshot, not
        // a record.
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
        let oldest = queue.front().copied().unwrap_or(at);
        Ok(RateAllowance {
            limit: per_minute,
            remaining: per_minute - queue.len() as u32,
            resets_in: WINDOW.saturating_sub(at.duration_since(oldest)),
        })
    }
}

/// The shared [`RateLimiter`] (issue #498): the window lives in
/// `oxsum.rate_windows`, one row per limited key, so every instance behind a
/// load balancer counts the same hits instead of each keeping its own and
/// double-admitting. Selected by `OXSUM_RATE_LIMIT_BACKEND=postgres`.
///
/// Semantics are `SlidingWindow`'s, on the server's clock: `admit` takes the
/// key's row `FOR UPDATE` — the serialization the issue asks for, so two
/// concurrent admits on different nodes cannot both fit inside the cap — drops
/// hits aged out of the trailing minute (`hit > now() - interval '60 seconds'`),
/// and appends `now()` when room remains. The `at: Instant` the trait passes is
/// ignored: the database's `now()` is the single timeline every instance shares,
/// which is exactly what a process clock cannot give.
///
/// A storage error fails open — the limiter is a protection knob, and the
/// wallet's `POOL_BUSY` is the real saturation refusal; an unreachable backend
/// answering `RateLimited` would take the gateway down harder than the traffic
/// it guards against. The error is logged, never swallowed silently.
pub struct PostgresRateLimiter {
    pool: PgPool,
}

impl PostgresRateLimiter {
    /// Wraps the pool the server already runs — the shared backend needs no
    /// connection of its own, only the same Postgres everything else uses.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Runs the async admit/peek against the pool. `admit` is synchronous in the
    /// trait (the in-memory impl needs no runtime), so the shared backend enters
    /// the runtime the caller is already inside: `block_in_place` hands the
    /// blocking wait to a worker thread while `Handle::block_on` drives the
    /// future. Called only from async contexts — every call site sits under
    /// axum's runtime — so a missing handle is a bug, answered by failing open
    /// the same way a storage error does.
    fn run<Fut>(&self, future: Fut) -> Result<RateAllowance, RateLimited>
    where
        Fut: std::future::Future<Output = Result<RateAllowance, RateLimited>>,
    {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
            Err(_) => {
                tracing::error!("rate limiter ran outside a tokio runtime; failing open");
                Ok(RateAllowance {
                    limit: 0,
                    remaining: 0,
                    resets_in: WINDOW,
                })
            }
        }
    }
}

/// The window's state after pruning, shared by `admit` and `peek` in SQL:
/// `counted` is how many hits the trailing minute still holds, `oldest` the
/// timestamp `RateLimited::retry_after` / `RateAllowance::resets_in` derive from.
struct WindowRead {
    counted: i64,
    oldest: Option<time::OffsetDateTime>,
}

impl PostgresRateLimiter {
    /// The admit path's window read, under the key's row lock (`FOR UPDATE`):
    /// ensures the row exists, locks it so concurrent admits across instances
    /// serialize, then prunes the hits aged out of the trailing minute and
    /// counts what remains — on the server's `now()`, the one timeline every
    /// instance shares.
    async fn read_for_admit(
        tx: &mut sqlx::PgConnection,
        key: Uuid,
    ) -> Result<WindowRead, sqlx::Error> {
        sqlx::query(
            "INSERT INTO oxsum.rate_windows (key_id, hits) VALUES ($1, '{}') \
             ON CONFLICT (key_id) DO NOTHING",
        )
        .bind(key)
        .execute(&mut *tx)
        .await?;
        sqlx::query("SELECT key_id FROM oxsum.rate_windows WHERE key_id = $1 FOR UPDATE")
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
        let row = sqlx::query(
            "UPDATE oxsum.rate_windows \
             SET hits = (SELECT COALESCE(array_agg(h), '{}') \
                         FROM unnest(hits) AS h \
                         WHERE h > now() - interval '60 seconds'), \
                 updated = now() \
             WHERE key_id = $1 \
             RETURNING cardinality(hits)::bigint AS counted, hits[1] AS oldest",
        )
        .bind(key)
        .fetch_one(&mut *tx)
        .await?;
        use sqlx::Row;
        Ok(WindowRead {
            counted: row.try_get("counted")?,
            oldest: row.try_get("oldest")?,
        })
    }

    /// The peek path's window read: a plain `SELECT`, no lock and no write — a
    /// snapshot that consumes nothing, matching `SlidingWindow::peek`. The count
    /// and oldest are computed over the still-fresh hits only; the rows it reads
    /// are left for the next admit's prune to clean.
    async fn read_for_peek(
        tx: &mut sqlx::PgConnection,
        key: Uuid,
    ) -> Result<WindowRead, sqlx::Error> {
        let row = sqlx::query(
            "SELECT COUNT(h) AS counted, MIN(h) AS oldest \
             FROM oxsum.rate_windows, unnest(hits) AS h \
             WHERE key_id = $1 AND h > now() - interval '60 seconds'",
        )
        .bind(key)
        .fetch_one(&mut *tx)
        .await?;
        use sqlx::Row;
        Ok(WindowRead {
            counted: row.try_get::<i64, _>("counted")?,
            oldest: row.try_get("oldest")?,
        })
    }

    /// Maps a pruned window onto the allowance or refusal the trait answers.
    /// `record` is whether to append `now()` — `admit` passes true, `peek` false.
    async fn judge(
        &self,
        key: Uuid,
        per_minute: u32,
        record: bool,
    ) -> Result<RateAllowance, RateLimited> {
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(error) => return fail_open(error),
        };
        let read = match if record {
            Self::read_for_admit(&mut tx, key).await
        } else {
            Self::read_for_peek(&mut tx, key).await
        } {
            Ok(read) => read,
            Err(error) => return fail_open(error),
        };
        let counted = read.counted;
        if counted >= i64::from(per_minute) {
            let _ = tx.commit().await;
            let retry_after = read
                .oldest
                .map(|oldest| {
                    let age = time::OffsetDateTime::now_utc() - oldest;
                    WINDOW.saturating_sub(Duration::from_secs(
                        age.whole_seconds().clamp(0, 60) as u64
                    ))
                })
                .unwrap_or(WINDOW);
            return Err(RateLimited {
                limit: per_minute,
                retry_after,
            });
        }
        if record
            && let Err(error) = sqlx::query(
                "UPDATE oxsum.rate_windows SET hits = array_append(hits, now()), \
                 updated = now() WHERE key_id = $1",
            )
            .bind(key)
            .execute(&mut *tx)
            .await
        {
            return fail_open(error);
        }
        let _ = tx.commit().await;
        // `remaining` is the free slots after this call: an admit consumed one,
        // a peek none — so the record flag is the difference.
        let consumed = u32::from(record);
        let resets_in = read
            .oldest
            .map(|oldest| {
                let age = time::OffsetDateTime::now_utc() - oldest;
                WINDOW.saturating_sub(Duration::from_secs(age.whole_seconds().clamp(0, 60) as u64))
            })
            .unwrap_or(WINDOW);
        Ok(RateAllowance {
            limit: per_minute,
            remaining: per_minute - counted as u32 - consumed,
            resets_in,
        })
    }
}

/// A storage failure uncaps rather than refuses — the limiter is a protection
/// knob, and `POOL_BUSY` at the wallet layer is the real saturation refusal.
fn fail_open(error: sqlx::Error) -> Result<RateAllowance, RateLimited> {
    tracing::error!(%error, "rate limiter backend failed; admitting");
    Ok(RateAllowance {
        limit: 0,
        remaining: 0,
        resets_in: WINDOW,
    })
}

impl RateLimiter for PostgresRateLimiter {
    fn admit(
        &self,
        key: Uuid,
        per_minute: u32,
        _at: Instant,
    ) -> Result<RateAllowance, RateLimited> {
        if per_minute == 0 {
            return Ok(RateAllowance {
                limit: 0,
                remaining: 0,
                resets_in: WINDOW,
            });
        }
        self.run(self.judge(key, per_minute, true))
    }

    fn peek(&self, key: Uuid, per_minute: u32, _at: Instant) -> Result<RateAllowance, RateLimited> {
        if per_minute == 0 {
            return Ok(RateAllowance {
                limit: 0,
                remaining: 0,
                resets_in: WINDOW,
            });
        }
        self.run(self.judge(key, per_minute, false))
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

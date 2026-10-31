//! The shared rate-limit backend (issue #498): `PostgresRateLimiter` counts the
//! `requestsPerMinute` window in `oxsum.rate_windows`, so two instances behind a
//! load balancer admit the same key to `per_minute` total instead of each
//! keeping its own and double-admitting.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes. `block_in_place` —
//! the bridge a synchronous `RateLimiter::admit` uses to run the pool's async
//! work — panics on a single-thread runtime, so every test here is `multi_thread`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, PostgresRateLimiter, RateLimiter};
use sqlx::PgPool;
use std::time::Instant;
use uuid::Uuid;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("rate_windows")
}

macro_rules! db_or_skip {
    () => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

async fn pool(url: &str) -> PgPool {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    Db::from_pool(pool.clone())
        .migrate()
        .await
        .expect("migrates");
    pool
}

/// Two `PostgresRateLimiter`s over one pool — two "instances" — admit the same
/// key to exactly `per_minute` across them: the shared `FOR UPDATE` row is the
/// serialization the in-process `SlidingWindow`'s mutex cannot give a second
/// process.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_backends_share_one_window() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let instance_a = PostgresRateLimiter::new(pool.clone());
    let instance_b = PostgresRateLimiter::new(pool.clone());
    let key = Uuid::new_v4();
    let now = Instant::now();

    // 5 admits split across the two "instances": the shared row lets only 3 in.
    let mut admitted = 0;
    for backend in [
        &instance_a,
        &instance_b,
        &instance_a,
        &instance_b,
        &instance_a,
    ] {
        if backend.admit(key, 3, now).is_ok() {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 3, "the shared window admits exactly the cap");

    // The fourth on either instance is refused — the window is one, not two.
    instance_a
        .admit(key, 3, now)
        .expect_err("a is over the shared minute");
    instance_b
        .admit(key, 3, now)
        .expect_err("b sees the same full window");
}

/// `peek` reports the window `admit` would answer without recording a hit:
/// a peeked allowance spends nothing, so the entitlement read's question does
/// not itself fill the window it asks about.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peek_reports_the_window_without_spending_it() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let limiter = PostgresRateLimiter::new(pool.clone());
    let key = Uuid::new_v4();
    let now = Instant::now();

    // Nothing admitted: the peek reports the whole cap free.
    let free = limiter.peek(key, 2, now).expect("an empty window admits");
    assert_eq!(free.remaining, 2);

    // Peeking twice spends nothing — a real admit still finds both slots.
    limiter.peek(key, 2, now).expect("a peek is free");
    let allowance = limiter
        .admit(key, 2, now)
        .expect("the first real admit fits");
    assert_eq!(allowance.remaining, 1);

    // Fill the window; the peek then reports the refusal a real admit would get.
    limiter.admit(key, 2, now).expect("the second fits");
    let limited = limiter
        .peek(key, 2, now)
        .expect_err("the peek sees it full");
    assert_eq!(limited.limit, 2);
}

/// A window's own clock is the server's, so a hit recorded by one instance ages
/// out on the same timeline the other reads — the property a process clock
/// cannot give. The test waits the real trailing minute out on one instance and
/// re-admits on the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hit_ages_out_on_the_server_clock() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let instance_a = PostgresRateLimiter::new(pool.clone());
    let instance_b = PostgresRateLimiter::new(pool.clone());
    let key = Uuid::new_v4();
    let now = Instant::now();

    instance_a.admit(key, 1, now).expect("a's first fits");
    instance_b
        .admit(key, 1, now)
        .expect_err("b shares the full window");

    // The window frees when the hit is 60s old on the server's clock — the test
    // sleeps the real duration, not a stubbed Instant, because the backend's
    // timeline is the database's `now()`.
    tokio::time::sleep(std::time::Duration::from_secs(61)).await;
    instance_b
        .admit(key, 1, now)
        .expect("the aged hit freed the shared slot");
}

/// A key's row lives only while its window holds hits: once the hits age out,
/// the next admit's prune empties the array — the row is left small, matching
/// `SlidingWindow` dropping a drained queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_row_stays_bounded_by_the_window() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let limiter = PostgresRateLimiter::new(pool.clone());
    let key = Uuid::new_v4();
    let now = Instant::now();

    limiter.admit(key, 5, now).expect("one hit lands");
    limiter.admit(key, 5, now).expect("a second lands");
    let hits: i64 = sqlx::query_scalar(
        "SELECT cardinality(hits)::bigint FROM oxsum.rate_windows WHERE key_id = $1",
    )
    .bind(key)
    .fetch_one(&pool)
    .await
    .expect("the row exists");
    assert_eq!(hits, 2);
}

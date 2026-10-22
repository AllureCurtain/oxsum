//! The demo catalog seed (issue #330), in its own suite database.
//!
//! The test needs an **empty** catalog to exercise `seed_demo_with_channel`'s
//! creation path. The `seed` suite's other tests price channels in the same
//! database, and `channel_prices` is append-only — a trigger refuses a DELETE —
//! so the catalog cannot be cleared in place. One scratch database per test
//! binary is `oxsum-testkit`'s isolation contract; this test is why it exists.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the test skips
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, SecretKey};
use sqlx::PgPool;

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("seed_catalog")
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

async fn db(url: &str) -> Db {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    db
}

/// A database with no catalog gets the demo channel from the sealing key, and
/// then the turns and the statement the seed promises (issue #330).
#[tokio::test]
async fn a_secret_seeds_a_catalog_and_history() {
    let url = db_or_skip!();
    let db = db(&url).await;

    let key = SecretKey::from_bytes([7; 32]);
    let report = db.seed_demo_with_channel(&key).await.unwrap();
    assert!(report.created);
    assert!(
        report.turns > 0,
        "the demo turns settle under the demo channel"
    );
    assert!(report.statement.is_some(), "last month closes");

    let catalog = db.catalog().await.unwrap();
    let models: Vec<&str> = catalog.iter().map(|m| m.model.as_str()).collect();
    assert!(models.contains(&"demo-chat"), "{models:?}");
    assert!(models.contains(&"demo-reasoner"), "{models:?}");

    // The rerun stays a no-op even though the channel now exists.
    let second = db.seed_demo_with_channel(&key).await.unwrap();
    assert!(!second.created);
    assert_eq!(second.turns, 0);
}

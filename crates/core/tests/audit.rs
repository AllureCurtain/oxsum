//! The admin audit log (issue #160): the storage contract the admin surface's
//! `record` calls and `GET /admin/audit` stand on. The tests need DATABASE_URL
//! and skip without it, like the other core suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, audit_action};
use uuid::Uuid;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
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

/// A migrated database; the audit table is shared state, so every test writes
/// under a unique key/tag and pages by action, never by absolute position.
async fn db(url: &str) -> Db {
    let pool: sqlx::PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    db
}

/// A unique action-shaped tag, so one test's rows never crowd another's when
/// the filter asks by it — `action` is a free text column, and a test-scoped
/// name is how a shared database stays parallel-safe.
fn scoped(tag: &str) -> String {
    format!("test.{tag}.{}", &Uuid::new_v4().simple().to_string()[..8])
}

#[tokio::test]
async fn a_recorded_row_pages_back_newest_first() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let action = scoped("order");

    for i in 0..3 {
        db.record_audit(
            &action,
            Some(&format!("target-{i}")),
            serde_json::json!({ "i": i }),
            None,
        )
        .await
        .expect("records")
        .expect("a row lands");
    }

    let page = db.audit_page(None, Some(&action), 10).await.expect("pages");
    assert_eq!(page.rows.len(), 3);
    assert_eq!(page.rows[0].detail["i"], 2, "newest first");
    assert_eq!(page.rows[2].detail["i"], 0);
    assert!(page.next_cursor.is_none());
    for row in &page.rows {
        assert_eq!(row.action, action);
        assert_eq!(row.actor, "operator");
    }
}

#[tokio::test]
async fn a_cursor_walks_the_log_page_by_page() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let action = scoped("walk");

    for i in 0..5 {
        db.record_audit(&action, None, serde_json::json!({ "i": i }), None)
            .await
            .expect("records");
    }

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = db
            .audit_page(cursor, Some(&action), 2)
            .await
            .expect("pages");
        for row in &page.rows {
            seen.push(row.detail["i"].as_i64().unwrap());
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
        assert!(seen.len() <= 5, "the walk must terminate");
    }
    assert_eq!(seen, [4, 3, 2, 1, 0], "every row, newest first, once");
}

#[tokio::test]
async fn a_keyed_replay_records_once() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let action = audit_action::DISCOUNT_CREATE;
    let key = format!("audit-test-{}", Uuid::new_v4());

    let first = db
        .record_audit(
            action,
            None,
            serde_json::json!({ "percent": 10 }),
            Some(&key),
        )
        .await
        .expect("records");
    assert!(first.is_some());
    let replay = db
        .record_audit(
            action,
            None,
            serde_json::json!({ "percent": 10 }),
            Some(&key),
        )
        .await
        .expect("replays without error");
    assert!(replay.is_none(), "the replay is the same event");

    let page = db.audit_page(None, Some(action), 100).await.expect("pages");
    let rows: Vec<_> = page
        .rows
        .iter()
        .filter(|row| row.idempotency_key.as_deref() == Some(key.as_str()))
        .collect();
    assert_eq!(rows.len(), 1, "one logical write, one row");
}

#[tokio::test]
async fn a_key_is_scoped_to_its_action() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let key = format!("audit-test-{}", Uuid::new_v4());

    let a = db
        .record_audit(
            audit_action::ORGANIZATION_ADJUST,
            Some("org"),
            serde_json::json!({}),
            Some(&key),
        )
        .await
        .expect("records");
    let b = db
        .record_audit(
            audit_action::ORGANIZATION_UPDATE,
            Some("org"),
            serde_json::json!({}),
            Some(&key),
        )
        .await
        .expect("a different action is a different event");
    assert!(a.is_some() && b.is_some());
}

#[tokio::test]
async fn an_unkeyed_call_always_records() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let action = scoped("unkeyed");

    db.record_audit(&action, None, serde_json::json!({}), None)
        .await
        .expect("records");
    let second = db
        .record_audit(&action, None, serde_json::json!({}), None)
        .await
        .expect("an unkeyed write is always its own event");
    assert!(second.is_some());

    let page = db.audit_page(None, Some(&action), 10).await.expect("pages");
    assert_eq!(page.rows.len(), 2);
}

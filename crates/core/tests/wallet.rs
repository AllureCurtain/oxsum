//! Wallet integration tests, running against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use oxsum_core::{Wallet, WalletError, verify_bundle};
use time::macros::date;

const D: time::Date = date!(2026 - 10 - 01);
const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

/// Each test uses a tenant name with a random suffix, so tests never interfere and reruns never collide.
fn fresh(name: &str) -> String {
    format!("{name}_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
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

#[tokio::test]
async fn tenants_are_isolated() {
    let url = db_or_skip!();
    let a = Wallet::open(&url, &fresh("a")).await.unwrap();
    let b = Wallet::open(&url, &fresh("b")).await.unwrap();

    a.top_up("a-1", 10 * ONE, D).await.unwrap();
    a.top_up("a-2", 5 * ONE, D).await.unwrap();
    b.top_up("b-1", 3 * ONE, D).await.unwrap();

    assert_eq!(a.available().await.unwrap(), 15 * ONE);
    assert_eq!(b.available().await.unwrap(), 3 * ONE);
    // Each tenant's log counts from zero, so B's tree head reveals nothing about how many entries A has.
    assert_eq!(a.log_size().await.unwrap(), 2);
    assert_eq!(b.log_size().await.unwrap(), 1);
    // The same idempotency key in different tenants never collides.
    assert!(a.top_up("same-key", ONE, D).await.unwrap().is_new);
    assert!(b.top_up("same-key", ONE, D).await.unwrap().is_new);
}

#[tokio::test]
async fn concurrent_holds_cannot_overdraw() {
    let url = db_or_skip!();
    let w = Arc::new(Wallet::open(&url, &fresh("race")).await.unwrap());
    w.top_up("fund", 10 * ONE, D).await.unwrap();

    let tasks: Vec<_> = (0..20)
        .map(|i| {
            let w = w.clone();
            tokio::spawn(async move { w.hold(&format!("hold-{i}"), 3 * ONE, D).await })
        })
        .collect();
    let mut accepted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => accepted += 1,
            Err(WalletError::InsufficientFunds) => {}
            Err(e) => panic!("unexpected: {e:?}"),
        }
    }
    // Balance 10, each hold 3: at most 3 can succeed.
    assert_eq!(accepted, 3);
    assert_eq!(w.available().await.unwrap(), ONE);
}

#[tokio::test]
async fn hold_then_partial_settle_refunds_the_rest() {
    let url = db_or_skip!();
    let w = Wallet::open(&url, &fresh("stream")).await.unwrap();
    w.top_up("fund", 10 * ONE, D).await.unwrap();

    w.hold("req-1:hold", 4 * ONE, D).await.unwrap();
    assert_eq!(w.available().await.unwrap(), 6 * ONE);

    w.settle("req-1:settle", 4 * ONE, 1_234_567, D)
        .await
        .unwrap();
    assert_eq!(w.available().await.unwrap(), 10 * ONE - 1_234_567);

    // Retrying the same settlement is idempotent; nothing is charged twice.
    let again = w
        .settle("req-1:settle", 4 * ONE, 1_234_567, D)
        .await
        .unwrap();
    assert!(!again.is_new);
    assert_eq!(w.available().await.unwrap(), 10 * ONE - 1_234_567);
}

#[tokio::test]
async fn settle_rejects_actual_above_hold() {
    let url = db_or_skip!();
    let w = Wallet::open(&url, &fresh("bounds")).await.unwrap();
    let err = w.settle("s", ONE, 2 * ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)));
}

#[tokio::test]
async fn bill_proof_verifies_and_catches_tampering() {
    let url = db_or_skip!();
    let w = Wallet::open(&url, &fresh("proof")).await.unwrap();
    w.top_up("fund", 10 * ONE, D).await.unwrap();
    let bill = w.hold("req-9:hold", 2 * ONE, D).await.unwrap();
    for i in 0..5 {
        w.top_up(&format!("noise-{i}"), ONE, D).await.unwrap();
    }

    let bundle = w.receipt_proof(bill.entry_id).await.unwrap().unwrap();
    let json = serde_json::to_string(&bundle).unwrap();
    assert!(verify_bundle(&json, &bill.content_hash).unwrap());

    // Simulate the server later rewriting this bill from 2.000000 to 0.500000.
    let tampered = json.replace("\"2.000000\"", "\"0.500000\"");
    assert_ne!(tampered, json, "fixture must contain the amount");
    assert!(!verify_bundle(&tampered, &bill.content_hash).unwrap());
}

#[tokio::test]
async fn reopening_a_tenant_keeps_its_books() {
    let url = db_or_skip!();
    let name = fresh("restart");
    Wallet::open(&url, &name)
        .await
        .unwrap()
        .top_up("fund", 7 * ONE, D)
        .await
        .unwrap();
    let reopened = Wallet::open(&url, &name).await.unwrap();
    assert_eq!(reopened.available().await.unwrap(), 7 * ONE);
}

/// Several new tenants migrating an empty database for the first time must not hit
/// btree_gist's unique constraint. Already fixed in crates/doubleentry; see docs/decisions.md.
#[tokio::test]
async fn concurrent_first_migrations_succeed() {
    let url = db_or_skip!();
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let url = url.clone();
            tokio::spawn(async move { Wallet::open(&url, &fresh(&format!("mig{i}"))).await })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
}

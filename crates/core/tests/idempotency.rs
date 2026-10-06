//! Request-level idempotency (issue #132): the claim lifecycle under
//! `oxsum.idempotency_records` — fresh, in-flight, mismatch, replay, release and
//! lazy expiry — plus the receipt completion the usage-row write performs. The
//! tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    Claim, Db, NewUser, SettlementKind, UsageRecord, UsageRow, WalletError, fingerprint,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
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

/// A migrated database and one registered organization to claim under.
async fn world(url: &str) -> (Db, Uuid) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let email = format!(
        "idem_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let registration = db
        .register(NewUser {
            email,
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .unwrap();
    (db, registration.organization.id)
}

/// The lifecycle: claim, collide, complete, replay, mismatch, release, reclaim.
#[tokio::test]
async fn a_claim_lives_its_lifecycle() {
    let url = db_or_skip!();
    let (db, org) = world(&url).await;
    let fp_a = fingerprint(b"{}");
    let fp_b = fingerprint(b"{\"other\": true}");

    let claim = db.claim_request(org, "k1", &fp_a, "req-1").await.unwrap();
    assert!(matches!(claim, Claim::Fresh { request_id } if request_id == "req-1"));

    // A retry while the turn runs is in flight, and under a different body a mismatch.
    let claim = db.claim_request(org, "k1", &fp_a, "req-2").await.unwrap();
    assert!(matches!(claim, Claim::InFlight));
    let claim = db.claim_request(org, "k1", &fp_b, "req-2").await.unwrap();
    assert!(matches!(claim, Claim::Mismatch));

    // Completed, the same fingerprint replays the stored answer under the
    // original request id.
    db.complete_request(org, "k1", 200, &json!({"object": "chat.completion"}))
        .await
        .unwrap();
    let claim = db.claim_request(org, "k1", &fp_a, "req-3").await.unwrap();
    let Claim::Replay {
        request_id,
        status,
        body,
    } = claim
    else {
        panic!("a completed claim replays")
    };
    assert_eq!(request_id, "req-1");
    assert_eq!(status, 200);
    assert_eq!(body["object"], "chat.completion");

    // A released claim is free again: a refusal that never reached the wallet
    // holds nothing.
    db.release_request(org, "k1").await.unwrap();
    let claim = db.claim_request(org, "k1", &fp_b, "req-4").await.unwrap();
    assert!(matches!(claim, Claim::Fresh { request_id } if request_id == "req-4"));
}

/// An expired record is released lazily on the next claim under the key.
#[tokio::test]
async fn an_expired_claim_is_reclaimable() {
    let url = db_or_skip!();
    let (db, org) = world(&url).await;
    db.claim_request(org, "old", &fingerprint(b"{}"), "req-1")
        .await
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.idempotency_records SET expires_at = now() - interval '1 second' \
         WHERE organization_id = $1 AND idempotency_key = 'old'",
    )
    .bind(org)
    .execute(db.pool())
    .await
    .unwrap();
    let claim = db
        .claim_request(org, "old", &fingerprint(b"{}"), "req-2")
        .await
        .unwrap();
    assert!(matches!(claim, Claim::Fresh { request_id } if request_id == "req-2"));
}

/// The key's bounds are validated in core, not the route layer.
#[tokio::test]
async fn a_key_outside_its_bounds_is_refused() {
    let url = db_or_skip!();
    let (db, org) = world(&url).await;
    let fp = fingerprint(b"{}");
    for key in ["", &"k".repeat(256)] {
        let error = db.claim_request(org, key, &fp, "req").await.unwrap_err();
        assert!(matches!(error, WalletError::InvalidInput(_)), "{error:?}");
    }
}

/// The usage-row write is what turns a streamed turn's claim into its receipt.
#[tokio::test]
async fn a_settled_usage_row_completes_the_claim() {
    let url = db_or_skip!();
    let (db, org) = world(&url).await;
    let request_id = Uuid::new_v4().to_string();
    let claim = db
        .claim_request(org, "stream", &fingerprint(b"{}"), &request_id)
        .await
        .unwrap();
    assert!(matches!(claim, Claim::Fresh { .. }));

    db.record_usage(&UsageRow {
        request_id: request_id.clone(),
        tenant_id: "no-such-ledger".to_owned(),
        key_id: None,
        model: "m".to_owned(),
        channel: "c".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: Uuid::new_v4(),
        usage: UsageRecord::tokens(10, 2).unwrap(),
        charged_minor: 12,
        freeze_minor: 100,
        upstream_cost_minor: None,
    })
    .await
    .unwrap();

    let claim = db
        .claim_request(org, "stream", &fingerprint(b"{}"), "req-later")
        .await
        .unwrap();
    let Claim::Replay {
        request_id: id,
        status,
        body,
    } = claim
    else {
        panic!("the settled turn answers its receipt")
    };
    assert_eq!(id, request_id);
    assert_eq!(status, 200);
    assert_eq!(body["object"], "oxsum.receipt");
    assert_eq!(body["chargedMinor"], 12);
}

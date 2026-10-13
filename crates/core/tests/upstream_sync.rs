//! Upstream price drift (issue #222): `diff_upstream` catches a changed,
//! added, or dropped rate; `sync_upstream_prices` opens, refreshes, and
//! resolves drift rows; and an opened drift broadcasts `anomaly.detected` to
//! every enabled subscriber. The DB tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    ANOMALY_DETECTED, Db, ModelPair, NewUser, SecretKey, UpstreamPrices, diff_upstream,
};
use sqlx::PgPool;
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

fn key() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

async fn world(url: &str) -> (Db, Uuid) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let registration = db
        .register(NewUser {
            email: format!(
                "drift_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .unwrap();
    (db, registration.organization.id)
}

fn rates(input: i64, output: i64) -> UpstreamPrices {
    UpstreamPrices {
        input_price_per_million: Some(input),
        output_price_per_million: Some(output),
        ..UpstreamPrices::default()
    }
}

/// An identical pair diffs to nothing.
#[test]
fn agreeing_rates_do_not_drift() {
    assert!(diff_upstream(&rates(1, 2), &rates(1, 2)).is_empty());
}

/// Every field reports its own drift row, named by its wire spelling.
#[test]
fn each_divergent_field_reports() {
    let stored = UpstreamPrices {
        input_price_per_million: Some(1),
        output_price_per_million: Some(2),
        cache_read_price_per_million: Some(3),
        cache_write_5m_price_per_million: Some(4),
        cache_write_1h_price_per_million: Some(5),
        reasoning_price_per_million: Some(6),
        cost_per_request: Some(7),
    };
    let live = UpstreamPrices {
        input_price_per_million: Some(10),
        output_price_per_million: Some(2),
        cache_read_price_per_million: Some(30),
        cache_write_5m_price_per_million: Some(4),
        cache_write_1h_price_per_million: Some(50),
        reasoning_price_per_million: Some(6),
        cost_per_request: Some(70),
    };
    let drift = diff_upstream(&stored, &live);
    let fields: Vec<_> = drift.iter().map(|d| d.field).collect();
    assert_eq!(
        fields,
        [
            "inputPricePerMillion",
            "cacheReadPricePerMillion",
            "cacheWrite1hPricePerMillion",
            "costPerRequest"
        ]
    );
    let input = drift
        .iter()
        .find(|d| d.field == "inputPricePerMillion")
        .unwrap();
    assert_eq!(input.stored_minor, Some(1));
    assert_eq!(input.live_minor, Some(10));
}

/// A `None` either side is not equal to a `Some` the other: a rate the catalog
/// added, and one it dropped, both drift.
#[test]
fn a_rate_only_one_side_carries_drifts() {
    let added = diff_upstream(&UpstreamPrices::default(), &rates(1, 2));
    assert!(added.iter().any(|d| d.field == "inputPricePerMillion"
        && d.stored_minor.is_none()
        && d.live_minor == Some(1)));

    let dropped = diff_upstream(&rates(1, 2), &UpstreamPrices::default());
    assert!(dropped.iter().any(|d| d.field == "outputPricePerMillion"
        && d.stored_minor == Some(2)
        && d.live_minor.is_none()));
}

/// A live rate that disagrees opens a drift row; a re-probe that still
/// disagrees refreshes it rather than opening a second; once the rates agree
/// the row resolves.
#[tokio::test]
async fn drift_opens_upserts_and_resolves() {
    let url = db_or_skip!();
    let (db, _org) = world(&url).await;
    let channel = format!("chan-{}", &Uuid::new_v4().simple().to_string()[..8]);

    let stored = rates(1, 2);
    let live = rates(9, 2);
    let pairs = [ModelPair {
        model: "m",
        stored: Some(&stored),
        live: Some(&live),
    }];
    db.sync_upstream_prices(&channel, &pairs).await.unwrap();

    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.upstream_price_drift \
         WHERE channel = $1 AND resolved_at IS NULL",
    )
    .bind(&channel)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(open, 1, "one field drifts: inputPricePerMillion");

    // Re-probing the same disagreement bumps last_seen_at but stays one row.
    db.sync_upstream_prices(&channel, &pairs).await.unwrap();
    let open_again: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.upstream_price_drift \
         WHERE channel = $1 AND resolved_at IS NULL",
    )
    .bind(&channel)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(open_again, 1, "the upsert never duplicates an open row");

    // The live catalog re-converges onto the stored rates: the row resolves.
    let agreed = rates(1, 2);
    let pairs = [ModelPair {
        model: "m",
        stored: Some(&stored),
        live: Some(&agreed),
    }];
    db.sync_upstream_prices(&channel, &pairs).await.unwrap();
    let resolved: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.upstream_price_drift \
         WHERE channel = $1 AND resolved_at IS NULL",
    )
    .bind(&channel)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(resolved, 0, "re-converged rates resolve the drift");
}

/// A model the live catalog stops carrying resolves its open drift.
#[tokio::test]
async fn a_dropped_model_resolves_its_drift() {
    let url = db_or_skip!();
    let (db, _org) = world(&url).await;
    let channel = format!("chan-{}", &Uuid::new_v4().simple().to_string()[..8]);

    let stored = rates(1, 2);
    let live = rates(9, 2);
    db.sync_upstream_prices(
        &channel,
        &[ModelPair {
            model: "m",
            stored: Some(&stored),
            live: Some(&live),
        }],
    )
    .await
    .unwrap();

    // The live catalog no longer prices the model: live is None.
    db.sync_upstream_prices(
        &channel,
        &[ModelPair {
            model: "m",
            stored: Some(&stored),
            live: None,
        }],
    )
    .await
    .unwrap();
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.upstream_price_drift \
         WHERE channel = $1 AND resolved_at IS NULL",
    )
    .bind(&channel)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(open, 0, "a model absent live resolves its open drift");
}

/// Opening a drift broadcasts `anomaly.detected` to every enabled subscriber —
/// platform scope, stamped with each endpoint's own organization.
#[tokio::test]
async fn an_opened_drift_broadcasts_to_subscribers() {
    let url = db_or_skip!();
    let (db, org) = world(&url).await;
    db.create_webhook(
        org,
        "https://subscriber.example.com/hook",
        &[ANOMALY_DETECTED.to_owned()],
        &key(),
    )
    .await
    .unwrap();

    let channel = format!("chan-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let stored = rates(1, 2);
    let live = rates(9, 2);
    db.sync_upstream_prices(
        &channel,
        &[ModelPair {
            model: "m",
            stored: Some(&stored),
            live: Some(&live),
        }],
    )
    .await
    .unwrap();

    // The broadcast fans out to every enabled subscriber across all orgs, so a
    // shared database may already hold `anomaly.detected` deliveries — assert
    // the row stamped for *this* test's organization, not whatever landed last.
    let (event_type, organization_id): (String, Uuid) = sqlx::query_as(
        "SELECT event_type, organization_id FROM oxsum.webhook_deliveries \
         WHERE event_type = $1 AND organization_id = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(ANOMALY_DETECTED)
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(event_type, ANOMALY_DETECTED);
    assert_eq!(
        organization_id, org,
        "the delivery is stamped for its endpoint's org"
    );
}

//! Outbound webhooks (issue #144): endpoint lifecycle, the queue `record_usage`
//! fills transactionally, and the attempt bookkeeping the worker writes back.
//! The tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    Db, DeliveryOutcome, NewUser, REQUEST_SETTLED, SecretKey, SettlementKind, UsageRecord,
    UsageRow, WalletError, retry_delay_secs, signature,
};
use serde_json::json;
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

/// A migrated database and a registered organization, returned as both its row id
/// and its tenant id (the 32-hex form `usage_records` carries).
async fn world(url: &str) -> (Db, Uuid, String) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let email = format!(
        "hook_{}@example.com",
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
    let org = registration.organization.id;
    (db, org, org.simple().to_string())
}

/// A minimal settled-turn row, as the gateway writes it.
fn usage_row(request_id: &str, tenant_id: &str) -> UsageRow {
    UsageRow {
        request_id: request_id.to_owned(),
        tenant_id: tenant_id.to_owned(),
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
    }
}

/// Create, list, delete — and the secret is never listed back out.
#[tokio::test]
async fn an_endpoint_lives_its_lifecycle() {
    let url = db_or_skip!();
    let (db, org, _) = world(&url).await;

    let created = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["request.settled".to_owned()],
            &key(),
        )
        .await
        .unwrap();
    assert!(created.secret.starts_with("whsec-"));
    assert!(created.secret.ends_with(&created.endpoint.secret_last4));
    assert_eq!(created.endpoint.events, ["request.settled".to_owned()]);
    assert!(created.endpoint.enabled);

    let list = db.list_webhooks(org).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, created.endpoint.id);

    // A deleted endpoint is answered once and then gone — deliveries included.
    let deleted = db.delete_webhook(org, created.endpoint.id).await.unwrap();
    assert!(deleted.is_some());
    assert!(db.list_webhooks(org).await.unwrap().is_empty());
    assert!(
        db.delete_webhook(org, created.endpoint.id)
            .await
            .unwrap()
            .is_none(),
        "deleting twice is a not-found, not an error"
    );
}

/// The endpoint write validates what a receiver must be.
#[tokio::test]
async fn malformed_endpoints_are_refused() {
    let url = db_or_skip!();
    let (db, org, _) = world(&url).await;
    let events = ["request.settled".to_owned()];
    for bad in [
        "not a url",
        "ftp://receiver.example/hook",
        "http://receiver.example/hook", // plaintext off localhost
        &format!("https://{}", "x".repeat(600)),
    ] {
        let error = db
            .create_webhook(org, bad, &events, &key())
            .await
            .unwrap_err();
        assert!(
            matches!(error, WalletError::InvalidInput(_)),
            "{bad}: {error:?}"
        );
    }
    for events in [vec![], vec!["turn.started".to_owned()]] {
        let error = db
            .create_webhook(org, "https://receiver.example/hook", &events, &key())
            .await
            .unwrap_err();
        assert!(matches!(error, WalletError::InvalidInput(_)), "{error:?}");
    }
    // `http` to localhost is the one plaintext exception — the developer's machine.
    db.create_webhook(
        org,
        "http://127.0.0.1:9/hook",
        &["request.settled".to_owned()],
        &key(),
    )
    .await
    .unwrap();
}

/// A settled turn enqueues one delivery per enabled subscribed endpoint, in the
/// usage row's transaction — and a replay of the same turn enqueues nothing.
#[tokio::test]
async fn a_settled_turn_enqueues_a_signed_delivery() {
    let url = db_or_skip!();
    let (db, org, tenant) = world(&url).await;
    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["request.settled".to_owned()],
            &key(),
        )
        .await
        .unwrap();

    // An organization with no endpoints settles and enqueues nothing.
    let other = db
        .register(NewUser {
            email: format!(
                "hook_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "x".repeat(12),
            organization_name: None,
        })
        .await
        .unwrap()
        .organization;
    db.record_usage(&usage_row(
        &format!("req-{}", Uuid::new_v4()),
        &other.id.simple().to_string(),
    ))
    .await
    .unwrap();

    let request_id = Uuid::new_v4().to_string();
    db.record_usage(&usage_row(&request_id, &tenant))
        .await
        .unwrap();
    // A replay of the same request id inserts no usage row — and no second delivery.
    db.record_usage(&usage_row(&request_id, &tenant))
        .await
        .unwrap();

    let deliveries = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .expect("the endpoint is the organization's");
    assert_eq!(deliveries.len(), 1);
    let delivery = &deliveries[0];
    assert_eq!(delivery.event_type, REQUEST_SETTLED);
    assert_eq!(delivery.status, "pending");
    assert_eq!(delivery.attempts, 0);

    // The stored payload is the signed envelope: id, type, created, data.
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM oxsum.webhook_deliveries WHERE delivery_id = $1")
            .bind(delivery.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored["type"], "request.settled");
    assert_eq!(stored["id"].as_str().unwrap(), delivery.id.to_string());
    assert_eq!(stored["data"]["requestId"], request_id);
    assert_eq!(stored["data"]["chargedMinor"], 12);

    // A usage row for a tenant with no organization row enqueues nothing
    // anywhere — synthetic tenants never webhook.
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.webhook_deliveries")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.record_usage(&usage_row(
        &format!("req-{}", Uuid::new_v4()),
        "no-such-ledger",
    ))
    .await
    .unwrap();
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.webhook_deliveries")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
}

/// The attempt bookkeeping: a failed answer reschedules on the backoff, and the
/// budget's last attempt marks the delivery failed.
#[tokio::test]
async fn attempts_backoff_and_exhaust() {
    let url = db_or_skip!();
    let (db, org, tenant) = world(&url).await;
    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["request.settled".to_owned()],
            &key(),
        )
        .await
        .unwrap();
    db.record_usage(&usage_row(&format!("req-{}", Uuid::new_v4()), &tenant))
        .await
        .unwrap();

    let due = db.claim_due_deliveries(Some(org)).await.unwrap();
    let item = due
        .iter()
        .find(|d| d.delivery.endpoint_id == endpoint.endpoint.id)
        .expect("the delivery is due");
    assert_eq!(item.url, "https://receiver.example/hook");
    let body: serde_json::Value = serde_json::from_str(&item.body).unwrap();
    assert_eq!(body["type"], "request.settled");

    // A failed first attempt stays pending, its next try on the backoff.
    db.record_delivery(
        &item.delivery,
        &DeliveryOutcome::AttemptFailed {
            status: Some(500),
            error: "the receiver answered 500".to_owned(),
            attempt: 1,
        },
    )
    .await
    .unwrap();
    let delivery = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .unwrap()[0]
        .clone();
    assert_eq!(delivery.status, "pending");
    assert_eq!(delivery.attempts, 1);
    assert_eq!(delivery.response_status, Some(500));
    let due_later: bool = sqlx::query_scalar(
        "SELECT next_attempt_at > now() + interval '5 seconds' \
         FROM oxsum.webhook_deliveries WHERE delivery_id = $1",
    )
    .bind(delivery.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        due_later,
        "a retried delivery is rescheduled on the backoff"
    );

    // The tenth attempt is the budget's end: re-claim it first, as the worker
    // would once the backoff's `next_attempt_at` arrives.
    sqlx::query(
        "UPDATE oxsum.webhook_deliveries SET next_attempt_at = now() \
         WHERE delivery_id = $1",
    )
    .bind(delivery.id)
    .execute(db.pool())
    .await
    .unwrap();
    let due = db.claim_due_deliveries(Some(org)).await.unwrap();
    let item = due
        .iter()
        .find(|d| d.delivery.id == delivery.id)
        .expect("the rescheduled delivery is claimed again");
    db.record_delivery(
        &item.delivery,
        &DeliveryOutcome::AttemptFailed {
            status: None,
            error: "unreachable".to_owned(),
            attempt: oxsum_core::MAX_DELIVERY_ATTEMPTS,
        },
    )
    .await
    .unwrap();
    let delivery = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .unwrap()[0]
        .clone();
    assert_eq!(delivery.status, "failed");
    assert_eq!(delivery.attempts, 2, "each recorded attempt counts once");

    // A failed delivery is not due, and a delivered one is finished.
    let due = db.claim_due_deliveries(Some(org)).await.unwrap();
    assert!(
        !due.iter()
            .any(|d| d.delivery.endpoint_id == endpoint.endpoint.id)
    );
}

/// The signature format is the Stripe convention — a receiver can verify it
/// with an off-the-shelf HMAC.
#[test]
fn the_signature_is_verifiable() {
    let signed = signature("whsec-test", 1_700_000_000, b"{}");
    let (t, v1) = signed.split_once(",v1=").unwrap();
    assert_eq!(t, "t=1700000000");
    assert_eq!(v1.len(), 64);
    // Deterministic over the same inputs, different under another body.
    assert_eq!(signed, signature("whsec-test", 1_700_000_000, b"{}"));
    assert_ne!(v1, {
        let other = signature("whsec-test", 1_700_000_000, b"{\"x\":1}");
        other.split_once(",v1=").unwrap().1.to_owned()
    });
    // The backoff ladder: fast early retries, then hourly.
    assert_eq!(retry_delay_secs(1), 10);
    assert_eq!(retry_delay_secs(3), 300);
    assert_eq!(retry_delay_secs(9), 3600);
}

/// An endpoint deleted between enqueue and delivery never sends: the join in
/// `claim_due_deliveries` is over live endpoints, and the FK cascade removes the row.
#[tokio::test]
async fn a_deleted_endpoint_stops_sending() {
    let url = db_or_skip!();
    let (db, org, tenant) = world(&url).await;
    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["request.settled".to_owned()],
            &key(),
        )
        .await
        .unwrap();
    db.record_usage(&usage_row(&format!("req-{}", Uuid::new_v4()), &tenant))
        .await
        .unwrap();
    db.delete_webhook(org, endpoint.endpoint.id).await.unwrap();
    let due = db.claim_due_deliveries(Some(org)).await.unwrap();
    assert!(
        !due.iter()
            .any(|d| d.delivery.endpoint_id == endpoint.endpoint.id)
    );
}

/// Envelope completeness: `json!` usage keeps this honest when the payload
/// shape drifts.
#[tokio::test]
async fn the_payload_names_the_bill() {
    let url = db_or_skip!();
    let (db, org, tenant) = world(&url).await;
    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["request.settled".to_owned()],
            &key(),
        )
        .await
        .unwrap();
    let request_id = format!("req-{}", Uuid::new_v4());
    db.record_usage(&usage_row(&request_id, &tenant))
        .await
        .unwrap();
    let stored: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM oxsum.webhook_deliveries d \
         JOIN oxsum.webhook_endpoints e ON e.endpoint_id = d.endpoint_id \
         WHERE e.endpoint_id = $1",
    )
    .bind(endpoint.endpoint.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        stored["data"],
        json!({
            "requestId": request_id,
            "model": "m",
            "channel": "c",
            "kind": "usage",
            "chargedMinor": 12,
            "freezeMinor": 100,
            "settlementEntryId": stored["data"]["settlementEntryId"],
            "inputTokens": 10,
            "outputTokens": 2,
        })
    );
}

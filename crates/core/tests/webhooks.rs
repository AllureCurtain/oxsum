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
    oxsum_testkit::suite_db_url("webhooks")
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
    let (db, org, tenant, _) = world_with_secret(url).await;
    (db, org, tenant)
}

/// `world` plus the registration key's secret, for the tests that need an
/// `ActingKey` to put real holds and settlements on the ledger.
async fn world_with_secret(url: &str) -> (Db, Uuid, String, String) {
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
    (
        db,
        org,
        org.simple().to_string(),
        registration.api_key.secret,
    )
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
        upstream_attempts: 1,
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
    // anywhere — synthetic tenants never webhook. The count is scoped to this
    // test's organization: the whole suite shares one database, so a parallel
    // test's delivery would trip a global count (issue #195).
    let deliveries = "SELECT count(*) FROM oxsum.webhook_deliveries \
                      WHERE organization_id = $1";
    let before: i64 = sqlx::query_scalar(deliveries)
        .bind(org)
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.record_usage(&usage_row(
        &format!("req-{}", Uuid::new_v4()),
        "no-such-ledger",
    ))
    .await
    .unwrap();
    let after: i64 = sqlx::query_scalar(deliveries)
        .bind(org)
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

/// `balance.low` fires once when the balance crosses under the endpoint's
/// threshold, latches, and re-arms only after a deposit lifts it back over
/// (issue #200).
#[tokio::test]
async fn balance_low_fires_once_per_crossing() {
    let url = db_or_skip!();
    let (db, org, tenant) = world(&url).await;
    let endpoint = db
        .create_webhook_with_threshold(
            org,
            "https://receiver.example/hook",
            &["balance.low".to_owned()],
            Some(1_000),
            &key(),
        )
        .await
        .unwrap();

    // A settle that leaves the balance over the line fires nothing.
    db.record_usage_balanced(
        &usage_row(&format!("req-{}", Uuid::new_v4()), &tenant),
        Some(1_500),
    )
    .await
    .unwrap();
    // Under the line: the crossing fires once.
    db.record_usage_balanced(
        &usage_row(&format!("req-{}", Uuid::new_v4()), &tenant),
        Some(800),
    )
    .await
    .unwrap();
    // Still under on the next settle: the latch holds, no second event.
    db.record_usage_balanced(
        &usage_row(&format!("req-{}", Uuid::new_v4()), &tenant),
        Some(600),
    )
    .await
    .unwrap();

    let deliveries = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .expect("the endpoint is the organization's");
    let lows: Vec<_> = deliveries
        .iter()
        .filter(|d| d.event_type == oxsum_core::BALANCE_LOW)
        .collect();
    assert_eq!(
        lows.len(),
        1,
        "one balance.low per crossing: {deliveries:?}"
    );
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM oxsum.webhook_deliveries WHERE delivery_id = $1")
            .bind(lows[0].id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored["data"]["availableMinor"], 800);
    assert_eq!(stored["data"]["thresholdMinor"], 1_000);

    // A deposit lifting the balance back over re-arms; the next dip fires again.
    let mut tx = db.pool().begin().await.unwrap();
    oxsum_core::Db::clear_balance_low(&mut tx, org, 2_000)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    db.record_usage_balanced(
        &usage_row(&format!("req-{}", Uuid::new_v4()), &tenant),
        Some(500),
    )
    .await
    .unwrap();
    let deliveries = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        deliveries
            .iter()
            .filter(|d| d.event_type == oxsum_core::BALANCE_LOW)
            .count(),
        2,
        "the re-armed threshold fires on the next crossing"
    );
}

/// `anomaly.detected` enqueues for the kinds the anomalies page reviews, and a
/// clean `usage` row enqueues nothing (issue #200).
#[tokio::test]
async fn an_anomalous_settlement_enqueues_anomaly_detected() {
    let url = db_or_skip!();
    let (db, org, tenant) = world(&url).await;
    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["anomaly.detected".to_owned()],
            &key(),
        )
        .await
        .unwrap();

    // A clean usage kind reports no anomaly.
    db.record_usage(&usage_row(&format!("req-{}", Uuid::new_v4()), &tenant))
        .await
        .unwrap();
    // A capped turn is the anomaly the page reviews.
    let mut capped = usage_row(&format!("req-{}", Uuid::new_v4()), &tenant);
    capped.kind = SettlementKind::Capped;
    db.record_usage(&capped).await.unwrap();

    let deliveries = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .unwrap();
    let anomalies: Vec<_> = deliveries
        .iter()
        .filter(|d| d.event_type == oxsum_core::ANOMALY_DETECTED)
        .collect();
    assert_eq!(
        anomalies.len(),
        1,
        "only the capped turn fires: {deliveries:?}"
    );
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM oxsum.webhook_deliveries WHERE delivery_id = $1")
            .bind(anomalies[0].id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored["data"]["kind"], "capped");
}

/// A manual top-up enqueues `deposit.credited` in the deposit's own transaction;
/// a replayed idempotency key enqueues nothing twice (issue #200).
#[tokio::test]
async fn a_manual_deposit_enqueues_deposit_credited() {
    let url = db_or_skip!();
    let (db, org, _tenant) = world(&url).await;
    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["deposit.credited".to_owned()],
            &key(),
        )
        .await
        .unwrap();

    let entry = Uuid::new_v4();
    db.record_manual_deposit(org, "topup-1", 5_000, entry, Some(5_000))
        .await
        .unwrap();
    // A replay of the same payment ref writes no row and enqueues nothing.
    db.record_manual_deposit(org, "topup-1", 5_000, entry, Some(5_000))
        .await
        .unwrap();

    let deliveries = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .unwrap();
    let credits: Vec<_> = deliveries
        .iter()
        .filter(|d| d.event_type == oxsum_core::DEPOSIT_CREDITED)
        .collect();
    assert_eq!(credits.len(), 1, "one deposit.credited per deposit");
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM oxsum.webhook_deliveries WHERE delivery_id = $1")
            .bind(credits[0].id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored["data"]["rail"], "manual");
    assert_eq!(stored["data"]["amountMinor"], 5_000);
    // The credit cleared any balance.low latch a settle had set.
    let armed: bool = sqlx::query_scalar(
        "SELECT balance_low_sent FROM oxsum.webhook_endpoints WHERE endpoint_id = $1",
    )
    .bind(endpoint.endpoint.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!armed);
}

/// A negative `balanceLowMinor` is refused; every catalog name subscribes.
#[tokio::test]
async fn the_catalog_and_threshold_validate() {
    let url = db_or_skip!();
    let (db, org, _) = world(&url).await;
    let error = db
        .create_webhook_with_threshold(
            org,
            "https://receiver.example/hook",
            &["balance.low".to_owned()],
            Some(-1),
            &key(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, WalletError::InvalidInput(_)), "{error:?}");

    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &[
                "request.settled".to_owned(),
                "org.suspended".to_owned(),
                "deposit.credited".to_owned(),
                "deposit.failed".to_owned(),
                "deposit.reversed".to_owned(),
                "balance.low".to_owned(),
                "statement.finalized".to_owned(),
                "statement.payment_overdue".to_owned(),
                "statement.payment_status_updated".to_owned(),
                "org.credit_limit_exceeded".to_owned(),
                "anomaly.detected".to_owned(),
                "budget.exhaustion_forecast".to_owned(),
            ],
            &key(),
        )
        .await
        .unwrap();
    assert_eq!(endpoint.endpoint.events.len(), 12);
    // An unknown name is still refused.
    let error = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["turn.started".to_owned()],
            &key(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, WalletError::InvalidInput(_)), "{error:?}");
}

/// The pure projection: `forecast_exhaustion` answers the date a periodically
/// limited key's committed spend, carried forward at the rate it has run so far
/// this window, crosses its `spendLimitMinor` — and only when that crossing
/// falls inside the window (issue #220).
#[test]
fn forecast_exhaustion_projects_the_crossing() {
    use oxsum_core::BudgetDuration;
    use time::macros::date;

    let monday = date!(2026 - 10 - 12); // a real Monday
    let today = date!(2026 - 10 - 13); // day 2 of the ISO week

    // Weekly, 10 limit, 4 committed in 2 days → rate 2/day → crosses day 5 (Fri).
    let forecast = oxsum_core::forecast_exhaustion(BudgetDuration::Weekly, monday, today, 4, 10)
        .expect("a crossing inside the week projects");
    assert_eq!(forecast.projected_exhaustion_date, date!(2026 - 10 - 16));
    assert_eq!(forecast.days_remaining, 3);
    assert_eq!(forecast.committed_minor, 4);
    assert_eq!(forecast.window_start, monday);
    assert_eq!(forecast.budget_duration, "weekly");

    // A slower burn never reaches the cap inside the window: nothing to warn.
    // 1 committed in 2 days → 0.5/day → 20 days to cross > the 5 remaining.
    assert!(
        oxsum_core::forecast_exhaustion(BudgetDuration::Weekly, monday, today, 1, 10).is_none()
    );

    // No spend or no limit: no projection.
    for (committed, limit) in [(0, 10), (-3, 10), (5, 0)] {
        assert!(
            oxsum_core::forecast_exhaustion(
                BudgetDuration::Weekly,
                monday,
                today,
                committed,
                limit,
            )
            .is_none(),
            "committed={committed} limit={limit}"
        );
    }

    // Already over the line: exhaustion is today, zero days remaining.
    let over =
        oxsum_core::forecast_exhaustion(BudgetDuration::Weekly, monday, today, 12, 10).unwrap();
    assert_eq!(over.projected_exhaustion_date, today);
    assert_eq!(over.days_remaining, 0);

    // Daily: the whole window is today, so committed at the limit projects to today.
    let daily =
        oxsum_core::forecast_exhaustion(BudgetDuration::Daily, today, today, 10, 10).unwrap();
    assert_eq!(daily.projected_exhaustion_date, today);
    assert_eq!(daily.days_remaining, 0);
}

/// The monthly window honors the real month length: a burn that crosses inside a
/// 31-day October projects, the same burn judged against a 30-day month does not.
#[test]
fn forecast_exhaustion_uses_the_real_month_length() {
    use oxsum_core::BudgetDuration;
    use time::macros::date;

    let october_first = date!(2026 - 10 - 01);
    let oct_14 = date!(2026 - 10 - 14);
    // 14 days elapsed, 14 committed → 1/day; limit 31 crosses on
    // ceil(31*14/14)-1 = 30 → Oct 31, the last day inside October.
    let forecast =
        oxsum_core::forecast_exhaustion(BudgetDuration::Monthly, october_first, oct_14, 14, 31)
            .expect("the 31st of October is inside the window");
    assert_eq!(forecast.projected_exhaustion_date, date!(2026 - 10 - 31));

    // The same rate and limit in a 30-day month: November's day 31 is outside it.
    let november_first = date!(2026 - 11 - 01);
    let nov_14 = date!(2026 - 11 - 14);
    assert!(
        oxsum_core::forecast_exhaustion(BudgetDuration::Monthly, november_first, nov_14, 14, 31,)
            .is_none(),
        "the same crossing lands on Dec 1, outside a 30-day window"
    );
}

/// `budget.exhaustion_forecast` fires once per key per window when the settled
/// figures project a crossing inside it, latches on the key row, and re-arms
/// when a constraint PATCH clears the latch (issue #220).
#[tokio::test]
async fn the_forecast_fires_once_per_window() {
    let url = db_or_skip!();
    let (db, org, tenant, secret) = world_with_secret(&url).await;
    let endpoint = db
        .create_webhook(
            org,
            "https://receiver.example/hook",
            &["budget.exhaustion_forecast".to_owned()],
            &key(),
        )
        .await
        .unwrap();

    // Committed spend comes from the ledger, so fund the wallet and authenticate
    // the registration's first key for the acting key the hold path binds.
    let (_, acting) = db
        .authenticate(&secret)
        .await
        .unwrap()
        .expect("the registration's key authenticates");
    let wallet = oxsum_core::Tenants::new(db.pool().clone())
        .get(&tenant)
        .await
        .unwrap();
    let today = time::OffsetDateTime::now_utc().date();
    wallet.top_up("topup", 1_000_000_000, today).await.unwrap();

    // A daily limit at exactly the committed amount: committed >= limit means
    // the projection is today — a deterministic fire, not a race with the clock.
    let key_id = acting.key_id;
    db.update_key_constraints(
        org,
        key_id,
        oxsum_core::KeyScope::Organization,
        oxsum_core::KeyConstraints {
            spend_limit_minor: Some(10_000),
            budget_duration: Some(oxsum_core::BudgetDuration::Daily),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    wallet
        .hold_for_key(&acting, None, "hold-1", "", 10_000, today)
        .await
        .unwrap();
    wallet.settle("hold-1", "", 10_000, today).await.unwrap();

    // The settle itself does not fire the event — `record_usage`'s row write
    // does, off the same committed figure the ledger just sealed.
    let mut row = usage_row(&format!("req-{}", Uuid::new_v4()), &tenant);
    row.key_id = Some(key_id);
    db.record_usage_balanced(&row, None).await.unwrap();

    let deliveries = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .unwrap();
    let forecasts: Vec<_> = deliveries
        .iter()
        .filter(|d| d.event_type == oxsum_core::BUDGET_EXHAUSTION_FORECAST)
        .collect();
    assert_eq!(
        forecasts.len(),
        1,
        "one forecast per key per window: {deliveries:?}"
    );
    let stored: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM oxsum.webhook_deliveries WHERE delivery_id = $1")
            .bind(forecasts[0].id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        stored["data"]["keyId"].as_str().unwrap(),
        key_id.to_string()
    );
    assert_eq!(stored["data"]["spendLimitMinor"], 10_000);
    assert_eq!(stored["data"]["budgetDuration"], "daily");
    assert_eq!(stored["data"]["daysRemaining"], 0);

    // A second usage write in the same window does not fire again — the latch
    // holds for the rest of the window. (A further hold would be refused anyway:
    // committed is already at the limit.)
    let mut row = usage_row(&format!("req-{}", Uuid::new_v4()), &tenant);
    row.key_id = Some(key_id);
    db.record_usage_balanced(&row, None).await.unwrap();
    let deliveries = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        deliveries
            .iter()
            .filter(|d| d.event_type == oxsum_core::BUDGET_EXHAUSTION_FORECAST)
            .count(),
        1,
        "the latch holds inside the window"
    );

    // Re-PATCHing the constraints clears the latch: the next settle forecasts
    // again under the new terms — a lower limit keeps the projection over.
    db.update_key_constraints(
        org,
        key_id,
        oxsum_core::KeyScope::Organization,
        oxsum_core::KeyConstraints {
            spend_limit_minor: Some(5_000),
            budget_duration: Some(oxsum_core::BudgetDuration::Daily),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut row = usage_row(&format!("req-{}", Uuid::new_v4()), &tenant);
    row.key_id = Some(key_id);
    db.record_usage_balanced(&row, None).await.unwrap();
    let deliveries = db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        deliveries
            .iter()
            .filter(|d| d.event_type == oxsum_core::BUDGET_EXHAUSTION_FORECAST)
            .count(),
        2,
        "a constraint change re-arms the forecast"
    );
}

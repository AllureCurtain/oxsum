//! The webhook delivery worker: signed POSTs to the endpoints organizations
//! registered.
//!
//! `record_usage` enqueues a durable row per settlement; [`deliver_due`] picks
//! up what is due, signs the stored body under the endpoint's secret — opened
//! with the same `OXSUM_SECRET_KEY` that seals channel credentials — and POSTs
//! it. A 2xx marks `delivered`; anything else counts the attempt and
//! reschedules on a backoff, marked `failed` once the budget runs out.

use std::time::Duration;

use oxsum_core::{Db, DeliveryOutcome, DueDelivery, SecretKey, signature};
use time::OffsetDateTime;

use crate::metrics::Metrics;

/// How often the worker looks for due deliveries.
pub const DELIVERY_INTERVAL: Duration = Duration::from_secs(30);

/// The longest a delivery POST may take before it counts as a failed attempt.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Delivers every due webhook row; the return count is for tests and logs.
///
/// Each row is attempted independently: one slow receiver never blocks the
/// batch, and a row another pass already finished is a no-op update.
pub async fn deliver_due(
    db: &Db,
    http: &reqwest::Client,
    secret: &SecretKey,
    metrics: &Metrics,
    organization: Option<uuid::Uuid>,
) {
    let due = match db.claim_due_deliveries(organization).await {
        Ok(due) => due,
        Err(error) => {
            tracing::error!(%error, "the webhook worker could not read its queue");
            return;
        }
    };
    for item in due {
        let outcome = deliver_one(http, secret, &item).await;
        crate::metrics::webhook_delivery(
            metrics,
            match &outcome {
                DeliveryOutcome::Delivered { .. } => "delivered",
                DeliveryOutcome::AttemptFailed { attempt, .. }
                    if *attempt >= oxsum_core::MAX_DELIVERY_ATTEMPTS =>
                {
                    "failed"
                }
                DeliveryOutcome::AttemptFailed { .. } => "retried",
            },
        );
        if let Err(error) = db.record_delivery(&item.delivery, &outcome).await {
            tracing::error!(%error, delivery = %item.delivery.id, "a webhook attempt could not be recorded");
        }
    }
}

/// One signed POST: the body is the stored payload's exact bytes, so the
/// signature a receiver verifies covers what was queued.
async fn deliver_one(
    http: &reqwest::Client,
    secret: &SecretKey,
    item: &DueDelivery,
) -> DeliveryOutcome {
    let attempt = item.delivery.attempts + 1;
    let Ok(signing_secret) = secret.open(&item.secret_sealed) else {
        // A secret that does not open is a deployment problem — the key rotated
        // under the row: count the attempt like a network failure, so the row
        // stays retryable rather than being failed permanently for what may be
        // a transient misconfiguration.
        return DeliveryOutcome::AttemptFailed {
            status: None,
            error: "the signing secret does not open under OXSUM_SECRET_KEY".to_owned(),
            attempt,
        };
    };
    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let result = http
        .post(&item.url)
        .header("content-type", "application/json")
        .header("x-oxsum-event", &item.delivery.event_type)
        .header("x-oxsum-delivery", item.delivery.id.to_string())
        .header(
            "x-oxsum-signature",
            signature(&signing_secret, timestamp, item.body.as_bytes()),
        )
        .body(item.body.clone())
        .timeout(DELIVERY_TIMEOUT)
        .send()
        .await;
    match result {
        Ok(response) => {
            let status = i32::from(response.status().as_u16());
            if response.status().is_success() {
                DeliveryOutcome::Delivered { status }
            } else {
                DeliveryOutcome::AttemptFailed {
                    status: Some(status),
                    error: format!("the receiver answered {status}"),
                    attempt,
                }
            }
        }
        Err(error) => DeliveryOutcome::AttemptFailed {
            status: None,
            error: format!("the receiver could not be reached: {error}"),
            attempt,
        },
    }
}

/// The background task: the queue is durable, so the interval only bounds how
/// late a delivery is, never whether it happens. Retries come from each row's
/// own `next_attempt_at`, not the interval.
pub fn spawn_worker(
    db: Db,
    http: reqwest::Client,
    secret: SecretKey,
    metrics: Metrics,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            deliver_due(&db, &http, &secret, &metrics, None).await;
            tokio::time::sleep(DELIVERY_INTERVAL).await;
        }
    })
}

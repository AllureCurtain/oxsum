//! Safe signed application signal delivery over immutable owner-approved destinations.
use oxsum_core::{Db, DeliveryOutcome, DueApplicationNotification, SecretKey, signature};
use oxsum_integrations::provider_http::ProviderHttp;
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// Send one bounded application batch, retaining original bytes and lease fences.
pub async fn deliver_due(
    db: &Db,
    http: &ProviderHttp,
    secret: &SecretKey,
    application: Option<Uuid>,
) {
    let due = match db.claim_application_notifications(application).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error,"application notification claim failed");
            return;
        }
    };
    let mut running = tokio::task::JoinSet::new();
    for item in due {
        if running.len() >= 8
            && let Some(done) = running.join_next().await
        {
            record(db, done).await;
        }
        let http = http.clone();
        let secret = secret.clone();
        running.spawn(async move {
            let outcome = deliver_one(&http, &secret, &item).await;
            (item, outcome)
        });
    }
    while let Some(done) = running.join_next().await {
        record(db, done).await;
    }
}
async fn record(
    db: &Db,
    result: Result<(DueApplicationNotification, DeliveryOutcome), tokio::task::JoinError>,
) {
    match result {
        Ok((item, outcome)) => {
            if let Err(error) = db
                .record_application_notification(&item.delivery, &outcome)
                .await
            {
                tracing::error!(%error,"application notification result failed");
            }
        }
        Err(error) => tracing::error!(%error,"application notification task failed"),
    }
}
async fn deliver_one(
    http: &ProviderHttp,
    secret: &SecretKey,
    item: &DueApplicationNotification,
) -> DeliveryOutcome {
    let failure = |error: &str, status| DeliveryOutcome::AttemptFailed {
        status,
        error: error.into(),
        attempt: item.delivery.attempts + 1,
    };
    let signing = match secret.open(&item.secret_sealed) {
        Ok(value) => value,
        Err(_) => return failure("signing key unavailable", None),
    };
    let request = match http
        .request(
            reqwest::Method::POST,
            &item.url,
            &item.destination_policy,
            &[],
        )
        .await
    {
        Ok(value) => value,
        Err(_) => return failure("destination blocked", None),
    };
    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let result = request
        .header("content-type", "application/json")
        .header("x-oxsum-event", &item.delivery.event_type)
        .header("x-oxsum-delivery", item.delivery.delivery_id.to_string())
        .header(
            "x-oxsum-signature",
            signature(&signing, timestamp, item.body.as_bytes()),
        )
        .body(item.body.clone())
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    match result {
        Ok(response) if response.status().is_success() => DeliveryOutcome::Delivered {
            status: i32::from(response.status().as_u16()),
        },
        Ok(response) => failure(
            "receiver rejected",
            Some(i32::from(response.status().as_u16())),
        ),
        Err(_) => failure("receiver unavailable", None),
    }
}

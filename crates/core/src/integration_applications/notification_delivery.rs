//! Bounded durable application delivery claims using the existing retry policy.
use super::notification_types::{ApplicationNotificationDelivery, DueApplicationNotification};
use super::notifications;
use crate::{Db, DeliveryOutcome, WalletError};
use sqlx::Row;
use uuid::Uuid;
impl Db {
    /// Claim at most 32 due signals, reclaiming expired 120-second leases.
    /// Disabled endpoints cannot start an unclaimed attempt; prior claims may finish.
    pub async fn claim_application_notifications(
        &self,
        application: Option<Uuid>,
    ) -> Result<Vec<DueApplicationNotification>, WalletError> {
        let rows=sqlx::query("WITH due AS (SELECT d.delivery_id FROM oxsum.application_notification_deliveries d JOIN oxsum.application_notification_endpoints e USING(endpoint_id) WHERE e.enabled AND ($1::uuid IS NULL OR e.application_id=$1) AND ((d.status='pending' AND d.next_attempt_at<=clock_timestamp()) OR (d.status='sending' AND d.claimed_at<=clock_timestamp()-interval '120 seconds')) ORDER BY d.next_attempt_at,d.delivery_id LIMIT 32 FOR UPDATE OF d SKIP LOCKED) UPDATE oxsum.application_notification_deliveries d SET status='sending',claimed_at=clock_timestamp() FROM due WHERE d.delivery_id=due.delivery_id RETURNING d.*, (SELECT url FROM oxsum.application_notification_endpoints WHERE endpoint_id=d.endpoint_id) AS url,(SELECT destination_policy FROM oxsum.application_notification_endpoints WHERE endpoint_id=d.endpoint_id) AS destination_policy,(SELECT secret_sealed FROM oxsum.application_notification_endpoints WHERE endpoint_id=d.endpoint_id) AS secret_sealed")
            .bind(application).fetch_all(self.pool()).await?;
        rows.iter()
            .map(|row| {
                Ok(DueApplicationNotification {
                    delivery: notifications::delivery(row)?,
                    url: row.try_get("url")?,
                    destination_policy: serde_json::from_value(row.try_get("destination_policy")?)
                        .map_err(crate::error::invalid)?,
                    secret_sealed: row.try_get("secret_sealed")?,
                    body: row.try_get("body")?,
                })
            })
            .collect()
    }
    /// Record only the current claim's result; stale workers cannot overwrite a retry.
    pub async fn record_application_notification(
        &self,
        delivery: &ApplicationNotificationDelivery,
        outcome: &DeliveryOutcome,
    ) -> Result<(), WalletError> {
        match outcome {
            DeliveryOutcome::Delivered { status } => {
                sqlx::query("UPDATE oxsum.application_notification_deliveries SET status='delivered',attempts=attempts+1,response_status=$3,last_error=NULL,delivered_at=clock_timestamp(),claimed_at=NULL WHERE delivery_id=$1 AND status='sending' AND claimed_at=$2")
                    .bind(delivery.delivery_id).bind(delivery.claimed_at).bind(status).execute(self.pool()).await?;
            }
            DeliveryOutcome::AttemptFailed { status, error, .. } => {
                let attempt = delivery.attempts + 1;
                // Persist only fixed sender categories; arbitrary receiver/error text stays private.
                let error = match error.as_str() {
                    "destination blocked"
                    | "receiver unavailable"
                    | "receiver rejected"
                    | "signing key unavailable" => error.as_str(),
                    _ => "delivery failed",
                };
                sqlx::query("UPDATE oxsum.application_notification_deliveries SET attempts=attempts+1,response_status=$3,last_error=$4,status=CASE WHEN attempts+1 >= $5 THEN 'failed' ELSE 'pending' END,next_attempt_at=clock_timestamp()+($6 || ' seconds')::interval,claimed_at=NULL WHERE delivery_id=$1 AND status='sending' AND claimed_at=$2")
                    .bind(delivery.delivery_id).bind(delivery.claimed_at).bind(status).bind(error).bind(crate::MAX_DELIVERY_ATTEMPTS).bind(crate::retry_delay_secs(attempt)).execute(self.pool()).await?;
            }
        }
        Ok(())
    }
}

//! Owner endpoint authority and separately scoped runtime notification reads.
use super::notification_types::{
    ApplicationNotificationDeliveries, ApplicationNotificationDelivery,
    ApplicationNotificationEndpoint, ApplicationNotificationEndpointRequest,
    ApplicationNotificationEndpoints, ApplicationNotificationMutation,
    CreatedApplicationNotificationEndpoint,
};
use crate::{
    ActingService, BusinessCapability, Db, IntegrationScope, SecretKey, WalletError,
    business_staff as authority, integration_credentials,
};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;
const EVENTS: &[&str] = &[
    "customer.changed",
    "key.changed",
    "entitlement.changed",
    "order.changed",
    "statement.changed",
    "evidence.published",
    "balance.changed",
];
pub(super) fn endpoint(
    row: &sqlx::postgres::PgRow,
) -> Result<ApplicationNotificationEndpoint, WalletError> {
    Ok(ApplicationNotificationEndpoint {
        endpoint_id: row.try_get("endpoint_id")?,
        application_id: row.try_get("application_id")?,
        url: row.try_get("url")?,
        destination_policy: serde_json::from_value(row.try_get("destination_policy")?)
            .map_err(crate::error::invalid)?,
        events: row.try_get("events")?,
        enabled: row.try_get("enabled")?,
        secret_last4: row.try_get("secret_last4")?,
        created_at: row.try_get("created_at")?,
    })
}
pub(super) fn delivery(
    row: &sqlx::postgres::PgRow,
) -> Result<ApplicationNotificationDelivery, WalletError> {
    Ok(ApplicationNotificationDelivery {
        delivery_id: row.try_get("delivery_id")?,
        endpoint_id: row.try_get("endpoint_id")?,
        organization_id: row.try_get("organization_id")?,
        event_type: row.try_get("event_type")?,
        entity_id: row.try_get("entity_id")?,
        status: row.try_get("status")?,
        attempts: row.try_get("attempts")?,
        next_attempt_at: row.try_get("next_attempt_at")?,
        response_status: row.try_get("response_status")?,
        last_error: row.try_get("last_error")?,
        created_at: row.try_get("created_at")?,
        delivered_at: row.try_get("delivered_at")?,
        claimed_at: row.try_get("claimed_at")?,
    })
}
async fn owner(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
    app: Uuid,
) -> Result<crate::BusinessPrincipal, WalletError> {
    let business = authority::lock_business(tx).await?;
    let actor =
        authority::require_capability(tx, business, token, BusinessCapability::IntegrationsManage)
            .await?;
    super::application_in(tx, business, app).await?;
    Ok(actor)
}
async fn service(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
) -> Result<Uuid, WalletError> {
    integration_credentials::require_integration(
        tx,
        service,
        IntegrationScope::NotificationsRead,
        None,
    )
    .await?;
    service
        .application_id
        .ok_or_else(|| WalletError::Forbidden("application credential required".into()))
}
async fn one(
    tx: &mut Transaction<'_, Postgres>,
    app: Uuid,
    id: Uuid,
) -> Result<ApplicationNotificationEndpoint, WalletError> {
    let row=sqlx::query("SELECT * FROM oxsum.application_notification_endpoints WHERE application_id=$1 AND endpoint_id=$2").bind(app).bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::NotFound("notification endpoint not found".into()))?;
    endpoint(&row)
}
async fn endpoints(
    tx: &mut Transaction<'_, Postgres>,
    app: Uuid,
) -> Result<ApplicationNotificationEndpoints, WalletError> {
    let rows=sqlx::query("SELECT * FROM oxsum.application_notification_endpoints WHERE application_id=$1 ORDER BY endpoint_id LIMIT 20").bind(app).fetch_all(&mut **tx).await?;
    Ok(ApplicationNotificationEndpoints {
        endpoints: rows.iter().map(endpoint).collect::<Result<_, _>>()?,
    })
}
async fn deliveries(
    tx: &mut Transaction<'_, Postgres>,
    app: Uuid,
    id: Uuid,
    after: Option<Uuid>,
    limit: i64,
) -> Result<ApplicationNotificationDeliveries, WalletError> {
    if !(1..=50).contains(&limit) {
        return Err(crate::error::invalid("limit must be 1 to 50"));
    }
    one(tx, app, id).await?;
    let rows=sqlx::query("SELECT * FROM oxsum.application_notification_deliveries WHERE endpoint_id=$1 AND ($2::uuid IS NULL OR delivery_id>$2) ORDER BY delivery_id LIMIT $3").bind(id).bind(after).bind(limit+1).fetch_all(&mut **tx).await?;
    let mut result = rows.iter().map(delivery).collect::<Result<Vec<_>, _>>()?;
    let more = result.len() > limit as usize;
    result.truncate(limit as usize);
    let next = if more {
        result.last().map(|d| d.delivery_id)
    } else {
        None
    };
    Ok(ApplicationNotificationDeliveries {
        deliveries: result,
        next_cursor: next,
    })
}
impl Db {
    /// Create an immutable owner-approved destination; retries never reveal its secret.
    pub async fn create_application_notification_endpoint(
        &self,
        token: &str,
        app: Uuid,
        request: &ApplicationNotificationEndpointRequest,
        key: &SecretKey,
    ) -> Result<CreatedApplicationNotificationEndpoint, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        if request.url.len() > 2000 || request.url.chars().any(|c| c.is_whitespace() || c == '\\') {
            return Err(crate::error::invalid("invalid notification destination"));
        }
        request.destination_policy.endpoint(&request.url)?;
        let mut events = request.events.clone();
        events.sort();
        events.dedup();
        if events.is_empty()
            || events.len() != request.events.len()
            || events.iter().any(|e| !EVENTS.contains(&e.as_str()))
        {
            return Err(crate::error::invalid(
                "select distinct supported lifecycle events",
            ));
        }
        let mut tx = self.pool().begin().await?;
        let actor = owner(&mut tx, token, app).await?;
        let detail = serde_json::json!({"applicationId":app,"request":request});
        let action = "business.application.notification.create";
        if let Some(id) = authority::replay(
            &mut tx,
            actor.business_id,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return Ok(CreatedApplicationNotificationEndpoint {
                endpoint: one(&mut tx, app, id).await?,
                secret: None,
            });
        }
        let (count,enabled):(i64,i64)=sqlx::query_as("SELECT count(*),count(*) FILTER(WHERE enabled) FROM oxsum.application_notification_endpoints WHERE application_id=$1").bind(app).fetch_one(&mut *tx).await?;
        if count >= 20 || enabled >= 2 {
            return Err(WalletError::Conflict(
                "application permits twenty retained and two enabled notification endpoints".into(),
            ));
        }
        let receipt = authority::record(
            &mut tx,
            actor.business_id,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let secret = crate::webhooks::mint_secret();
        sqlx::query("INSERT INTO oxsum.application_notification_endpoints(endpoint_id,application_id,url,destination_policy,events,secret_sealed,secret_last4) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(receipt.id).bind(app).bind(&request.url).bind(serde_json::json!(request.destination_policy)).bind(events).bind(key.seal(&secret)?).bind(&secret[secret.len()-4..]).execute(&mut *tx).await?;
        let result = one(&mut tx, app, receipt.id).await?;
        tx.commit().await?;
        Ok(CreatedApplicationNotificationEndpoint {
            endpoint: result,
            secret: Some(secret),
        })
    }
    /// List this application's safe endpoints under current owner authority.
    pub async fn business_application_notification_endpoints(
        &self,
        token: &str,
        app: Uuid,
    ) -> Result<ApplicationNotificationEndpoints, WalletError> {
        let mut tx = self.pool().begin().await?;
        owner(&mut tx, token, app).await?;
        endpoints(&mut tx, app).await
    }
    /// List safe endpoint metadata with the separate notificationsRead grant.
    pub async fn application_notification_endpoints(
        &self,
        actor: &ActingService,
    ) -> Result<ApplicationNotificationEndpoints, WalletError> {
        let mut tx = self.pool().begin().await?;
        let app = service(&mut tx, actor).await?;
        endpoints(&mut tx, app).await
    }
    /// Disable future enqueue and claims; a previously claimed attempt may finish.
    pub async fn disable_application_notification_endpoint(
        &self,
        token: &str,
        app: Uuid,
        id: Uuid,
        request: &ApplicationNotificationMutation,
    ) -> Result<ApplicationNotificationEndpoint, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let actor = owner(&mut tx, token, app).await?;
        one(&mut tx, app, id).await?;
        let detail = serde_json::json!({"applicationId":app,"endpointId":id,"request":request});
        let action = "business.application.notification.disable";
        if authority::replay(
            &mut tx,
            actor.business_id,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        .is_none()
        {
            authority::record(
                &mut tx,
                actor.business_id,
                actor.user_id,
                &request.idempotency_key,
                action,
                &detail,
            )
            .await?;
            sqlx::query("UPDATE oxsum.application_notification_endpoints SET enabled=false WHERE endpoint_id=$1").bind(id).execute(&mut *tx).await?;
        }
        let result = one(&mut tx, app, id).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Read bounded current delivery status under owner authority.
    pub async fn business_application_notification_deliveries(
        &self,
        token: &str,
        app: Uuid,
        id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<ApplicationNotificationDeliveries, WalletError> {
        let mut tx = self.pool().begin().await?;
        owner(&mut tx, token, app).await?;
        deliveries(&mut tx, app, id, after, limit).await
    }
    /// Read only this application's bounded delivery status with current credentials.
    pub async fn application_notification_deliveries(
        &self,
        actor: &ActingService,
        id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<ApplicationNotificationDeliveries, WalletError> {
        let mut tx = self.pool().begin().await?;
        let app = service(&mut tx, actor).await?;
        deliveries(&mut tx, app, id, after, limit).await
    }
    /// Start a new bounded automatic cycle for one failed delivery, preserving its bytes.
    pub async fn retry_application_notification(
        &self,
        token: &str,
        app: Uuid,
        id: Uuid,
        delivery_id: Uuid,
        request: &ApplicationNotificationMutation,
    ) -> Result<ApplicationNotificationDelivery, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let actor = owner(&mut tx, token, app).await?;
        let target = one(&mut tx, app, id).await?;
        let row=sqlx::query("SELECT * FROM oxsum.application_notification_deliveries WHERE endpoint_id=$1 AND delivery_id=$2 FOR UPDATE").bind(id).bind(delivery_id).fetch_optional(&mut *tx).await?.ok_or_else(||WalletError::NotFound("notification delivery not found".into()))?;
        let detail = serde_json::json!({"applicationId":app,"endpointId":id,"deliveryId":delivery_id,"request":request});
        let action = "business.application.notification.retry";
        if authority::replay(
            &mut tx,
            actor.business_id,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        .is_none()
        {
            if !target.enabled || row.try_get::<String, _>("status")? != "failed" {
                return Err(WalletError::Conflict(
                    "only a failed delivery to an enabled endpoint can retry".into(),
                ));
            }
            authority::record(
                &mut tx,
                actor.business_id,
                actor.user_id,
                &request.idempotency_key,
                action,
                &detail,
            )
            .await?;
            sqlx::query("UPDATE oxsum.application_notification_deliveries SET status='pending',attempts=0,claimed_at=NULL,next_attempt_at=clock_timestamp() WHERE delivery_id=$1").bind(delivery_id).execute(&mut *tx).await?;
        }
        let row = sqlx::query(
            "SELECT * FROM oxsum.application_notification_deliveries WHERE delivery_id=$1",
        )
        .bind(delivery_id)
        .fetch_one(&mut *tx)
        .await?;
        let answer = delivery(&row)?;
        tx.commit().await?;
        Ok(answer)
    }
}

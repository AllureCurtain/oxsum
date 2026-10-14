use axum::extract::{Extension, Path, State};
use serde::Deserialize;
use uuid::Uuid;

use oxsum_core::{CreatedWebhook, Principal, WalletError, WebhookDelivery, WebhookEndpoint};

use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateWebhookReq {
    url: String,
    events: Vec<String>,
    /// The available-balance line `balance.low` fires below (issue #200);
    /// absent or null is off.
    balance_low_minor: Option<i64>,
    /// The consumption percentages `budget.threshold_crossed` fires at
    /// (issue #232) — integers 1–100; absent or empty is off.
    budget_thresholds_percent: Option<Vec<i32>>,
}

/// Register a webhook endpoint; the signing secret is answered once, here.
pub(crate) async fn create_webhook(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<CreateWebhookReq>,
) -> ApiResult<CreatedWebhook> {
    // Signing needs the sealing key; a deployment without one cannot register an
    // endpoint it could never sign for.
    let Some(secret) = state.config.secret() else {
        return Err(WalletError::Misconfigured(
            "webhooks need OXSUM_SECRET_KEY to sign deliveries".to_owned(),
        )
        .into());
    };
    ok(state
        .db
        .create_webhook_with_threshold(
            principal.organization().id,
            &r.url,
            &r.events,
            r.balance_low_minor,
            r.budget_thresholds_percent.as_deref().unwrap_or(&[]),
            secret,
        )
        .await?)
}

/// The organization's webhook endpoints, newest first.
pub(crate) async fn list_webhooks(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<WebhookEndpoint>> {
    ok(state.db.list_webhooks(principal.organization().id).await?)
}

/// Delete an endpoint — an id of another organization is not found, not forbidden.
pub(crate) async fn delete_webhook(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(endpoint_id): Path<Uuid>,
) -> ApiResult<WebhookEndpoint> {
    match state
        .db
        .delete_webhook(principal.organization().id, endpoint_id)
        .await?
    {
        Some(endpoint) => ok(endpoint),
        None => Err(ApiError::not_found()),
    }
}

/// An endpoint's recent deliveries — an id of another organization is not found.
pub(crate) async fn webhook_deliveries(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(endpoint_id): Path<Uuid>,
) -> ApiResult<Vec<WebhookDelivery>> {
    match state
        .db
        .webhook_deliveries(principal.organization().id, endpoint_id)
        .await?
    {
        Some(deliveries) => ok(deliveries),
        None => Err(ApiError::not_found()),
    }
}

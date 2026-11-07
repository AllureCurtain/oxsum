//! Merchant callback transport. Signature verification precedes normalization.
use super::business::personal_token;
use super::{ApiResult, ok};
use crate::AppState;
use crate::error::{ApiError, ApiQuery};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use oxsum_core::{MerchantEventPage, MerchantRail, MerchantSigningContext};
use oxsum_integrations::merchant_events;
use serde::Deserialize;
use uuid::Uuid;
#[derive(Deserialize)]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}
pub(crate) async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(merchant): Path<Uuid>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<MerchantEventPage> {
    ok(state
        .db
        .business_merchant_events(
            &personal_token(&headers)?,
            merchant,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}
fn signed(context: &MerchantSigningContext, headers: &HeaderMap, body: &[u8]) -> bool {
    let name = match context.rail {
        MerchantRail::Hyperswitch => "x-webhook-signature-512",
        MerchantRail::Stripe => "stripe-signature",
        MerchantRail::Airwallex => "x-signature",
    };
    merchant_events::verify(
        context,
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        headers.get("x-timestamp").and_then(|v| v.to_str().ok()),
        body,
        time::OffsetDateTime::now_utc().unix_timestamp(),
    )
}
pub(crate) async fn webhook(
    State(state): State<AppState>,
    Path((merchant, version)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    let context = state
        .db
        .merchant_signing_context(merchant, version, secret)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if !signed(&context, &headers, &body) {
        return Err(ApiError::Unauthorized);
    }
    receive(&state, &context, &body).await
}
async fn receive(
    state: &AppState,
    context: &MerchantSigningContext,
    body: &[u8],
) -> Result<StatusCode, ApiError> {
    let (event, digest) = merchant_events::normalize(context, body)
        .map_err(|_| ApiError::Validation("unreadable merchant event".into()))?;
    let receipt = state
        .db
        .receive_merchant_event(context, &event, &digest)
        .await?;
    crate::merchant_payments::apply_legacy_event(&state.db, &state.tenants, &receipt).await?;
    crate::merchant_payments::apply_order_event(&state.db, &state.tenants, &receipt).await?;
    Ok(StatusCode::OK)
}
/// Compatibility callback URL resolves its one environment merchant first.
/// It may try retained versions only inside this trusted merchant namespace.
pub(crate) async fn environment(
    state: &AppState,
    rail: MerchantRail,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<StatusCode, ApiError> {
    let secret = state.config.secret().ok_or_else(ApiError::not_found)?;
    let context = state
        .db
        .environment_merchant(rail, secret)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let contexts = state
        .db
        .merchant_signing_versions(context.merchant_id, secret)
        .await?;
    for context in contexts {
        if !signed(&context, headers, body) {
            continue;
        }
        let (mut event, digest) = merchant_events::normalize(&context, body)
            .map_err(|_| ApiError::Validation("unreadable merchant event".into()))?;
        // Historical provider payloads may omit our metadata. Only an already
        // stored reference inside this trusted merchant may supply that context.
        let bound = match event.provider_payment_id.as_deref() {
            Some(provider) => {
                state
                    .db
                    .merchant_payment_by_provider(context.merchant_id, provider)
                    .await?
            }
            None => None,
        };
        if let Some(bound) = bound {
            if bound.version_id != context.version.id {
                continue;
            }
            if event.payment_id.is_none() {
                event.payment_id = Some(bound.payment_id);
            }
            if event.organization_id.is_none() {
                event.organization_id = Some(bound.organization_id);
            }
        }
        let receipt = state
            .db
            .receive_merchant_event(&context, &event, &digest)
            .await?;
        crate::merchant_payments::apply_legacy_event(&state.db, &state.tenants, &receipt).await?;
        crate::merchant_payments::apply_order_event(&state.db, &state.tenants, &receipt).await?;
        return Ok(StatusCode::OK);
    }
    Err(ApiError::Unauthorized)
}

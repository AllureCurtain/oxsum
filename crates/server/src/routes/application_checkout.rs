//! Application-only checkout routes reuse production provider recovery.
use super::{ApiResult, business, ok, orders};
use crate::AppState;
use crate::error::{ApiError, ApiJson, ApiQuery};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use oxsum_core::{
    ApplicationCheckoutRequest, ApplicationCheckoutSelection, ApplicationCheckoutTerms,
    PaymentOrder, PaymentOrderPage,
};
use serde::Deserialize;
use uuid::Uuid;

pub(crate) async fn quote(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<Uuid>,
    ApiJson(selection): ApiJson<ApplicationCheckoutSelection>,
) -> ApiResult<ApplicationCheckoutTerms> {
    let service = business::integration(&state, &headers).await?;
    ok(state
        .db
        .application_checkout_quote(&service, org, &selection)
        .await?)
}
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<Uuid>,
    ApiJson(request): ApiJson<ApplicationCheckoutRequest>,
) -> ApiResult<orders::OrderCheckout> {
    let service = business::integration(&state, &headers).await?;
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    let (order, _) = state
        .db
        .create_application_checkout(&service, org, &request, secret)
        .await?;
    checkout(&state, org, order, &request).await
}
async fn checkout(
    state: &AppState,
    org: Uuid,
    order: PaymentOrder,
    request: &ApplicationCheckoutRequest,
) -> ApiResult<orders::OrderCheckout> {
    let description = orders::purpose_description(order.purpose);
    let order = orders::drive_checkout(
        state,
        org,
        order,
        description,
        Some(request.return_url.clone()),
        Some(request.cancel_url.clone()),
    )
    .await?;
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    let context = state
        .db
        .merchant_signing_context(
            order.merchant_id.ok_or(ApiError::Internal)?,
            order.merchant_version_id.ok_or(ApiError::Internal)?,
            secret,
        )
        .await?
        .ok_or(ApiError::Internal)?;
    ok(orders::OrderCheckout {
        provider_payment_id: order.provider_payment_id.clone(),
        client_secret: order.checkout["clientSecret"].as_str().map(str::to_owned),
        checkout_url: order.checkout["checkoutUrl"].as_str().map(str::to_owned),
        publishable_key: context.version.publishable_key.clone(),
        sdk_url: context.version.sdk_url.clone(),
        order,
    })
}
pub(crate) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<PaymentOrder> {
    let service = business::integration(&state, &headers).await?;
    ok(state
        .db
        .application_checkout_order(&service, org, id, false)
        .await?
        .0)
}
pub(crate) async fn resume(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<orders::OrderCheckout> {
    let service = business::integration(&state, &headers).await?;
    let (order, request) = state
        .db
        .application_checkout_order(&service, org, id, true)
        .await?;
    checkout(&state, org, order, &request).await
}
#[derive(Deserialize)]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<Uuid>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<PaymentOrderPage> {
    let service = business::integration(&state, &headers).await?;
    ok(state
        .db
        .application_checkout_orders(&service, org, page.after, page.limit.unwrap_or(20))
        .await?)
}

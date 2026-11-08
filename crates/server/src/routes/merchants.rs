//! Merchant transport; current authority and atomic configuration live in core.

use crate::AppState;
use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use oxsum_core::{
    MerchantCreateRequest, MerchantMutationResult, MerchantPage, MerchantStatusRequest,
    MerchantVersionPage, MerchantVersionRequest,
};
use serde::Deserialize;
use uuid::Uuid;
#[derive(Deserialize)]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}
#[derive(Deserialize)]
pub(crate) struct Versions {
    after: Option<i64>,
    limit: Option<i64>,
}
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<MerchantPage> {
    ok(state
        .db
        .business_merchants(
            &super::business::personal_token(&headers)?,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}
pub(crate) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<MerchantCreateRequest>,
) -> ApiResult<MerchantMutationResult> {
    let token = super::business::personal_token(&headers)?;
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    ok(state.db.create_merchant(&token, &request, secret).await?)
}
pub(crate) async fn versions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(merchant): Path<Uuid>,
    ApiQuery(page): ApiQuery<Versions>,
) -> ApiResult<MerchantVersionPage> {
    ok(state
        .db
        .business_merchant_versions(
            &super::business::personal_token(&headers)?,
            merchant,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}
pub(crate) async fn rotate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(merchant): Path<Uuid>,
    ApiJson(request): ApiJson<MerchantVersionRequest>,
) -> ApiResult<MerchantMutationResult> {
    let token = super::business::personal_token(&headers)?;
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    ok(state
        .db
        .rotate_merchant(&token, merchant, &request, secret)
        .await?)
}
pub(crate) async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(merchant): Path<Uuid>,
    ApiJson(request): ApiJson<MerchantStatusRequest>,
) -> ApiResult<MerchantMutationResult> {
    ok(state
        .db
        .set_merchant_enabled(
            &super::business::personal_token(&headers)?,
            merchant,
            &request,
        )
        .await?)
}

/// Current safe business checkout route settings.
pub(crate) async fn checkout_routes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<oxsum_core::CheckoutRoute>> {
    ok(state
        .db
        .business_checkout_routes(&super::business::personal_token(&headers)?)
        .await?)
}

/// Explicit owner selection, separate from legacy callback bootstrap.
pub(crate) async fn select_checkout_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::CheckoutRouteRequest>,
) -> ApiResult<oxsum_core::CheckoutRouteMutation> {
    ok(state
        .db
        .set_checkout_route(&super::business::personal_token(&headers)?, &request)
        .await?)
}

/// Available choices for the authenticated customer.
pub(crate) async fn checkout_options(
    State(state): State<AppState>,
    axum::Extension(principal): axum::Extension<oxsum_core::Principal>,
) -> ApiResult<Vec<oxsum_core::CheckoutRoute>> {
    ok(state
        .db
        .customer_checkout_options(principal.organization().id)
        .await?)
}

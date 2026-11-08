//! Published bundle catalog and audited business commercial mutations.
use super::{ApiResult, ok};
use crate::AppState;
use crate::error::ApiJson;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use oxsum_core::{BundleMutation, BundleOffer, BundlePublishRequest, BundleRetireRequest};
use uuid::Uuid;

pub(crate) async fn catalog(State(state): State<AppState>) -> ApiResult<Vec<BundleOffer>> {
    ok(state.db.bundle_catalog().await?)
}
pub(crate) async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<BundleOffer>> {
    ok(state
        .db
        .business_bundle_offers(&super::business::personal_token(&headers)?)
        .await?)
}
pub(crate) async fn publish(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<BundlePublishRequest>,
) -> ApiResult<BundleMutation> {
    ok(state
        .db
        .publish_bundle(&super::business::personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn retire(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    ApiJson(request): ApiJson<BundleRetireRequest>,
) -> ApiResult<BundleMutation> {
    ok(state
        .db
        .retire_bundle(&super::business::personal_token(&headers)?, id, &request)
        .await?)
}

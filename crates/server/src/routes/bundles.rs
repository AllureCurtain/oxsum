//! Published bundle catalog and audited business commercial mutations.
use super::{ApiResult, ok};
use crate::AppState;
use crate::error::ApiJson;
use axum::extract::{Path, State};
use oxsum_core::{BundleMutation, BundleOffer, BundlePublishRequest, BundleRetireRequest};
use uuid::Uuid;

pub(crate) async fn catalog(State(state): State<AppState>) -> ApiResult<Vec<BundleOffer>> {
    ok(state.db.bundle_catalog().await?)
}
pub(crate) async fn history(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
) -> ApiResult<Vec<BundleOffer>> {
    ok(state.db.business_bundle_offers(&token).await?)
}
pub(crate) async fn publish(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiJson(request): ApiJson<BundlePublishRequest>,
) -> ApiResult<BundleMutation> {
    ok(state.db.publish_bundle(&token, &request).await?)
}
pub(crate) async fn retire(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(id): Path<Uuid>,
    ApiJson(request): ApiJson<BundleRetireRequest>,
) -> ApiResult<BundleMutation> {
    ok(state.db.retire_bundle(&token, id, &request).await?)
}

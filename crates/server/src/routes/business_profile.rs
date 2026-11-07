//! Public identity read and personal owner profile management.

use axum::extract::State;
use axum::http::HeaderMap;
use oxsum_core::{BusinessProfile, BusinessProfileMutation, BusinessProfileRequest};

use crate::AppState;
use crate::config::Branding;
use crate::error::ApiJson;
use crate::routes::{ApiResult, business, ok};

pub(crate) async fn branding(State(state): State<AppState>) -> ApiResult<Branding> {
    ok(state
        .db
        .public_business_profile(state.config.branding())
        .await?)
}

pub(crate) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<BusinessProfile> {
    ok(state
        .db
        .business_profile(
            &business::personal_token(&headers)?,
            state.config.branding(),
        )
        .await?)
}

pub(crate) async fn save(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<BusinessProfileRequest>,
) -> ApiResult<BusinessProfileMutation> {
    ok(state
        .db
        .set_business_profile(&business::personal_token(&headers)?, &request)
        .await?)
}

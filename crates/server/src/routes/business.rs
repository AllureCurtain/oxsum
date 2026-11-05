//! Initial owner consent is separate from customer organization ownership.
use crate::{
    AppState,
    error::{ApiError, ApiJson},
    routes::{ApiResult, ok},
};
use axum::extract::State;
use axum::http::HeaderMap;

pub(crate) async fn accept_setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessSetupRequest>,
) -> ApiResult<oxsum_core::BusinessSetupResult> {
    // A bearer key cannot select a customer user or replace personal consent.
    if headers.contains_key("authorization") {
        return Err(ApiError::Forbidden(
            "setup acceptance requires a personal session".into(),
        ));
    }
    let token = crate::auth::session_cookie_value(&headers).ok_or(ApiError::Unauthorized)?;
    ok(state.db.accept_business_setup(&token, &request).await?)
}
pub(crate) async fn register_owner(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<oxsum_core::BusinessSetupRegistrationRequest>,
) -> ApiResult<oxsum_core::BusinessSetupResult> {
    ok(state.db.register_business_owner(&request).await?)
}

pub(crate) fn personal_token(headers: &HeaderMap) -> Result<String, ApiError> {
    if headers.contains_key("authorization") {
        return Err(ApiError::Forbidden(
            "business authority requires a personal session".into(),
        ));
    }
    crate::auth::session_cookie_value(headers).ok_or(ApiError::Unauthorized)
}

pub(crate) async fn context(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<oxsum_core::SessionContextInfo> {
    ok(state.db.session_context(&personal_token(&headers)?).await?)
}
pub(crate) async fn select_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::SessionContextRequest>,
) -> ApiResult<oxsum_core::SessionContextInfo> {
    ok(state
        .db
        .switch_session_context(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn business(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<oxsum_core::BusinessPrincipal> {
    ok(state
        .db
        .business_capability(
            &personal_token(&headers)?,
            oxsum_core::BusinessCapability::OverviewRead,
        )
        .await?)
}
pub(crate) async fn staff(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<oxsum_core::BusinessStaff>> {
    ok(state.db.business_staff(&personal_token(&headers)?).await?)
}
pub(crate) async fn set_staff(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessStaffRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .set_business_staff(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn revoke_staff(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessStaffRevokeRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .revoke_business_staff(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn transfer_ownership(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessOwnershipRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .transfer_business_ownership(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn registration(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessRegistrationRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .set_business_registration(&personal_token(&headers)?, &request)
        .await?)
}

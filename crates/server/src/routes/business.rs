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

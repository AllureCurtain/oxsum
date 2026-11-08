//! Current-actor business setup facts; no probes or secrets in the response.

use axum::extract::State;
use axum::http::HeaderMap;

use crate::AppState;
use crate::routes::{ApiResult, business, ok};

pub(crate) async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<oxsum_core::BusinessReadiness> {
    ok(state
        .db
        .business_readiness(
            &business::personal_token(&headers)?,
            state.config.secret(),
            state.config.public_url().is_some(),
        )
        .await?)
}

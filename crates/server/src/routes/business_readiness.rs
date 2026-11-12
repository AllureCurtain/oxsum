//! Current-actor business setup facts; no probes or secrets in the response.

use axum::extract::State;

use crate::AppState;
use crate::routes::{ApiResult, ok};

pub(crate) async fn get(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
) -> ApiResult<oxsum_core::BusinessReadiness> {
    ok(state
        .db
        .business_readiness(
            &token,
            state.config.secret(),
            state.config.public_url().is_some(),
        )
        .await?)
}

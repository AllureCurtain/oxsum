//! Deployment-only ownership inspection. Staff setup/capabilities are separate
//! from customer membership and are introduced by the business authority task.
use crate::{
    AppState,
    routes::{ApiResult, ok},
};
use axum::extract::{Path, State};
use uuid::Uuid;

pub(crate) async fn ownership(
    State(state): State<AppState>,
) -> ApiResult<oxsum_core::BusinessOwnership> {
    ok(state.db.business_ownership().await?)
}

pub(crate) async fn customer(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
) -> ApiResult<oxsum_core::CustomerProfile> {
    let business = state.db.business_ownership().await?.business;
    ok(state
        .db
        .customer_profile(business.id, organization_id)
        .await?)
}

pub(crate) async fn setup_token(
    State(state): State<AppState>,
    crate::error::ApiJson(request): crate::error::ApiJson<oxsum_core::BusinessSetupTokenRequest>,
) -> ApiResult<oxsum_core::BusinessSetupToken> {
    ok(state
        .db
        .mint_business_setup(oxsum_core::SetupAuthority::Deployment, &request)
        .await?)
}

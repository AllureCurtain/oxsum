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

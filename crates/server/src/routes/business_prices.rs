//! Staff transport for sale books. Core owns authority, timelines and exact rating.

use crate::AppState;
use crate::error::{ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use axum::extract::{Path, State};
use oxsum_core::{
    SaleBookHistory, SaleBookPage, SaleChangeRequest, SaleMutationResult, SalePreviewRequest,
    SalePricePreview, SaleSchedule,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}
pub(crate) async fn books(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<SaleBookPage> {
    ok(state
        .db
        .sale_books(&token, page.after, page.limit.unwrap_or(50))
        .await?)
}
pub(crate) async fn history(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(id): Path<Uuid>,
) -> ApiResult<SaleBookHistory> {
    ok(state.db.sale_book_history(&token, id).await?)
}
pub(crate) async fn customer(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<SaleSchedule>> {
    ok(state.db.customer_sale_terms(&token, id).await?)
}
pub(crate) async fn change(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiJson(request): ApiJson<SaleChangeRequest>,
) -> ApiResult<SaleMutationResult> {
    ok(state.db.change_sale_terms(&token, &request).await?)
}
pub(crate) async fn preview(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiJson(request): ApiJson<SalePreviewRequest>,
) -> ApiResult<SalePricePreview> {
    ok(state.db.preview_sale_price(&token, &request).await?)
}

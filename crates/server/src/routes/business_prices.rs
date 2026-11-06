//! Staff transport for sale books. Core owns authority, timelines and exact rating.
use super::business::personal_token;
use crate::{
    AppState,
    error::{ApiJson, ApiQuery},
    routes::{ApiResult, ok},
};
use axum::{
    extract::{Path, State},
    http::HeaderMap,
};
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
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<SaleBookPage> {
    ok(state
        .db
        .sale_books(
            &personal_token(&headers)?,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}
pub(crate) async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<SaleBookHistory> {
    ok(state
        .db
        .sale_book_history(&personal_token(&headers)?, id)
        .await?)
}
pub(crate) async fn customer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Vec<SaleSchedule>> {
    ok(state
        .db
        .customer_sale_terms(&personal_token(&headers)?, id)
        .await?)
}
pub(crate) async fn change(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SaleChangeRequest>,
) -> ApiResult<SaleMutationResult> {
    ok(state
        .db
        .change_sale_terms(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SalePreviewRequest>,
) -> ApiResult<SalePricePreview> {
    ok(state
        .db
        .preview_sale_price(&personal_token(&headers)?, &request)
        .await?)
}

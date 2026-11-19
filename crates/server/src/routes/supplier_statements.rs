//! Personal Business supplier source import and frozen comparison routes.
use crate::AppState;
use crate::auth::PersonalToken;
use crate::error::{ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use axum::extract::{Extension, Path, State};
use oxsum_core::supplier_statements::{SupplierComparisonRequest, SupplierStatementImport};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct History {
    before: Option<Uuid>,
}
pub(crate) async fn choices(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
) -> ApiResult<Vec<Value>> {
    ok(state.db.supplier_statement_choices(&token).await?)
}
pub(crate) async fn preview(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
    ApiJson(input): ApiJson<SupplierStatementImport>,
) -> ApiResult<Value> {
    ok(state.db.preview_supplier_statement(&token, &input).await?)
}
pub(crate) async fn import(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
    ApiJson(input): ApiJson<SupplierStatementImport>,
) -> ApiResult<Value> {
    ok(state.db.import_supplier_statement(&token, &input).await?)
}
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
    ApiQuery(query): ApiQuery<History>,
) -> ApiResult<Vec<Value>> {
    ok(state.db.supplier_statements(&token, query.before).await?)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Selection {
    comparison_id: Option<Uuid>,
}
pub(crate) async fn comparisons(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
    Path(id): Path<Uuid>,
    ApiQuery(query): ApiQuery<History>,
) -> ApiResult<Vec<Value>> {
    ok(state
        .db
        .supplier_statement_comparisons(&token, id, query.before)
        .await?)
}
pub(crate) async fn detail(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
    Path(id): Path<Uuid>,
    ApiQuery(query): ApiQuery<Selection>,
) -> ApiResult<Value> {
    ok(state
        .db
        .supplier_statement(&token, id, query.comparison_id)
        .await?)
}
pub(crate) async fn compare(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
    Path(id): Path<Uuid>,
    ApiJson(input): ApiJson<SupplierComparisonRequest>,
) -> ApiResult<Value> {
    ok(state
        .db
        .compare_supplier_statement(&token, id, &input)
        .await?)
}

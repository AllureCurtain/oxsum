//! Plan statements (issue #612, T21): the customer-visible billing document a
//! plan-period close issues — the `invoice_journals` receivable restated in
//! customer vocabulary. The customer reads its own statements
//! (`/api/v1/org/statements`); business staff reads cross-organization
//! (`/api/v1/business/statements`, `ReportsRead`) and runs release-review
//! under `FundsWrite` — extend the grace window or void the statement with
//! reversing lines, every release audited in `finance_audit`.

use super::{ApiResult, ok};
use crate::AppState;
use crate::error::{ApiError, ApiJson, ApiQuery};
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use oxsum_core::{PlanStatementDetail, PlanStatementPage, Principal};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /api/v1/org/statements`: the caller's own plan statements, newest
/// first — the billing read the plan surface carries.
pub(crate) async fn my_statements(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<PlanStatementPage> {
    ok(state
        .db
        .plan_statements(
            principal.organization().id,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/org/statements/{id}`: one statement plus its lines, scoped to
/// the owning organization.
pub(crate) async fn my_statement(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(statement_id): Path<Uuid>,
) -> ApiResult<PlanStatementDetail> {
    let statement = state
        .db
        .plan_statement(principal.organization().id, statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ok(statement)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BusinessStatementPage {
    /// Narrow to one lifecycle state — `pastDue` is the dunning work list.
    state: Option<String>,
    /// Scope to one customer organization.
    organization_id: Option<Uuid>,
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /api/v1/business/statements`: the staff cross-organization statement
/// list — `ReportsRead`, a personal session, never a machine credential.
pub(crate) async fn business_statements(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<BusinessStatementPage>,
) -> ApiResult<PlanStatementPage> {
    ok(state
        .db
        .business_plan_statements(
            &super::business::personal_token(&headers)?,
            page.state.as_deref(),
            page.organization_id,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/business/statements/{id}`: one statement plus its lines.
pub(crate) async fn business_statement(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(statement_id): Path<Uuid>,
) -> ApiResult<PlanStatementDetail> {
    let statement = state
        .db
        .business_plan_statement(&super::business::personal_token(&headers)?, statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ok(statement)
}

/// `POST /api/v1/business/statements/{id}/release-review`: the staff
/// release-review — `extendGrace` restores `past_due → issued` (re-opens the
/// grace window), `void` atomically reverses its proven journal and statement.
/// Other past-grace debt retains dunning. A `finance_audit` row records actor,
/// reason and financial entry. `FundsWrite`; an exact replay is inert.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReleaseReviewBody {
    idempotency_key: String,
    /// `extendGrace` or `void`.
    action: String,
    reason: String,
}

pub(crate) async fn release_review(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(statement_id): Path<Uuid>,
    ApiJson(r): ApiJson<ReleaseReviewBody>,
) -> ApiResult<PlanStatementDetail> {
    ok(state
        .db
        .release_statement_review(
            &super::business::personal_token(&headers)?,
            statement_id,
            &r.action,
            &r.reason,
            &r.idempotency_key,
        )
        .await?)
}

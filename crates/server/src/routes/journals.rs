//! Plan fee journals (issue #611, T20): the per-period invoices the plan
//! lifecycle writes. The customer reads its own journals and their lines
//! (`/api/v1/org/plan/journals`); business staff reads cross-organization
//! (`/api/v1/business/journals`, `ReportsRead`) and voids under `FundsWrite`
//! — a correction that appends reversing lines and the mirroring ledger entry,
//! never an edit or a delete.
use super::business::personal_token;
use super::{ApiResult, ok};
use crate::AppState;
use crate::error::{ApiError, ApiJson, ApiQuery};
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use oxsum_core::{InvoiceJournalDetail, InvoiceJournalPage, Principal};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /api/v1/org/plan/journals`: the caller's own journals, newest first —
/// the billing read the plan surface carries.
pub(crate) async fn my_journals(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<InvoiceJournalPage> {
    ok(state
        .db
        .plan_journals(
            principal.organization().id,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/org/plan/journals/{id}`: one journal plus its lines, scoped
/// to the owning organization.
pub(crate) async fn my_journal(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(journal_id): Path<Uuid>,
) -> ApiResult<InvoiceJournalDetail> {
    let journal = state
        .db
        .plan_journal(principal.organization().id, journal_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ok(journal)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BusinessJournalPage {
    /// Narrow to one lifecycle state — `posted` is the dunning work list.
    state: Option<String>,
    /// Scope to one customer binding.
    customer_plan_id: Option<Uuid>,
    /// Scope to one period of a binding.
    period_index: Option<i32>,
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /api/v1/business/journals`: the staff cross-organization journal
/// list — `ReportsRead`, a personal session, never a machine credential.
pub(crate) async fn business_journals(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<BusinessJournalPage>,
) -> ApiResult<InvoiceJournalPage> {
    ok(state
        .db
        .business_journals(
            &personal_token(&headers)?,
            page.state.as_deref(),
            page.customer_plan_id,
            page.period_index,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/business/journals/{id}`: one journal plus its lines.
pub(crate) async fn business_journal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(journal_id): Path<Uuid>,
) -> ApiResult<InvoiceJournalDetail> {
    let journal = state
        .db
        .business_journal(&personal_token(&headers)?, journal_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ok(journal)
}

/// `POST /api/v1/business/journals/{id}/void`: cancel the journal — reversing
/// lines append so the bill nets to zero, the mirroring entry posts, and a
/// `finance_audit` row records the reason. `FundsWrite`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct VoidJournalBody {
    idempotency_key: String,
    reason: String,
}

pub(crate) async fn void_journal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(journal_id): Path<Uuid>,
    ApiJson(r): ApiJson<VoidJournalBody>,
) -> ApiResult<InvoiceJournalDetail> {
    let token = personal_token(&headers)?;
    // The reversal posts to the journal organization's ledger; resolve the
    // wallet from the journal's own row before the staff check runs — the
    // same order the dispute resolve uses.
    let tenant: Option<String> = sqlx::query_scalar(
        "SELECT o.tenant_id FROM oxsum.invoice_journals j \
         JOIN oxsum.organizations o ON o.organization_id=j.organization_id \
         WHERE j.journal_id=$1",
    )
    .bind(journal_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let Some(tenant) = tenant else {
        return Err(ApiError::not_found());
    };
    let wallet = state.tenants.get(&tenant).await.map_err(ApiError::from)?;
    ok(state
        .db
        .void_invoice_journal(
            &token,
            &wallet,
            journal_id,
            &r.reason,
            &r.idempotency_key,
            crate::today(),
        )
        .await?)
}

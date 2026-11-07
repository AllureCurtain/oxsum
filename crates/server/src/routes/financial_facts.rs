//! Financial facts, cost coverage and reconciliation (issue #613, T22).
//!
//! The staff read model: `GET /api/v1/business/financial-facts` answers the
//! eight-category figures (`ReportsRead`), `GET …/cost-coverage` the
//! `cost_basis`/provider/day breakdown (`ReportsRead`), and
//! `GET …/reconciliation-runs` the run history plus its exceptions
//! (`ReportsRead`). A manual run and the exception close/repair are finance
//! actions under `FundsWrite`, each audited in `finance_audit`.
//!
//! The customer's own projection is `GET /api/v1/org/usage-report` — the same
//! `financial_facts` read scoped to the caller's organization, so the
//! customer's total and the business's sum for that organization are one
//! figure by construction, never two totals that could disagree.

use super::{ApiResult, ok, parse_day};
use crate::AppState;
use crate::error::{ApiError, ApiJson, ApiQuery};
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use oxsum_core::{
    CostCoverage, FinancialFacts, Principal, ReconciliationException, ReconciliationRepair,
    ReconciliationRun, TimeBasis,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FactsQuery {
    organization_id: Option<Uuid>,
    model: Option<String>,
    channel: Option<String>,
    from: Option<String>,
    to: Option<String>,
    time_basis: Option<String>,
}

fn facts_query(
    q: &FactsQuery,
) -> Result<(Option<time::Date>, Option<time::Date>, TimeBasis), ApiError> {
    let from = q.from.as_deref().map(parse_day).transpose()?;
    let to = q.to.as_deref().map(parse_day).transpose()?;
    let basis = q
        .time_basis
        .as_deref()
        .map(TimeBasis::parse)
        .transpose()
        .map_err(|e| ApiError::Validation(e.to_string()))?
        .unwrap_or_default();
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(ApiError::Validation("`from` must not be after `to`".into()));
    }
    Ok((from, to, basis))
}

/// `GET /api/v1/business/financial-facts`: the eight-category figures —
/// `ReportsRead`, a personal session, never a machine credential.
pub(crate) async fn business_financial_facts(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(q): ApiQuery<FactsQuery>,
) -> ApiResult<FinancialFacts> {
    let (from, to, basis) = facts_query(&q)?;
    ok(state
        .db
        .business_financial_facts(
            &super::business::personal_token(&headers)?,
            &oxsum_core::FactFilter {
                organization_id: q.organization_id,
                model: q.model,
                channel: q.channel,
                from,
                to,
                time_basis: basis,
            },
        )
        .await?)
}

/// `GET /api/v1/org/usage-report`: the caller's own eight-category projection
/// — the same `financial_facts` read scoped to the organization, so the
/// customer's total reconciles to the business sum by construction.
pub(crate) async fn org_usage_report(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(q): ApiQuery<FactsQuery>,
) -> ApiResult<FinancialFacts> {
    let (from, to, basis) = facts_query(&q)?;
    ok(state
        .db
        .org_usage_report(principal.organization().id, from, to, basis)
        .await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoverageQuery {
    organization_id: Option<Uuid>,
    from: Option<String>,
    to: Option<String>,
}

/// `GET /api/v1/business/cost-coverage`: the `cost_basis` breakdown and the
/// provider/day detail the upstream-bill reconciliation reads — `ReportsRead`.
pub(crate) async fn business_cost_coverage(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(q): ApiQuery<CoverageQuery>,
) -> ApiResult<CostCoverage> {
    let from = q.from.as_deref().map(parse_day).transpose()?;
    let to = q.to.as_deref().map(parse_day).transpose()?;
    ok(state
        .db
        .business_cost_coverage(
            &super::business::personal_token(&headers)?,
            q.organization_id,
            from,
            to,
        )
        .await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunPage {
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /api/v1/business/reconciliation-runs`: the run history — `ReportsRead`.
pub(crate) async fn business_runs(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<RunPage>,
) -> ApiResult<Vec<ReconciliationRun>> {
    ok(state
        .db
        .reconciliation_runs(
            &super::business::personal_token(&headers)?,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/business/reconciliation-runs/{id}`: one run.
pub(crate) async fn business_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<Uuid>,
) -> ApiResult<ReconciliationRun> {
    let run = state
        .db
        .reconciliation_run(&super::business::personal_token(&headers)?, run_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ok(run)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExceptionQuery {
    /// `true` lists only still-open exceptions — the work list.
    open: Option<bool>,
}

/// `GET /api/v1/business/reconciliation-runs/{id}/exceptions`: a run's
/// exceptions — `ReportsRead`.
pub(crate) async fn business_run_exceptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(run_id): Path<Uuid>,
    ApiQuery(q): ApiQuery<ExceptionQuery>,
) -> ApiResult<Vec<ReconciliationException>> {
    ok(state
        .db
        .reconciliation_exceptions(
            &super::business::personal_token(&headers)?,
            Some(run_id),
            q.open.unwrap_or(false),
        )
        .await?)
}

/// `GET /api/v1/business/reconciliation-exceptions`: every open exception —
/// the work list the console drains. `ReportsRead`.
pub(crate) async fn business_exceptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(q): ApiQuery<ExceptionQuery>,
) -> ApiResult<Vec<ReconciliationException>> {
    ok(state
        .db
        .reconciliation_exceptions(
            &super::business::personal_token(&headers)?,
            None,
            q.open.unwrap_or(true),
        )
        .await?)
}

/// `POST /api/v1/business/reconciliation-runs`: a staff-requested manual run —
/// `FundsWrite` + finance_audit.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RunBody {
    idempotency_key: String,
    period_start: String,
    period_end: String,
}

pub(crate) async fn request_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(r): ApiJson<RunBody>,
) -> ApiResult<ReconciliationRun> {
    let start = parse_day(&r.period_start)?;
    let end = parse_day(&r.period_end)?;
    ok(state
        .db
        .request_reconciliation_run(
            &super::business::personal_token(&headers)?,
            start,
            end,
            &r.idempotency_key,
        )
        .await?)
}

/// `POST /api/v1/business/reconciliation-exceptions/{id}/resolve` and
/// `…/waive`: the explicit close — `resolve` names the corrective outcome,
/// `waive` why the drift is not a defect. `FundsWrite` + finance_audit.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CloseBody {
    idempotency_key: String,
    resolution: String,
}

pub(crate) async fn resolve_exception(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(exception_id): Path<Uuid>,
    ApiJson(r): ApiJson<CloseBody>,
) -> ApiResult<ReconciliationException> {
    ok(state
        .db
        .close_reconciliation_exception(
            &super::business::personal_token(&headers)?,
            exception_id,
            "resolve",
            &r.resolution,
            &r.idempotency_key,
        )
        .await?)
}

pub(crate) async fn waive_exception(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(exception_id): Path<Uuid>,
    ApiJson(r): ApiJson<CloseBody>,
) -> ApiResult<ReconciliationException> {
    ok(state
        .db
        .close_reconciliation_exception(
            &super::business::personal_token(&headers)?,
            exception_id,
            "waive",
            &r.resolution,
            &r.idempotency_key,
        )
        .await?)
}

/// `POST /api/v1/business/reconciliation-exceptions/{id}/repairs`: the
/// corrective action — `FundsWrite` + finance_audit + a repairs row.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RepairBody {
    idempotency_key: String,
    /// `reclassifyCost` | `adjustJournal` | `linkPayment` | `markDuplicate`.
    action: String,
    reason: String,
    /// The action's arguments — `reclassifyCost` takes `costBasis` and an
    /// optional `upstreamCostMinor`; the link/adjust actions carry their
    /// target's id.
    #[serde(default)]
    patch: serde_json::Value,
}

fn snake(action: &str) -> &str {
    match action {
        "reclassifyCost" => "reclassify_cost",
        "adjustJournal" => "adjust_journal",
        "linkPayment" => "link_payment",
        "markDuplicate" => "mark_duplicate",
        other => other,
    }
}

pub(crate) async fn repair_exception(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(exception_id): Path<Uuid>,
    ApiJson(r): ApiJson<RepairBody>,
) -> ApiResult<ReconciliationRepair> {
    let action = snake(&r.action).to_owned();
    ok(state
        .db
        .repair_reconciliation_exception(
            &super::business::personal_token(&headers)?,
            exception_id,
            &action,
            &r.reason,
            r.patch,
            &r.idempotency_key,
        )
        .await?)
}

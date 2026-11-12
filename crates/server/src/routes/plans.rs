//! Sold plans and the customer plan lifecycle (issue #610, T19).
//!
//! Two surfaces. The customer reads the published catalogue
//! (`GET /api/v1/plans`, `GET /api/v1/plans/{id}`), its own live plan
//! (`GET /api/v1/org/plan`), and acts on it (`cancel`, `change`). The business
//! staff manages the catalogue (`/api/v1/business/plans`) and a customer's
//! binding (`/api/v1/business/customers/{id}/plan`), each behind its
//! capability — a personal session token, never an organization's key.
use super::{ApiResult, ok};
use crate::AppState;
use crate::error::{ApiError, ApiJson};
use axum::Extension;
use axum::extract::{Path, State};
use oxsum_core::{
    BusinessCapability, CustomerPlan, NewSoldPlan, PeriodAuthorization, PeriodKind, PlanPeriod,
    Principal, SoldPlan,
};
use serde::Deserialize;
use uuid::Uuid;

// ---- customer surface -------------------------------------------------------

/// Customer-scoped, authoritative fee and period quote; no provider I/O.
pub(crate) async fn payment_quote(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<oxsum_core::PlanPaymentQuote> {
    ok(state
        .db
        .plan_payment_quote(principal.organization().id)
        .await?)
}

/// `GET /api/v1/plans`: the published catalogue — every version still on sale
/// across the business's plans. A customer reads this to choose a bind.
pub(crate) async fn list_plans(State(state): State<AppState>) -> ApiResult<Vec<SoldPlan>> {
    // The catalogue is deployment-wide (one business owns the catalogue), so
    // the read resolves the singleton rather than a caller-scoped business.
    let business = state.db.default_business().await?;
    ok(state
        .db
        .sold_plans(business)
        .await?
        .into_iter()
        .filter(|p| p.published_at.is_some() && p.retired_at.is_none())
        .collect())
}

/// `GET /api/v1/plans/{id}`: one sold version, for the bind form's terms.
pub(crate) async fn get_plan(
    State(state): State<AppState>,
    Path(plan_version_id): Path<Uuid>,
) -> ApiResult<SoldPlan> {
    let plan = state
        .db
        .sold_plan(plan_version_id)
        .await?
        .filter(|p| p.published_at.is_some() && p.retired_at.is_none())
        .ok_or_else(ApiError::not_found)?;
    ok(plan)
}

/// `GET /api/v1/org/plan`: the caller's own live plan and its period history —
/// the entitlement surface the dashboard reads.
pub(crate) async fn my_plan(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<serde_json::Value> {
    let organization = principal.organization();
    let plan = state.db.customer_plan(organization.id).await?;
    let periods = match &plan {
        Some(p) => state.db.plan_periods(p.customer_plan_id).await?,
        None => Vec::new(),
    };
    // The credit standing beside the binding (issue #612, T21): the cap, what
    // the open statements still owe plus current holds, and the nearest due
    // date — the customer's own credit read next to its plan.
    let credit = state.db.plan_credit_standing(organization.id).await?;
    ok(serde_json::json!({ "plan": plan, "periods": periods, "credit": credit }))
}

/// `POST /api/v1/org/plan/cancel`: mark `cancelAtPeriodEnd` — the paid period
/// serves to its end but does not renew. Explicit, reversible by a new bind.
pub(crate) async fn cancel_my_plan(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<CustomerPlan> {
    ok(state
        .db
        .cancel_customer_plan(principal.organization().id)
        .await?)
}

/// `POST /api/v1/org/plan/change`: book a swap to another sold version at the
/// next boundary — authorized there, never pro-rated mid-period.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ChangePlanReq {
    plan_version_id: Uuid,
}
pub(crate) async fn change_my_plan(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<ChangePlanReq>,
) -> ApiResult<CustomerPlan> {
    ok(state
        .db
        .schedule_customer_plan_change(principal.organization().id, r.plan_version_id)
        .await?)
}

// ---- business staff surface -------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PublishPlanReq {
    /// `planId` of an existing plan publishes its next version; absent mints a
    /// new plan at version 1.
    plan_id: Option<Uuid>,
    name: String,
    price_minor: i64,
    currency: String,
    quota_minor: Option<i64>,
    period_kind: PeriodKind,
    overage_enabled: Option<bool>,
    sort_order: Option<i32>,
}

/// `GET /api/v1/business/plans`: the whole catalogue, drafts and retired
/// included — the staff list.
pub(crate) async fn staff_plans(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
) -> ApiResult<Vec<SoldPlan>> {
    let principal = state
        .db
        .business_capability(&token, BusinessCapability::CatalogWrite)
        .await?;
    ok(state.db.sold_plans(principal.business_id).await?)
}

/// `POST /api/v1/business/plans`: publish a version — new plan or the next
/// version of one the `planId` names.
pub(crate) async fn publish_plan(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    ApiJson(r): ApiJson<PublishPlanReq>,
) -> ApiResult<SoldPlan> {
    let principal = state
        .db
        .business_capability(&token, BusinessCapability::CatalogWrite)
        .await?;
    let new = NewSoldPlan {
        plan_id: r.plan_id,
        name: r.name,
        price_minor: r.price_minor,
        currency: r.currency,
        quota_minor: r.quota_minor,
        period_kind: r.period_kind,
        overage_enabled: r.overage_enabled.unwrap_or(false),
        sort_order: r.sort_order.unwrap_or(0),
    };
    ok(state
        .db
        .publish_sold_plan(principal.business_id, &new)
        .await?)
}

/// `POST /api/v1/business/plans/{id}/retire`: stop selling a version.
pub(crate) async fn retire_plan(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(plan_version_id): Path<Uuid>,
) -> ApiResult<serde_json::Value> {
    let _principal = state
        .db
        .business_capability(&token, BusinessCapability::CatalogWrite)
        .await?;
    let retired = state.db.retire_sold_plan(plan_version_id).await?;
    if !retired {
        return Err(ApiError::not_found());
    }
    ok(serde_json::json!({ "retired": true }))
}

/// `GET /api/v1/business/customers/{id}/plan`: the customer's live plan plus
/// its authorized periods — the staff's entitlement read.
pub(crate) async fn customer_plan(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(organization_id): Path<Uuid>,
) -> ApiResult<serde_json::Value> {
    let _principal = state
        .db
        .business_capability(&token, BusinessCapability::CustomersRead)
        .await?;
    let plan = state.db.customer_plan(organization_id).await?;
    let periods: Vec<PlanPeriod> = match &plan {
        Some(p) => state.db.plan_periods(p.customer_plan_id).await?,
        None => Vec::new(),
    };
    ok(serde_json::json!({ "plan": plan, "periods": periods }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BindPlanReq {
    plan_version_id: Uuid,
    /// A free plan binds on an explicit complimentary grant; the grant key the
    /// caller supplies becomes period 0's `authorization_ref`. A priced plan
    /// must be absent — it binds `pending_payment` and activates on the order
    /// its caller then mints, never on a fabricated grant.
    complimentary_grant: Option<Uuid>,
}

/// `PUT /api/v1/business/customers/{id}/plan`: bind the customer to a sold
/// version — `pending_payment` for a priced plan, active at once on a grant
/// for a free one.
pub(crate) async fn bind_customer_plan(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(organization_id): Path<Uuid>,
    ApiJson(r): ApiJson<BindPlanReq>,
) -> ApiResult<CustomerPlan> {
    let _principal = state
        .db
        .business_capability(&token, BusinessCapability::CustomersWrite)
        .await?;
    let auth = match r.complimentary_grant {
        Some(grant) => PeriodAuthorization::Complimentary(grant),
        None => PeriodAuthorization::PaymentOrder(Uuid::nil()),
    };
    ok(state
        .db
        .bind_customer_plan(organization_id, r.plan_version_id, auth)
        .await?)
}

/// `POST /api/v1/business/customers/{id}/plan/cancel`: the staff-side
/// boundary-cancel — same semantics as the customer's own cancel.
pub(crate) async fn cancel_customer_plan(
    State(state): State<AppState>,
    axum::Extension(token): axum::Extension<crate::auth::PersonalToken>,
    Path(organization_id): Path<Uuid>,
) -> ApiResult<CustomerPlan> {
    let _principal = state
        .db
        .business_capability(&token, BusinessCapability::CustomersWrite)
        .await?;
    ok(state.db.cancel_customer_plan(organization_id).await?)
}

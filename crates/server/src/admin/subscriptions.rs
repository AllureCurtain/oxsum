//! `/api/v1/admin`: subscription quota plans and the binds that place
//! organizations on them (issue #496).
//!
//! A plan is the catalogue the operator owns — `PUT` by name is naturally
//! idempotent, replaying the same body writes the same row. A bind is the
//! organization's link: one active subscription per organization, and moving
//! between plans closes the old period as `upgrade` or `downgrade` under the
//! plan's `sort_order` before the new one opens.

use axum::extract::{Path, State};
use oxsum_core::{ResetPeriod, audit_action};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};
use crate::{AppState, admin::record};

/// The body of `PUT …/subscription-plans/{name}`: the whole plan — the write
/// replaces, so an absent optional field clears its old value.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SetPlanReq {
    /// Quota per period in credit minor units; `null` or absent for unmetered.
    #[serde(default, deserialize_with = "crate::admin::double_option")]
    quota_minor: Option<Option<i64>>,
    /// The cadence `consumed_minor` resets on.
    reset_period: ResetPeriod,
    /// Whether an exhausted plan may fall through to the wallet. Snapshotted
    /// onto the subscription at bind time.
    #[serde(default = "default_overflow")]
    allow_wallet_overflow: bool,
    /// What the plan sells for, minor units — the admin surface's answer to
    /// "what does this plan cost".
    #[serde(default)]
    price_minor: Option<i64>,
    /// The Stripe `price_…` a checkout names, when the plan sells there.
    #[serde(default)]
    stripe_price_id: Option<String>,
    /// Whether new subscriptions may bind. `false` keeps the history but
    /// refuses the plan to new binds.
    #[serde(default = "default_enabled")]
    enabled: bool,
    /// The bind direction's ordering: a move to a higher sort_order closes
    /// the old period `upgrade`, lower closes `downgrade`.
    #[serde(default)]
    sort_order: i32,
}

fn default_overflow() -> bool {
    true
}

fn default_enabled() -> bool {
    true
}

/// Every plan in the catalogue.
pub(crate) async fn plans(
    State(state): State<AppState>,
) -> ApiResult<Vec<oxsum_core::SubscriptionPlan>> {
    ok(state.db.subscription_plans().await?)
}

/// Creates or replaces a plan. A `PUT` by name is naturally idempotent: the
/// request carries no idempotency key because replaying the same body writes
/// the same row.
pub(crate) async fn set_plan(
    State(state): State<AppState>,
    Path(name): Path<String>,
    ApiJson(request): ApiJson<SetPlanReq>,
) -> ApiResult<oxsum_core::SubscriptionPlan> {
    let plan = state
        .db
        .set_subscription_plan(
            &name,
            request.quota_minor.flatten(),
            request.reset_period,
            request.allow_wallet_overflow,
            request.price_minor,
            request.stripe_price_id.as_deref(),
            request.enabled,
            request.sort_order,
        )
        .await?;
    record(
        &state,
        audit_action::SUBSCRIPTION_PLAN_SET,
        Some(name),
        serde_json::to_value(&plan).map_err(|_| ApiError::Internal)?,
        None,
    )
    .await?;
    ok(plan)
}

/// The body of `PUT …/organizations/{organizationId}/subscription`: which
/// plan, and which rail's reference when it was bought on one.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BindReq {
    plan_id: Uuid,
    /// What opened the link: `admin` on this surface; a rail name for a
    /// paid subscription the rail's own flow bound.
    #[serde(default = "default_source")]
    source: String,
    /// The Stripe subscription id the webhook renews against, when bound
    /// on that rail.
    #[serde(default)]
    stripe_subscription_id: Option<String>,
}

fn default_source() -> String {
    "admin".to_owned()
}

/// Binds the organization to a plan, or moves it between plans. A replayed
/// bind to the same plan answers the standing subscription; a different plan
/// closes the old period as `upgrade` or `downgrade` and opens a new one.
pub(crate) async fn bind(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    ApiJson(request): ApiJson<BindReq>,
) -> ApiResult<oxsum_core::OrgSubscription> {
    let subscription = state
        .db
        .bind_subscription(
            organization_id,
            request.plan_id,
            &request.source,
            request.stripe_subscription_id.as_deref(),
        )
        .await?;
    record(
        &state,
        audit_action::SUBSCRIPTION_BIND,
        Some(subscription.subscription_id.to_string()),
        serde_json::json!({
            "organizationId": organization_id,
            "planId": request.plan_id,
            "source": request.source,
            "stripeSubscriptionId": request.stripe_subscription_id,
        }),
        None,
    )
    .await?;
    ok(subscription)
}

/// Ends the organization's active subscription: the period closes as
/// `cancelled`, the draws it opened stay settleable, and the wallet never
/// sees a subscription share again.
pub(crate) async fn cancel(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
) -> ApiResult<serde_json::Value> {
    let ended = state.db.cancel_subscription(organization_id).await?;
    if !ended {
        return Err(ApiError::not_found());
    }
    record(
        &state,
        audit_action::SUBSCRIPTION_CANCEL,
        Some(organization_id.to_string()),
        serde_json::json!({}),
        None,
    )
    .await?;
    ok(serde_json::json!({}))
}

//! `/api/v1/admin`: tiers, discounts and redemption codes — the billing
//! knobs that shape what an organization pays.

use axum::extract::{Path, State};
use oxsum_core::{Discount, NewDiscount, TierProfile, audit_action};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};
use crate::{AppState, admin::record};

/// The body of `PUT …/tiers/{tier}`: the whole capability package — the write
/// replaces, so an absent field clears its old value rather than keeping it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SetTierReq {
    requests_per_minute: Option<i32>,
    model_allowlist: Option<Vec<String>>,
}

/// Every tier profile, with the count of organizations assigned to it.
pub(crate) async fn tiers(State(state): State<AppState>) -> ApiResult<Vec<TierProfile>> {
    ok(state.db.tiers().await?)
}

/// Creates or replaces a tier profile. A `PUT` by name is naturally
/// idempotent: replaying the same body writes the same package, so the
/// request carries no idempotency key.
pub(crate) async fn set_tier(
    State(state): State<AppState>,
    Path(tier): Path<String>,
    ApiJson(request): ApiJson<SetTierReq>,
) -> ApiResult<TierProfile> {
    let profile = state
        .db
        .set_tier(&tier, request.requests_per_minute, request.model_allowlist)
        .await?;
    record(
        &state,
        audit_action::TIER_SET,
        Some(tier),
        serde_json::json!({
            "requestsPerMinute": profile.requests_per_minute,
            "modelAllowlist": profile.model_allowlist,
        }),
        None,
    )
    .await?;
    ok(profile)
}

/// Retires a tier profile. A tier organizations are still assigned to refuses —
/// a deleted package never silently uncaps its members.
pub(crate) async fn delete_tier(
    State(state): State<AppState>,
    Path(tier): Path<String>,
) -> ApiResult<serde_json::Value> {
    state
        .db
        .delete_tier(&tier)
        .await?
        .ok_or_else(ApiError::not_found)?;
    record(
        &state,
        audit_action::TIER_DELETE,
        Some(tier),
        serde_json::json!({}),
        None,
    )
    .await?;
    ok(serde_json::json!({}))
}

/// The body of `POST …/discounts`: scope, percent, window, label — and the
/// idempotency key that makes a retry the same row.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateDiscountReq {
    percent: i32,
    organization_id: Option<Uuid>,
    model: Option<String>,
    label: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    valid_from: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    valid_until: Option<OffsetDateTime>,
    idempotency_key: String,
}

/// Every discount, newest first, ended or not — the history a settled bill's
/// `discountPercent` points back at.
pub(crate) async fn discounts(State(state): State<AppState>) -> ApiResult<Vec<Discount>> {
    ok(state.db.discounts().await?)
}

/// Creates a discount: the percent off the priced sum that matching turns
/// settle at, snapshotted into the settlement description when it applies.
/// The idempotency key's replay answers the row it created; under different
/// fields it is a conflict.
pub(crate) async fn create_discount(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CreateDiscountReq>,
) -> ApiResult<Discount> {
    let discount = state
        .db
        .create_discount(
            &request.idempotency_key,
            &NewDiscount {
                percent: request.percent,
                organization_id: request.organization_id,
                model: request.model,
                label: request.label.clone(),
                valid_from: request.valid_from,
                valid_until: request.valid_until,
            },
        )
        .await?;
    record(
        &state,
        audit_action::DISCOUNT_CREATE,
        request.organization_id.map(|id| id.to_string()),
        serde_json::to_value(&discount).map_err(|_| ApiError::Internal)?,
        Some(&request.idempotency_key),
    )
    .await?;
    ok(discount)
}

/// Ends a discount early: turns starting after the call no longer qualify, and
/// the row stays in the list — the history a settled bill cites.
pub(crate) async fn end_discount(
    State(state): State<AppState>,
    Path(discount_id): Path<Uuid>,
) -> ApiResult<Discount> {
    match state.db.end_discount(discount_id).await? {
        Some(discount) => {
            record(
                &state,
                audit_action::DISCOUNT_END,
                Some(discount_id.to_string()),
                serde_json::json!({}),
                None,
            )
            .await?;
            ok(discount)
        }
        None => Err(ApiError::not_found()),
    }
}

/// The body of `POST …/redemption-codes`: how many codes, worth how much, and
/// optionally when the batch stops redeeming.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MintCodesReq {
    count: i64,
    amount_minor: i64,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

/// Mints a batch of redemption codes: the codes themselves are answered once,
/// here, and the database keeps only their hashes — whoever holds a code can
/// redeem it through `POST /api/v1/redemptions`.
pub(crate) async fn mint_codes(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<MintCodesReq>,
) -> ApiResult<oxsum_core::CodeBatch> {
    let batch = state
        .db
        .mint_codes(request.count, request.amount_minor, request.expires_at)
        .await?;
    // The codes themselves are secrets held by their recipients — the audit row
    // records the batch, never what was in it.
    record(
        &state,
        audit_action::CODES_MINT,
        Some(batch.batch_id.to_string()),
        serde_json::json!({
            "count": batch.count,
            "amountMinor": batch.amount_minor,
            "expiresAt": batch.expires_at,
        }),
        None,
    )
    .await?;
    ok(batch)
}

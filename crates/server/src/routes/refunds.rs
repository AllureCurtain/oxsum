//! Corrections, partial refunds and disputes on payment orders (issue #608).
//!
//! The customer asks for review — it can never grant itself money; staff
//! resolves under `RefundsWrite`. The durable `provider_pending` reservation
//! commits before any provider call, and the signed `refund_*`/`dispute_*`
//! webhook is the only source of terminal truth: this surface can never mark
//! a refund succeeded.
use super::{ApiResult, business::personal_token, ok};
use crate::{
    AppState,
    error::{ApiError, ApiJson, ApiQuery},
};
use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
};
use oxsum_core::{
    DisputeResolution, NewRefundRequest, OrderCreditNotePage, OrderDispute, OrderDisputePage,
    OrderRefund, OrderRefundPage, RefundDecision,
};
use serde::Deserialize;
use uuid::Uuid;

/// `POST /api/v1/orders/{orderId}/refund-request`: the amount asked back in
/// credit-minor — `None` names the whole refundable remainder — the reason,
/// and the idempotency key a replay answers the standing refund under.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RefundRequestBody {
    idempotency_key: String,
    amount_minor: Option<i64>,
    reason: String,
}

pub(crate) async fn request_refund(
    State(state): State<AppState>,
    Extension(principal): Extension<oxsum_core::Principal>,
    Path(order_id): Path<Uuid>,
    ApiJson(r): ApiJson<RefundRequestBody>,
) -> ApiResult<OrderRefund> {
    let organization = principal.organization();
    let refund = state
        .db
        .request_order_refund(
            organization.id,
            order_id,
            &NewRefundRequest {
                amount_minor: r.amount_minor,
                reason: r.reason,
                request_key: r.idempotency_key,
                requested_by: principal.user_id(),
            },
        )
        .await?;
    ok(refund)
}

/// `GET /api/v1/orders/{orderId}/refunds`: the caller's refunds on one order
/// plus the refundable remainder it may still ask back.
pub(crate) async fn order_refunds(
    State(state): State<AppState>,
    Extension(principal): Extension<oxsum_core::Principal>,
    Path(order_id): Path<Uuid>,
) -> ApiResult<serde_json::Value> {
    let organization = principal.organization();
    let refunds = state.db.order_refunds(organization.id, order_id).await?;
    let refundable = state
        .db
        .order_refundable_minor(organization.id, order_id)
        .await?;
    ok(serde_json::json!({
        "refunds": refunds,
        "refundableMinor": refundable,
    }))
}

#[derive(Deserialize)]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /api/v1/business/refunds`: the staff finance list, newest reads page
/// by cursor — a `ReportsRead` action.
pub(crate) async fn business_refunds(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<OrderRefundPage> {
    ok(state
        .db
        .business_refunds(
            &personal_token(&headers)?,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/business/refunds/{refundId}`: one refund the business owns.
pub(crate) async fn business_refund(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(refund_id): Path<Uuid>,
) -> ApiResult<OrderRefund> {
    let refund = state
        .db
        .business_refund(&personal_token(&headers)?, refund_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ok(refund)
}

/// `POST /api/v1/business/refunds/{refundId}/resolve`: approve or decline a
/// requested refund — `RefundsWrite`. Approve commits the durable reservation
/// and the credit note, then issues the PSP call; the answer is the
/// `provider_pending` refund, never a promise the provider refunded.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResolveBody {
    idempotency_key: String,
    decision: RefundDecision,
    reason: Option<String>,
}

pub(crate) async fn resolve_refund(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(refund_id): Path<Uuid>,
    ApiJson(r): ApiJson<ResolveBody>,
) -> ApiResult<OrderRefund> {
    let (refund, needs_provider_call) = state
        .db
        .resolve_order_refund(
            &personal_token(&headers)?,
            refund_id,
            r.decision,
            r.reason.as_deref(),
            &r.idempotency_key,
        )
        .await?;
    if needs_provider_call {
        issue_provider_refund(&state, &refund).await?;
    }
    ok(refund)
}

/// The PSP half of an approval: the reservation already commits, so the call
/// issues against the order's provider payment — the refund id is the
/// provider's idempotency key. A transport failure leaves the row
/// `provider_pending` for the retry pass; a definite refusal releases the
/// reservation.
async fn issue_provider_refund(state: &AppState, refund: &OrderRefund) -> Result<(), ApiError> {
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    let Some(target) = state.db.refund_retry_target(refund.refund_id).await? else {
        return Ok(());
    };
    let Some(context) = state
        .db
        .merchant_signing_context(target.merchant_id, target.merchant_version_id, secret)
        .await?
    else {
        state
            .db
            .fail_order_refund(refund.refund_id, "merchant is gone")
            .await?;
        return Ok(());
    };
    let Some(provider_payment) = target.provider_payment_id.as_deref() else {
        state
            .db
            .fail_order_refund(refund.refund_id, "no provider payment bound")
            .await?;
        return Ok(());
    };
    match oxsum_integrations::merchant_checkout::create_refund(
        &context,
        refund.refund_id,
        provider_payment,
        refund.display_amount_minor,
        &refund.display_currency,
        &refund.reason,
        &state.provider_http,
    )
    .await
    {
        Ok(handle) => {
            state
                .db
                .note_refund_provider(refund.refund_id, &handle.provider_refund_id)
                .await?;
            // The reservation stays pending on the signed event; the retry
            // pass's next attempt is the liveness probe a missing webhook gets.
            state.db.retry_order_refund(refund.refund_id).await?;
        }
        Err(_) => {
            state
                .db
                .fail_order_refund(refund.refund_id, "provider refund call failed")
                .await?;
        }
    }
    Ok(())
}

/// `GET /api/v1/business/credit-notes`: the order-linked corrections the
/// refund flow issued — finance read.
pub(crate) async fn business_credit_notes(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<OrderCreditNotePage> {
    ok(state
        .db
        .business_order_credit_notes(
            &personal_token(&headers)?,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/business/disputes`: contested charges only — never refunds.
pub(crate) async fn business_disputes(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<OrderDisputePage> {
    ok(state
        .db
        .business_disputes(
            &personal_token(&headers)?,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/business/audit`: the business finance audit — one immutable
/// row per staff financial action, newest first. `ReportsRead`; `action`
/// narrows to one audited action. The staff console's audit page (T24).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditPage {
    after: Option<Uuid>,
    action: Option<String>,
    limit: Option<i64>,
}
pub(crate) async fn business_audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(page): ApiQuery<AuditPage>,
) -> ApiResult<oxsum_core::FinanceAuditPage> {
    ok(state
        .db
        .business_finance_audit(
            &personal_token(&headers)?,
            page.after,
            page.action.as_deref(),
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `POST /api/v1/business/disputes/{disputeId}/resolve`: `evidenceDue`,
/// `won`, `lost` or `closed`. `lost` re-charges the drawn receivable under
/// the dispute's own ledger key — the audit names the actor and reason.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DisputeResolveBody {
    idempotency_key: String,
    resolution: DisputeResolution,
    reason: Option<String>,
}

pub(crate) async fn resolve_dispute(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(dispute_id): Path<Uuid>,
    ApiJson(r): ApiJson<DisputeResolveBody>,
) -> ApiResult<OrderDispute> {
    // The re-charge posts to the dispute organization's ledger; resolve the
    // wallet from the dispute's own row before the staff check runs.
    let tenant: Option<String> = sqlx::query_scalar(
        "SELECT o.tenant_id FROM oxsum.disputes d \
         JOIN oxsum.organizations o ON o.organization_id=d.organization_id \
         WHERE d.dispute_id=$1",
    )
    .bind(dispute_id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|_| ApiError::Internal)?;
    let Some(tenant) = tenant else {
        return Err(ApiError::not_found());
    };
    let wallet = state.tenants.get(&tenant).await.map_err(ApiError::from)?;
    ok(state
        .db
        .resolve_order_dispute(
            &wallet,
            &personal_token(&headers)?,
            dispute_id,
            r.resolution,
            r.reason.as_deref(),
            &r.idempotency_key,
            crate::today(),
        )
        .await?)
}

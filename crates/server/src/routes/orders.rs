//! Payment orders (issue #606): create, read, list and fulfillment status for
//! the customer, plus the business reconciliation report.
//!
//! The customer creates an order in the display currency it was quoted; the
//! server rates the conversion to ledger credits at the rate it owns, pins the
//! terms, and drives the provider checkout — so a client-supplied rate or a
//! ledger amount is refused, and a replayed create returns the standing order
//! rather than a second charge. Paid is only ever established by the signed
//! webhook; this surface can never mark an order paid.
use super::{ApiResult, ok};
use crate::{
    AppState,
    error::{ApiError, ApiJson, ApiQuery},
};
use axum::{
    Extension,
    extract::{Path, State},
};
use oxsum_core::{
    MerchantRail, NewPaymentOrder, OrderRate, PaymentOrder, PaymentOrderPage, PaymentOrderPurpose,
    PaymentReconciliation, Principal,
};
use serde::Deserialize;
use uuid::Uuid;

/// `POST /api/v1/orders`: the order's quoted terms and the idempotency key that
/// anchors it. `amountMinor`/`rate` are deliberately absent — the server rates
/// the conversion, so a client-supplied figure is a schema rejection, never a
/// negotiated term.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CreateOrderReq {
    idempotency_key: String,
    rail: MerchantRail,
    purpose: PaymentOrderPurpose,
    /// The fiat minor amount the provider will charge.
    display_amount_minor: i64,
    display_currency: String,
    /// The purpose's argument: the plan a `planPeriod` activates, the
    /// receivable an `invoiceRepayment` settles.
    purpose_ref: Option<Uuid>,
    /// A `bundle` order's promo lot, in credit-minor; absent/zero elsewhere.
    promo_minor: Option<i64>,
}

/// The create response: the pinned order plus the provider checkout handles the
/// browser completes payment against — never a promise the order is paid.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrderCheckout {
    order: PaymentOrder,
    provider_payment_id: Option<String>,
    client_secret: Option<String>,
    checkout_url: Option<String>,
    publishable_key: Option<String>,
    sdk_url: Option<String>,
}

pub(crate) async fn create_order(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<CreateOrderReq>,
) -> ApiResult<OrderCheckout> {
    let organization = principal.organization();
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    // The order rates against the organization's effective display currency —
    // the server's own figure, resolved exactly as the bill renders it.
    let display = state.config.display_for(organization).ok_or_else(|| {
        ApiError::Validation("no display currency is configured to rate a payment order".into())
    })?;
    let rate = OrderRate {
        mantissa: display.rate_mantissa(),
        scale: display.rate_scale(),
    };
    let context = state
        .db
        .environment_merchant(r.rail, secret)
        .await?
        .ok_or_else(|| {
            ApiError::ServiceUnavailable("the requested payment rail is not configured".into())
        })?;
    let new = NewPaymentOrder {
        purpose: r.purpose,
        display_amount_minor: r.display_amount_minor,
        display_currency: r.display_currency.clone(),
        purpose_ref: r.purpose_ref,
        idempotency_key: r.idempotency_key.clone(),
        promo_minor: r.promo_minor.unwrap_or(0),
    };
    let order = state
        .db
        .create_payment_order(&context, organization.id, &new, &rate)
        .await?;
    // The checkout: bind the provider reference to the order's durable UUID
    // before answering, so a replayed create never issues a second charge. A
    // still-`created` order drives the PSP call; one already bound returns its
    // standing handles.
    let binding = state
        .db
        .merchant_payment_binding(order.order_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let description = order_description(&new);
    let (provider, client_secret, checkout_url) = if let Some(bound) =
        order.provider_payment_id.clone()
    {
        // Already bound by a prior create or an early signed callback: return
        // the stored checkout handles rather than issue a second charge — the
        // checkout a replayed create answers is the one the first minted.
        let handles = &order.checkout;
        (
            bound,
            handles["clientSecret"].as_str().map(str::to_owned),
            handles["checkoutUrl"].as_str().map(str::to_owned),
        )
    } else {
        let public = state.config.public_url().ok_or_else(|| {
            ApiError::ServiceUnavailable("public return URL is not configured".into())
        })?;
        let created = oxsum_integrations::merchant_checkout::create_order(
            &context,
            &binding,
            description,
            &state.provider_http,
            public,
        )
        .await
        .map_err(|_| {
            ApiError::ServiceUnavailable(
                "merchant checkout request did not complete; retry the same idempotency key".into(),
            )
        })?;
        let handles = serde_json::json!({
            "clientSecret": created.client_secret,
            "checkoutUrl": created.checkout_url,
        });
        state
            .db
            .note_order_provider(order.order_id, &created.provider_payment_id, &handles)
            .await?;
        (
            created.provider_payment_id,
            created.client_secret,
            created.checkout_url,
        )
    };
    // Re-read the order so the answer reflects the provider_pending move.
    let order = state
        .db
        .payment_order(organization.id, order.order_id)
        .await?
        .ok_or(ApiError::Internal)?;
    ok(OrderCheckout {
        order,
        provider_payment_id: Some(provider),
        client_secret,
        checkout_url,
        publishable_key: context.version.publishable_key.clone(),
        sdk_url: context.version.sdk_url.clone(),
    })
}

/// The description the provider's page shows for the order's purpose — what the
/// customer is paying for, named distinctly from the fulfillment the order grants.
fn order_description(new: &NewPaymentOrder) -> &'static str {
    match new.purpose {
        PaymentOrderPurpose::WalletTopup => "oxsum wallet top-up",
        PaymentOrderPurpose::Bundle => "oxsum credit bundle",
        PaymentOrderPurpose::PlanPeriod => "oxsum plan period",
        PaymentOrderPurpose::InvoiceRepayment => "oxsum invoice repayment",
    }
}

#[derive(Deserialize)]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /api/v1/orders`: the caller's own orders, newest reads page by cursor.
pub(crate) async fn list_orders(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiQuery(page): ApiQuery<Page>,
) -> ApiResult<PaymentOrderPage> {
    ok(state
        .db
        .organization_payment_orders(
            principal.organization().id,
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

/// `GET /api/v1/orders/{id}`: one order the caller's organization owns.
pub(crate) async fn get_order(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(order_id): Path<Uuid>,
) -> ApiResult<PaymentOrder> {
    let order = state
        .db
        .payment_order(principal.organization().id, order_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ok(order)
}

/// `GET /api/v1/orders/{id}/fulfillment`: the order's fulfillment status — the
/// order's lifecycle state plus the fulfillment record once it lands. `paid`
/// but unfulfilled is the honest mid-state the recovery sweep drains.
pub(crate) async fn order_fulfillment(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(order_id): Path<Uuid>,
) -> ApiResult<serde_json::Value> {
    let order = state
        .db
        .payment_order(principal.organization().id, order_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let fulfillment = state.db.payment_fulfillment(order_id).await?;
    ok(serde_json::json!({
        "orderId": order.order_id,
        "status": order.status,
        "paidAt": order
            .paid_at
            .and_then(|at| at.format(&time::format_description::well_known::Rfc3339).ok()),
        "fulfillment": fulfillment,
    }))
}

/// `GET /api/v1/business/payment-reconciliation`: the paid/fulfilled/
/// needs_attention reconciliation the operator reads — a business staff
/// `ReportsRead` action, so it takes the personal token, not a customer key.
pub(crate) async fn reconciliation(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<PaymentReconciliation> {
    ok(state
        .db
        .payment_reconciliation(&super::business::personal_token(&headers)?)
        .await?)
}

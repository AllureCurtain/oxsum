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
use crate::AppState;
use crate::error::{ApiError, ApiJson, ApiQuery};
use axum::Extension;
use axum::extract::{Path, State};
use oxsum_core::{
    MerchantRail, NewManualOrder, NewPaymentOrder, PaymentOrder, PaymentOrderPage,
    PaymentOrderPurpose, PaymentReconciliation, Principal,
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
    expected_plan_quote: Option<String>,
    expected_statement_quote: Option<String>,
    statement_amount_minor: Option<i64>,
    expected_collection_revision: Option<i64>,
    idempotency_key: String,
    rail: MerchantRail,
    expected_checkout: Option<oxsum_core::CheckoutChoice>,
    purpose: PaymentOrderPurpose,
    /// The fiat minor amount the provider will charge.
    display_amount_minor: i64,
    display_currency: String,
    /// The purpose's argument: the plan a `planPeriod` activates, the
    /// statement a `statementRepayment` settles, or a published bundle version.
    /// Legacy `invoiceRepayment` requires a reference but settles CreditLine.
    purpose_ref: Option<Uuid>,
    /// Deprecated bundle assertion; absent/zero or exactly the offer's amount.
    promo_minor: Option<i64>,
    /// Where the provider returns the customer's browser after payment. Absent,
    /// the order's own `/app/orders?order={id}` page is the return — the same
    /// page the checkout-session recovery drives back to.
    return_url: Option<String>,
    /// The rail's cancel destination, where it supports one (Stripe's
    /// `cancel_url`); ignored by rails with a single return.
    cancel_url: Option<String>,
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
    // Return/cancel overrides must be absolute http(s) URLs or site-absolute
    // paths — a bare scheme-relative or malformed value is a schema rejection,
    // never something the provider's page could bounce through.
    for url in [&r.return_url, &r.cancel_url].into_iter().flatten() {
        let ok = url.starts_with("https://")
            || url.starts_with("http://")
            || (url.starts_with('/') && !url.starts_with("//"));
        if !ok || url.len() > 2000 || url.chars().any(char::is_control) {
            return Err(ApiError::Validation(
                "returnUrl/cancelUrl must be absolute http(s) URLs or site paths".into(),
            ));
        }
    }
    let organization = principal.organization();
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    let new = NewPaymentOrder {
        expected_statement_quote: r.expected_statement_quote,
        statement_amount_minor: r.statement_amount_minor,
        expected_plan_quote: r.expected_plan_quote,
        expected_collection_revision: r.expected_collection_revision,
        purpose: r.purpose,
        display_amount_minor: r.display_amount_minor,
        display_currency: r.display_currency.clone(),
        purpose_ref: r.purpose_ref,
        idempotency_key: r.idempotency_key.clone(),
        promo_minor: r.promo_minor.unwrap_or(0),
    };
    let (order, context) = state
        .db
        .create_routed_payment_order(
            organization.id,
            r.rail,
            r.expected_checkout.as_ref(),
            secret,
            &new,
        )
        .await?;
    let order = drive_checkout(
        &state,
        organization.id,
        order,
        order_description(&new),
        r.return_url.clone(),
        r.cancel_url.clone(),
    )
    .await?;
    ok(OrderCheckout {
        publishable_key: context.version.publishable_key.clone(),
        sdk_url: context.version.sdk_url.clone(),
        provider_payment_id: order.provider_payment_id.clone(),
        client_secret: order.checkout["clientSecret"].as_str().map(str::to_owned),
        checkout_url: order.checkout["checkoutUrl"].as_str().map(str::to_owned),
        order,
    })
}

/// The checkout drive both create and checkout-session share (issue #607): a
/// still-`created` order with no provider reference issues the PSP call and
/// binds the result; one already bound — by a prior create, a session recovery
/// or an early signed callback — returns its standing handles, so a retry
/// never issues a second charge. Returns the re-read order.
async fn drive_checkout(
    state: &AppState,
    organization: Uuid,
    order: PaymentOrder,
    description: &str,
    return_url: Option<String>,
    cancel_url: Option<String>,
) -> Result<PaymentOrder, ApiError> {
    if order.purpose == PaymentOrderPurpose::Bundle
        && order.bundle_offer.is_none()
        && order.status != "fulfilled"
    {
        return Err(ApiError::Conflict(
            "legacy bundle has no authoritative offer; operator review required".into(),
        ));
    }
    if order.purpose == PaymentOrderPurpose::PlanPeriod
        && order.plan_payment.is_none()
        && order.status != "fulfilled"
    {
        return Err(ApiError::Conflict(
            "legacy plan order has no authoritative period quote; operator review required".into(),
        ));
    }
    if order.provider_payment_id.is_some() || order.status != "created" {
        return Ok(order);
    }
    let secret = state.config.secret().ok_or_else(|| {
        ApiError::ServiceUnavailable("merchant encryption is not configured".into())
    })?;
    let context = state
        .db
        .merchant_signing_context(
            order.merchant_id.ok_or(ApiError::Internal)?,
            order.merchant_version_id.ok_or(ApiError::Internal)?,
            secret,
        )
        .await?
        .ok_or(ApiError::Internal)?;
    let binding = state
        .db
        .merchant_payment_binding(order.order_id)
        .await?
        .ok_or(ApiError::Internal)?;
    let public = state.config.public_url().ok_or_else(|| {
        ApiError::ServiceUnavailable("public return URL is not configured".into())
    })?;
    let urls = oxsum_integrations::merchant_checkout::ReturnUrls {
        return_url,
        cancel_url,
    };
    let created = oxsum_integrations::merchant_checkout::create_order(
        &context,
        &binding,
        description,
        &state.provider_http,
        public,
        &urls,
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
    state
        .db
        .payment_order(organization, order.order_id)
        .await?
        .ok_or(ApiError::Internal)
}

/// The description the provider's page shows for the order's purpose — what the
/// customer is paying for, named distinctly from the fulfillment the order grants.
fn order_description(new: &NewPaymentOrder) -> &'static str {
    purpose_description(new.purpose)
}

fn purpose_description(purpose: PaymentOrderPurpose) -> &'static str {
    match purpose {
        PaymentOrderPurpose::WalletTopup => "oxsum wallet top-up",
        PaymentOrderPurpose::Bundle => "oxsum credit bundle",
        PaymentOrderPurpose::PlanPeriod => "oxsum plan period",
        PaymentOrderPurpose::InvoiceRepayment => "oxsum invoice repayment",
        PaymentOrderPurpose::StatementRepayment => "oxsum statement repayment",
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    after: Option<Uuid>,
    limit: Option<i64>,
    /// Narrow the history to one purpose or one lifecycle state — the order
    /// list's filters (issue #607).
    purpose: Option<PaymentOrderPurpose>,
    status: Option<String>,
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
            page.purpose,
            page.status.as_deref(),
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

/// The live checkout session the React page polls (issue #607): the stored
/// provider handles plus the order's current status — the recovery a refreshed
/// or returned browser drives. A still-`created` order re-issues the provider
/// call under the same order-UUID idempotency; `failed`/`expired`/`fulfilled`
/// answer their truth with whatever handles were stored.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CheckoutSession {
    status: String,
    provider_payment_id: Option<String>,
    client_secret: Option<String>,
    checkout_url: Option<String>,
    publishable_key: Option<String>,
    sdk_url: Option<String>,
}

/// `GET /api/v1/orders/{id}/checkout-session`: the order's checkout handles and
/// lifecycle truth in one read — never a paid promise, `paid` still comes only
/// from the signed webhook.
pub(crate) async fn checkout_session(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(order_id): Path<Uuid>,
) -> ApiResult<CheckoutSession> {
    let organization = principal.organization();
    let order = state
        .db
        .payment_order(organization.id, order_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    // A manual order and a terminal provider order answer their truth directly
    // — only a live `created` order may still need its provider call driven.
    let order = if order.source == "provider" {
        let description = purpose_description(order.purpose);
        drive_checkout(&state, organization.id, order, description, None, None).await?
    } else {
        order
    };
    // The publishable key/sdk URL come off the pinned merchant version — for a
    // manual order there is none, and the page gets nulls it never mounts.
    let (publishable_key, sdk_url) = match (order.merchant_id, order.merchant_version_id) {
        (Some(merchant), Some(version)) => {
            let secret = state.config.secret().ok_or_else(|| {
                ApiError::ServiceUnavailable("merchant encryption is not configured".into())
            })?;
            let context = state
                .db
                .merchant_signing_context(merchant, version, secret)
                .await?;
            match context {
                Some(context) => (
                    context.version.publishable_key.clone(),
                    context.version.sdk_url.clone(),
                ),
                None => (None, None),
            }
        }
        _ => (None, None),
    };
    ok(CheckoutSession {
        status: order.status.clone(),
        provider_payment_id: order.provider_payment_id.clone(),
        client_secret: order.checkout["clientSecret"].as_str().map(str::to_owned),
        checkout_url: order.checkout["checkoutUrl"].as_str().map(str::to_owned),
        publishable_key,
        sdk_url,
    })
}

/// The operator's cross-organization order page (issue #607): every customer's
/// orders, filterable by organization/purpose/status — a business staff
/// `ReportsRead` read, so a personal session, never a machine credential.
pub(crate) async fn business_orders(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    ApiQuery(page): ApiQuery<BusinessOrdersPage>,
) -> ApiResult<PaymentOrderPage> {
    let token = super::business::personal_token(&headers)?;
    // `ReportsRead` gates the whole-business listing — the same capability the
    // reconciliation report already requires.
    state
        .db
        .business_capability(&token, oxsum_core::BusinessCapability::ReportsRead)
        .await?;
    ok(state
        .db
        .business_payment_orders(
            page.organization_id,
            page.purpose,
            page.status.as_deref(),
            page.after,
            page.limit.unwrap_or(50),
        )
        .await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BusinessOrdersPage {
    organization_id: Option<Uuid>,
    purpose: Option<PaymentOrderPurpose>,
    status: Option<String>,
    after: Option<Uuid>,
    limit: Option<i64>,
}

/// `POST /api/v1/business/orders`: a staff-recorded manual receipt (issue
/// #607). The fiat receipt rates to credits at the business collection
/// rate, the order lands `paid` on the receipt's own evidence — reference
/// unique per business — and the same `fulfill_payment_order` the sweep runs
/// books the top-up exactly once. `FundsWrite` is the capability; the replay
/// of an idempotency key returns the standing fulfilled order.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManualOrderReq {
    expected_collection_revision: Option<i64>,
    organization_id: Uuid,
    idempotency_key: String,
    /// `walletTopup` only — a receipt books wallet cash or it is not a receipt.
    purpose: Option<PaymentOrderPurpose>,
    display_amount_minor: i64,
    display_currency: String,
    /// The receipt's own reference — a bank transaction id or remittance note.
    reference: String,
    reason: String,
}

/// The manual order as the staff surface answers it: the fulfilled order plus
/// the fulfillment record that named its grant.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualOrderRes {
    order: PaymentOrder,
    fulfillment: Option<oxsum_core::PaymentFulfillment>,
}

pub(crate) async fn create_manual_order(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    ApiJson(r): ApiJson<ManualOrderReq>,
) -> ApiResult<ManualOrderRes> {
    let token = super::business::personal_token(&headers)?;
    if r.purpose
        .is_some_and(|p| p != PaymentOrderPurpose::WalletTopup)
    {
        return Err(ApiError::Validation(
            "a manual receipt only funds a walletTopup order".into(),
        ));
    }
    let new = NewManualOrder {
        expected_collection_revision: r.expected_collection_revision,
        organization: r.organization_id,
        purpose: PaymentOrderPurpose::WalletTopup,
        display_amount_minor: r.display_amount_minor,
        display_currency: r.display_currency,
        idempotency_key: r.idempotency_key,
        reference: r.reference,
        reason: r.reason,
    };
    let order = state.db.create_manual_payment_order(&token, &new).await?;
    let organization = state.db.organization_by_id(order.organization_id).await?;
    // Replay: the standing order answers with its standing fulfillment — the
    // unique-evidence check and the idempotency replay already ran inside
    // `create_manual_payment_order`'s transaction.
    let fulfillment = if order.status == "fulfilled" {
        state.db.payment_fulfillment(order.order_id).await?
    } else {
        // The order is `paid` on its receipt's evidence; the grant runs through
        // the identical path the recovery sweep drains — wallet cash under the
        // order-derived ledger key, once ever.
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        Some(
            state
                .db
                .fulfill_payment_order(order.order_id, &wallet, crate::today())
                .await?,
        )
    };
    let order = state
        .db
        .payment_order(order.organization_id, order.order_id)
        .await?
        .ok_or(ApiError::Internal)?;
    ok(ManualOrderRes { order, fulfillment })
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

/// Retained original allocation and return evidence for personal finance readers.
pub(crate) async fn statement_payment_evidence(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(order): Path<Uuid>,
) -> ApiResult<Vec<oxsum_core::StatementCashEffect>> {
    let token = super::business::personal_token(&headers)?;
    ok(state.db.statement_payment_evidence(&token, order).await?)
}

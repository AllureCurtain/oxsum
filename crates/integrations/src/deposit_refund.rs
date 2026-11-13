//! Provider-scoped recovery for a pinned legacy deposit reversal.
//!
//! The lease/validate/record flow is rail-agnostic: only the wire adapter
//! differs. Hyperswitch authenticates with an `api-key` header and reads
//! `refunds/{id}`; Airwallex exchanges `x-client-id`/`x-api-key` for a bearer
//! token, then reads `pa/refunds/{id}`; Stripe Bearer-auths and resolves the
//! pinned `cs_…` session to the `pi_…` a `v1/refunds` call names. A successful
//! HTTP answer never establishes a terminal refund outcome — only an exact
//! authenticated event moves the durable row.
use crate::provider_http::ProviderHttp;
use oxsum_core::{DepositRefund, MerchantRail, MerchantSigningContext};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

/// Unknown, unavailable or contradictory result; carries no sensitive payload.
#[derive(Debug)]
pub struct UnknownRefund;
const TIMEOUT: Duration = Duration::from_secs(10);

/// The decimal major-units amount Airwallex's wire uses, exact-integer derived.
fn decimal(minor: i64) -> String {
    format!("{}.{:02}", minor / 100, minor % 100)
}

/// Reads a known refund by id. `None` only on an exact provider not-found
/// answer; every other outcome is `Err(UnknownRefund)`.
async fn read(
    context: &MerchantSigningContext,
    http: &ProviderHttp,
    id: &str,
) -> Result<Option<Value>, UnknownRefund> {
    match context.rail {
        MerchantRail::Hyperswitch => hyperswitch_read(context, http, id).await,
        MerchantRail::Airwallex => airwallex_read(context, http, id).await,
        MerchantRail::Stripe => stripe_read(context, http, id).await,
    }
}

/// Creates a new body-identified refund and returns the provider's response
/// body for validation. A non-2xx or unreadable answer is `Err(UnknownRefund)`
/// — the caller then re-reads before concluding anything.
async fn create(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    http: &ProviderHttp,
    id: &str,
) -> Result<Value, UnknownRefund> {
    match context.rail {
        MerchantRail::Hyperswitch => hyperswitch_create(context, target, http, id).await,
        MerchantRail::Airwallex => airwallex_create(context, target, http, id).await,
        MerchantRail::Stripe => stripe_create(context, target, http, id).await,
    }
}

/// Checks a provider body against the pinned reversal; a contradiction is
/// `Err(UnknownRefund)`, never a claimed outcome. Returns the recorded provider
/// refund handle plus, for the rail that resolved a fresh payment anchor, the
/// canonical provider payment id the durable row should carry.
fn validated(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    id: &str,
    body: &Value,
) -> Result<ValidatedRefund, UnknownRefund> {
    match context.rail {
        MerchantRail::Hyperswitch => {
            hyperswitch_validated(context, target, id, body).map(ValidatedRefund::handle)
        }
        MerchantRail::Airwallex => {
            airwallex_validated(target, id, body).map(ValidatedRefund::handle)
        }
        MerchantRail::Stripe => stripe_validated(target, body),
    }
}

/// A validated provider answer: the refund handle to record plus the canonical
/// provider payment anchor when the rail resolved a fresh one (Stripe resolves
/// the durable `cs_…` to the refund's `pi_…`; the other rails keep their
/// original anchor so `provider_payment_id` stays `None`).
pub struct ValidatedRefund {
    /// The provider refund handle (`re_…`/`rfd_…`/request id).
    pub handle: String,
    /// The canonical provider payment id the durable row should carry, when
    /// the arm resolved a different anchor than the one it was seeded with.
    pub provider_payment_id: Option<String>,
}
impl ValidatedRefund {
    /// Wraps a rail that keeps its seeded anchor — only the handle is new.
    fn handle(handle: String) -> Self {
        Self {
            handle,
            provider_payment_id: None,
        }
    }
}

/// Hyperswitch authenticates with its `api-key` header; a 404 carrying `HE_02`
/// is the only exact not-found answer.
async fn hyperswitch_read(
    context: &MerchantSigningContext,
    http: &ProviderHttp,
    id: &str,
) -> Result<Option<Value>, UnknownRefund> {
    let response = http
        .request(
            Method::GET,
            &context.version.api_url,
            &context.version.destination_policy,
            &["refunds", id],
        )
        .await
        .map_err(|_| UnknownRefund)?
        .header("api-key", context.api_key())
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| UnknownRefund)?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|_| UnknownRefund)?;
    if status == StatusCode::NOT_FOUND && body["error"]["code"] == "HE_02" {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(UnknownRefund);
    }
    Ok(Some(body))
}

async fn hyperswitch_create(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    http: &ProviderHttp,
    id: &str,
) -> Result<Value, UnknownRefund> {
    let response = http
        .request(
            Method::POST,
            &context.version.api_url,
            &context.version.destination_policy,
            &["refunds"],
        )
        .await
        .map_err(|_| UnknownRefund)?
        .header("api-key", context.api_key())
        .header("idempotency-key", id)
        .json(&json!({"refund_id":id,"payment_id":target.provider_payment_id,"amount":target.amount_minor,"reason":target.reason,
            "metadata":{"deposit_id":target.deposit_id,"payment_id":target.deposit_id,"organization_id":target.organization_id}}))
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| UnknownRefund)?;
    if !response.status().is_success() {
        return Err(UnknownRefund);
    }
    response.json().await.map_err(|_| UnknownRefund)
}

fn hyperswitch_validated(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    id: &str,
    body: &Value,
) -> Result<String, UnknownRefund> {
    if body["refund_id"] != id
        || body["payment_id"].as_str() != target.provider_payment_id.as_deref()
        || body["amount"].as_i64() != Some(target.amount_minor)
        || body["currency"] != target.currency
        || (!body["merchant_id"].is_null() && body["merchant_id"] != context.account_id)
    {
        return Err(UnknownRefund);
    }
    for (name, expected) in [
        ("deposit_id", target.deposit_id),
        ("payment_id", target.deposit_id),
        ("organization_id", target.organization_id),
    ] {
        if (target.body_identity || !body["metadata"][name].is_null())
            && body["metadata"][name] != expected.to_string()
        {
            return Err(UnknownRefund);
        }
    }
    Ok(id.to_owned())
}

/// Airwallex exchanges the `x-client-id`/`x-api-key` pair for a short-lived
/// bearer token before every Payment Acceptance call — the same login shape
/// `merchant_checkout` uses.
async fn airwallex_token(
    context: &MerchantSigningContext,
    http: &ProviderHttp,
) -> Result<String, UnknownRefund> {
    let login = http
        .request(
            Method::POST,
            &context.version.api_url,
            &context.version.destination_policy,
            &["api", "v1", "authentication", "login"],
        )
        .await
        .map_err(|_| UnknownRefund)?
        .header("x-client-id", context.client_id().ok_or(UnknownRefund)?)
        .header("x-api-key", context.api_key())
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| UnknownRefund)?;
    if !login.status().is_success() {
        return Err(UnknownRefund);
    }
    let login: Value = login.json().await.map_err(|_| UnknownRefund)?;
    login["token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or(UnknownRefund)
}

/// `GET /api/v1/pa/refunds/{id}` returns the refund object. A 404 is the exact
/// not-found answer; Airwallex's `code`/`message` error envelope carries the
/// detail but the status alone is enough to read "no such refund".
async fn airwallex_read(
    context: &MerchantSigningContext,
    http: &ProviderHttp,
    id: &str,
) -> Result<Option<Value>, UnknownRefund> {
    let token = airwallex_token(context, http).await?;
    let response = http
        .request(
            Method::GET,
            &context.version.api_url,
            &context.version.destination_policy,
            &["api", "v1", "pa", "refunds", id],
        )
        .await
        .map_err(|_| UnknownRefund)?
        .bearer_auth(token)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| UnknownRefund)?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|_| UnknownRefund)?;
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(UnknownRefund);
    }
    Ok(Some(body))
}

/// `POST /api/v1/pa/refunds/create` with the durable request identity in the
/// body (`request_id` is Airwallex's idempotency key) and the parent intent
/// under `payment_intent_id`. Amount goes out as a decimal major-unit token.
async fn airwallex_create(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    http: &ProviderHttp,
    id: &str,
) -> Result<Value, UnknownRefund> {
    let token = airwallex_token(context, http).await?;
    let amount = serde_json::value::RawValue::from_string(decimal(target.amount_minor))
        .map_err(|_| UnknownRefund)?;
    #[derive(serde::Serialize)]
    struct Body<'a> {
        request_id: String,
        payment_intent_id: String,
        amount: &'a serde_json::value::RawValue,
        currency: &'a str,
        reason: &'a str,
        metadata: Value,
    }
    let body = Body {
        request_id: id.to_owned(),
        payment_intent_id: target.provider_payment_id.clone().unwrap_or_default(),
        amount: &amount,
        currency: &target.currency,
        reason: &target.reason,
        metadata: json!({"deposit_id":target.deposit_id,"payment_id":target.deposit_id,"organization_id":target.organization_id}),
    };
    let response = http
        .request(
            Method::POST,
            &context.version.api_url,
            &context.version.destination_policy,
            &["api", "v1", "pa", "refunds", "create"],
        )
        .await
        .map_err(|_| UnknownRefund)?
        .bearer_auth(token)
        .json(&body)
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| UnknownRefund)?;
    if !response.status().is_success() {
        return Err(UnknownRefund);
    }
    response.json().await.map_err(|_| UnknownRefund)
}

/// Re-parse a refund body's `amount` lexeme into integer minor units, the same
/// exact path the webhook uses — a decimal `50.0`/`50.00` token converts
/// without a binary float. Returns `None` for a missing or malformed amount.
fn airwallex_amount_minor(body: &Value) -> Option<i64> {
    crate::merchant_events::decimal_minor_text(&body["amount"].to_string())
}

/// Airwallex's refund object names itself `id` (`rfd_…`) and its parent intent
/// `payment_intent_id`; the amount is decimal major units parsed back to minor.
/// The provider handle returned is `body["id"]` — unlike Hyperswitch, which
/// uses our request id as the refund id, Airwallex mints its own `rfd_…`.
fn airwallex_validated(
    target: &DepositRefund,
    _id: &str,
    body: &Value,
) -> Result<String, UnknownRefund> {
    // `id` is either the durable request identity (a fresh body-identified
    // create) or an already-known `rfd_…` provider handle (a retrieval). The
    // body must agree with whichever identity it carries: a fresh create
    // echoes `request_id` back, a retrieval echoes `id`.
    let handle = body["id"].as_str().filter(|s| !s.is_empty());
    let request_id = target.request_id.to_string();
    let identity_ok = if let Some(known) = target.provider_refund_id.as_deref() {
        handle == Some(known)
    } else {
        body["request_id"].as_str() == Some(request_id.as_str())
    };
    let amount_minor = airwallex_amount_minor(body);
    if !identity_ok
        || body["payment_intent_id"].as_str() != target.provider_payment_id.as_deref()
        || amount_minor != Some(target.amount_minor)
        || body["currency"] != target.currency
    {
        return Err(UnknownRefund);
    }
    for (name, expected) in [
        ("deposit_id", target.deposit_id),
        ("payment_id", target.deposit_id),
        ("organization_id", target.organization_id),
    ] {
        if (target.body_identity || !body["metadata"][name].is_null())
            && body["metadata"][name] != expected.to_string()
        {
            return Err(UnknownRefund);
        }
    }
    handle.map(str::to_owned).ok_or(UnknownRefund)
}

/// Stripe authenticates every call with `Authorization: Bearer <sk_…>` — the
/// `api_key` the merchant version holds. Refunds are charge/intent-scoped, not
/// session-scoped, so a `cs_…` payment reference must resolve to its
/// `payment_intent` (`pi_…`) before a refund can name it.
async fn stripe_read(
    context: &MerchantSigningContext,
    http: &ProviderHttp,
    id: &str,
) -> Result<Option<Value>, UnknownRefund> {
    let response = http
        .request(
            Method::GET,
            &context.version.api_url,
            &context.version.destination_policy,
            &["v1", "refunds", id],
        )
        .await
        .map_err(|_| UnknownRefund)?
        .bearer_auth(context.api_key())
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| UnknownRefund)?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|_| UnknownRefund)?;
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(UnknownRefund);
    }
    Ok(Some(body))
}

/// Resolves the durable provider payment reference to the `payment_intent`
/// (`pi_…`) a Stripe refund names. A `pi_…` is already canonical and needs no
/// lookup; a `cs_…` resolves through `GET /v1/checkout/sessions/{id}` — a
/// session that never paid has no `payment_intent`, so it stays `UnknownRefund`
/// rather than fabricating an anchor. Shared with the order-refund arm in
/// [`crate::merchant_checkout`]: a Stripe order's durable reference is also the
/// `cs_…` Checkout Session id, which `v1/refunds` cannot accept either.
pub(crate) async fn resolve_stripe_payment_intent(
    context: &MerchantSigningContext,
    provider_payment_id: &str,
    http: &ProviderHttp,
) -> Result<String, UnknownRefund> {
    if provider_payment_id.starts_with("pi_") {
        return Ok(provider_payment_id.to_owned());
    }
    let response = http
        .request(
            Method::GET,
            &context.version.api_url,
            &context.version.destination_policy,
            &["v1", "checkout", "sessions", provider_payment_id],
        )
        .await
        .map_err(|_| UnknownRefund)?
        .bearer_auth(context.api_key())
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| UnknownRefund)?;
    if !response.status().is_success() {
        return Err(UnknownRefund);
    }
    let body: Value = response.json().await.map_err(|_| UnknownRefund)?;
    body["payment_intent"]
        .as_str()
        .filter(|s| s.starts_with("pi_"))
        .map(str::to_owned)
        .ok_or(UnknownRefund)
}

/// `POST /v1/refunds` with the durable request identity in the
/// `Idempotency-Key` header (Stripe idempotency is a header, not a body field).
/// The refund names its parent by `payment_intent` — the `cs_…` the deposit
/// pinned is resolved first. Amount is integer minor units; Stripe is
/// minor-native, unlike Airwallex's decimal-major wire.
async fn stripe_create(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    http: &ProviderHttp,
    id: &str,
) -> Result<Value, UnknownRefund> {
    let payment_intent = resolve_stripe_payment_intent(
        context,
        target.provider_payment_id.as_deref().ok_or(UnknownRefund)?,
        http,
    )
    .await?;
    let response = http
        .request(
            Method::POST,
            &context.version.api_url,
            &context.version.destination_policy,
            &["v1", "refunds"],
        )
        .await
        .map_err(|_| UnknownRefund)?
        .bearer_auth(context.api_key())
        .header("idempotency-key", id)
        .form(&[
            ("payment_intent", payment_intent),
            ("amount", target.amount_minor.to_string()),
            ("metadata[deposit_id]", target.deposit_id.to_string()),
            ("metadata[payment_id]", target.deposit_id.to_string()),
            (
                "metadata[organization_id]",
                target.organization_id.to_string(),
            ),
        ])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|_| UnknownRefund)?;
    if !response.status().is_success() {
        return Err(UnknownRefund);
    }
    response.json().await.map_err(|_| UnknownRefund)
}

/// A Stripe refund object names itself `id` (`re_…`) and its parent intent
/// `payment_intent` (`pi_…`). The provider handle returned is `body["id"]` —
/// Stripe mints it, like Airwallex, unlike Hyperswitch's request-id reuse.
/// `payment_intent` is returned as the canonical anchor: the durable row's
/// `cs_…` is rewritten to it so the settle event's `pi_…` comparison matches.
fn stripe_validated(
    target: &DepositRefund,
    body: &Value,
) -> Result<ValidatedRefund, UnknownRefund> {
    let handle = body["id"]
        .as_str()
        .filter(|s| s.starts_with("re_"))
        .map(str::to_owned);
    let payment_intent = body["payment_intent"]
        .as_str()
        .filter(|s| s.starts_with("pi_"))
        .map(str::to_owned);
    let (Some(handle), Some(payment_intent)) = (handle, payment_intent) else {
        return Err(UnknownRefund);
    };
    // Stripe answers lowercase ISO-4217 (`usd`); the durable currency is the
    // uppercase form the binding pinned.
    let currency_ok = body["currency"]
        .as_str()
        .is_some_and(|c| c.eq_ignore_ascii_case(&target.currency));
    if body["amount"].as_i64() != Some(target.amount_minor) || !currency_ok {
        return Err(UnknownRefund);
    }
    for (name, expected) in [
        ("deposit_id", target.deposit_id),
        ("payment_id", target.deposit_id),
        ("organization_id", target.organization_id),
    ] {
        if (target.body_identity || !body["metadata"][name].is_null())
            && body["metadata"][name] != expected.to_string()
        {
            return Err(UnknownRefund);
        }
    }
    Ok(ValidatedRefund {
        handle,
        provider_payment_id: Some(payment_intent),
    })
}

/// Reads a known operation, or creates a new body-identified refund only after
/// an exact not-found response. Every response is checked against pinned terms;
/// a successful HTTP answer never establishes a terminal refund outcome.
pub async fn recover(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    http: &ProviderHttp,
) -> Result<ValidatedRefund, UnknownRefund> {
    if target.amount_minor <= 0
        || target.merchant_id != Some(context.merchant_id)
        || target.version_id != Some(context.version.id)
        || target.provider_payment_id.is_none()
    {
        return Err(UnknownRefund);
    }
    let id = match &target.provider_refund_id {
        Some(id) => id.clone(),
        None if target.body_identity => target.request_id.to_string(),
        None => return Err(UnknownRefund),
    };
    if let Some(body) = read(context, http, &id).await? {
        return validated(context, target, &id, &body);
    }
    if !target.body_identity || target.provider_refund_id.is_some() {
        return Err(UnknownRefund);
    }
    if let Ok(body) = create(context, target, http, &id).await {
        return validated(context, target, &id, &body);
    }
    let body = read(context, http, &id).await?.ok_or(UnknownRefund)?;
    validated(context, target, &id, &body)
}

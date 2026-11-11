//! Provider-scoped recovery for a pinned legacy deposit reversal.
//!
//! The lease/validate/record flow is rail-agnostic: only the wire adapter
//! differs. Hyperswitch authenticates with an `api-key` header and reads
//! `refunds/{id}`; Airwallex exchanges `x-client-id`/`x-api-key` for a bearer
//! token, then reads `pa/refunds/{id}`. A successful HTTP answer never
//! establishes a terminal refund outcome — only an exact authenticated event
//! moves the durable row.
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
        // Stripe refund recovery is deliberately deferred (see decisions.md).
        MerchantRail::Stripe => Err(UnknownRefund),
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
        MerchantRail::Stripe => Err(UnknownRefund),
    }
}

/// Checks a provider body against the pinned reversal; a contradiction is
/// `Err(UnknownRefund)`, never a claimed outcome.
fn validated(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    id: &str,
    body: &Value,
) -> Result<String, UnknownRefund> {
    match context.rail {
        MerchantRail::Hyperswitch => hyperswitch_validated(context, target, id, body),
        MerchantRail::Airwallex => airwallex_validated(target, id, body),
        MerchantRail::Stripe => Err(UnknownRefund),
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

/// Reads a known operation, or creates a new body-identified refund only after
/// an exact not-found response. Every response is checked against pinned terms;
/// a successful HTTP answer never establishes a terminal refund outcome.
pub async fn recover(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    http: &ProviderHttp,
) -> Result<String, UnknownRefund> {
    if target.amount_minor <= 0
        || target.merchant_id != Some(context.merchant_id)
        || target.version_id != Some(context.version.id)
        || target.provider_payment_id.is_none()
        || context.rail == MerchantRail::Stripe
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

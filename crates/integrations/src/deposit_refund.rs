//! Hyperswitch v1 recovery for a pinned legacy deposit reversal.
use crate::provider_http::ProviderHttp;
use oxsum_core::{DepositRefund, MerchantRail, MerchantSigningContext};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

/// Unknown, unavailable or contradictory result; carries no sensitive payload.
#[derive(Debug)]
pub struct UnknownRefund;
const TIMEOUT: Duration = Duration::from_secs(10);

async fn read(
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
fn validated(
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
/// Reads a known operation, or creates a new body-identified refund only after
/// an exact not-found response. Every response is checked against pinned terms;
/// a successful HTTP answer never establishes a terminal refund outcome.
pub async fn recover(
    context: &MerchantSigningContext,
    target: &DepositRefund,
    http: &ProviderHttp,
) -> Result<String, UnknownRefund> {
    if target.amount_minor <= 0
        || context.rail != MerchantRail::Hyperswitch
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
    let response=http.request(Method::POST,&context.version.api_url,&context.version.destination_policy,&["refunds"])
        .await.map_err(|_|UnknownRefund)?.header("api-key",context.api_key()).header("idempotency-key",&id)
        .json(&json!({"refund_id":id,"payment_id":target.provider_payment_id,"amount":target.amount_minor,"reason":target.reason,
            "metadata":{"deposit_id":target.deposit_id,"payment_id":target.deposit_id,"organization_id":target.organization_id}}))
        .timeout(TIMEOUT).send().await;
    if let Ok(response) = response
        && response.status().is_success()
        && let Ok(body) = response.json::<Value>().await
    {
        return validated(context, target, &id, &body);
    }
    let body = read(context, http, &id).await?.ok_or(UnknownRefund)?;
    validated(context, target, &id, &body)
}

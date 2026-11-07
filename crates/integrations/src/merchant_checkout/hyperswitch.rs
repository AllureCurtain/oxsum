//! Hyperswitch v1 body identities and read-before-create recovery.
use super::{MerchantCheckoutError, MerchantCheckoutHandle, RefundHandle, RefundRequestError};
use crate::provider_http::ProviderHttp;
use oxsum_core::{
    MerchantPaymentBinding, MerchantPaymentPurpose, MerchantSigningContext, OrderRefund,
};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

async fn retrieve(
    context: &MerchantSigningContext,
    http: &ProviderHttp,
    resource: &str,
    id: &str,
) -> Result<Option<Value>, MerchantCheckoutError> {
    let response = http
        .request(
            Method::GET,
            &context.version.api_url,
            &context.version.destination_policy,
            &[resource, id],
        )
        .await
        .map_err(|_| MerchantCheckoutError)?
        .header("api-key", context.api_key())
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|_| MerchantCheckoutError)?;
    let status = response.status();
    let value: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
    // A proxy's generic 404 is not evidence that the operation is absent.
    if status == StatusCode::NOT_FOUND && value["error"]["code"] == "HE_02" {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(MerchantCheckoutError);
    }
    Ok(Some(value))
}

fn payment_handle(
    context: &MerchantSigningContext,
    payment: &MerchantPaymentBinding,
    id: &str,
    value: &Value,
) -> Result<MerchantCheckoutHandle, MerchantCheckoutError> {
    if value["payment_id"] != id
        || value["amount"].as_i64() != Some(payment.amount_minor)
        || value["currency"] != payment.currency
        || value["merchant_id"] != context.account_id
    {
        return Err(MerchantCheckoutError);
    }
    let metadata = &value["metadata"];
    for (field, expected) in [
        ("payment_id", payment.payment_id),
        ("organization_id", payment.organization_id),
        (
            match payment.purpose {
                MerchantPaymentPurpose::Order => "order_id",
                MerchantPaymentPurpose::LegacyDeposit => "deposit_id",
            },
            payment.payment_id,
        ),
    ] {
        if (payment.hyperswitch_body_id || !metadata[field].is_null())
            && metadata[field] != expected.to_string()
        {
            return Err(MerchantCheckoutError);
        }
    }
    Ok(MerchantCheckoutHandle {
        provider_payment_id: id.to_owned(),
        client_secret: Some(super::field(value, "client_secret")?),
        checkout_url: None,
    })
}

pub(super) async fn payment(
    context: &MerchantSigningContext,
    payment: &MerchantPaymentBinding,
    http: &ProviderHttp,
    mut body: Value,
) -> Result<MerchantCheckoutHandle, MerchantCheckoutError> {
    let id = match &payment.provider_payment_id {
        Some(id) => id.clone(),
        None if payment.hyperswitch_body_id => payment.payment_id.to_string(),
        // Historical unbound attempts may have reached the switch already.
        None => return Err(MerchantCheckoutError),
    };
    if let Some(value) = retrieve(context, http, "payments", &id).await? {
        return payment_handle(context, payment, &id, &value);
    }
    if payment.provider_payment_id.is_some() {
        return Err(MerchantCheckoutError);
    }
    body["payment_id"] = Value::String(id.clone());
    let response = http
        .request(
            Method::POST,
            &context.version.api_url,
            &context.version.destination_policy,
            &["payments"],
        )
        .await
        .map_err(|_| MerchantCheckoutError)?
        .header("api-key", context.api_key())
        .header("idempotency-key", &id)
        .json(&body)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await;
    if let Ok(response) = response
        && response.status().is_success()
        && let Ok(value) = response.json::<Value>().await
    {
        return payment_handle(context, payment, &id, &value);
    }
    // Duplicate-body-ID responses are non-2xx in v1.129. A dropped successful
    // response has the same recovery: retrieve and verify the original terms.
    let value = retrieve(context, http, "payments", &id)
        .await?
        .ok_or(MerchantCheckoutError)?;
    payment_handle(context, payment, &id, &value)
}

fn refund_handle(
    context: &MerchantSigningContext,
    refund: &OrderRefund,
    payment: &str,
    id: &str,
    value: &Value,
) -> Result<RefundHandle, RefundRequestError> {
    if value["refund_id"] != id
        || value["payment_id"] != payment
        || value["amount"].as_i64() != Some(refund.display_amount_minor)
        || value["currency"] != refund.display_currency
        || (!value["merchant_id"].is_null() && value["merchant_id"] != context.account_id)
    {
        return Err(RefundRequestError::Unknown);
    }
    let metadata = &value["metadata"];
    for (field, expected) in [
        ("refund_id", refund.refund_id),
        ("payment_id", refund.order_id),
        ("organization_id", refund.organization_id),
    ] {
        if (refund.hyperswitch_body_id || !metadata[field].is_null())
            && metadata[field] != expected.to_string()
        {
            return Err(RefundRequestError::Unknown);
        }
    }
    // The old writer mislabeled order_id with the refund UUID. Accept that
    // exact historical alias only for rows explicitly fenced by migration.
    if (refund.hyperswitch_body_id || !metadata["order_id"].is_null())
        && metadata["order_id"] != refund.order_id.to_string()
        && (refund.hyperswitch_body_id || metadata["order_id"] != refund.refund_id.to_string())
    {
        return Err(RefundRequestError::Unknown);
    }
    Ok(RefundHandle {
        provider_refund_id: id.to_owned(),
        status: super::field(value, "status")?,
    })
}

pub(super) async fn refund(
    context: &MerchantSigningContext,
    refund: &OrderRefund,
    payment: &str,
    http: &ProviderHttp,
    metadata: Value,
) -> Result<RefundHandle, RefundRequestError> {
    let id = match &refund.provider_refund_id {
        Some(id) => id.clone(),
        None if refund.hyperswitch_body_id => refund.refund_id.to_string(),
        None => return Err(RefundRequestError::Unknown),
    };
    if let Some(value) = retrieve(context, http, "refunds", &id).await? {
        return refund_handle(context, refund, payment, &id, &value);
    }
    if refund.provider_refund_id.is_some() {
        return Err(RefundRequestError::Unknown);
    }
    let response = http
        .request(Method::POST, &context.version.api_url, &context.version.destination_policy, &["refunds"])
        .await.map_err(|_| RefundRequestError::Unknown)?
        .header("api-key", context.api_key()).header("idempotency-key", &id)
        .json(&serde_json::json!({"refund_id":id,"payment_id":payment,"amount":refund.display_amount_minor,
            "reason":refund.reason,"metadata":metadata}))
        .timeout(REQUEST_TIMEOUT).send().await;
    let mut refused = false;
    if let Ok(response) = response {
        let status = response.status();
        if let Ok(value) = response.json::<Value>().await {
            if status.is_success() {
                return refund_handle(context, refund, payment, &id, &value);
            }
            // Pinned v1.129 rejects excess amount before inserting the refund
            // or contacting the connector. Generic 4xx/5xx/connector failures
            // cannot establish non-creation and remain unknown.
            refused = status == StatusCode::BAD_REQUEST && value["error"]["code"] == "IR_13";
        }
    }
    if let Some(value) = retrieve(context, http, "refunds", &id).await? {
        return refund_handle(context, refund, payment, &id, &value);
    }
    Err(if refused {
        RefundRequestError::Refused
    } else {
        RefundRequestError::Unknown
    })
}

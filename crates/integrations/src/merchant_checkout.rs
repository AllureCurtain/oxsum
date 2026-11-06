//! Credential-bearing compatibility checkout through a pinned merchant version.
use crate::provider_http::ProviderHttp;
use oxsum_core::{MerchantPaymentBinding, MerchantRail, MerchantSigningContext};
use reqwest::Method;
use serde_json::{Value, json};
#[derive(Debug, Clone, Copy)]
pub struct MerchantCheckoutError;
pub struct MerchantCheckoutHandle {
    pub provider_payment_id: String,
    pub client_secret: Option<String>,
    pub checkout_url: Option<String>,
}
fn field(value: &Value, name: &str) -> Result<String, MerchantCheckoutError> {
    value[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or(MerchantCheckoutError)
}
fn decimal(minor: i64) -> String {
    format!("{}.{:02}", minor / 100, minor % 100)
}
/// T15 uses purpose-specific requests; this retains the legacy USD contract
/// while enforcing the same immutable merchant identity and expected terms.
pub async fn create(
    context: &MerchantSigningContext,
    payment: &MerchantPaymentBinding,
    http: &ProviderHttp,
    public_url: &str,
) -> Result<MerchantCheckoutHandle, MerchantCheckoutError> {
    if payment.currency != "USD"
        || payment.merchant_id != context.merchant_id
        || payment.version_id != context.version.id
    {
        return Err(MerchantCheckoutError);
    }
    let metadata = json!({"payment_id":payment.payment_id,"deposit_id":payment.payment_id,"organization_id":payment.organization_id});
    let base = &context.version.api_url;
    let policy = &context.version.destination_policy;
    let result = match context.rail {
        MerchantRail::Hyperswitch => {
            let response=http.request(Method::POST,base,policy,&["payments"]).await.map_err(|_|MerchantCheckoutError)?
                .header("api-key",context.api_key()).header("idempotency-key",payment.payment_id.to_string())
                .json(&json!({"amount":payment.amount_minor,"currency":payment.currency,"confirm":false,"capture_method":"automatic","description":"oxsum credits","return_url":format!("{}/app/topup?hyperswitch=return",public_url.trim_end_matches('/')),"metadata":metadata}))
                .send().await.map_err(|_|MerchantCheckoutError)?;
            if !response.status().is_success() {
                return Err(MerchantCheckoutError);
            }
            let result: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
            MerchantCheckoutHandle {
                provider_payment_id: field(&result, "payment_id")?,
                client_secret: Some(field(&result, "client_secret")?),
                checkout_url: None,
            }
        }
        MerchantRail::Stripe => {
            let response = http
                .request(Method::POST, base, policy, &["v1", "checkout", "sessions"])
                .await
                .map_err(|_| MerchantCheckoutError)?
                .bearer_auth(context.api_key())
                .header("idempotency-key", payment.payment_id.to_string())
                .form(&[
                    ("mode", "payment".into()),
                    ("client_reference_id", payment.payment_id.to_string()),
                    ("metadata[payment_id]", payment.payment_id.to_string()),
                    (
                        "metadata[organization_id]",
                        payment.organization_id.to_string(),
                    ),
                    (
                        "success_url",
                        format!(
                            "{}/app/topup?session=success",
                            public_url.trim_end_matches('/')
                        ),
                    ),
                    (
                        "cancel_url",
                        format!(
                            "{}/app/topup?session=cancel",
                            public_url.trim_end_matches('/')
                        ),
                    ),
                    ("line_items[0][quantity]", "1".into()),
                    ("line_items[0][price_data][currency]", "usd".into()),
                    (
                        "line_items[0][price_data][unit_amount]",
                        payment.amount_minor.to_string(),
                    ),
                    (
                        "line_items[0][price_data][product_data][name]",
                        "oxsum credits".into(),
                    ),
                ])
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !response.status().is_success() {
                return Err(MerchantCheckoutError);
            }
            let result: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
            MerchantCheckoutHandle {
                provider_payment_id: field(&result, "id")?,
                client_secret: None,
                checkout_url: Some(field(&result, "url")?),
            }
        }
        MerchantRail::Airwallex => {
            let login = http
                .request(
                    Method::POST,
                    base,
                    policy,
                    &["api", "v1", "authentication", "login"],
                )
                .await
                .map_err(|_| MerchantCheckoutError)?
                .header(
                    "x-client-id",
                    context.client_id().ok_or(MerchantCheckoutError)?,
                )
                .header("x-api-key", context.api_key())
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !login.status().is_success() {
                return Err(MerchantCheckoutError);
            }
            let login: Value = login.json().await.map_err(|_| MerchantCheckoutError)?;
            let token = field(&login, "token")?;
            // Airwallex expects a numeric decimal; RawValue inserts the exact
            // integer-derived token without an intermediate binary float.
            let amount = serde_json::value::RawValue::from_string(decimal(payment.amount_minor))
                .map_err(|_| MerchantCheckoutError)?;
            #[derive(serde::Serialize)]
            struct Body<'a> {
                request_id: String,
                amount: &'a serde_json::value::RawValue,
                currency: &'a str,
                merchant_order_id: String,
                return_url: String,
                metadata: Value,
            }
            let body = Body {
                request_id: payment.payment_id.to_string(),
                amount: &amount,
                currency: &payment.currency,
                merchant_order_id: payment.payment_id.to_string(),
                return_url: format!("{}/app/topup", public_url.trim_end_matches('/')),
                metadata,
            };
            let response = http
                .request(
                    Method::POST,
                    base,
                    policy,
                    &["api", "v1", "pa", "payment_intents", "create"],
                )
                .await
                .map_err(|_| MerchantCheckoutError)?
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !response.status().is_success() {
                return Err(MerchantCheckoutError);
            }
            let result: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
            let id = field(&result, "id")?;
            let mut url = url::Url::parse(
                context
                    .version
                    .checkout_url
                    .as_deref()
                    .ok_or(MerchantCheckoutError)?,
            )
            .map_err(|_| MerchantCheckoutError)?;
            url.query_pairs_mut().append_pair("intent_id", &id);
            MerchantCheckoutHandle {
                provider_payment_id: id,
                client_secret: Some(field(&result, "client_secret")?),
                checkout_url: Some(url.to_string()),
            }
        }
    };
    Ok(result)
}
/// A purpose-scoped order checkout (issue #606): like [`create`], but the
/// charged amount/currency are the order's display terms, which may be any
/// ISO-4217 fiat — the binding pins what the provider charges, and the credit
/// the order books is a separate fact the provider never sees. The provider's
/// idempotency key stays the durable local payment UUID, so a retry of the
/// same order never issues a second charge.
///
/// `description` names what the order buys (a wallet top-up, a bundle, a plan
/// period, an invoice repayment) — the line the customer sees on the provider's
/// page, distinct from the fulfillment the order's own purpose grants.
pub async fn create_order(
    context: &MerchantSigningContext,
    payment: &MerchantPaymentBinding,
    description: &str,
    http: &ProviderHttp,
    public_url: &str,
) -> Result<MerchantCheckoutHandle, MerchantCheckoutError> {
    if payment.merchant_id != context.merchant_id || payment.version_id != context.version.id {
        return Err(MerchantCheckoutError);
    }
    let metadata = json!({"payment_id":payment.payment_id,"order_id":payment.payment_id,"organization_id":payment.organization_id});
    let base = &context.version.api_url;
    let policy = &context.version.destination_policy;
    let result = match context.rail {
        MerchantRail::Hyperswitch => {
            let response=http.request(Method::POST,base,policy,&["payments"]).await.map_err(|_|MerchantCheckoutError)?
                .header("api-key",context.api_key()).header("idempotency-key",payment.payment_id.to_string())
                .json(&json!({"amount":payment.amount_minor,"currency":payment.currency,"confirm":false,"capture_method":"automatic","description":description,"return_url":format!("{}/app/orders?order={}&hyperswitch=return",public_url.trim_end_matches('/'),payment.payment_id),"metadata":metadata}))
                .send().await.map_err(|_|MerchantCheckoutError)?;
            if !response.status().is_success() {
                return Err(MerchantCheckoutError);
            }
            let result: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
            MerchantCheckoutHandle {
                provider_payment_id: field(&result, "payment_id")?,
                client_secret: Some(field(&result, "client_secret")?),
                checkout_url: None,
            }
        }
        MerchantRail::Stripe => {
            let response = http
                .request(Method::POST, base, policy, &["v1", "checkout", "sessions"])
                .await
                .map_err(|_| MerchantCheckoutError)?
                .bearer_auth(context.api_key())
                .header("idempotency-key", payment.payment_id.to_string())
                .form(&[
                    ("mode", "payment".into()),
                    ("client_reference_id", payment.payment_id.to_string()),
                    ("metadata[payment_id]", payment.payment_id.to_string()),
                    (
                        "metadata[organization_id]",
                        payment.organization_id.to_string(),
                    ),
                    (
                        "success_url",
                        format!(
                            "{}/app/orders?order={}&session=success",
                            public_url.trim_end_matches('/'),
                            payment.payment_id
                        ),
                    ),
                    (
                        "cancel_url",
                        format!(
                            "{}/app/orders?order={}&session=cancel",
                            public_url.trim_end_matches('/'),
                            payment.payment_id
                        ),
                    ),
                    ("line_items[0][quantity]", "1".into()),
                    (
                        "line_items[0][price_data][currency]",
                        payment.currency.to_lowercase(),
                    ),
                    (
                        "line_items[0][price_data][unit_amount]",
                        payment.amount_minor.to_string(),
                    ),
                    (
                        "line_items[0][price_data][product_data][name]",
                        description.to_string(),
                    ),
                ])
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !response.status().is_success() {
                return Err(MerchantCheckoutError);
            }
            let result: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
            MerchantCheckoutHandle {
                provider_payment_id: field(&result, "id")?,
                client_secret: None,
                checkout_url: Some(field(&result, "url")?),
            }
        }
        MerchantRail::Airwallex => {
            let login = http
                .request(
                    Method::POST,
                    base,
                    policy,
                    &["api", "v1", "authentication", "login"],
                )
                .await
                .map_err(|_| MerchantCheckoutError)?
                .header(
                    "x-client-id",
                    context.client_id().ok_or(MerchantCheckoutError)?,
                )
                .header("x-api-key", context.api_key())
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !login.status().is_success() {
                return Err(MerchantCheckoutError);
            }
            let login: Value = login.json().await.map_err(|_| MerchantCheckoutError)?;
            let token = field(&login, "token")?;
            let amount = serde_json::value::RawValue::from_string(decimal(payment.amount_minor))
                .map_err(|_| MerchantCheckoutError)?;
            #[derive(serde::Serialize)]
            struct Body<'a> {
                request_id: String,
                amount: &'a serde_json::value::RawValue,
                currency: &'a str,
                merchant_order_id: String,
                return_url: String,
                metadata: Value,
            }
            let body = Body {
                request_id: payment.payment_id.to_string(),
                amount: &amount,
                currency: &payment.currency,
                merchant_order_id: payment.payment_id.to_string(),
                return_url: format!(
                    "{}/app/orders?order={}",
                    public_url.trim_end_matches('/'),
                    payment.payment_id
                ),
                metadata,
            };
            let response = http
                .request(
                    Method::POST,
                    base,
                    policy,
                    &["api", "v1", "pa", "payment_intents", "create"],
                )
                .await
                .map_err(|_| MerchantCheckoutError)?
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !response.status().is_success() {
                return Err(MerchantCheckoutError);
            }
            let result: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
            let id = field(&result, "id")?;
            let mut url = url::Url::parse(
                context
                    .version
                    .checkout_url
                    .as_deref()
                    .ok_or(MerchantCheckoutError)?,
            )
            .map_err(|_| MerchantCheckoutError)?;
            url.query_pairs_mut().append_pair("intent_id", &id);
            MerchantCheckoutHandle {
                provider_payment_id: id,
                client_secret: Some(field(&result, "client_secret")?),
                checkout_url: Some(url.to_string()),
            }
        }
    };
    Ok(result)
}

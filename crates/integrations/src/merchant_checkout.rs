//! Credential-bearing compatibility checkout through a pinned merchant version.
use crate::provider_http::ProviderHttp;
use oxsum_core::{MerchantPaymentBinding, MerchantRail, MerchantSigningContext, OrderRefund};
use reqwest::Method;
use serde_json::{Value, json};
use std::time::Duration;

mod hyperswitch;
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
            hyperswitch::payment(context, payment, http, json!({
                "amount": payment.amount_minor, "currency": payment.currency,
                "confirm": false, "capture_method": "automatic", "description": "oxsum credits",
                "return_url": format!("{}/app/topup?hyperswitch=return", public_url.trim_end_matches('/')),
                "metadata": metadata,
            })).await?
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
/// The return/cancel overrides a create may carry (issue #607): absent, the
/// provider returns the browser to the order's own `/app/orders?order={id}`
/// page. An absolute http(s) URL is used as given; a site-absolute path
/// (`/...`) resolves under the deployment's public URL; anything else is
/// refused by the route before it reaches here.
#[derive(Default)]
pub struct ReturnUrls {
    pub return_url: Option<String>,
    pub cancel_url: Option<String>,
}

impl ReturnUrls {
    /// The provider's return destination: the override, or the order's own
    /// status page. `suffix` carries the rail's distinguishing query (the
    /// switch's `hyperswitch=return`, Stripe's `session=success`).
    fn resolve(&self, public_url: &str, payment_id: uuid::Uuid, suffix: &str) -> String {
        let base = public_url.trim_end_matches('/');
        let default = format!("{base}/app/orders?order={payment_id}{suffix}");
        match self.return_url.as_deref() {
            Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                url.to_owned()
            }
            Some(path) if path.starts_with('/') => format!("{base}{path}"),
            _ => default,
        }
    }

    /// Stripe's separate cancel destination: the override, or the return
    /// resolution with the cancel suffix.
    fn resolve_cancel(&self, public_url: &str, payment_id: uuid::Uuid) -> String {
        let base = public_url.trim_end_matches('/');
        match self.cancel_url.as_deref() {
            Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                url.to_owned()
            }
            Some(path) if path.starts_with('/') => format!("{base}{path}"),
            _ => format!("{base}/app/orders?order={payment_id}&session=cancel"),
        }
    }
}

/// What issuing a refund returns: the provider's own refund id — what the
/// `refund_*` webhooks name — and the status the provider answered (`pending`
/// is the norm; a provider that refunds synchronously may answer
/// `succeeded`, which the signed event still confirms).
pub struct RefundHandle {
    pub provider_refund_id: String,
    pub status: String,
}

/// A sanitized provider outcome; unknown results never free a reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefundRequestError {
    /// A verified refusal before an operation was created.
    Refused,
    /// The external result is unknown, unavailable or contradicts pinned terms.
    Unknown,
}
impl From<MerchantCheckoutError> for RefundRequestError {
    fn from(_: MerchantCheckoutError) -> Self {
        Self::Unknown
    }
}

/// Issues a refund against a payment order's provider payment (issue #608):
/// the cash half of a correction, requested only after the durable
/// `provider_pending` reservation commits — a crash between the reservation
/// and this call leaves a retryable row, never a lost refund.
///
/// `refund` is the durable reservation. New Hyperswitch operations use its
/// UUID in the actual body identity field and retrieve it before any retry. `provider_payment_id` is the payment the order was charged under;
/// `amount_minor`/`currency` are the fiat terms the refund carries (the
/// display figure the order pinned — the credit take-back is a separate
/// ledger fact the provider never sees). The refund's own `metadata.refund_id`
/// points back so the `refund_*`/`dispute_*` events walk to the row without
/// trusting the payment id alone.
///
/// # Errors
///
/// [`RefundRequestError`] separates verified pre-creation refusals from unknown
/// outcomes. Unknown transport/status/body results retain the reservation.
pub async fn create_refund(
    context: &MerchantSigningContext,
    refund: &OrderRefund,
    provider_payment_id: &str,
    http: &ProviderHttp,
) -> Result<RefundHandle, RefundRequestError> {
    if context.rail != MerchantRail::Hyperswitch && refund.provider_refund_id.is_some() {
        return Err(RefundRequestError::Unknown);
    }
    let refund_id = refund.refund_id;
    let amount_minor = refund.display_amount_minor;
    let currency = &refund.display_currency;
    let reason = &refund.reason;
    let metadata = json!({"refund_id":refund_id,"order_id":refund.order_id,"payment_id":refund.order_id,"organization_id":refund.organization_id});
    let base = &context.version.api_url;
    let policy = &context.version.destination_policy;
    let result = match context.rail {
        MerchantRail::Hyperswitch => {
            return hyperswitch::refund(context, refund, provider_payment_id, http, metadata).await;
        }
        MerchantRail::Stripe => {
            let response = http
                .request(Method::POST, base, policy, &["v1", "refunds"])
                .await
                .map_err(|_| MerchantCheckoutError)?
                .bearer_auth(context.api_key())
                .header("idempotency-key", refund_id.to_string())
                .form(&[
                    ("payment_intent", provider_payment_id.to_string()),
                    ("amount", amount_minor.to_string()),
                    ("metadata[refund_id]", refund_id.to_string()),
                ])
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !response.status().is_success() {
                return Err(RefundRequestError::Unknown);
            }
            let result: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
            RefundHandle {
                provider_refund_id: field(&result, "id")?,
                status: result["status"].as_str().unwrap_or("pending").to_owned(),
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
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !login.status().is_success() {
                return Err(RefundRequestError::Unknown);
            }
            let login: Value = login.json().await.map_err(|_| MerchantCheckoutError)?;
            let token = field(&login, "token")?;
            let amount = serde_json::value::RawValue::from_string(decimal(amount_minor))
                .map_err(|_| MerchantCheckoutError)?;
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
                request_id: refund_id.to_string(),
                payment_intent_id: provider_payment_id.to_string(),
                amount: &amount,
                currency,
                reason,
                metadata,
            };
            let response = http
                .request(
                    Method::POST,
                    base,
                    policy,
                    &["api", "v1", "pa", "refunds", "create"],
                )
                .await
                .map_err(|_| MerchantCheckoutError)?
                .bearer_auth(token)
                .json(&body)
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .map_err(|_| MerchantCheckoutError)?;
            if !response.status().is_success() {
                return Err(RefundRequestError::Unknown);
            }
            let result: Value = response.json().await.map_err(|_| MerchantCheckoutError)?;
            RefundHandle {
                provider_refund_id: field(&result, "id")
                    .or_else(|_| field(&result, "refund_id"))?,
                status: result["status"].as_str().unwrap_or("pending").to_owned(),
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
    urls: &ReturnUrls,
) -> Result<MerchantCheckoutHandle, MerchantCheckoutError> {
    if payment.merchant_id != context.merchant_id || payment.version_id != context.version.id {
        return Err(MerchantCheckoutError);
    }
    let metadata = json!({"payment_id":payment.payment_id,"order_id":payment.payment_id,"organization_id":payment.organization_id});
    let base = &context.version.api_url;
    let policy = &context.version.destination_policy;
    let result = match context.rail {
        MerchantRail::Hyperswitch => hyperswitch::payment(
            context,
            payment,
            http,
            json!({
                "amount": payment.amount_minor, "currency": payment.currency,
                "confirm": false, "capture_method": "automatic", "description": description,
                "return_url": urls.resolve(public_url, payment.payment_id, "&hyperswitch=return"),
                "metadata": metadata,
            }),
        )
        .await?,
        MerchantRail::Stripe => {
            let success_url = urls.resolve(public_url, payment.payment_id, "&session=success");
            let cancel_url = urls.resolve_cancel(public_url, payment.payment_id);
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
                    ("success_url", success_url),
                    ("cancel_url", cancel_url),
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
                return_url: urls.resolve(public_url, payment.payment_id, ""),
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

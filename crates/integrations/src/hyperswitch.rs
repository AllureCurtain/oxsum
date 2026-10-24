//! The Hyperswitch deposit rail (issue #319): the client the top-up endpoint
//! creates payments through, and the HMAC the switch's outgoing webhook
//! verifies with.
//!
//! The switch signs each delivery with HMAC-SHA512 over the exact serialized
//! body and sends the hex digest in `X-Webhook-Signature-512`, keyed by the
//! business profile's `payment_response_hash_key`. Unlike Stripe's `t,v1`
//! envelope there is no timestamp in the header: the event's `event_id` is what
//! makes a replay idempotent.
//!
//! `OXSUM_HYPERSWITCH_BASE_URL` is normally `https://sandbox.hyperswitch.io`
//! (or the self-hosted origin); it is overridable so a test stands its stub
//! where the switch would be. The publishable key never reaches this client —
//! the browser SDK uses it — but the rail carries it so one config answers
//! every environment question.
//!
//! The request shape is the v1 payments API: `POST /payments` with an `api-key`
//! header, the amount in minor units, `confirm: false` (the browser confirms
//! with the returned `client_secret`), automatic capture, and `metadata`
//! naming the deposit row the webhook will credit.

use hmac::{KeyInit, Mac, SimpleHmac};
use serde::Deserialize;
use sha2::Sha512;

/// A configured Hyperswitch rail. `Config` holds it in an `Option`; `None`
/// means the deployment takes no switch payments.
#[derive(Clone)]
pub struct Hyperswitch {
    /// `snd_…`/`live_…`: sent as `api-key` to `POST /payments`.
    api_key: String,
    /// The business profile's `payment_response_hash_key`: verifies
    /// `X-Webhook-Signature-512` on inbound webhooks.
    webhook_secret: String,
    /// The API origin, no trailing slash.
    base_url: String,
    /// `pk_snd_…`/`pk_live_…`: what the browser SDK initializes with.
    publishable_key: String,
    /// The deployment's public origin (`OXSUM_PUBLIC_URL`) — the payment's
    /// `return_url` is built on it.
    public_url: String,
    /// The origin the browser loads `HyperLoader.js` from — the hosted SDK
    /// bundle, or a self-hosted `hyperswitch-web` service. No trailing slash.
    sdk_url: String,
}

impl std::fmt::Debug for Hyperswitch {
    /// `Config` derives `Debug`, so the keys would print with it — the API key
    /// and the webhook secret are redacted here rather than trusting nobody
    /// ever logs a Config.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hyperswitch")
            .field("api_key", &"<redacted>")
            .field("webhook_secret", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("publishable_key", &self.publishable_key)
            .field("public_url", &self.public_url)
            .field("sdk_url", &self.sdk_url)
            .finish()
    }
}

/// What creating a payment returns: the switch's payment id (recorded on the
/// deposit's metadata so the webhook finds it) and the `client_secret` the
/// browser SDK confirms against.
pub struct PaymentHandle {
    /// The switch's `pay_…` id.
    pub payment_id: String,
    /// Fed to the browser SDK; never logged.
    pub client_secret: String,
}

/// What issuing a refund returns: the switch's refund id (recorded on the
/// deposit's metadata so the `refund_*` webhooks find it) and the status the
/// switch answered — `pending` while the connector works it, `succeeded`
/// when the money already moved, `failed` when it refused outright.
pub struct RefundHandle {
    /// The switch's `ref_…` id.
    pub refund_id: String,
    /// `pending`, `succeeded` or `failed` — the initial `refundStatus` the
    /// deposit's `meta` carries.
    pub status: String,
}

/// A leg of the Hyperswitch round-trip failed. The top-up endpoint maps it to
/// 502 — the caller asked for a payment and the switch could not answer one.
pub struct HyperswitchError(pub String);

/// The switch's outgoing webhook, as far as this deployment reads it: the
/// event identity, its type, and the resource it names.
#[derive(Debug, Deserialize)]
pub struct WebhookEvent {
    /// The switch's event id — the idempotency anchor.
    pub event_id: String,
    /// `payment_succeeded`, `payment_failed`, `refund_succeeded`, …
    pub event_type: String,
    /// The resource: `type` is `payment_details`, `refund_details`, …
    pub content: WebhookContent,
}

/// The `content` of an outgoing webhook.
#[derive(Debug, Deserialize)]
pub struct WebhookContent {
    /// `payment_details`, `refund_details`, `dispute_details`, …
    #[serde(rename = "type")]
    pub kind: String,
    /// The matching resource, by the switch's own schema.
    pub object: serde_json::Value,
}

impl Hyperswitch {
    /// Builds the client from its pieces.
    #[must_use]
    pub fn new(
        api_key: String,
        webhook_secret: String,
        base_url: String,
        publishable_key: String,
        public_url: String,
        sdk_url: String,
    ) -> Self {
        Self {
            api_key,
            webhook_secret,
            base_url: base_url.trim_end_matches('/').to_owned(),
            publishable_key,
            public_url: public_url.trim_end_matches('/').to_owned(),
            sdk_url: sdk_url.trim_end_matches('/').to_owned(),
        }
    }

    /// The key the browser SDK initializes with.
    #[must_use]
    pub fn publishable_key(&self) -> &str {
        &self.publishable_key
    }

    /// The origin the browser loads `HyperLoader.js` from (issue #320): the
    /// dashboard prepends it to `/HyperLoader.js` when it mounts Unified
    /// Checkout — the API origin and the SDK origin are different services,
    /// so the rail carries both.
    #[must_use]
    pub fn sdk_url(&self) -> &str {
        &self.sdk_url
    }

    /// Whether `X-Webhook-Signature-512` on `body` verifies under the business
    /// profile's hash key. A missing or malformed header is a failed
    /// verification, not a different error — the caller maps every miss to the
    /// same 401.
    #[must_use]
    pub fn verify_signature(&self, header: &str, body: &[u8]) -> bool {
        let mut mac =
            <SimpleHmac<Sha512> as KeyInit>::new_from_slice(self.webhook_secret.as_bytes())
                .unwrap_or_else(|_| <SimpleHmac<Sha512> as KeyInit>::new(&Default::default()));
        mac.update(body);
        let expected: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        constant_time_eq(expected.as_bytes(), header.trim().as_bytes())
    }

    /// Creates a payment for `amount_minor` of `currency`.
    ///
    /// `deposit_id` is placed in the payment's `metadata` so the webhook can
    /// name the deposit row it confirms, and the `return_url` brings the payer
    /// back to the dashboard. `confirm: false` is deliberate: the browser
    /// confirms with the returned `client_secret`, so the card data never
    /// touches this deployment.
    ///
    /// # Errors
    ///
    /// `HyperswitchError` on a transport failure, a non-2xx answer, or a body
    /// with no `payment_id`/`client_secret`.
    pub async fn create_payment(
        &self,
        http: &reqwest::Client,
        deposit_id: uuid::Uuid,
        amount_minor: i64,
        currency: &str,
    ) -> Result<PaymentHandle, HyperswitchError> {
        #[derive(Deserialize)]
        struct Payment {
            payment_id: String,
            client_secret: String,
        }
        let response = http
            .post(format!("{}/payments", self.base_url))
            .header("api-key", &self.api_key)
            .json(&serde_json::json!({
                "amount": amount_minor,
                "currency": currency,
                "confirm": false,
                "capture_method": "automatic",
                "description": "oxsum credits",
                "return_url": format!("{}/app/topup?hyperswitch=return", self.public_url),
                "metadata": { "deposit_id": deposit_id.to_string() },
            }))
            .send()
            .await
            .map_err(|e| HyperswitchError(format!("payment request failed: {e}")))?;
        if !response.status().is_success() {
            return Err(HyperswitchError(format!(
                "the switch answered {}",
                response.status()
            )));
        }
        let payment: Payment = response
            .json()
            .await
            .map_err(|e| HyperswitchError(format!("payment answer unreadable: {e}")))?;
        Ok(PaymentHandle {
            payment_id: payment.payment_id,
            client_secret: payment.client_secret,
        })
    }

    /// Issues a refund against `payment_id` for `amount_minor` — the
    /// payment-provider half of reversing a credited deposit (issue #360).
    ///
    /// `reason` is the operator's reversal reason, carried to the switch, and
    /// `metadata.deposit_id` names the deposit row so the `refund_*` webhooks
    /// can walk back to it without relying on the payment id alone.
    ///
    /// # Errors
    ///
    /// `HyperswitchError` on a transport failure, a non-2xx answer, or a body
    /// with no `refund_id`. A refusal (the switch will not refund the payment)
    /// surfaces as an error carrying the switch's message — the caller records
    /// it as `refundStatus: failed`, it is never silent.
    pub async fn create_refund(
        &self,
        http: &reqwest::Client,
        payment_id: &str,
        deposit_id: uuid::Uuid,
        amount_minor: i64,
        reason: &str,
    ) -> Result<RefundHandle, HyperswitchError> {
        #[derive(Deserialize)]
        struct Refund {
            refund_id: String,
            status: String,
        }
        let response = http
            .post(format!("{}/refunds", self.base_url))
            .header("api-key", &self.api_key)
            .json(&serde_json::json!({
                "payment_id": payment_id,
                "amount": amount_minor,
                "reason": reason,
                "metadata": { "deposit_id": deposit_id.to_string() },
            }))
            .send()
            .await
            .map_err(|e| HyperswitchError(format!("refund request failed: {e}")))?;
        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(HyperswitchError(format!(
                "the switch refused the refund: {body}"
            )));
        }
        let refund: Refund = response
            .json()
            .await
            .map_err(|e| HyperswitchError(format!("refund answer unreadable: {e}")))?;
        Ok(RefundHandle {
            refund_id: refund.refund_id,
            status: refund.status,
        })
    }
}

/// Constant-time comparison — a signature check must not leak where it
/// stopped matching, or an attacker could rebuild a valid tag one byte at a
/// time.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (l, r) in left.iter().zip(right.iter()) {
        diff |= l ^ r;
    }
    diff == 0
}

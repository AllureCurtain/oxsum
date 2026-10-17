//! The Stripe deposit rail (issue #201): the narrow client the top-up
//! endpoint creates checkout sessions through, and the signature the
//! provider's webhook verifies with. Like the GitHub OAuth client, the
//! base URLs are overridable so a test can stand a stub where `api.stripe.com`
//! would be.
//!
//! Two env-gated credentials: `OXSUM_STRIPE_SECRET_KEY` opens the API (the
//! checkout-session POST), `OXSUM_STRIPE_WEBHOOK_SECRET` verifies the
//! `Stripe-Signature` header on inbound webhooks. Both together or neither —
//! a half-configured rail only fails inside a request, where the operator
//! is not watching.
//!
//! The webhook signature is the same `t=<unix>,v1=<hex>` shape oxsum's own
//! outbound webhooks use (crates/core/src/webhooks.rs): the signed payload is
//! `"{t}.{body}"`, the MAC is HMAC-SHA256 under the endpoint secret.

use hmac::{KeyInit, Mac, SimpleHmac};
use serde::Deserialize;
use sha2::Sha256;

/// A configured Stripe rail: the API key that creates checkout sessions and
/// the webhook secret that verifies inbound signatures. `Config` holds it in
/// an `Option`; `None` means the deployment takes no card payments.
#[derive(Clone)]
pub struct Stripe {
    /// `sk_live_…`/`sk_test_…`: sent `Authorization: Bearer` to `api.stripe.com`.
    secret_key: String,
    /// `whsec_…`: verifies the `Stripe-Signature` header on inbound webhooks.
    webhook_secret: String,
    /// `https://api.stripe.com` normally; overridable so a test points the
    /// checkout-session POST at a stub.
    api_url: String,
    /// The deployment's public origin (`OXSUM_PUBLIC_URL`) — checkout
    /// success/cancel redirects are built on it.
    public_url: String,
}

impl std::fmt::Debug for Stripe {
    /// `Config` derives `Debug`, so the keys would print with it — both are
    /// redacted here rather than trusting nobody ever logs a Config.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stripe")
            .field("secret_key", &"<redacted>")
            .field("webhook_secret", &"<redacted>")
            .field("api_url", &self.api_url)
            .field("public_url", &self.public_url)
            .finish()
    }
}

/// What creating a checkout session returns: the URL the org user's browser
/// is sent to. Stripe answers a session object; only `url` matters here —
/// the deposit id is carried in `client_reference_id`, which the webhook
/// echoes back.
pub struct CheckoutSession {
    /// The hosted-payment-page URL: `https://checkout.stripe.com/…`.
    pub url: String,
    /// Stripe's own session id (`cs_…`), recorded as the deposit's
    /// `payment_ref` so the webhook finds the deposit it credits.
    pub session_id: String,
}

/// A leg of the Stripe round-trip failed. The top-up endpoint maps it to 502
/// — the caller asked for a checkout session and Stripe could not answer one.
pub struct StripeError(pub String);

impl Stripe {
    /// Builds the client from its pieces. `public_url` is the deployment's
    /// public origin; the checkout success/cancel paths are appended when a
    /// session is created.
    #[must_use]
    pub fn new(
        secret_key: String,
        webhook_secret: String,
        api_url: String,
        public_url: String,
    ) -> Self {
        Self {
            secret_key,
            webhook_secret,
            api_url: api_url.trim_end_matches('/').to_owned(),
            public_url: public_url.trim_end_matches('/').to_owned(),
        }
    }

    /// Creates a Stripe Checkout Session for `amount_minor` cents of credit.
    ///
    /// `deposit_id` is placed in `client_reference_id` so the webhook can name
    /// the deposit row it confirms; `session_id` becomes the deposit's
    /// `payment_ref` — the rail-scoped idempotency anchor.
    ///
    /// # Errors
    ///
    /// `StripeError` on a transport failure, a non-2xx answer, or a body with
    /// no `url`/`id`.
    pub async fn create_checkout_session(
        &self,
        http: &reqwest::Client,
        deposit_id: uuid::Uuid,
        amount_minor: i64,
    ) -> Result<CheckoutSession, StripeError> {
        #[derive(Deserialize)]
        struct Session {
            id: String,
            url: Option<String>,
        }
        let session: Session = http
            .post(format!("{}/v1/checkout/sessions", self.api_url))
            .bearer_auth(&self.secret_key)
            .form(&[
                ("mode", "payment".to_owned()),
                ("client_reference_id", deposit_id.to_string()),
                (
                    "success_url",
                    format!("{}/app/topup?session=success", self.public_url),
                ),
                (
                    "cancel_url",
                    format!("{}/app/topup?session=cancel", self.public_url),
                ),
                ("line_items[0][quantity]", "1".to_owned()),
                ("line_items[0][price_data][currency]", "usd".to_owned()),
                (
                    "line_items[0][price_data][unit_amount]",
                    amount_minor.to_string(),
                ),
                (
                    "line_items[0][price_data][product_data][name]",
                    "oxsum credits".to_owned(),
                ),
            ])
            .send()
            .await
            .map_err(|e| StripeError(format!("checkout session request failed: {e}")))?
            .json()
            .await
            .map_err(|e| StripeError(format!("checkout session response unreadable: {e}")))?;
        let url = session
            .url
            .ok_or_else(|| StripeError("checkout session answered no url".into()))?;
        Ok(CheckoutSession {
            url,
            session_id: session.id,
        })
    }

    /// Whether `Stripe-Signature` on `body` verifies under the webhook secret.
    ///
    /// The header is `t=<unix>,v1=<hex>`; the MAC covers `"{t}.{body}"`. A
    /// malformed header is a failed verification, not a different error —
    /// the caller maps every miss to the same 401.
    #[must_use]
    pub fn verify_signature(&self, header: &str, body: &[u8]) -> bool {
        let mut timestamp = None;
        let mut signature = None;
        for part in header.split(',') {
            if let Some(value) = part.strip_prefix("t=") {
                timestamp = Some(value);
            } else if let Some(value) = part.strip_prefix("v1=") {
                signature = Some(value);
            }
        }
        let (Some(t), Some(v1)) = (timestamp, signature) else {
            return false;
        };
        let Ok(t) = t.parse::<i64>() else {
            return false;
        };
        // Recompute the expected header value and compare the signature part —
        // the same construction oxsum's own signature() uses, so the check is
        // byte-for-byte identical to what a Stripe SDK would do.
        let expected = signature_value(&self.webhook_secret, t, body);
        constant_time_eq(expected.as_bytes(), v1.as_bytes())
    }
}

/// The `v1=` half of the signature header: hex(HMAC-SHA256(secret, "{t}.{body}")).
fn signature_value(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = <SimpleHmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| <SimpleHmac<Sha256> as KeyInit>::new(&Default::default()));
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

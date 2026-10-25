//! The Airwallex deposit rail (issue #285): the narrow client the top-up
//! endpoint creates payment intents through, and the signature the provider's
//! webhook verifies with. Like the Stripe client, the base URLs are
//! overridable so a test can stand a stub where `api.airwallex.com` would be.
//!
//! Three credentials, all-or-none at boot like the Stripe trio:
//! `OXSUM_AIRWALLEX_CLIENT_ID` and `OXSUM_AIRWALLEX_API_KEY` open the API
//! (exchanged for a short-lived bearer token at
//! `/api/v1/authentication/login`), `OXSUM_AIRWALLEX_WEBHOOK_SECRET` verifies
//! the `x-signature` header on inbound webhooks. `OXSUM_PUBLIC_URL` roots the
//! `return_url` the payer's browser lands back on.
//!
//! The webhook signature differs from Stripe's `t,v1` envelope: Airwallex
//! sends `x-timestamp` (unix milliseconds) and `x-signature` (hex) as two
//! headers, and the signed content is `timestamp + raw_body` — no separator.
//! The MAC is HMAC-SHA256 under the webhook secret, compared in constant time.
//!
//! The hosted-payment-page redirect needs no signed URL: the page at
//! `checkout.airwallex.com` reads the PaymentIntent `id` from the query
//! string and the SDK completes the handshake against `client_secret`, which
//! the intent answer returns beside it.

use hmac::{KeyInit, Mac, SimpleHmac};
use serde::Deserialize;
use sha2::Sha256;

/// A configured Airwallex rail: the credential pair that mints bearer tokens
/// and the webhook secret that verifies inbound signatures. `Config` holds it
/// in an `Option`; `None` means the deployment takes no Airwallex payments.
#[derive(Clone)]
pub struct Airwallex {
    /// The web-app Developer key pair: sent as `x-client-id`/`x-api-key` to
    /// `/api/v1/authentication/login` for a 30-minute bearer token.
    client_id: String,
    /// The `x-api-key` half of the pair — a secret the way Stripe's
    /// `sk_…` is.
    api_key: String,
    /// Verifies the `x-signature` header on inbound webhooks.
    webhook_secret: String,
    /// `https://api.airwallex.com` normally; overridable so a test points the
    /// login and intent POSTs at a stub.
    api_url: String,
    /// `https://checkout.airwallex.com` normally; the hosted-payment-page
    /// origin the payer's browser is redirected to with the intent id.
    checkout_url: String,
    /// The deployment's public origin (`OXSUM_PUBLIC_URL`) — the intent's
    /// `return_url` is built on it.
    public_url: String,
    /// The HTTP client this rail uses. It is the only outbound call site in
    /// the workspace that caps TLS at 1.2: the demo environment's TLS 1.3
    /// implementation produces records rustls cannot decrypt (issue #374),
    /// while TLS 1.2 answers cleanly. The cap is rail-scoped so the rest of
    /// the workspace keeps TLS 1.3.
    http: reqwest::Client,
    /// The minted bearer token and when it stops being accepted — cached on
    /// the client so a burst of intents costs one login, not one per intent.
    token: std::sync::Arc<tokio::sync::Mutex<Option<Token>>>,
}

/// A bearer token `/api/v1/authentication/login` answered: the value and the
/// UTC instant Airwallex says it expires (30 minutes out, normally). The
/// client refreshes early — a token that dies mid-request is a spurious 502.
struct Token {
    value: String,
    expires_at: time::OffsetDateTime,
}

impl std::fmt::Debug for Airwallex {
    /// `Config` derives `Debug`, so the keys would print with it — the API key
    /// and the webhook secret are redacted here rather than trusting nobody
    /// ever logs a Config.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Airwallex")
            .field("client_id", &"<redacted>")
            .field("api_key", &"<redacted>")
            .field("webhook_secret", &"<redacted>")
            .field("api_url", &self.api_url)
            .field("checkout_url", &self.checkout_url)
            .field("public_url", &self.public_url)
            .finish()
    }
}

/// What creating a payment intent returns: the pieces the deposit row and the
/// payer's browser each need.
pub struct PaymentIntent {
    /// Airwallex's intent id (`int_…`), recorded in `meta` so the webhook
    /// finds the deposit it confirms.
    pub intent_id: String,
    /// The `client_secret` the hosted page's SDK confirms against — stored on
    /// the deposit's `meta` so a replayed intent can answer it again without a
    /// second Airwallex call.
    pub client_secret: String,
    /// The hosted-payment-page URL the browser follows: the checkout origin
    /// with the intent id in the query — the same role Stripe's session `url`
    /// plays.
    pub redirect_url: String,
}

/// A leg of the Airwallex round-trip failed. The top-up endpoint maps it to
/// 502 — the caller asked for a payment intent and Airwallex could not answer
/// one.
pub struct AirwallexError(pub String);

impl Airwallex {
    /// Builds the client from its pieces. `public_url` is the deployment's
    /// public origin; the intent's `return_url` is appended when an intent is
    /// created. `http` is the transport the rail uses — the caller passes one
    /// capped at TLS 1.2 to match this endpoint's record format (issue #374);
    /// an unconstrained client is the default for tests and deployments whose
    /// TLS path is already known to work.
    #[must_use]
    pub fn new(
        client_id: String,
        api_key: String,
        webhook_secret: String,
        api_url: String,
        checkout_url: String,
        public_url: String,
        http: reqwest::Client,
    ) -> Self {
        Self {
            client_id,
            api_key,
            webhook_secret,
            api_url: api_url.trim_end_matches('/').to_owned(),
            checkout_url: checkout_url.trim_end_matches('/').to_owned(),
            public_url: public_url.trim_end_matches('/').to_owned(),
            http,
            token: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Creates a PaymentIntent for `amount_minor` cents of credit.
    ///
    /// `deposit_id` is carried as `merchant_order_id` — the webhook's
    /// `data.object.merchant_order_id` echoes it back, the same role Stripe's
    /// `client_reference_id` plays. `request_id` is the deposit id too: the
    /// provider-side idempotency key, so a retried create never mints a second
    /// intent for one deposit.
    ///
    /// `amount_minor` converts to the wire's decimal major units: credits are
    /// charged USD the way the Stripe rail prices them, and the intent
    /// records `received_minor` in cents when the webhook reports it back.
    ///
    /// # Errors
    ///
    /// `AirwallexError` on a transport failure, a login refusal, a non-2xx
    /// answer, or a body with no `id`/`client_secret`.
    pub async fn create_payment_intent(
        &self,
        deposit_id: uuid::Uuid,
        amount_minor: i64,
    ) -> Result<PaymentIntent, AirwallexError> {
        let token = self.bearer().await?;
        #[derive(Deserialize)]
        struct Intent {
            id: String,
            client_secret: Option<String>,
        }
        let intent_response = self
            .http
            .post(format!("{}/api/v1/pa/payment_intents/create", self.api_url))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "request_id": deposit_id.to_string(),
                "amount": amount_minor as f64 / 100.0,
                "currency": "USD",
                "merchant_order_id": deposit_id.to_string(),
                "return_url": format!("{}/app/topup", self.public_url),
                "order": {
                    "type": "physical_goods",
                    "products": [{
                        "name": "oxsum credits",
                        "quantity": 1,
                        "type": "service",
                        "unit_price": amount_minor as f64 / 100.0,
                    }],
                },
            }))
            .send()
            .await
            .map_err(|e| AirwallexError(format!("payment intent request failed: {e}")))?;
        if !intent_response.status().is_success() {
            let status = intent_response.status();
            let body = intent_response.text().await.unwrap_or_default();
            return Err(AirwallexError(format!(
                "payment intent answered {status}: {}",
                &body[..body.len().min(200)]
            )));
        }
        let intent: Intent = intent_response
            .json()
            .await
            .map_err(|e| AirwallexError(format!("payment intent response unreadable: {e}")))?;
        let client_secret = intent
            .client_secret
            .ok_or_else(|| AirwallexError("payment intent answered no client_secret".into()))?;
        Ok(PaymentIntent {
            redirect_url: format!("{}?intent_id={}", self.checkout_url, intent.id),
            intent_id: intent.id,
            client_secret,
        })
    }

    /// A bearer token for the Payment Acceptance API: the cached one while it
    /// is still good, else a fresh `/api/v1/authentication/login`. The login
    /// answer carries `token` and `expires_at` (RFC 3339); when `expires_at`
    /// is absent the cache assumes the documented 30 minutes minus a margin.
    async fn bearer(&self) -> Result<String, AirwallexError> {
        let now = time::OffsetDateTime::now_utc();
        {
            let guard = self.token.lock().await;
            if let Some(token) = guard.as_ref()
                && token.expires_at > now + time::Duration::minutes(2)
            {
                return Ok(token.value.clone());
            }
        }
        #[derive(Deserialize)]
        struct Login {
            token: String,
            expires_at: Option<String>,
        }
        let response = self
            .http
            .post(format!("{}/api/v1/authentication/login", self.api_url))
            .header("x-client-id", &self.client_id)
            .header("x-api-key", &self.api_key)
            // The login endpoint wants a Content-Length even though the
            // credential headers carry the payload — bodyless POSTs answer
            // 411 with an HTML page, which is what the decode-error path
            // would otherwise surface as "unreadable".
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| AirwallexError(format!("login request failed: {e}")))?;
        if !response.status().is_success() {
            return Err(AirwallexError(format!(
                "login answered {}",
                response.status()
            )));
        }
        let login: Login = response
            .json()
            .await
            .map_err(|e| AirwallexError(format!("login response unreadable: {e}")))?;
        let expires_at = login
            .expires_at
            .and_then(|raw| {
                time::OffsetDateTime::parse(&raw, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_else(|| now + time::Duration::minutes(28));
        let mut guard = self.token.lock().await;
        *guard = Some(Token {
            value: login.token.clone(),
            expires_at,
        });
        Ok(login.token)
    }

    /// Whether `x-signature` on `body` verifies under the webhook secret.
    ///
    /// Airwallex signs `x-timestamp + raw_body` — the timestamp header's value
    /// concatenated with the request body, no separator — and sends the hex
    /// HMAC-SHA256 in `x-signature`. A malformed header pair is a failed
    /// verification, not a different error: the caller maps every miss to the
    /// same 401.
    #[must_use]
    pub fn verify_signature(&self, timestamp: &str, signature: &str, body: &[u8]) -> bool {
        if timestamp.is_empty() || signature.is_empty() {
            return false;
        }
        // Recompute the expected value over `timestamp + body` and compare —
        // the construction Airwallex's own verification docs describe, so the
        // check is byte-for-byte what an Airwallex SDK would do.
        let expected = signature_value(&self.webhook_secret, timestamp, body);
        constant_time_eq(expected.as_bytes(), signature.as_bytes())
    }
}

/// The `x-signature` value: hex(HMAC-SHA256(secret, timestamp + body)).
fn signature_value(secret: &str, timestamp: &str, body: &[u8]) -> String {
    let mut mac = <SimpleHmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| <SimpleHmac<Sha256> as KeyInit>::new(&Default::default()));
    mac.update(timestamp.as_bytes());
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

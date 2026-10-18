//! The x402-rs facilitator client (issue #282): the narrow HTTP client the
//! facilitator-delegated rail talks to — `POST /verify` proves a payment
//! authorization's signature and limits, `POST /settle` lands it on-chain.
//!
//! The facilitator runs as a sidecar (`ghcr.io/x402-rs/x402-facilitator` or
//! the hosted `facilitator.x402.rs`): oxsum keeps the single-binary-plus-
//! Postgres shape by delegating chain RPC, signer keys and finality waits to
//! it rather than embedding them.
//!
//! The wire is the V1 protocol: `{x402Version, paymentPayload,
//! paymentRequirements}` in, `{isValid, payer, invalidReason}` out of verify
//! and `{success, transaction, payer, network, error_reason}` out of settle —
//! snake_case on the settle wire, exactly as the facilitator serves it.

use serde::{Deserialize, Serialize};

/// A configured facilitator endpoint. `Config` holds it inside
/// `X402FacilitatorVerifier`; `base_url` is `OXSUM_X402_FACILITATOR_URL`
/// trimmed of its trailing slash.
#[derive(Clone)]
pub struct Facilitator {
    /// `http://localhost:8080` for the sidecar, `https://facilitator.x402.rs`
    /// for the hosted endpoint — overridable so a test stands a stub there.
    base_url: String,
}

impl std::fmt::Debug for Facilitator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Facilitator")
            .field("base_url", &self.base_url)
            .finish()
    }
}

/// The V1 verify/settle request body. The payload is whatever the caller
/// presented (`{x402Version, scheme, network, payload}` decoded off the
/// base64); the requirements are what the deployment demands the payment
/// satisfy — scheme, network, `maxAmountRequired`, `payTo`, `asset`, resource.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyRequest<'a> {
    x402_version: u32,
    payment_payload: &'a serde_json::Value,
    payment_requirements: &'a serde_json::Value,
}

/// The `/verify` answer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    /// Whether the facilitator honors the payload.
    pub is_valid: bool,
    /// The payer the facilitator decoded — present when valid.
    pub payer: Option<String>,
    /// Why it refused — present when invalid, logged never answered.
    pub invalid_reason: Option<String>,
}

/// The `/settle` answer — snake_case on this wire, unlike verify's camelCase.
#[derive(Deserialize)]
pub struct SettleResponse {
    /// Whether the chain accepted the transfer.
    pub success: bool,
    /// The on-chain transaction hash when `success`.
    pub transaction: Option<String>,
    /// The network the settlement ran on.
    pub network: String,
    /// Why the settlement failed when `success` is false.
    pub error_reason: Option<String>,
}

/// A leg of the facilitator round-trip failed. Callers map it to the same
/// refusal an unverified payload earns — which leg broke is logged, not told.
pub struct FacilitatorError(pub String);

impl Facilitator {
    /// Builds the client around the endpoint's base URL.
    #[must_use]
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// `POST /verify`: the facilitator checks the payload against the
    /// requirements — signature validity, amount, nonce, expiry — without
    /// moving anything. A transport failure is an error, a refusal is an
    /// answer.
    pub async fn verify(
        &self,
        http: &reqwest::Client,
        payment_payload: &serde_json::Value,
        payment_requirements: &serde_json::Value,
    ) -> Result<VerifyResponse, FacilitatorError> {
        http.post(format!("{}/verify", self.base_url))
            .json(&VerifyRequest {
                x402_version: 1,
                payment_payload,
                payment_requirements,
            })
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| FacilitatorError(format!("verify request failed: {e}")))?
            .json()
            .await
            .map_err(|e| FacilitatorError(format!("verify response unreadable: {e}")))
    }

    /// `POST /settle`: lands the verified pair on-chain and answers the
    /// transaction hash. The same request shape as verify — the facilitator
    /// settles exactly what it proved.
    pub async fn settle(
        &self,
        http: &reqwest::Client,
        payment_payload: &serde_json::Value,
        payment_requirements: &serde_json::Value,
    ) -> Result<SettleResponse, FacilitatorError> {
        http.post(format!("{}/settle", self.base_url))
            .json(&VerifyRequest {
                x402_version: 1,
                payment_payload,
                payment_requirements,
            })
            // Settle waits on a chain receipt; the timeout is generous beside
            // verify's, bounded so a stuck RPC cannot hold the request forever.
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| FacilitatorError(format!("settle request failed: {e}")))?
            .json()
            .await
            .map_err(|e| FacilitatorError(format!("settle response unreadable: {e}")))
    }
}

//! The facilitator-delegated x402 verifier (issue #282): the real rail the
//! `RailVerifier` seam was cut for. Where [`Ed25519RailVerifier`] checked a
//! test-scheme signature against a pinned key, this implementation delegates
//! to the x402-rs facilitator sidecar — `POST /verify` proves the EIP-3009
//! authorization's signature and limits, `POST /settle` lands the transfer
//! on-chain and hands back its transaction hash.
//!
//! The caller's `payment` is the x402 `PaymentPayload` — `{x402Version,
//! scheme, network, payload:{signature, authorization:{from, to, value,
//! validAfter, validBefore, nonce}}}` — base64url'd exactly as the protocol's
//! `X-PAYMENT` header carries it. The deployment's requirements (network,
//! asset contract, `payTo` address) come from configuration, not the request:
//! the server states what it will accept, the facilitator proves the payment
//! meets it. `authorization.nonce` lands in `payment_ref`, so one signed
//! authorization credits once ever.
//!
//! The credit mapping is 1:1 — the price the payment names is the chain
//! asset's atomic units (USDC's 6 decimals), the credit the ledger books is
//! the same 1e6-per-unit scale, so `value` = `amountMinor` with no conversion.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use oxsum_core::{RailVerifier, VerifiedPayment};
use serde::Deserialize;

use crate::facilitator::Facilitator;

/// The facilitator-delegated verifier: the sidecar client plus the payment
/// requirements the deployment states. `Config::x402` selects it when the
/// `OXSUM_X402_FACILITATOR_*` set is complete.
pub struct X402FacilitatorVerifier {
    facilitator: Facilitator,
    http: reqwest::Client,
    /// The V1 `network` name — `base-sepolia`, `base`, … (`OXSUM_X402_NETWORK`).
    network: String,
    /// The chain asset the deployment accepts — the USDC contract address
    /// (`OXSUM_X402_ASSET`).
    asset: String,
    /// The address payments must name as `to` (`OXSUM_X402_PAY_TO`).
    pay_to: String,
}

impl std::fmt::Debug for X402FacilitatorVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X402FacilitatorVerifier")
            .field("facilitator", &self.facilitator)
            .field("network", &self.network)
            .field("asset", &self.asset)
            .field("pay_to", &self.pay_to)
            .finish()
    }
}

/// The V1 payment payload's shape — what the caller's `payment` base64
/// decodes to. Only the fields oxsum reads are typed; the rest rides through
/// as raw JSON so the facilitator sees the wire exactly as presented.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaymentPayload {
    x402_version: u32,
    scheme: String,
    network: String,
    payload: PaymentPayloadInner,
}

#[derive(Deserialize)]
struct PaymentPayloadInner {
    authorization: Authorization,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Authorization {
    from: String,
    value: String,
    nonce: String,
}

impl X402FacilitatorVerifier {
    /// Builds the verifier from its pieces. `network`, `asset` and `pay_to`
    /// are the deployment's acceptance criteria; `facilitator` is the sidecar
    /// endpoint; `http` is the shared client the app hands every outbound
    /// call.
    #[must_use]
    pub fn new(
        facilitator: Facilitator,
        http: reqwest::Client,
        network: String,
        asset: String,
        pay_to: String,
    ) -> Self {
        Self {
            facilitator,
            http,
            network,
            asset,
            pay_to,
        }
    }

    /// The `paymentRequirements` the deployment states for a top-up of
    /// `amount_minor` credits — the shape the facilitator's verify/settle
    /// pair consumes. `resource` names what the payment buys; the description
    /// is informational only.
    fn requirements(&self, amount_minor: i64) -> serde_json::Value {
        serde_json::json!({
            "scheme": "exact",
            "network": self.network,
            "maxAmountRequired": amount_minor.to_string(),
            "resource": "https://oxsum/topup",
            "description": "oxsum credit top-up",
            "mimeType": "application/json",
            "payTo": self.pay_to,
            "maxTimeoutSeconds": 60,
            "asset": self.asset,
        })
    }
}

impl RailVerifier for X402FacilitatorVerifier {
    /// Delegates verification to the facilitator: decode the wire, state the
    /// requirements (the caller-declared `amount_minor`, else the value the
    /// authorization itself commits), and let the sidecar check the EIP-3009
    /// signature, amount, nonce and expiry. `None` for anything that is not a
    /// valid, honored payment — parse failure, a foreign network/scheme, a
    /// facilitator refusal or an unreachable sidecar are indistinguishable at
    /// the seam.
    fn verify<'a>(
        &'a self,
        payload: &'a [u8],
        amount_minor: Option<i64>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<VerifiedPayment>> + Send + 'a>,
    > {
        Box::pin(async move {
            let payload_str = std::str::from_utf8(payload).ok()?.trim();
            let json = BASE64URL.decode(payload_str).ok()?;
            let wire: serde_json::Value = serde_json::from_slice(&json).ok()?;
            let parsed: PaymentPayload = serde_json::from_value(wire.clone()).ok()?;
            // The deployment's own gates run before the facilitator's round
            // trip: the V1 wire this speaks, the `exact` scheme oxsum prices,
            // and the configured network — a payment on a chain or scheme we
            // do not serve is refused without asking the sidecar.
            if parsed.x402_version != 1
                || parsed.scheme != "exact"
                || parsed.network != self.network
            {
                return None;
            }
            let declared: i64 = parsed.payload.authorization.value.parse().ok()?;
            let required = amount_minor.unwrap_or(declared);
            if required <= 0 {
                return None;
            }
            let requirements = self.requirements(required);
            let response = self
                .facilitator
                .verify(&self.http, &wire, &requirements)
                .await
                .ok()?;
            if !response.is_valid {
                return None;
            }
            Some(VerifiedPayment {
                amount_minor: required,
                payment_ref: parsed.payload.authorization.nonce,
                payer: response.payer.unwrap_or(parsed.payload.authorization.from),
                // The pair the facilitator verified rides into settle
                // verbatim — the pinned-wire rule: settle exactly what was
                // proven, never a re-serialization of it.
                context: serde_json::json!({
                    "paymentPayload": wire,
                    "paymentRequirements": requirements,
                }),
            })
        })
    }

    /// Lands the verified pair on-chain: `POST /settle` with the same
    /// payload/requirements the verify leg answered on. `None` for a
    /// refused settlement or an unreachable sidecar — the deposit stays
    /// `confirmed`, the caller sees the refusal, and the nonce anchor keeps
    /// the retry clean.
    fn settle<'a>(
        &'a self,
        payment: &'a VerifiedPayment,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Option<oxsum_core::RailSettlement>> + Send + 'a>,
    > {
        Box::pin(async move {
            let response = self
                .facilitator
                .settle(
                    &self.http,
                    &payment.context["paymentPayload"],
                    &payment.context["paymentRequirements"],
                )
                .await
                .ok()?;
            if !response.success {
                return None;
            }
            Some(oxsum_core::RailSettlement {
                tx_hash: response.transaction.unwrap_or_default(),
            })
        })
    }
}

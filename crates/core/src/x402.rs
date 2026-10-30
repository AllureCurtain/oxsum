//! The x402-shaped deposit rail: a mandate-verified crypto payment interface
//! (issue #264).
//!
//! x402-style payment rails and signed spend mandates are the "payment/agent
//! surface" the roadmap deferred — a payer authorizes a charge out of band and
//! the resource server settles it on a payment rail. A real chain integration
//! is out of scope for this wallet; the *interface* is built honestly instead.
//! A caller `POST`s a signed payment authorization — `{scheme, network, asset,
//! amountMinor, payTo, nonce, signature}` — and the rail verifies the
//! signature shape and credits the deposit through the same pending → credited
//! walk every rail shares.
//!
//! The seam is the point: [`RailVerifier`] is the trait a real EIP-712/x402
//! verifier (typed-data hashing, a chain-RPC payer lookup, settlement-finality
//! checking) drops into later. Today's implementation is
//! [`Ed25519RailVerifier`], a stand-in "test scheme" — the same canonical-body
//! Ed25519 `verify_strict` idiom `mandates` uses — that verifies the
//! authorization's signature against a key the operator pins. What is *not*
//! built, deliberately: no chain RPC, no settlement finality check, no real
//! wallet signatures — the verification boundary is stubbed, and the
//! bookkeeping path behind it is real.
//!
//! One deposit per signed `nonce`: `payment_ref` carries it, and the
//! `(rail, organization, payment_ref)` UNIQUE makes a replayed authorization
//! credit once — the same idempotency anchor every rail writes under.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::WalletError;
use crate::wallet::{Receipt, Wallet};

/// The rail `POST /api/v1/topups/x402` writes under.
pub const RAIL_X402: &str = "x402";

/// The scheme today's stand-in verifier accepts — x402's `exact` authorization
/// scheme name kept for shape; a real rail names its own.
pub const SCHEME_EXACT: &str = "exact";
/// The network today's stand-in signs on — deliberately *not* a chain name, so
/// a real verifier's network (`eip155:*`, `solana:*`) cannot collide.
pub const NETWORK_ED25519: &str = "oxsum-ed25519";
/// The only asset the wallet credits.
pub const ASSET_CREDIT: &str = "OXSUM-CREDIT";

/// A verified payment authorization: what the rail credits, and the reference
/// the deposit anchors on.
#[derive(Debug, Clone)]
pub struct VerifiedPayment {
    /// The amount the authorization commits, minor units.
    pub amount_minor: i64,
    /// The payment reference — the signed `nonce`. One authorization credits
    /// once: `(rail, organization, payment_ref)` is the deposit's UNIQUE.
    pub payment_ref: String,
    /// The payer identity the authorization names — an address-shaped string
    /// today, recorded in the deposit's `meta` for reconciliation.
    pub payer: String,
    /// Verifier-private context the `settle` leg needs: the facilitator
    /// verifier stores the `paymentPayload`/`paymentRequirements` pair it
    /// answered `/verify` on, so `settle` re-sends them verbatim (the pinned-
    /// wire rule again — the facilitator settles exactly what it verified).
    /// The stand-in leaves it `Null`; it settles nothing.
    #[doc(hidden)]
    pub context: serde_json::Value,
}

/// What `settle` returns when the rail lands the payment: the on-chain
/// transaction hash the deposit records for traceability. The stand-in never
/// produces one — a test scheme settles nothing on any chain.
#[derive(Debug, Clone)]
pub struct RailSettlement {
    /// The facilitator's `transaction` field — `0x…` on EVM networks.
    pub tx_hash: String,
}

/// Why a verified payment could not land — what the rail knows about a refused
/// or unreachable settlement, for the deposit's own record. The caller still
/// sees the refusal indistinguishably; the reason is reconciliation
/// provenance, written into `deposits.meta` so an operator can read a refused
/// settle without the sidecar's logs (issue #439).
#[derive(Debug, Clone)]
pub struct SettleRefusal {
    /// The facilitator's `error_reason`, or a local description when the rail
    /// could not reach settlement at all (sidecar down, unreadable answer).
    pub reason: String,
}

/// The seam a real crypto rail drops into: given the authorization payload the
/// caller presented, prove it is a payment the rail honors, then land it.
///
/// `verify` answers the [`VerifiedPayment`] the deposit credits — amount,
/// reference and payer — or `None` for a payload the rail does not honor.
/// `settle` moves the money: the facilitator implementation POSTs the verified
/// pair to the sidecar's `/settle` and returns the chain's transaction hash;
/// the stand-in's default implementation settles to `None` — there is nothing
/// to move. A `None` settle after a `Some` verify is the rail's "could not
/// land" — the deposit stays `confirmed`, the caller sees a refusal, and the
/// next attempt replays cleanly off the nonce anchor.
///
/// The methods are boxed futures rather than `async fn` so the trait stays
/// object-safe: `Config` hands every request a `&dyn RailVerifier`.
///
/// `Send + Sync` because the verifier lives on the shared `Config` the app
/// hands every request; `Debug` because `Config` derives it — implementations
/// must redact their own secrets.
pub trait RailVerifier: std::fmt::Debug + Send + Sync {
    /// Verify a presented authorization payload. `None` means the payload is
    /// not a payment this verifier honors — malformed, wrong scheme, or a bad
    /// signature are indistinguishable to the caller.
    fn verify<'a>(
        &'a self,
        payload: &'a [u8],
        amount_minor: Option<i64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<VerifiedPayment>> + Send + 'a>>;

    /// Land a verified payment. `Err` means the rail could not settle — a
    /// refused or unreachable facilitator, a reverted transfer — which the
    /// caller maps to the same refusal an unverified payload earns; the
    /// [`SettleRefusal`] it carries is written into the deposit's `meta` for
    /// reconciliation, never answered to the caller.
    fn settle<'a>(
        &'a self,
        _payment: &'a VerifiedPayment,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RailSettlement, SettleRefusal>> + Send + 'a>,
    > {
        Box::pin(async {
            Err(SettleRefusal {
                reason: "the rail settles nothing".to_owned(),
            })
        })
    }
}

/// The authorization's unsigned body — the fields the signature covers, in
/// declaration order (the canonical order `serde_json` keeps for a struct).
/// The x402 shape — `scheme`, `network`, `asset`, `payTo`, `nonce` — is kept so
/// a real EIP-712 wire maps field-for-field; `signature` is the only field the
/// canonical body omits.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizationBody<'a> {
    scheme: &'a str,
    network: &'a str,
    asset: &'a str,
    amount_minor: i64,
    pay_to: &'a str,
    nonce: &'a str,
    payer: &'a str,
}

/// The wire form the caller presents: the canonical body plus its signature,
/// base64url'd. `signature` signs the canonical serialization of the body
/// *without* it, so the bytes verified are the bytes a signer produced.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorizationWire {
    scheme: String,
    network: String,
    asset: String,
    amount_minor: i64,
    pay_to: String,
    nonce: String,
    payer: String,
    signature: String,
}

/// The stand-in verifier: an Ed25519 signature over the canonical
/// authorization body, checked against a key the operator pins
/// (`OXSUM_X402_VERIFYING_KEY`). Documented as a stand-in — a real x402 rail
/// replaces this with its own [`RailVerifier`] and nothing else changes.
///
/// The pin is the *payer's* public half: the deployment trusts authorizations
/// signed under that key. A deployment without one runs no x402 rail —
/// `POST /api/v1/topups/x402` answers 503, the way the unconfigured Stripe
/// rail does.
#[derive(Debug, Clone)]
pub struct Ed25519RailVerifier {
    key: VerifyingKey,
}

impl Ed25519RailVerifier {
    /// Builds the verifier around the pinned payer key.
    #[must_use]
    pub fn new(key: VerifyingKey) -> Self {
        Self { key }
    }

    /// Parses the pinned key from its hex form — the config's
    /// `OXSUM_X402_VERIFYING_KEY`.
    ///
    /// # Errors
    ///
    /// [`WalletError::Misconfigured`] when the value is not a 32-byte hex key.
    pub fn from_hex(raw: &str) -> Result<Self, WalletError> {
        let mut bytes = [0u8; 32];
        let raw = raw.trim();
        if raw.len() != 64 {
            return Err(WalletError::Misconfigured(
                "OXSUM_X402_VERIFYING_KEY must be 64 hex characters".into(),
            ));
        }
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).map_err(|_| {
                WalletError::Misconfigured("OXSUM_X402_VERIFYING_KEY is not hex".into())
            })?;
        }
        let key = VerifyingKey::from_bytes(&bytes).map_err(|_| {
            WalletError::Misconfigured("OXSUM_X402_VERIFYING_KEY is invalid".into())
        })?;
        Ok(Self::new(key))
    }

    /// Rebuilds the canonical body the signature covers — the unsigned fields
    /// in declaration order, compact JSON, no whitespace. The same rule the
    /// mandate's `canonical_body` pins.
    fn canonical_body(wire: &AuthorizationWire) -> Vec<u8> {
        serde_json::to_vec(&AuthorizationBody {
            scheme: &wire.scheme,
            network: &wire.network,
            asset: &wire.asset,
            amount_minor: wire.amount_minor,
            pay_to: &wire.pay_to,
            nonce: &wire.nonce,
            payer: &wire.payer,
        })
        .unwrap_or_default()
    }
}

impl RailVerifier for Ed25519RailVerifier {
    /// Verifies a base64url'd authorization against the pinned key. `None`
    /// answers every way the payload fails — bad base64, bad JSON, a scheme or
    /// network this stand-in does not serve, or a bad signature — because
    /// which check caught it is the verifier's business.
    fn verify<'a>(
        &'a self,
        payload: &'a [u8],
        _amount_minor: Option<i64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<VerifiedPayment>> + Send + 'a>>
    {
        Box::pin(async move {
            let payload_str = std::str::from_utf8(payload).ok()?.trim();
            let json = BASE64URL.decode(payload_str).ok()?;
            let wire: AuthorizationWire = serde_json::from_slice(&json).ok()?;
            // The stand-in's own shape: an `exact` scheme on the test network,
            // the credit asset. Anything else belongs to a different rail.
            if wire.scheme != SCHEME_EXACT
                || wire.network != NETWORK_ED25519
                || wire.asset != ASSET_CREDIT
            {
                return None;
            }
            if wire.amount_minor <= 0 || wire.nonce.is_empty() {
                return None;
            }
            let sig_bytes: [u8; 64] = BASE64URL
                .decode(wire.signature.trim())
                .ok()?
                .try_into()
                .ok()?;
            let signature = Signature::from_bytes(&sig_bytes);
            // Verify over exactly the canonical bytes — the pinned-wire rule.
            self.key
                .verify_strict(&Self::canonical_body(&wire), &signature)
                .ok()?;
            Some(VerifiedPayment {
                amount_minor: wire.amount_minor,
                payment_ref: wire.nonce,
                payer: wire.payer,
                context: serde_json::Value::Null,
            })
        })
    }

    /// The stand-in "settles" by declaration — the pinned key *is* the
    /// settlement authority in a test, so an empty hash records that no
    /// chain transaction exists. The deposit row stays `NULL` on the column.
    fn settle<'a>(
        &'a self,
        _payment: &'a VerifiedPayment,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RailSettlement, SettleRefusal>> + Send + 'a>,
    > {
        Box::pin(async {
            Ok(RailSettlement {
                tx_hash: String::new(),
            })
        })
    }
}

impl crate::Db {
    /// Credits a verified x402 authorization into `organization`'s wallet:
    /// verify the payload, settle it on the rail once, record the deposit
    /// `confirmed` with the rail's transaction hash, run the ledger credit,
    /// flip the row to `credited` — the same walk the redemption and Stripe
    /// rails take, with `payment_ref` carrying the signed nonce as the
    /// idempotency anchor.
    ///
    /// `amount_minor` is the caller-declared credit amount the rail must
    /// honor: the facilitator verifier builds `paymentRequirements` around it
    /// (the stand-in ignores it — the signed body commits the amount).
    ///
    /// One nonce settles once. The deposit row is claimed `FOR UPDATE` before
    /// settle runs, so two requests racing the same authorization serialize on
    /// the row: the second waits out the first's settle, sees the transaction
    /// hash already recorded, and skips straight to the ledger credit — the
    /// chain is asked to move the money exactly once, because a `transferWithAuthorization`
    /// replay is a revert the caller should not have to hit.
    ///
    /// The `deposit.credited` event and `balance.low` latch-clear fire only on
    /// the actual `confirmed → credited` flip, like every other rail (issue
    /// #200). `received_minor` records what the authorization declared paid —
    /// the same `amount_minor` the signed body commits, so there is no
    /// under/over gap.
    ///
    /// `verifier` decides whether the payload is a payment the rail honors; an
    /// unverified payload or a settlement the rail could not land is
    /// [`WalletError::Forbidden`]. The returned receipt is the wallet's own
    /// answer — `is_new` is `false` on a replay.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for an unverified authorization or a
    /// settlement the rail refuses; storage and ledger failures surface as
    /// [`WalletError`].
    pub async fn credit_x402_deposit(
        &self,
        wallet: &Wallet,
        organization_id: Uuid,
        verifier: &dyn RailVerifier,
        payload: &[u8],
        amount_minor: Option<i64>,
        on: time::Date,
    ) -> Result<Receipt, WalletError> {
        let payment = verifier
            .verify(payload, amount_minor)
            .await
            .ok_or_else(|| WalletError::Forbidden("the payment is not verified".into()))?;

        // Claim the deposit row first and commit it: the INSERT stakes the
        // nonce anchor and the row lands `confirmed`, durable whether or not
        // the settle that follows lands — a refused settle must leave the
        // claim standing so a retry under the same nonce can resume it rather
        // than losing the deposit entirely.
        let deposit_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, \
                  received_minor, status, meta) \
             VALUES ($1, $2, $3, $4, $5, $5, 'confirmed', $6) \
             ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
        )
        .bind(deposit_id)
        .bind(organization_id)
        .bind(RAIL_X402)
        .bind(&payment.payment_ref)
        .bind(payment.amount_minor)
        .bind(serde_json::json!({
            "scheme": SCHEME_EXACT,
            "network": NETWORK_ED25519,
            "asset": ASSET_CREDIT,
            "payer": payment.payer,
        }))
        .execute(self.pool())
        .await?;

        // Now take the row `FOR UPDATE` across the settle call: the lock makes
        // the "did we already move the money" read serial — the loser of a
        // same-nonce race waits for the winner's commit and sees
        // `facilitator_tx_hash` filled rather than settling a second time
        // on-chain. The claim itself is already durable, so a settle refusal
        // rolls back only the hash write, not the deposit.
        let mut tx = self.pool().begin().await?;
        let (status, settled_hash): (String, Option<String>) = sqlx::query_as(
            "SELECT status, facilitator_tx_hash FROM oxsum.deposits \
             WHERE rail = $1 AND organization_id = $2 AND payment_ref = $3 \
             FOR UPDATE",
        )
        .bind(RAIL_X402)
        .bind(organization_id)
        .bind(&payment.payment_ref)
        .fetch_one(&mut *tx)
        .await?;
        if settled_hash.is_none() && status != "credited" {
            // The verify/settle split is the rail's business: the facilitator
            // lands the transfer on-chain here, the stand-in settles by
            // declaration. A rail that cannot land the verified payment
            // refuses the top-up — an unsettled credit would be free money.
            // The lock is held across the call so a concurrent request with
            // the same authorization serializes behind it. A settle refusal
            // commits only the reason write — the deposit claim already
            // committed above, so the row stays `confirmed` for a retry to
            // land (issue #439).
            let settlement = match verifier.settle(&payment).await {
                Ok(settlement) => settlement,
                Err(refusal) => {
                    // The refusal's reason is reconciliation provenance —
                    // recorded on the standing `confirmed` row so an operator
                    // reads *why* the rail would not land the payment without
                    // the sidecar's logs — never answered to the caller.
                    sqlx::query(
                        "UPDATE oxsum.deposits \
                         SET meta = meta || jsonb_build_object('settleRefusal', $1::text), \
                             updated_at = now() \
                         WHERE rail = $2 AND organization_id = $3 AND payment_ref = $4",
                    )
                    .bind(&refusal.reason)
                    .bind(RAIL_X402)
                    .bind(organization_id)
                    .bind(&payment.payment_ref)
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    return Err(WalletError::Forbidden(
                        "the payment could not settle".into(),
                    ));
                }
            };
            sqlx::query(
                "UPDATE oxsum.deposits \
                 SET facilitator_tx_hash = NULLIF($1, ''), updated_at = now() \
                 WHERE rail = $2 AND organization_id = $3 AND payment_ref = $4",
            )
            .bind(&settlement.tx_hash)
            .bind(RAIL_X402)
            .bind(organization_id)
            .bind(&payment.payment_ref)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        // The credit replays under the nonce-derived key: a retried or raced
        // call answers the standing receipt (`is_new: false`), never a second
        // credit.
        let receipt = wallet
            .top_up(
                &format!("x402:{}", payment.payment_ref),
                payment.amount_minor,
                on,
            )
            .await?;

        // Flip the standing row to credited where it is not already — on a
        // replay `deposit_id` is this call's fresh id, so look the row up by
        // the nonce anchor instead. The event and latch-clear fire only on the
        // actual flip.
        let mut tx = self.pool().begin().await?;
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'credited', entry_id = $1, updated_at = now() \
             WHERE rail = $2 AND organization_id = $3 AND payment_ref = $4 \
               AND status <> 'credited'",
        )
        .bind(*receipt.entry_id.as_uuid())
        .bind(RAIL_X402)
        .bind(organization_id)
        .bind(&payment.payment_ref)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if flipped > 0 {
            Self::enqueue_event(
                &mut tx,
                organization_id,
                crate::webhooks::DEPOSIT_CREDITED,
                serde_json::json!({
                    "rail": RAIL_X402,
                    "paymentRef": payment.payment_ref,
                    "amountMinor": payment.amount_minor,
                    "entryId": receipt.entry_id.as_uuid(),
                }),
            )
            .await?;
            if let Ok(available) = wallet.available().await {
                Self::clear_balance_low(&mut tx, organization_id, available).await?;
            }
        }
        tx.commit().await?;
        Ok(receipt)
    }
}

//! The Solana deposit rail (issue #1117): a top-up paid in native SOL or the
//! configured SPL token to the deployment's deposit address, credited once the
//! transfer is found and validated on-chain.
//!
//! The flow follows the Solana Pay recipe: `POST /api/v1/topups/solana` opens
//! a `pending` deposit whose `meta` carries a unique `reference` account — a
//! throwaway address that only marks this intent — and the amount of SOL (or
//! SPL token, in its own minor units) the payment must deliver. The caller
//! pays it from any wallet; the rail then either polls `pending` deposits on
//! a periodic pass or answers `POST /api/v1/topups/solana/confirm` with the
//! caller-supplied signature, and each path runs the same verify: locate the
//! transaction that references the intent (`getSignaturesForAddress`), then
//! prove it honors the terms (`getTransaction` — recipient, amount, mint and
//! the `reference` marker itself).
//!
//! The `payment_ref` anchors on the `reference` address, so one intent opens
//! one deposit ever; the located transaction's signature lands in
//! `facilitator_tx_hash` and keys the ledger credit `solana:<signature>`, so
//! a re-delivered poll or a raced confirm credits nothing twice. The
//! `pending → confirmed → credited` walk, the `deposit.credited` event and
//! the `balance.low` latch-clear are the ones every rail shares.
//!
//! Devnet in this cut: `OXSUM_SOLANA_RATE_MICRO` is the static rate the
//! intent locks in minor-credits per SOL — the stand-in a mainnet deployment
//! swaps for a price oracle recorded per intent. No real funds, no mainnet.

use rand::Rng;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::wallet::Wallet;
use crate::webhooks::DEPOSIT_CREDITED;
use crate::x402::{RailSettlement, RailVerifier, SettleRefusal, VerifiedPayment};

/// The rail `POST /api/v1/topups/solana` writes under.
pub const RAIL_SOLANA: &str = "solana";

/// One Solana deposit-intent `open` may quote an intent this many seconds
/// before the poll expires it (default when the config omits it): Solana
/// Pay's own unbounded-poll guard, so a never-paid intent does not poll
/// forever.
const DEFAULT_INTENT_TTL_SECS: i64 = 600;

/// The rail's deployment: everything `verify` needs to decide whether a
/// transaction honors an intent, and everything `open_solana_deposit` needs
/// to write the intent's `meta`. Mirrors `SolanaConfig` in integrations so
/// core stays free of the HTTP type — the server hands the verifier one
/// already built from the environment.
#[derive(Clone)]
pub struct SolanaTerms {
    /// The deposit address every intent pays (`OXSUM_SOLANA_PAY_TO`).
    pub pay_to: String,
    /// The SPL mint a top-up must pay, or `None` for native SOL.
    pub spl_token_mint: Option<String>,
    /// Base units per whole asset the rail prices in (`OXSUM_SOLANA_DECIMALS`,
    /// 9 for SOL / 6 for USDC). `rate_micro` is per `10^asset_decimals` units —
    /// the conversion is mint-aware so a 6-decimal mint does not misprice by
    /// 1000× the way a fixed-1e9 SOL scale would.
    pub asset_decimals: u32,
    /// Minor-credits per whole asset unit the intent locks
    /// (`OXSUM_SOLANA_RATE_MICRO`) — one whole SOL (10^asset_decimals base
    /// units) buys `rate_micro` minor-credits.
    pub rate_micro: i64,
    /// Seconds a `pending` intent stays payable before the poll expires it.
    pub intent_ttl_secs: i64,
}

impl SolanaTerms {
    /// The default TTL when a deployment does not state one.
    #[must_use]
    pub fn ttl(&self) -> i64 {
        if self.intent_ttl_secs > 0 {
            self.intent_ttl_secs
        } else {
            DEFAULT_INTENT_TTL_SECS
        }
    }
}

/// What a `pending` solana deposit looks up by its `payment_ref`/`reference`.
/// `(deposit_id, organization_id, amount_minor)` — everything `credit_solana_
/// deposit` needs to resume the walk.
type PendingIntent = (Uuid, Uuid, i64);

/// The rail's verifier: knows how to locate and validate a transfer on the
/// configured RPC endpoint against the deployment's terms. The actual RPC
/// client lives in `crates/integrations::solana` — this struct wraps whatever
/// `ChainLookup` the server built (the real client, or a test's stub), so
/// core never speaks JSON-RPC itself.
///
/// `SolanaTerms` is bundled in so `open`/`credit`/`poll` read their criteria
/// from one place rather than threading `pay_to`/`rate_micro` through every
/// call.
#[derive(Clone)]
pub struct SolanaRailVerifier {
    /// The chain lookup the verifier delegates `find_reference` /
    /// `validate_transfer` to.
    chain: std::sync::Arc<dyn ChainLookup>,
    /// The deployment's acceptance criteria.
    terms: SolanaTerms,
}

impl std::fmt::Debug for SolanaRailVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaRailVerifier")
            .field("pay_to", &self.terms.pay_to)
            .field("spl_token_mint", &self.terms.spl_token_mint)
            .field("commitment_rate", &self.terms.rate_micro)
            .finish_non_exhaustive()
    }
}

/// The seam the verifier talks to: the two calls the Solana Pay recipe needs,
/// expressed on the deployment's configured RPC. `crates/integrations::
/// solana::SolanaRpcClient` is the production impl; tests stub it in-process.
/// Object-safe boxed futures so `Config` can hold `Arc<dyn ChainLookup>`.
pub trait ChainLookup: std::fmt::Debug + Send + Sync {
    /// `getSignaturesForAddress(reference)`: the newest confirmed
    /// transactions touching the intent's marker, newest first — a Vec, not a
    /// single hit, because a later dishonoring transaction on the same marker
    /// must not mask an earlier honoring one (the verifier walks the list and
    /// tries each until one validates). Empty when none has landed yet.
    fn find_reference<'a>(
        &'a self,
        reference: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>>;

    /// `getTransaction(signature)`: prove the located transaction pays the
    /// `ExpectedTransfer`'s terms — recipient, delivered amount, mint, and
    /// the `reference` marker present. `None` means it does not.
    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a ExpectedTransfer,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>>;
}

/// The demand a `pending` intent places on the chain-side transfer, mirroring
/// `integrations::solana::ExpectedTransfer` without importing the HTTP type.
#[derive(Debug, Clone)]
pub struct ExpectedTransfer {
    /// The deposit address the transfer must pay.
    pub recipient: String,
    /// The amount it must deliver (lamports, or the SPL mint's base units).
    pub amount: i64,
    /// The SPL mint it must spend, or `None` for native SOL.
    pub spl_token_mint: Option<String>,
    /// The intent's `reference` marker the transaction must carry.
    pub reference: String,
}

/// A located-and-validated transfer, mirroring `integrations::solana::
/// ValidatedTransfer`.
#[derive(Debug, Clone)]
pub struct ValidatedTransfer {
    /// The transaction's signature.
    pub signature: String,
    /// The amount it delivered, in the asset's own minor units.
    pub delivered: i64,
}

impl SolanaRailVerifier {
    /// Builds the verifier around its chain lookup and terms.
    #[must_use]
    pub fn new(chain: std::sync::Arc<dyn ChainLookup>, terms: SolanaTerms) -> Self {
        Self { chain, terms }
    }

    /// The deployment's acceptance terms — the `meta` an intent writes.
    #[must_use]
    pub fn terms(&self) -> &SolanaTerms {
        &self.terms
    }

    /// The terms an opened intent locked in its `meta` — the figures verify
    /// must honor. Falls back to the deployment's current terms for any field
    /// an older intent did not record, so a deposit opened before a config
    /// change still verifies under what it promised. `meta` is the deposit's
    /// own column, not caller input, so a poisoned field simply does not
    /// override — it was written by `open_solana_deposit` itself.
    fn locked_terms(&self, meta: &serde_json::Value) -> SolanaTerms {
        let get = |key: &str| meta.get(key);
        SolanaTerms {
            pay_to: get("payTo")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| self.terms.pay_to.clone(), str::to_owned),
            spl_token_mint: match meta.get("splToken") {
                Some(serde_json::Value::Null) => None,
                Some(v) => v
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| self.terms.spl_token_mint.clone()),
                None => self.terms.spl_token_mint.clone(),
            },
            asset_decimals: get("assetDecimals")
                .and_then(serde_json::Value::as_u64)
                .map_or(self.terms.asset_decimals, |d| d as u32),
            rate_micro: get("rateMicro")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(self.terms.rate_micro),
            intent_ttl_secs: self.terms.intent_ttl_secs,
        }
    }

    /// Verifies a presented intent `payload` against the terms locked in
    /// `meta` — the intent's own promise, not the deployment's live config.
    /// This is what `credit_solana_deposit` calls once it holds the row's
    /// `meta`; a mid-flight `OXSUM_SOLANA_*` change cannot retro-price or
    /// re-target a pending deposit.
    pub async fn verify_locked<'a>(
        &'a self,
        payload: &'a [u8],
        amount_minor: Option<i64>,
        meta: &'a serde_json::Value,
    ) -> Option<VerifiedPayment> {
        let locked = self.locked_terms(meta);
        self.verify_with(payload, amount_minor, &locked).await
    }

    /// The shared verify body: `terms` is either the deployment's live
    /// `SolanaTerms` (the `verify` seam, used wherever no locked intent is in
    /// hand) or the intent's own `locked_terms` (the credit path).
    async fn verify_with(
        &self,
        payload: &[u8],
        amount_minor: Option<i64>,
        terms: &SolanaTerms,
    ) -> Option<VerifiedPayment> {
        let reference = std::str::from_utf8(payload).ok()?.trim();
        if reference.is_empty() {
            return None;
        }
        let signatures = self.chain.find_reference(reference).await;
        // The intent's declared credit amount is what the deposit opened
        // for — `amount_minor` is the caller's claim, the opened deposit's
        // `amount_minor` is authoritative. Convert it to the base units the
        // payment had to deliver at the locked rate and decimals.
        let opened = amount_minor?;
        let expected = credits_to_asset_units(opened, terms.rate_micro, terms.asset_decimals);
        // Walk every signature the marker indexed, newest first, and take the
        // first that honors the intent — a later failed or dishonoring
        // transaction on the same marker must not stall the earlier paying
        // one.
        let mut validated = None;
        for signature in &signatures {
            validated = self
                .chain
                .validate_transfer(
                    signature,
                    &ExpectedTransfer {
                        recipient: terms.pay_to.clone(),
                        amount: expected,
                        spl_token_mint: terms.spl_token_mint.clone(),
                        reference: reference.to_owned(),
                    },
                )
                .await;
            if validated.is_some() {
                break;
            }
        }
        let validated = validated?;
        Some(VerifiedPayment {
            // The credit the ledger books is the opened amount — the payment
            // met its terms. `receivedCredits` converts the delivered asset
            // units back at the locked rate so the deposit records what the
            // chain paid in the ledger's own units; an over/under lands there
            // for a human, never auto-patched.
            amount_minor: opened,
            payment_ref: reference.to_owned(),
            payer: validated.signature.clone(),
            context: json!({
                "signature": validated.signature,
                "delivered": validated.delivered,
                "receivedCredits": asset_units_to_credits(
                    validated.delivered,
                    terms.rate_micro,
                    terms.asset_decimals,
                ),
            }),
        })
    }
}

impl RailVerifier for SolanaRailVerifier {
    /// Verifies a presented intent `payload` — the deposit's `reference`
    /// address — by finding the transaction that references it and proving it
    /// pays the intent's terms. `None` when no matching transfer has landed
    /// yet, or when the located transaction does not honor the intent (wrong
    /// recipient, underpayment, wrong mint, missing marker): every refusal is
    /// indistinguishable at the seam, as the `RailVerifier` contract requires.
    fn verify<'a>(
        &'a self,
        payload: &'a [u8],
        amount_minor: Option<i64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<VerifiedPayment>> + Send + 'a>>
    {
        Box::pin(async move {
            // The generic seam verifies under the deployment's live terms —
            // the credit path overrides with the intent's locked meta via
            // `verify_locked` so a pending deposit can never be retro-priced.
            self.verify_with(payload, amount_minor, &self.terms.clone())
                .await
        })
    }

    /// The chain already settled the transfer by the time `verify` finds it,
    /// so `settle` only confirms the signature `verify` recorded — there is
    /// nothing left to move. The `RailSettlement` carries it for the
    /// `facilitator_tx_hash` column, matching the x402 rail's shape.
    fn settle<'a>(
        &'a self,
        payment: &'a VerifiedPayment,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<RailSettlement, SettleRefusal>> + Send + 'a>,
    > {
        Box::pin(async move {
            Ok(RailSettlement {
                tx_hash: payment.context["signature"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
    }
}

/// Base units per whole asset the rail prices in: `10^asset_decimals`, capped
/// at `i64::MAX`'s range — a mint's decimals are at most 9 on Solana today,
/// but the bound keeps the scale in `i64` whatever the config says.
fn asset_scale(decimals: u32) -> i64 {
    10_i64.saturating_pow(decimals.min(18))
}

/// Converts a credit amount in minor units to the asset units the payment
/// must deliver at the locked rate, rounding *up* (the customer's direction —
/// a fraction of a base unit charges the buyer the whole unit so the seller
/// is never underpaid). `rate_micro` is minor-credits per whole asset
/// (`10^asset_decimals` base units — 1e9 lamports for SOL, 1e6 for USDC), so
/// `units = ceil(credits_minor * 10^decimals / rate_micro)`. Multiplication
/// precedes division and the product saturates, so a huge intent clamps
/// rather than wrapping negative.
fn credits_to_asset_units(credits_minor: i64, rate_micro: i64, decimals: u32) -> i64 {
    if rate_micro <= 0 {
        return 0;
    }
    // ceil(n / d) for positive integers = (n + d - 1) / d. Scale the credit
    // figure by the mint's units-per-whole first so the ceil lands on base
    // units, not on whole units — a fraction of a unit still charges the next
    // base unit up.
    let numerator = credits_minor.saturating_mul(asset_scale(decimals));
    numerator.saturating_add(rate_micro - 1) / rate_micro
}

/// The inverse of [`credits_to_asset_units`]: the credit minor-units a
/// delivered amount of base units is worth at the locked rate —
/// `received_minor`'s figure. `credits = delivered * rate_micro /
/// 10^decimals`, the remainder dropping toward zero (the customer's
/// direction on the seller's side of the rail).
fn asset_units_to_credits(asset_units: i64, rate_micro: i64, decimals: u32) -> i64 {
    if rate_micro <= 0 {
        return 0;
    }
    asset_units
        .saturating_mul(rate_micro)
        .saturating_div(asset_scale(decimals))
}

/// Generates a fresh `reference` marker: 32 random bytes base58-encoded, the
/// same shape a Solana public key takes — it is only ever a read-only account
/// key on the paying transaction, never a signer, so no keypair is kept.
fn new_reference() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bs58::encode(bytes).into_string()
}

impl Db {
    /// Opens a Solana-rail deposit intent: `pending`, keyed by `payment_ref =
    /// <reference>` — the per-intent marker address the paying transaction
    /// must carry, minted here so the UNIQUE anchor is a value only this
    /// intent could name. Returns the `(deposit_id, reference, amount_sol)` the
    /// caller needs to pay, or `None`'s `false` on a `payment_ref` collision —
    /// the same replay-safe insert every `open_*` rail makes.
    ///
    /// `meta` records what the intent declares: the `reference`, the `payTo`
    /// address and `splToken` (null for SOL), the `solLamports` the payment
    /// must deliver at the locked rate, the `rateSource`/`rateLockedAt` of the
    /// static devnet rate, and the `expiresAt` the poll honors.
    pub async fn open_solana_deposit(
        &self,
        organization_id: Uuid,
        deposit_id: Uuid,
        amount_minor: i64,
        terms: &SolanaTerms,
    ) -> Result<Option<(String, i64, i64)>, WalletError> {
        if amount_minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a deposit's amount must be positive".into(),
            ));
        }
        let reference = new_reference();
        let lamports = credits_to_asset_units(amount_minor, terms.rate_micro, terms.asset_decimals);
        let expires_at = time::OffsetDateTime::now_utc() + time::Duration::seconds(terms.ttl());
        let inserted = sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, status, meta) \
             VALUES ($1, $2, $3, $4, $5, 'pending', $6) \
             ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
        )
        .bind(deposit_id)
        .bind(organization_id)
        .bind(RAIL_SOLANA)
        .bind(&reference)
        .bind(amount_minor)
        .bind(json!({
            "reference": reference,
            "payTo": terms.pay_to,
            "splToken": terms.spl_token_mint,
            "solLamports": lamports,
            "rateMicro": terms.rate_micro,
            "assetDecimals": terms.asset_decimals,
            "rateSource": "config",
            "rateLockedAt": time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            "expiresAt": expires_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        }))
        .execute(self.pool())
        .await?
        .rows_affected();
        if inserted == 0 {
            return Ok(None);
        }
        Ok(Some((reference, lamports, terms.ttl())))
    }

    /// Finds the pending intent a `reference` names — the lookup the confirm
    /// route runs on a replayed submission, answering the same `(deposit_id,
    /// organization_id, amount_minor)` rather than opening a second intent.
    /// An intent past its `meta.expiresAt` is not payable — the same gate the
    /// poll's expiry sweep enforces — so a payment landing after expiry never
    /// finds its intent here.
    pub async fn find_solana_deposit(
        &self,
        organization_id: Uuid,
        reference: &str,
    ) -> Result<Option<PendingIntent>, WalletError> {
        let row = sqlx::query(
            "SELECT deposit_id, organization_id, amount_minor \
             FROM oxsum.deposits \
             WHERE rail = $1 AND organization_id = $2 AND payment_ref = $3 \
               AND status = 'pending' \
               AND (meta->>'expiresAt')::timestamptz > now()",
        )
        .bind(RAIL_SOLANA)
        .bind(organization_id)
        .bind(reference)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => Ok(Some((
                row.try_get("deposit_id")?,
                row.try_get("organization_id")?,
                row.try_get("amount_minor")?,
            ))),
            None => Ok(None),
        }
    }

    /// Verifies and credits a `pending` solana deposit: the poll pass and the
    /// confirm route share this walk. The deposit row is taken `FOR UPDATE`,
    /// the `reference` in `meta` drives `verifier.verify`, the located
    /// transaction's signature is recorded in `facilitator_tx_hash`, and the
    /// ledger credit runs under `solana:<signature>` — a replayed poll or a
    /// raced confirm replays the standing receipt rather than crediting
    /// twice. `received_minor` records the asset units the chain delivered
    /// converted back at the locked rate; a mismatch with `amount_minor`
    /// stays for a human.
    ///
    /// Returns the receipt when a credit ran, `None` when the deposit was
    /// already `credited` or is no longer `pending`. A verification failure
    /// (no matching transfer yet, or a dishonoring one) is `Err(Forbidden)` —
    /// the poll surfaces it as "not paid yet" and the confirm route refuses
    /// the submission.
    pub async fn credit_solana_deposit(
        &self,
        wallet: &Wallet,
        deposit_id: Uuid,
        verifier: &SolanaRailVerifier,
        on: time::Date,
    ) -> Result<Option<crate::wallet::Receipt>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT organization_id, amount_minor, status, payment_ref, meta \
             FROM oxsum.deposits \
             WHERE deposit_id = $1 AND rail = $2 \
               AND (meta->>'expiresAt')::timestamptz > now() \
             FOR UPDATE",
        )
        .bind(deposit_id)
        .bind(RAIL_SOLANA)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let organization_id: Uuid = row.try_get("organization_id")?;
        let amount_minor: i64 = row.try_get("amount_minor")?;
        let status: String = row.try_get("status")?;
        let payment_ref: String = row.try_get("payment_ref")?;
        let meta: serde_json::Value = row.try_get("meta")?;
        // Only a `pending` intent proceeds — `confirmed`/`credited` is done,
        // `expired` lapsed.
        if status != "pending" {
            tx.commit().await?;
            return Ok(None);
        }
        // Verify under the terms this intent locked in `meta` — its own
        // promise, not the deployment's live config, so a mid-flight
        // OXSUM_SOLANA_* change cannot reprice or re-target it (issue #1127).
        let payment = verifier
            .verify_locked(payment_ref.as_bytes(), Some(amount_minor), &meta)
            .await
            .ok_or_else(|| WalletError::Forbidden("the payment is not verified".into()))?;
        // Record the located signature before the credit runs — the chain's
        // own idempotency anchor, durable whether the credit lands this pass.
        let settlement = verifier
            .settle(&payment)
            .await
            .map_err(|refusal| WalletError::Forbidden(refusal.reason.clone()))?;
        // One signature credits one deposit ever: a transaction carrying two
        // references pays once, so a second intent naming an already-used
        // signature must be refused, not credited in a second organization's
        // ledger (issue #1124). The pre-check gives a clean refusal; the
        // `deposits_solana_tx_hash` partial unique index is the backstop for
        // the race where two intents claim the same signature at once.
        let already_used = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM oxsum.deposits \
             WHERE rail = $1 AND facilitator_tx_hash = $2 AND deposit_id <> $3 \
             LIMIT 1",
        )
        .bind(RAIL_SOLANA)
        .bind(&settlement.tx_hash)
        .bind(deposit_id)
        .fetch_optional(&mut *tx)
        .await?
        .is_some();
        if already_used {
            return Err(WalletError::Forbidden(
                "the paying transaction already credited another deposit".into(),
            ));
        }
        sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'confirmed', facilitator_tx_hash = $1, \
                 received_minor = $2, updated_at = now() \
             WHERE deposit_id = $3 AND status = 'pending'",
        )
        .bind(&settlement.tx_hash)
        .bind(payment.context["receivedCredits"].as_i64())
        .bind(deposit_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            // A unique violation on deposits_solana_tx_hash means a concurrent
            // intent claimed this signature first — a double-spend refusal,
            // not a storage fault.
            if let sqlx::Error::Database(ref db) = e
                && db.code().as_deref() == Some("23505")
            {
                return WalletError::Forbidden(
                    "the paying transaction already credited another deposit".into(),
                );
            }
            WalletError::from(e)
        })?;
        // The credit replays under the signature-derived key: a retried or
        // raced call answers the standing receipt (`is_new: false`), never a
        // second credit.
        let credit_key = format!("solana:{}", settlement.tx_hash);
        let receipt = wallet.top_up(&credit_key, payment.amount_minor, on).await?;
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'credited', entry_id = $1, \
                 meta = meta || jsonb_build_object('signature', $2::text), \
                 updated_at = now() \
             WHERE deposit_id = $3 AND status <> 'credited'",
        )
        .bind(*receipt.entry_id.as_uuid())
        .bind(&settlement.tx_hash)
        .bind(deposit_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if flipped > 0 {
            Self::enqueue_event(
                &mut tx,
                organization_id,
                DEPOSIT_CREDITED,
                json!({
                    "rail": RAIL_SOLANA,
                    "paymentRef": payment_ref,
                    "amountMinor": payment.amount_minor,
                    "signature": settlement.tx_hash,
                    "entryId": receipt.entry_id.as_uuid(),
                }),
            )
            .await?;
            if let Ok(available) = wallet.available().await {
                Self::clear_balance_low(&mut tx, organization_id, available).await?;
            }
        }
        tx.commit().await?;
        Ok(Some(receipt))
    }

    /// Moves a Solana deposit to `expired` — the poll's outcome once
    /// `meta.expiresAt` has passed without a matching transfer, a no-op on a
    /// deposit that already ended its walk.
    pub async fn expire_solana_deposit(&self, deposit_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.deposits SET status = 'expired', updated_at = now() \
             WHERE deposit_id = $1 AND rail = $2 AND status IN ('pending', 'confirmed')",
        )
        .bind(deposit_id)
        .bind(RAIL_SOLANA)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The periodic pass: walks every `pending` solana deposit whose
    /// `meta.expiresAt` has not passed, runs `credit_solana_deposit` under a
    /// `FOR UPDATE` claim, and expires the lapsed ones. Unleadered — each row
    /// serializes on its own lock and the `solana:<signature>` credit key
    /// makes a raced pass a replay, never a double credit.
    ///
    /// Returns `(checked, credited, expired)` counts for the job's log line.
    /// The `tenants` map resolves each deposit's organization to its wallet —
    /// the same pattern `recover_x402_credits` uses.
    pub async fn poll_solana_deposits(
        &self,
        verifier: &SolanaRailVerifier,
        tenants: &crate::Tenants,
        on: time::Date,
    ) -> Result<(u64, u64, u64), WalletError> {
        // Fetch the work list first: pending intents not yet expired, oldest
        // first so a backlog drains in order. Expired rows are swept after.
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT d.deposit_id, d.organization_id \
             FROM oxsum.deposits d \
             WHERE d.rail = $1 AND d.status = 'pending' \
               AND (d.meta->>'expiresAt')::timestamptz > now() \
             ORDER BY d.created_at \
             LIMIT 256",
        )
        .bind(RAIL_SOLANA)
        .fetch_all(self.pool())
        .await?;
        // Expire lapsed intents in one statement — the same status-guarded
        // sweep every `expire_*` variant makes: `WHERE status='pending'` skips
        // a row mid-credit, which lands confirmed/credited under its own lock,
        // so the sweep races a credit safely without holding a lock itself.
        let expired_count = sqlx::query(
            "UPDATE oxsum.deposits SET status = 'expired', updated_at = now() \
             WHERE rail = $1 AND status = 'pending' \
               AND (meta->>'expiresAt')::timestamptz <= now()",
        )
        .bind(RAIL_SOLANA)
        .execute(self.pool())
        .await?
        .rows_affected();

        let mut credited: u64 = 0;
        for (deposit_id, organization_id) in &rows {
            // Resolve the organization to its wallet — same lookup the route
            // layer runs per request.
            let tenant_id: String = sqlx::query_scalar(
                "SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1",
            )
            .bind(organization_id)
            .fetch_one(self.pool())
            .await?;
            let wallet = tenants.get(&tenant_id).await?;
            match self
                .credit_solana_deposit(&wallet, *deposit_id, verifier, on)
                .await
            {
                Ok(Some(_)) => credited += 1,
                Ok(None) => {}
                // A verification refusal is "not paid yet" — the row stays
                // pending for the next pass.
                Err(WalletError::Forbidden(_)) => {}
                // Any other failure logs and moves to the next intent: one
                // bad row must not stall every intent behind it for another
                // whole pass.
                Err(error) => {
                    tracing::warn!(
                        deposit_id = %deposit_id,
                        error = %error,
                        "the solana deposit credit failed; the row stays pending"
                    );
                }
            }
        }
        Ok((rows.len() as u64, credited, expired_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conversion is mint-aware: `rate_micro` minor-credits buy one whole
    /// asset, and the mint's decimals — not a hardcoded 1e9 — set the base
    /// units a whole asset divides into. SOL (9) and USDC (6) price the same
    /// credit figure into very different base units.
    #[test]
    fn credits_to_asset_units_honors_the_mint_decimals() {
        // 1000 minor-credits at 200_000_000 minor/whole unit.
        // SOL (9 decimals): 1000 * 1e9 / 200e6 = 5_000 lamports.
        assert_eq!(credits_to_asset_units(1000, 200_000_000, 9), 5_000);
        // USDC (6 decimals): 1000 * 1e6 / 200e6 = 5 base units. A fixed-1e9
        // scale would have priced this 1000× too high — the bug this fixes.
        assert_eq!(credits_to_asset_units(1000, 200_000_000, 6), 5);
    }

    #[test]
    fn credits_to_asset_units_rounds_up_for_the_seller() {
        // A fraction of a base unit charges the next whole unit up.
        assert_eq!(credits_to_asset_units(1, 200_000_000, 9), 5);
        assert_eq!(credits_to_asset_units(1, 200_000_000, 6), 1);
    }

    #[test]
    fn asset_units_to_credits_is_the_inverse() {
        assert_eq!(asset_units_to_credits(5_000, 200_000_000, 9), 1000);
        assert_eq!(asset_units_to_credits(5, 200_000_000, 6), 1000);
        // The remainder drops toward zero (seller's side of the rail).
        assert_eq!(asset_units_to_credits(4_999, 200_000_000, 9), 999);
    }
}

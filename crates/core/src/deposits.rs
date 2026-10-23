//! Deposits: the single record of money arriving, one row per rail payment (decision D2,
//! issue #118).
//!
//! Every rail — the manual top-up, a redemption code, later Stripe or a chain watcher —
//! writes one row on `oxsum.deposits` keyed by `(rail, payment_ref)`, and every row that
//! reaches `credited` names the ledger entry that moved the wallet. The purchased pool is
//! what deposits credit: granted credit never arrives through a rail.
//!
//! The redemption rail owns `oxsum.redemption_codes`: the operator mints a batch of
//! `oxr-` codes and the holder redeems one through [`Db::redeem_code`]. Like invitation
//! tokens and API keys the table keeps only the code's SHA-256, so a dump redeems
//! nothing and the mint answer is the only place the plaintext exists.

use std::fmt::Write;

use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::wallet::{Receipt, Wallet};

/// Every redemption code starts with this, so the value is recognizable in logs.
const CODE_MARK: &str = "oxr-";
const CODE_BYTES: usize = 32;
/// The most codes one mint call may produce.
pub const MAX_BATCH: i64 = 1000;

/// The rail `POST /api/v1/topups` writes under.
pub const RAIL_MANUAL: &str = "manual";
/// The rail redemption codes write under.
pub const RAIL_REDEMPTION: &str = "redemption";
/// The rail Stripe Checkout writes under (issue #201).
pub const RAIL_STRIPE: &str = "stripe";
/// The rail Airwallex's hosted payment page writes under (issue #285).
pub const RAIL_AIRWALLEX: &str = "airwallex";
/// The rail the Hyperswitch switch writes under (issue #319).
pub const RAIL_HYPERSWITCH: &str = "hyperswitch";

/// A freshly minted batch: the only time the codes are visible.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeBatch {
    pub batch_id: Uuid,
    pub count: i64,
    pub amount_minor: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    pub codes: Vec<String>,
}

/// What a redeem answers: the ledger receipt of the credit and the amount it carried.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Redemption {
    #[serde(flatten)]
    pub receipt: Receipt,
    pub amount_minor: i64,
}

impl Db {
    /// Records a manual-rail deposit for a credit the wallet already booked: every
    /// `POST /topups` leaves a row here too, so `deposits` is the one table
    /// reconciliation (P4-1) reads for money in. The caller's idempotency key is the
    /// payment reference — a replayed top-up sees its own row and writes nothing.
    ///
    /// A landing deposit enqueues `deposit.credited` for every subscribed webhook
    /// endpoint in the same transaction (issue #200), so the row and its
    /// notification commit or roll back together — and a replayed top-up, which
    /// inserts no row, enqueues nothing. The credit also clears every armed
    /// `balance.low` latch the new balance now sits above.
    ///
    /// `available` is the wallet's balance after the credit, read by the caller
    /// that holds the wallet; the latch clears only when the deposit lifted the
    /// organization back over the line.
    pub async fn record_manual_deposit(
        &self,
        organization_id: Uuid,
        idempotency_key: &str,
        amount_minor: i64,
        entry_id: Uuid,
        available: Option<i64>,
    ) -> Result<(), WalletError> {
        let mut tx = self.pool().begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, \
                  received_minor, status, entry_id) \
             VALUES ($1, $2, $3, $4, $5, $5, 'credited', $6) \
             ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(organization_id)
        .bind(RAIL_MANUAL)
        .bind(idempotency_key)
        .bind(amount_minor)
        .bind(entry_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted > 0 {
            Self::enqueue_event(
                &mut tx,
                organization_id,
                crate::webhooks::DEPOSIT_CREDITED,
                serde_json::json!({
                    "rail": RAIL_MANUAL,
                    "paymentRef": idempotency_key,
                    "amountMinor": amount_minor,
                    "entryId": entry_id,
                }),
            )
            .await?;
            if let Some(available) = available {
                Self::clear_balance_low(&mut tx, organization_id, available).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Opens a Stripe-rail deposit before the checkout session exists: `pending`,
    /// keyed by `payment_ref = <idempotency_key>`, which stays the row's anchor
    /// for its whole life — the session id lands in `meta` when Stripe answers.
    /// The caller's idempotency key makes the intent row itself replay-safe: a
    /// retried `POST /topups/stripe` with the same key finds the same pending
    /// deposit rather than opening a second one.
    ///
    /// The deposit must exist before the session is created: the session's
    /// `client_reference_id` carries the deposit id, so a row must already be
    /// there for the webhook to find.
    ///
    /// Returns `true` when the row was inserted — a `false` answer means the
    /// idempotency key already opened this deposit, and the caller should look
    /// up the existing row rather than open a second checkout session.
    ///
    /// A non-positive amount is refused the way every credit write is.
    pub async fn open_stripe_deposit(
        &self,
        organization_id: Uuid,
        deposit_id: Uuid,
        idempotency_key: &str,
        amount_minor: i64,
    ) -> Result<bool, WalletError> {
        if amount_minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a deposit's amount must be positive".into(),
            ));
        }
        let inserted = sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, status) \
             VALUES ($1, $2, $3, $4, $5, 'pending') \
             ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
        )
        .bind(deposit_id)
        .bind(organization_id)
        .bind(RAIL_STRIPE)
        .bind(idempotency_key)
        .bind(amount_minor)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(inserted > 0)
    }

    /// Finds the pending deposit an idempotency key already opened — the lookup
    /// `POST /topups/stripe` runs on a replayed intent, to answer the same
    /// checkout URL instead of minting a second Stripe session. Returns the
    /// deposit's id and the checkout URL Stripe answered when the session was
    /// created (recorded in `meta`).
    pub async fn find_stripe_deposit(
        &self,
        organization_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<(Uuid, Option<String>)>, WalletError> {
        let row = sqlx::query(
            "SELECT deposit_id, meta->>'stripeCheckoutUrl' AS checkout_url \
             FROM oxsum.deposits \
             WHERE rail = $1 AND organization_id = $2 AND payment_ref = $3",
        )
        .bind(RAIL_STRIPE)
        .bind(organization_id)
        .bind(idempotency_key)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => {
                use sqlx::Row;
                Ok(Some((
                    row.try_get("deposit_id")?,
                    row.try_get("checkout_url")?,
                )))
            }
            None => Ok(None),
        }
    }

    /// Records Stripe's session id and checkout URL in the deposit's metadata.
    /// The `payment_ref` stays the caller's idempotency key — the UNIQUE
    /// constraint is what makes a replayed intent idempotent — while
    /// `meta.stripeSessionId` is the handle the webhook names when it comes
    /// back, and `meta.stripeCheckoutUrl` is what a replayed intent answers.
    pub async fn stripe_session_id(
        &self,
        deposit_id: Uuid,
        session_id: &str,
        checkout_url: &str,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.deposits \
             SET meta = meta || jsonb_build_object(\
                     'stripeSessionId', $1::text, \
                     'stripeCheckoutUrl', $2::text\
                 ), \
                 updated_at = now() \
             WHERE deposit_id = $3 AND rail = $4 AND status = 'pending'",
        )
        .bind(session_id)
        .bind(checkout_url)
        .bind(deposit_id)
        .bind(RAIL_STRIPE)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Finds the Stripe deposit a webhook names: `meta.stripeSessionId`
    /// matches the session Stripe calls back for. `payment_ref` is the
    /// caller's idempotency key, not the session id — the webhook identifies
    /// the deposit through the session id, which the intent handler wrote
    /// into metadata when the session was created.
    pub async fn find_stripe_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<(Uuid, Uuid, String)>, WalletError> {
        let row = sqlx::query(
            "SELECT d.deposit_id, d.organization_id, o.tenant_id \
             FROM oxsum.deposits d \
             JOIN oxsum.organizations o ON o.organization_id = d.organization_id \
             WHERE d.rail = $1 AND d.meta->>'stripeSessionId' = $2 \
             ORDER BY d.created_at DESC LIMIT 1",
        )
        .bind(RAIL_STRIPE)
        .bind(session_id)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => {
                use sqlx::Row;
                Ok(Some((
                    row.try_get("deposit_id")?,
                    row.try_get("organization_id")?,
                    row.try_get("tenant_id")?,
                )))
            }
            None => Ok(None),
        }
    }

    /// Moves a Stripe deposit to `confirmed` — the webhook's first act on
    /// `checkout.session.completed`, before the ledger credit runs. The row is
    /// taken `FOR UPDATE` so two concurrent webhook deliveries for the same
    /// session cannot both proceed to credit.
    ///
    /// Returns `true` when the row moved, `false` when it was already past
    /// `pending` — a replayed or raced webhook answers `false` and credits
    /// nothing twice.
    pub async fn confirm_stripe_deposit(&self, deposit_id: Uuid) -> Result<bool, WalletError> {
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'confirmed', updated_at = now() \
             WHERE deposit_id = $1 AND rail = $2 AND status = 'pending'",
        )
        .bind(deposit_id)
        .bind(RAIL_STRIPE)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(flipped > 0)
    }

    /// Credits the wallet for a confirmed Stripe deposit and flips the row to
    /// `credited` — the same `deposit.credited` event and `balance.low`
    /// latch-clear as every other rail, in the deposit-update transaction.
    ///
    /// The credit runs under the fixed key `stripe:<session_id>`: a replayed
    /// webhook finds its own credited deposit and writes nothing twice, and a
    /// crash between the ledger append and the row flip leaves a confirmed
    /// deposit the retry resumes.
    ///
    /// `received_minor` is what Stripe reports the session paid: a number that
    /// disagrees with `amount_minor` is recorded for a human, never
    /// auto-patched (decision D2).
    ///
    /// Returns the receipt and organization id when a credit ran, `None` when
    /// the deposit was already credited or was never confirmed.
    pub async fn credit_stripe_deposit(
        &self,
        wallet: &Wallet,
        deposit_id: Uuid,
        session_id: &str,
        received_minor: Option<i64>,
        on: time::Date,
    ) -> Result<Option<(Receipt, Uuid)>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT organization_id, amount_minor, status, payment_ref \
             FROM oxsum.deposits \
             WHERE deposit_id = $1 AND rail = $2 \
             FOR UPDATE",
        )
        .bind(deposit_id)
        .bind(RAIL_STRIPE)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        use sqlx::Row;
        let organization_id: Uuid = row.try_get("organization_id")?;
        let amount_minor: i64 = row.try_get("amount_minor")?;
        let status: String = row.try_get("status")?;
        let payment_ref: String = row.try_get("payment_ref")?;
        // `confirmed` is the only state a credit proceeds from: `pending` was
        // never confirmed (the session expired before paying), `credited` is
        // already done.
        if status != "confirmed" {
            tx.commit().await?;
            return Ok(None);
        }
        // The ledger append replays under the session id — a webhook retried
        // after a crash finds its own entry rather than opening a second one.
        let receipt = wallet
            .top_up(&format!("stripe:{session_id}"), amount_minor, on)
            .await?;
        // Record what the rail reported paid; a mismatch with amount_minor is
        // a human's call, never auto-patched (decision D2).
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'credited', entry_id = $1, received_minor = $2, \
                 updated_at = now() \
             WHERE deposit_id = $3 AND status = 'confirmed'",
        )
        .bind(*receipt.entry_id.as_uuid())
        .bind(received_minor)
        .bind(deposit_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if flipped > 0 {
            Self::enqueue_event(
                &mut tx,
                organization_id,
                crate::webhooks::DEPOSIT_CREDITED,
                serde_json::json!({
                    "rail": RAIL_STRIPE,
                    "paymentRef": payment_ref,
                    "amountMinor": amount_minor,
                    "entryId": receipt.entry_id.as_uuid(),
                }),
            )
            .await?;
            if let Ok(available) = wallet.available().await {
                Self::clear_balance_low(&mut tx, organization_id, available).await?;
            }
        }
        tx.commit().await?;
        Ok(Some((receipt, organization_id)))
    }

    /// Moves a Stripe deposit to `expired` — the `checkout.session.expired`
    /// webhook's terminal state, a no-op on a row that already ended its walk.
    pub async fn expire_stripe_deposit(&self, deposit_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.deposits SET status = 'expired', updated_at = now() \
             WHERE deposit_id = $1 AND rail = $2 AND status IN ('pending', 'confirmed')",
        )
        .bind(deposit_id)
        .bind(RAIL_STRIPE)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Opens an Airwallex-rail deposit before the payment intent exists:
    /// `pending`, keyed by `payment_ref = <idempotency_key>`, which stays the
    /// row's anchor for its whole life — the intent id lands in `meta` when
    /// Airwallex answers. Same shape [`Db::open_stripe_deposit`] proves: the
    /// caller's key makes the intent row itself replay-safe.
    ///
    /// Returns `true` when the row was inserted — a `false` answer means the
    /// idempotency key already opened this deposit, and the caller should look
    /// up the existing row rather than open a second intent.
    pub async fn open_airwallex_deposit(
        &self,
        organization_id: Uuid,
        deposit_id: Uuid,
        idempotency_key: &str,
        amount_minor: i64,
    ) -> Result<bool, WalletError> {
        if amount_minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a deposit's amount must be positive".into(),
            ));
        }
        let inserted = sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, status) \
             VALUES ($1, $2, $3, $4, $5, 'pending') \
             ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
        )
        .bind(deposit_id)
        .bind(organization_id)
        .bind(RAIL_AIRWALLEX)
        .bind(idempotency_key)
        .bind(amount_minor)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(inserted > 0)
    }

    /// Finds the pending deposit an idempotency key already opened — the
    /// lookup `POST /topups/airwallex` runs on a replayed intent, to answer
    /// the same hosted-page URL and `client_secret` instead of minting a
    /// second PaymentIntent. Returns the deposit's id and the intent id,
    /// `client_secret` and URL recorded in `meta`.
    pub async fn find_airwallex_deposit(
        &self,
        organization_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<(Uuid, Option<String>, Option<String>, Option<String>)>, WalletError> {
        let row = sqlx::query(
            "SELECT deposit_id, meta->>'airwallexIntentId' AS intent_id, \
                    meta->>'airwallexClientSecret' AS client_secret, \
                    meta->>'airwallexCheckoutUrl' AS checkout_url \
             FROM oxsum.deposits \
             WHERE rail = $1 AND organization_id = $2 AND payment_ref = $3",
        )
        .bind(RAIL_AIRWALLEX)
        .bind(organization_id)
        .bind(idempotency_key)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => {
                use sqlx::Row;
                Ok(Some((
                    row.try_get("deposit_id")?,
                    row.try_get("intent_id")?,
                    row.try_get("client_secret")?,
                    row.try_get("checkout_url")?,
                )))
            }
            None => Ok(None),
        }
    }

    /// Records Airwallex's intent id, `client_secret` and the hosted-page URL
    /// in the deposit's metadata. The `payment_ref` stays the caller's
    /// idempotency key — the UNIQUE constraint is what makes a replayed intent
    /// idempotent — while `meta.airwallexIntentId` is the handle the webhook
    /// names when it comes back, and `meta.airwallexCheckoutUrl` /
    /// `meta.airwallexClientSecret` are what a replayed intent answers.
    pub async fn airwallex_intent(
        &self,
        deposit_id: Uuid,
        intent_id: &str,
        client_secret: &str,
        checkout_url: &str,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.deposits \
             SET meta = meta || jsonb_build_object(\
                     'airwallexIntentId', $1::text, \
                     'airwallexClientSecret', $2::text, \
                     'airwallexCheckoutUrl', $3::text\
                 ), \
                 updated_at = now() \
             WHERE deposit_id = $4 AND rail = $5 AND status = 'pending'",
        )
        .bind(intent_id)
        .bind(client_secret)
        .bind(checkout_url)
        .bind(deposit_id)
        .bind(RAIL_AIRWALLEX)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Finds the Airwallex deposit a webhook names: `meta.airwallexIntentId`
    /// matches the intent Airwallex calls back for. `payment_ref` is the
    /// caller's idempotency key, not the intent id — the webhook identifies
    /// the deposit through the intent id, which the intent handler wrote into
    /// metadata when the PaymentIntent was created.
    pub async fn find_airwallex_by_intent(
        &self,
        intent_id: &str,
    ) -> Result<Option<(Uuid, Uuid, String)>, WalletError> {
        let row = sqlx::query(
            "SELECT d.deposit_id, d.organization_id, o.tenant_id \
             FROM oxsum.deposits d \
             JOIN oxsum.organizations o ON o.organization_id = d.organization_id \
             WHERE d.rail = $1 AND d.meta->>'airwallexIntentId' = $2 \
             ORDER BY d.created_at DESC LIMIT 1",
        )
        .bind(RAIL_AIRWALLEX)
        .bind(intent_id)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => {
                use sqlx::Row;
                Ok(Some((
                    row.try_get("deposit_id")?,
                    row.try_get("organization_id")?,
                    row.try_get("tenant_id")?,
                )))
            }
            None => Ok(None),
        }
    }

    /// Moves an Airwallex deposit to `confirmed` — the webhook's first act on
    /// `payment_intent.succeeded`, before the ledger credit runs.
    ///
    /// Returns `true` when the row moved, `false` when it was already past
    /// `pending` — a replayed or raced webhook answers `false` and credits
    /// nothing twice.
    pub async fn confirm_airwallex_deposit(&self, deposit_id: Uuid) -> Result<bool, WalletError> {
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'confirmed', updated_at = now() \
             WHERE deposit_id = $1 AND rail = $2 AND status = 'pending'",
        )
        .bind(deposit_id)
        .bind(RAIL_AIRWALLEX)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(flipped > 0)
    }

    /// Credits the wallet for a confirmed Airwallex deposit and flips the row
    /// to `credited` — the same `deposit.credited` event and `balance.low`
    /// latch-clear as every other rail, in the deposit-update transaction.
    ///
    /// The credit runs under the fixed key `airwallex:<intent_id>`: a replayed
    /// webhook finds its own credited deposit and writes nothing twice, and a
    /// crash between the ledger append and the row flip leaves a confirmed
    /// deposit the retry resumes.
    ///
    /// `received_minor` is what Airwallex reports the intent paid (decimal
    /// major units converted back to cents): a number that disagrees with
    /// `amount_minor` is recorded for a human, never auto-patched (decision
    /// D2).
    ///
    /// Returns the receipt and organization id when a credit ran, `None` when
    /// the deposit was already credited or was never confirmed.
    pub async fn credit_airwallex_deposit(
        &self,
        wallet: &Wallet,
        deposit_id: Uuid,
        intent_id: &str,
        received_minor: Option<i64>,
        on: time::Date,
    ) -> Result<Option<(Receipt, Uuid)>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT organization_id, amount_minor, status, payment_ref \
             FROM oxsum.deposits \
             WHERE deposit_id = $1 AND rail = $2 \
             FOR UPDATE",
        )
        .bind(deposit_id)
        .bind(RAIL_AIRWALLEX)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        use sqlx::Row;
        let organization_id: Uuid = row.try_get("organization_id")?;
        let amount_minor: i64 = row.try_get("amount_minor")?;
        let status: String = row.try_get("status")?;
        let payment_ref: String = row.try_get("payment_ref")?;
        // `confirmed` is the only state a credit proceeds from: `pending` was
        // never confirmed (the intent expired before paying), `credited` is
        // already done.
        if status != "confirmed" {
            tx.commit().await?;
            return Ok(None);
        }
        // The ledger append replays under the intent id — a webhook retried
        // after a crash finds its own entry rather than opening a second one.
        let receipt = wallet
            .top_up(&format!("airwallex:{intent_id}"), amount_minor, on)
            .await?;
        // Record what the rail reported paid; a mismatch with amount_minor is
        // a human's call, never auto-patched (decision D2).
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'credited', entry_id = $1, received_minor = $2, \
                 updated_at = now() \
             WHERE deposit_id = $3 AND status = 'confirmed'",
        )
        .bind(*receipt.entry_id.as_uuid())
        .bind(received_minor)
        .bind(deposit_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if flipped > 0 {
            Self::enqueue_event(
                &mut tx,
                organization_id,
                crate::webhooks::DEPOSIT_CREDITED,
                serde_json::json!({
                    "rail": RAIL_AIRWALLEX,
                    "paymentRef": payment_ref,
                    "amountMinor": amount_minor,
                    "entryId": receipt.entry_id.as_uuid(),
                }),
            )
            .await?;
            if let Ok(available) = wallet.available().await {
                Self::clear_balance_low(&mut tx, organization_id, available).await?;
            }
        }
        tx.commit().await?;
        Ok(Some((receipt, organization_id)))
    }

    /// Moves an Airwallex deposit to `expired` — a terminal-state webhook's
    /// outcome (`payment_intent.cancelled`, `payment_intent.expired`), a no-op
    /// on a row that already ended its walk.
    pub async fn expire_airwallex_deposit(&self, deposit_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.deposits SET status = 'expired', updated_at = now() \
             WHERE deposit_id = $1 AND rail = $2 AND status IN ('pending', 'confirmed')",
        )
        .bind(deposit_id)
        .bind(RAIL_AIRWALLEX)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Opens a Hyperswitch-rail deposit before the payment exists: `pending`,
    /// keyed by `payment_ref = <idempotency_key>`, which stays the row's anchor
    /// for its whole life — the switch's payment id lands in `meta` when it
    /// answers. Same shape [`Db::open_stripe_deposit`] proves: the caller's key
    /// makes the intent row itself replay-safe.
    ///
    /// Returns `true` when the row was inserted — a `false` answer means the
    /// idempotency key already opened this deposit, and the caller should look
    /// up the existing row rather than create a second payment.
    ///
    /// A non-positive amount is refused the way every credit write is.
    pub async fn open_hyperswitch_deposit(
        &self,
        organization_id: Uuid,
        deposit_id: Uuid,
        idempotency_key: &str,
        amount_minor: i64,
    ) -> Result<bool, WalletError> {
        if amount_minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a deposit's amount must be positive".into(),
            ));
        }
        let inserted = sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, status) \
             VALUES ($1, $2, $3, $4, $5, 'pending') \
             ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
        )
        .bind(deposit_id)
        .bind(organization_id)
        .bind(RAIL_HYPERSWITCH)
        .bind(idempotency_key)
        .bind(amount_minor)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(inserted > 0)
    }

    /// Finds the pending deposit an idempotency key already opened — the
    /// lookup `POST /topups/hyperswitch` runs on a replayed intent, to answer
    /// the same payment id and client secret instead of creating a second
    /// switch payment. Both handles live in `meta`.
    pub async fn find_hyperswitch_deposit(
        &self,
        organization_id: Uuid,
        idempotency_key: &str,
    ) -> Result<Option<(Uuid, Option<String>, Option<String>)>, WalletError> {
        let row = sqlx::query(
            "SELECT deposit_id, \
                    meta->>'hyperswitchPaymentId' AS payment_id, \
                    meta->>'hyperswitchClientSecret' AS client_secret \
             FROM oxsum.deposits \
             WHERE rail = $1 AND organization_id = $2 AND payment_ref = $3",
        )
        .bind(RAIL_HYPERSWITCH)
        .bind(organization_id)
        .bind(idempotency_key)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => Ok(Some((
                row.try_get("deposit_id")?,
                row.try_get("payment_id")?,
                row.try_get("client_secret")?,
            ))),
            None => Ok(None),
        }
    }

    /// Records the switch's payment id and client secret in the deposit's
    /// metadata. `payment_ref` stays the caller's idempotency key — the UNIQUE
    /// constraint is what makes a replayed intent idempotent — while
    /// `meta.hyperswitchPaymentId` is the handle the webhook names when it
    /// comes back.
    pub async fn hyperswitch_payment(
        &self,
        deposit_id: Uuid,
        payment_id: &str,
        client_secret: &str,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.deposits \
             SET meta = meta || jsonb_build_object(\
                     'hyperswitchPaymentId', $1::text, \
                     'hyperswitchClientSecret', $2::text\
                 ), \
                 updated_at = now() \
             WHERE deposit_id = $3 AND rail = $4 AND status = 'pending'",
        )
        .bind(payment_id)
        .bind(client_secret)
        .bind(deposit_id)
        .bind(RAIL_HYPERSWITCH)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Finds the Hyperswitch deposit a webhook names: `meta.hyperswitchPaymentId`
    /// matches the payment the switch calls back for.
    pub async fn find_hyperswitch_by_payment(
        &self,
        payment_id: &str,
    ) -> Result<Option<(Uuid, Uuid, String)>, WalletError> {
        let row = sqlx::query(
            "SELECT d.deposit_id, d.organization_id, o.tenant_id \
             FROM oxsum.deposits d \
             JOIN oxsum.organizations o ON o.organization_id = d.organization_id \
             WHERE d.rail = $1 AND d.meta->>'hyperswitchPaymentId' = $2 \
             ORDER BY d.created_at DESC LIMIT 1",
        )
        .bind(RAIL_HYPERSWITCH)
        .bind(payment_id)
        .fetch_optional(self.pool())
        .await?;
        match row {
            Some(row) => Ok(Some((
                row.try_get("deposit_id")?,
                row.try_get("organization_id")?,
                row.try_get("tenant_id")?,
            ))),
            None => Ok(None),
        }
    }

    /// Moves a Hyperswitch deposit to `confirmed` — the webhook's first act on
    /// `payment_succeeded`, before the ledger credit runs.
    ///
    /// Returns `true` when the row moved, `false` when it was already past
    /// `pending` — a replayed or raced webhook answers `false` and credits
    /// nothing twice.
    pub async fn confirm_hyperswitch_deposit(&self, deposit_id: Uuid) -> Result<bool, WalletError> {
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'confirmed', updated_at = now() \
             WHERE deposit_id = $1 AND rail = $2 AND status = 'pending'",
        )
        .bind(deposit_id)
        .bind(RAIL_HYPERSWITCH)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(flipped > 0)
    }

    /// Credits the wallet for a confirmed Hyperswitch deposit and flips the
    /// row to `credited` — the same `deposit.credited` event and `balance.low`
    /// latch-clear as every other rail, in the deposit-update transaction.
    ///
    /// The credit runs under the fixed key `hyperswitch:<payment_id>`: a
    /// replayed webhook finds its own credited deposit and writes nothing
    /// twice, and a crash between the ledger append and the row flip leaves a
    /// confirmed deposit the retry resumes.
    ///
    /// `received_minor` is what the switch reports paid: a number that
    /// disagrees with `amount_minor` is recorded for a human, never
    /// auto-patched (decision D2).
    pub async fn credit_hyperswitch_deposit(
        &self,
        wallet: &Wallet,
        deposit_id: Uuid,
        payment_id: &str,
        received_minor: Option<i64>,
        on: time::Date,
    ) -> Result<Option<(Receipt, Uuid)>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT organization_id, amount_minor, status, payment_ref \
             FROM oxsum.deposits \
             WHERE deposit_id = $1 AND rail = $2 \
             FOR UPDATE",
        )
        .bind(deposit_id)
        .bind(RAIL_HYPERSWITCH)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let organization_id: Uuid = row.try_get("organization_id")?;
        let amount_minor: i64 = row.try_get("amount_minor")?;
        let status: String = row.try_get("status")?;
        let payment_ref: String = row.try_get("payment_ref")?;
        if status != "confirmed" {
            tx.commit().await?;
            return Ok(None);
        }
        let receipt = wallet
            .top_up(&format!("hyperswitch:{payment_id}"), amount_minor, on)
            .await?;
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'credited', entry_id = $1, received_minor = $2, \
                 updated_at = now() \
             WHERE deposit_id = $3 AND status = 'confirmed'",
        )
        .bind(*receipt.entry_id.as_uuid())
        .bind(received_minor)
        .bind(deposit_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if flipped > 0 {
            Self::enqueue_event(
                &mut tx,
                organization_id,
                crate::webhooks::DEPOSIT_CREDITED,
                serde_json::json!({
                    "rail": RAIL_HYPERSWITCH,
                    "paymentRef": payment_ref,
                    "amountMinor": amount_minor,
                    "entryId": receipt.entry_id.as_uuid(),
                }),
            )
            .await?;
            if let Ok(available) = wallet.available().await {
                Self::clear_balance_low(&mut tx, organization_id, available).await?;
            }
        }
        tx.commit().await?;
        Ok(Some((receipt, organization_id)))
    }

    /// Moves a Hyperswitch deposit to `expired` — a terminal-state webhook's
    /// outcome (`payment_cancelled`, `payment_expired`), a no-op on a row that
    /// already ended its walk.
    pub async fn expire_hyperswitch_deposit(&self, deposit_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.deposits SET status = 'expired', updated_at = now() \
             WHERE deposit_id = $1 AND rail = $2 AND status IN ('pending', 'confirmed')",
        )
        .bind(deposit_id)
        .bind(RAIL_HYPERSWITCH)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Records one provider webhook event — the idempotency anchor the
    /// Hyperswitch handler inserts before it acts (issue #319). Returns `true`
    /// when the row was new; `false` means this `event_id` was already
    /// processed and the caller must acknowledge without acting again.
    pub async fn record_payment_event(
        &self,
        event_id: &str,
        rail: &str,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Result<bool, WalletError> {
        let inserted = sqlx::query(
            "INSERT INTO oxsum.payment_events (event_id, rail, event_type, payload) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (event_id) DO NOTHING",
        )
        .bind(event_id)
        .bind(rail)
        .bind(event_type)
        .bind(payload)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(inserted > 0)
    }

    /// Mints `count` redemption codes worth `amount_minor` each: `oxr-` plus 32 random
    /// bytes from the operating system's CSPRNG. Only the SHA-256 of each lands in the
    /// table — this answer is the only place the codes are readable.
    ///
    /// `expires_at` may be null: a code without an expiry never becomes invalid on its
    /// own. One batch shares one `batch_id`, so one mint call is traceable as a group
    /// in deposit metadata and a future admin list.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a count outside `1..=MAX_BATCH` or a
    /// non-positive amount; storage failures surface as [`WalletError`].
    pub async fn mint_codes(
        &self,
        count: i64,
        amount_minor: i64,
        expires_at: Option<OffsetDateTime>,
    ) -> Result<CodeBatch, WalletError> {
        if !(1..=MAX_BATCH).contains(&count) {
            return Err(WalletError::InvalidInput(format!(
                "count must be 1..={MAX_BATCH}"
            )));
        }
        if amount_minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a code's amount must be positive".into(),
            ));
        }
        if expires_at.is_some_and(|at| at <= OffsetDateTime::now_utc()) {
            return Err(WalletError::InvalidInput(
                "an expiry must be in the future".into(),
            ));
        }
        let batch_id = Uuid::new_v4();
        let mut codes = Vec::with_capacity(count as usize);
        let mut tx = self.pool().begin().await?;
        for _ in 0..count {
            let code = generate_code();
            sqlx::query(
                "INSERT INTO oxsum.redemption_codes \
                     (code_id, code_hash, batch_id, amount_minor, expires_at) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(Uuid::new_v4())
            .bind(hash_code(&code).as_slice())
            .bind(batch_id)
            .bind(amount_minor)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;
            codes.push(code);
        }
        tx.commit().await?;
        Ok(CodeBatch {
            batch_id,
            count,
            amount_minor,
            expires_at,
            codes,
        })
    }

    /// Redeems a code into `organization`'s wallet: one atomic claim, then the ledger
    /// credit, then the deposit closed as credited.
    ///
    /// The claim is the invitation shape — the code's row is taken `FOR UPDATE`, the
    /// spend and the deposit (`confirmed`) commit in one transaction, so two
    /// redeems of one code cannot both win. The ledger append runs under the fixed
    /// idempotency key `redemption:<code_id>` after the claim commits: a replay
    /// rewrites nothing, and a redeem retried after a crash finds its own
    /// confirmed deposit and resumes the credit instead of opening a second one.
    /// The deposit then flips to `credited` carrying the entry that moved the money.
    ///
    /// A code that is unknown, spent by another organization or expired is
    /// [`WalletError::NotFound`] whichever it is — which of the three is wrong is the
    /// holder's business. A code this same organization already redeemed replays
    /// its own credit, so a caller's retry after a lost answer still gets a receipt.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an unusable code; storage and ledger failures
    /// surface as [`WalletError`].
    pub async fn redeem_code(
        &self,
        wallet: &Wallet,
        organization_id: Uuid,
        code: &str,
        on: time::Date,
    ) -> Result<Redemption, WalletError> {
        let invalid = || WalletError::NotFound("the code is not valid".into());
        if code.len() != CODE_MARK.len() + CODE_BYTES * 2 || !code.starts_with(CODE_MARK) {
            return Err(invalid());
        }
        // The claim transaction: lock the row, spend it, and record the deposit —
        // all or nothing, so the ledger credit that follows is always replayable.
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT code_id, batch_id, amount_minor, redeemed_by, deposit_id \
             FROM oxsum.redemption_codes \
             WHERE code_hash = $1 AND (expires_at IS NULL OR expires_at > now()) \
             FOR UPDATE",
        )
        .bind(hash_code(code).as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Err(invalid());
        };
        let code_id: Uuid = row.try_get("code_id")?;
        let amount_minor: i64 = row.try_get("amount_minor")?;
        let redeemed_by: Option<Uuid> = row.try_get("redeemed_by")?;
        let deposit_id: Option<Uuid> = row.try_get("deposit_id")?;
        let deposit_id = match redeemed_by {
            // This organization's own redeem, retried: resume the deposit it opened.
            Some(by) if by == organization_id => deposit_id.ok_or_else(|| {
                WalletError::InvalidInput("spent code without its deposit".into())
            })?,
            // Unspent: claim it for this organization, opening the deposit in the
            // same transaction.
            None => {
                let id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO oxsum.deposits \
                         (deposit_id, organization_id, rail, payment_ref, amount_minor, \
                          received_minor, status, meta) \
                     VALUES ($1, $2, $3, $4, $5, $5, 'confirmed', $6)",
                )
                .bind(id)
                .bind(organization_id)
                .bind(RAIL_REDEMPTION)
                .bind(code_id.to_string())
                .bind(amount_minor)
                .bind(serde_json::json!({ "batchId": row.try_get::<Uuid, _>("batch_id")? }))
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE oxsum.redemption_codes \
                     SET redeemed_at = now(), redeemed_by = $1, deposit_id = $2 \
                     WHERE code_id = $3",
                )
                .bind(organization_id)
                .bind(id)
                .bind(code_id)
                .execute(&mut *tx)
                .await?;
                id
            }
            // Spent by another organization: indistinguishable from a bad code.
            Some(_) => return Err(invalid()),
        };
        tx.commit().await?;

        // The credit replays under its own key: a redeploy of this same redeem — the
        // caller's retry or a second in-flight call — writes nothing twice.
        let receipt = wallet
            .top_up(&redeem_key(code_id), amount_minor, on)
            .await?;
        // The deposit flips to `credited`, the `deposit.credited` event and the
        // `balance.low` latch-clear share the transaction (issue #200): a retried
        // redeem whose deposit is already credited flips no row and enqueues
        // nothing.
        let mut tx = self.pool().begin().await?;
        let flipped = sqlx::query(
            "UPDATE oxsum.deposits \
             SET status = 'credited', entry_id = $1, updated_at = now() \
             WHERE deposit_id = $2 AND status <> 'credited'",
        )
        .bind(*receipt.entry_id.as_uuid())
        .bind(deposit_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if flipped > 0 {
            Self::enqueue_event(
                &mut tx,
                organization_id,
                crate::webhooks::DEPOSIT_CREDITED,
                serde_json::json!({
                    "rail": RAIL_REDEMPTION,
                    "paymentRef": code_id.to_string(),
                    "amountMinor": amount_minor,
                    "entryId": receipt.entry_id.as_uuid(),
                }),
            )
            .await?;
            if let Ok(available) = wallet.available().await {
                Self::clear_balance_low(&mut tx, organization_id, available).await?;
            }
        }
        tx.commit().await?;
        Ok(Redemption {
            receipt,
            amount_minor,
        })
    }
}

/// The idempotency key a code's credit writes under — fixed, so a credit is replayable
/// without ever looking the code up twice.
fn redeem_key(code_id: Uuid) -> String {
    format!("redemption:{code_id}")
}

/// A new redemption code: the mark plus 32 random bytes, hex-encoded like an API key
/// secret or an invitation token.
fn generate_code() -> String {
    let mut bytes = [0u8; CODE_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut code = String::with_capacity(CODE_MARK.len() + CODE_BYTES * 2);
    code.push_str(CODE_MARK);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(code, "{byte:02x}");
    }
    code
}

/// SHA-256 of the code, the only form the database ever holds.
fn hash_code(code: &str) -> Vec<u8> {
    Sha256::digest(code.as_bytes()).to_vec()
}

/// One deposit as the operator console lists it (issue #354): the money-in
/// record, never the ledger entry itself — the entry id names that.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminDeposit {
    /// The deposit's own id.
    pub deposit_id: Uuid,
    /// The organization the money arrived for.
    pub organization_id: Uuid,
    /// Its display name, so a row reads without a second lookup.
    pub organization: String,
    /// The rail that carried it (`manual`, `redemption`, `stripe`, `hyperswitch`, …).
    pub rail: String,
    /// The rail's reference — an idempotency key, a charge id, a code id.
    pub payment_ref: String,
    /// What the deposit was opened for.
    pub amount_minor: i64,
    /// What the rail reported paying, when it reported a different figure.
    pub received_minor: Option<i64>,
    /// `pending`, `confirmed`, `credited`, `reversed` or `expired`.
    pub status: String,
    /// The ledger entry that credited the wallet, once `credited`.
    pub entry_id: Option<Uuid>,
    /// When the deposit row opened.
    pub created_at: OffsetDateTime,
}

impl Db {
    /// The newest deposits, platform-wide or for one organization (issue #354).
    ///
    /// Read-only: the console lists what the rails recorded; nothing here
    /// confirms, credits or reverses a deposit.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn admin_deposits(
        &self,
        organization: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<AdminDeposit>, WalletError> {
        let rows = sqlx::query(
            "SELECT d.deposit_id, d.organization_id, o.name AS organization, d.rail, \
                    d.payment_ref, d.amount_minor, d.received_minor, d.status, \
                    d.entry_id, d.created_at \
             FROM oxsum.deposits d \
             JOIN oxsum.organizations o ON o.organization_id = d.organization_id \
             WHERE ($1::uuid IS NULL OR d.organization_id = $1) \
             ORDER BY d.created_at DESC, d.deposit_id DESC \
             LIMIT $2",
        )
        .bind(organization)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(AdminDeposit {
                    deposit_id: row.try_get("deposit_id")?,
                    organization_id: row.try_get("organization_id")?,
                    organization: row.try_get("organization")?,
                    rail: row.try_get("rail")?,
                    payment_ref: row.try_get("payment_ref")?,
                    amount_minor: row.try_get("amount_minor")?,
                    received_minor: row.try_get("received_minor")?,
                    status: row.try_get("status")?,
                    entry_id: row.try_get("entry_id")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }
}

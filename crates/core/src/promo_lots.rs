//! Promotional lots and prepaid bundles (issue #609).
//!
//! A promo lot is a grant of credit with a provenance and an optional expiry.
//! The money lives in the ledger's `Equity:Promo` pool — the pool every funded
//! draw takes first, because it is the balance that can be lost — and the
//! `oxsum.promo_lots` row is the projection that says which grant still funds
//! how much of it. `promo_draws` records which lots a hold pinned at
//! admission, so settlement debits the rows admission named rather than
//! re-scanning, and a release returns to exactly them.
//!
//! A lot is written once at its source event and never edited: an admin grant,
//! a bundle fulfillment's promo leg, or the one-time migration of the
//! aggregate bonus pool. Only `remaining_minor` moves — draws decrement it, a
//! release returns to it while the lot is open — and a close lands once:
//! expiry burns the unspent remainder to `Equity:PromoExpiry`, and a refund's
//! cancellation does the same. Promo is a grant, not a liability: nothing in
//! this module can turn it back into refundable cash.

use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::wallet::Wallet;

/// One promo lot, as the admin and customer surfaces read it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoLot {
    /// The lot's own id.
    pub lot_id: Uuid,
    /// The organization the lot belongs to.
    pub organization_id: Uuid,
    /// Where the lot came from: `admin`, `grant`, `bundle_promo` or
    /// `legacy_migration`.
    pub source: String,
    /// The source event's own identity — the idempotency anchor.
    pub source_ref: String,
    /// What the grant posted, immutable.
    pub amount_minor: i64,
    /// The unreserved part still fundable.
    pub remaining_minor: i64,
    /// The part open holds currently reserve against the lot.
    pub reserved_minor: i64,
    /// When the lot lapses; `None` means it never expires.
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// When the grant landed.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When a close that was not an expiry landed; `None` while the lot is open.
    #[serde(with = "time::serde::rfc3339::option")]
    pub closed_at: Option<OffsetDateTime>,
    /// `expired` or `cancelled` on a closed lot.
    pub close_reason: Option<String>,
}

impl Db {
    /// Grants a promo lot into one organization's wallet: the `promo_lots` row
    /// and the `debit Adjustments / credit Promo` ledger entry commit inside
    /// one transaction, so the grant cannot exist without the projection that
    /// names it.
    ///
    /// `source`/`source_ref` are the lot's provenance and idempotency anchor —
    /// the unique pair makes a replayed grant a no-op answering the standing
    /// lot. `expires_at` of `None` never expires.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount or a malformed
    /// source reference; storage and ledger failures surface as
    /// [`WalletError`].
    #[allow(clippy::too_many_arguments)]
    pub async fn grant_promo_lot(
        &self,
        wallet: &Wallet,
        organization_id: Uuid,
        source: &str,
        source_ref: &str,
        amount_minor: i64,
        expires_at: Option<OffsetDateTime>,
        on: time::Date,
    ) -> Result<PromoLot, WalletError> {
        if !(1..=200).contains(&source_ref.len()) || source_ref.chars().any(char::is_control) {
            return Err(WalletError::InvalidInput(
                "a promo lot's source reference is 1..=200 characters".into(),
            ));
        }
        let ledger_key = format!("promo-lot:{organization_id}:{source}:{source_ref}");
        let mut tx = self.pool().begin().await?;
        let (lot_id, _receipt) = wallet
            .grant_promo_lot_in(
                &mut tx,
                organization_id,
                source,
                source_ref,
                &ledger_key,
                amount_minor,
                expires_at,
                on,
            )
            .await?;
        tx.commit().await?;
        self.promo_lot(lot_id)
            .await?
            .ok_or_else(|| WalletError::InvalidInput("the promo lot did not land".into()))
    }

    /// One promo lot by id, or `None`.
    pub async fn promo_lot(&self, lot_id: Uuid) -> Result<Option<PromoLot>, WalletError> {
        let row = sqlx::query(
            "SELECT lot_id, organization_id, source, source_ref, amount_minor, remaining_minor, \
                    expires_at, created_at, closed_at, close_reason, \
                    COALESCE((SELECT SUM(d.amount_minor - COALESCE(d.charged_minor,0) - COALESCE(d.released_minor,0)) \
                              FROM oxsum.promo_draws d WHERE d.lot_id = promo_lots.lot_id \
                                AND d.charged_minor IS NULL), 0)::bigint AS reserved_minor \
             FROM oxsum.promo_lots WHERE lot_id = $1",
        )
        .bind(lot_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(promo_lot_from_row).transpose()
    }

    /// One organization's promo lots, soonest-expiring first — the same order
    /// the admission allocation draws them in.
    pub async fn promo_lots(
        &self,
        organization_id: Uuid,
        include_closed: bool,
    ) -> Result<Vec<PromoLot>, WalletError> {
        let rows = sqlx::query(
            "SELECT lot_id, organization_id, source, source_ref, amount_minor, remaining_minor, \
                    expires_at, created_at, closed_at, close_reason, \
                    COALESCE((SELECT SUM(d.amount_minor - COALESCE(d.charged_minor,0) - COALESCE(d.released_minor,0)) \
                              FROM oxsum.promo_draws d WHERE d.lot_id = promo_lots.lot_id \
                                AND d.charged_minor IS NULL), 0)::bigint AS reserved_minor \
             FROM oxsum.promo_lots \
             WHERE organization_id = $1 AND ($2 OR closed_at IS NULL) \
             ORDER BY expires_at ASC NULLS LAST, lot_id",
        )
        .bind(organization_id)
        .bind(include_closed)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(promo_lot_from_row).collect()
    }

    /// Cancels a lot's unspent remainder: the part open draws still reserve
    /// stays reserved — it burns when those holds release into the closed lot
    /// — and the unreserved remainder posts `debit Promo / credit
    /// PromoExpiry`, the net-zero marker a refund books instead of turning the
    /// grant back into cash (issue #609). The purchased half of a refunded
    /// bundle unwinds through the deposit machinery; this is only ever the
    /// grant.
    ///
    /// Idempotent by `close_reason`: a second cancel of a closed lot answers
    /// its standing row. Returns the lot's row after the write.
    pub async fn cancel_promo_lot(
        &self,
        wallet: &Wallet,
        lot_id: Uuid,
        on: time::Date,
    ) -> Result<PromoLot, WalletError> {
        let mut tx = self.pool().begin().await?;
        // The lot row is locked below and the burn appends in the same
        // transaction; the append lock goes first so the order matches every
        // hold's promo pin — append advisory lock, then promo row locks
        // (issue #723).
        wallet.lock_append_in(&mut tx).await?;
        let row = sqlx::query(
            "SELECT remaining_minor, closed_at FROM oxsum.promo_lots WHERE lot_id = $1 FOR UPDATE",
        )
        .bind(lot_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Err(WalletError::NotFound("unknown promo lot".into()));
        };
        let remaining: i64 = row.try_get("remaining_minor")?;
        let already_closed: bool = row
            .try_get::<Option<OffsetDateTime>, _>("closed_at")?
            .is_some();
        if !already_closed {
            if remaining > 0 {
                // Burn the unreserved remainder — never back to a refundable
                // pool. The ledger write rides this transaction.
                wallet
                    .burn_promo_in(&mut tx, &format!("promo-cancel:{lot_id}"), remaining, on)
                    .await?;
            }
            sqlx::query(
                "UPDATE oxsum.promo_lots SET remaining_minor = 0, closed_at = now(), \
                 close_reason = 'cancelled' WHERE lot_id = $1",
            )
            .bind(lot_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        self.promo_lot(lot_id)
            .await?
            .ok_or_else(|| WalletError::InvalidInput("the promo lot did not land".into()))
    }

    /// The expiry pass (job kind `expire-lots`): every open lot whose
    /// `expires_at` passed burns its unreserved remainder to
    /// `Equity:PromoExpiry` and closes `expired`. The part an open hold pinned
    /// is already out of `remaining_minor` — it burns separately when the
    /// hold's own settlement releases into the closed lot — so a lot expiring
    /// mid-stream never double-charges a turn (issue #609, A20).
    ///
    /// Deterministic per lot: the expiry entry's key derives from the lot id,
    /// so a pass that races a lot's close or replays after a crash posts the
    /// same ledger write once. Returns how many lots this pass closed.
    pub async fn expire_due_promo_lots(
        &self,
        tenants: &crate::Tenants,
        limit: i64,
        on: time::Date,
    ) -> Result<u64, WalletError> {
        // `remaining_minor > 0` cannot filter here: a lot whose whole remainder
        // open holds pinned still must close — its draws' released shares burn
        // at settlement, which only happens once `closed_at` is set.
        let due = sqlx::query(
            "SELECT lot_id, organization_id FROM oxsum.promo_lots \
             WHERE closed_at IS NULL AND expires_at <= now() \
             ORDER BY expires_at, lot_id LIMIT $1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        let mut expired = 0u64;
        for row in &due {
            let lot_id: Uuid = row.try_get("lot_id")?;
            let organization: Uuid = row.try_get("organization_id")?;
            // The ledger write needs the organization's wallet; its tenant id
            // resolves through the same organizations row the lot belongs to.
            let tenant: Option<String> = sqlx::query_scalar(
                "SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1",
            )
            .bind(organization)
            .fetch_optional(self.pool())
            .await?;
            let Some(tenant) = tenant else { continue };
            let wallet = tenants.get(&tenant).await?;
            if self.expire_one_lot(&wallet, lot_id, on).await? {
                expired += 1;
            }
        }
        Ok(expired)
    }

    /// One lot's expiry, inside its own transaction: `FOR UPDATE` serializes
    /// the close against a racing settlement's draw-release and a racing pass.
    /// Returns whether this call closed the lot (a replay answers false).
    async fn expire_one_lot(
        &self,
        wallet: &Wallet,
        lot_id: Uuid,
        on: time::Date,
    ) -> Result<bool, WalletError> {
        let mut tx = self.pool().begin().await?;
        // The close locks the lot row and burns in the same transaction. The
        // append lock precedes the row lock so an expiry can never hold the
        // row a hold's pin waits for while itself waiting on the append lock
        // (issue #723).
        wallet.lock_append_in(&mut tx).await?;
        let row = sqlx::query(
            "SELECT remaining_minor, closed_at FROM oxsum.promo_lots WHERE lot_id = $1 FOR UPDATE",
        )
        .bind(lot_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(false);
        };
        let remaining: i64 = row.try_get("remaining_minor")?;
        if row
            .try_get::<Option<OffsetDateTime>, _>("closed_at")?
            .is_some()
        {
            tx.rollback().await?;
            return Ok(false);
        }
        if remaining > 0 {
            wallet
                .burn_promo_in(&mut tx, &format!("promo-expire:{lot_id}"), remaining, on)
                .await?;
        }
        sqlx::query(
            "UPDATE oxsum.promo_lots SET remaining_minor = 0, closed_at = now(), \
             close_reason = 'expired' WHERE lot_id = $1 AND closed_at IS NULL",
        )
        .bind(lot_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

/// Reads one row into the wire shape.
fn promo_lot_from_row(row: &sqlx::postgres::PgRow) -> Result<PromoLot, WalletError> {
    Ok(PromoLot {
        lot_id: row.try_get("lot_id")?,
        organization_id: row.try_get("organization_id")?,
        source: row.try_get("source")?,
        source_ref: row.try_get("source_ref")?,
        amount_minor: row.try_get("amount_minor")?,
        remaining_minor: row.try_get("remaining_minor")?,
        reserved_minor: row.try_get("reserved_minor")?,
        expires_at: row.try_get("expires_at")?,
        created_at: row.try_get("created_at")?,
        closed_at: row.try_get("closed_at")?,
        close_reason: row.try_get("close_reason")?,
    })
}

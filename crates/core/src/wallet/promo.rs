//! Promotional lots: pin, consume, grant and burn, plus legacy-grant migration.

use crate::error::WalletError;
use doubleentry::{Draft, Entry};

use super::{
    Credits, LEGACY_PROMO_KEY, RECLASS_KEY, Receipt, SCALE, Wallet, currency, description_of,
    entry_id_for, idem, positive, stored_receipt,
};
use sqlx::Row;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

impl Wallet {
    /// The promo draw-pinning [`hold`](Self::hold) and
    /// [`hold_for_key`](Self::hold_for_key) run inside the hold's transaction:
    /// walks this organization's open lots in deterministic draw order —
    /// soonest expiry first, never-expiring last — decrements each lot's
    /// `remaining_minor` by what the hold's promo share takes from it, and
    /// records each pick in `promo_draws` so the settlement debits the rows
    /// admission named (issue #609).
    ///
    /// The caller already computed `promo_minor` from the ledger's own pool
    /// availability inside this same transaction, so the lots' rows and the
    /// pool agree by construction: `FOR UPDATE` serializes the decrement
    /// against a racing hold's allocation and against the expiry pass, and the
    /// lot's `CHECK (remaining_minor >= 0)` is the last-line guard a stale read
    /// could never over-draw.
    pub(super) async fn pin_promo_draws_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        hold_key: &str,
        promo_minor: i64,
    ) -> Result<(), WalletError> {
        let Some(organization) = self.organization_id_hint_in(tx).await? else {
            // A bare ledger schema has no oxsum rows at all: the promo pool
            // stays empty there and `pool_split` would never take from it, so
            // reaching here means the model world asked for promo money it
            // cannot name — an internal contradiction, not user error.
            return Err(WalletError::InvalidInput(
                "promo draws need an organization".into(),
            ));
        };
        let mut left = promo_minor;
        // Lock in the allocation's own order so two racing holds cannot
        // deadlock: both walk the same list in the same order. An open lot is
        // pinnable even past its `expires_at`: the ledger pool counts its
        // remainder until the expiry pass burns it, so the pin and the split
        // must agree — and pinning a due lot first is exactly the
        // most-restricted-money-first order the pools draw in. The pass closes
        // the row under the same `FOR UPDATE`; a settlement that releases into
        // the closed lot burns rather than refunds.
        let lots = sqlx::query(
            "SELECT lot_id, remaining_minor FROM oxsum.promo_lots \
             WHERE organization_id = $1 AND closed_at IS NULL AND remaining_minor > 0 \
             ORDER BY expires_at ASC NULLS LAST, lot_id FOR UPDATE",
        )
        .bind(organization)
        .fetch_all(&mut **tx)
        .await?;
        for lot in &lots {
            if left <= 0 {
                break;
            }
            let lot_id: Uuid = lot.try_get("lot_id")?;
            let remaining: i64 = lot.try_get("remaining_minor")?;
            let take = left.min(remaining);
            sqlx::query(
                "UPDATE oxsum.promo_lots SET remaining_minor = remaining_minor - $2 \
                 WHERE lot_id = $1",
            )
            .bind(lot_id)
            .bind(take)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "INSERT INTO oxsum.promo_draws (hold_key, tenant_id, lot_id, amount_minor) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(hold_key)
            .bind(&self.tenant_id)
            .bind(lot_id)
            .bind(take)
            .execute(&mut **tx)
            .await?;
            left -= take;
        }
        if left > 0 {
            // The ledger's pool availability and the lots' rows drifted apart —
            // the invariant tests check — so the hold cannot name the lots its
            // promo share funds. Refusing is honest; drawing unattributed
            // promo would let settlement pick a different lot than admission.
            return Err(WalletError::InsufficientFunds);
        }
        Ok(())
    }
    /// The draw-consumption [`settle`](Self::settle) and
    /// [`settle_in`](Self::settle_in) run inside the settlement's transaction
    /// *before* the entry appends: walks the hold's pinned `promo_draws` in
    /// the order admission took them, marks each row's
    /// `charged_minor`/`released_minor` (summing to its `amount_minor`
    /// exactly), and returns each released share to its lot while the lot is
    /// still open — or totals it for the caller to burn after the entry lands,
    /// because only the release's pending credits let the burn's settled debit
    /// keep `FundedReservations`. Returns the amount to burn via
    /// [`burn_promo_in`](Self::burn_promo_in); 0 when every released share
    /// went home.
    ///
    /// `promo_charged` is `min(actual, promo_held)`: the promo part of the
    /// charge the entry writes. A drawn-from closed lot burns rather than
    /// refunds — expiry and correction never turn promo into cash (issue
    /// #609).
    pub(super) async fn consume_promo_draws_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        hold_key: &str,
        promo_held: i64,
        actual_minor: i64,
    ) -> Result<i64, WalletError> {
        if promo_held <= 0 {
            return Ok(0);
        }
        let promo_charged = actual_minor.min(promo_held);
        let mut charge_left = promo_charged;
        let draws = sqlx::query(
            "SELECT d.lot_id, d.amount_minor, l.closed_at \
             FROM oxsum.promo_draws d JOIN oxsum.promo_lots l ON l.lot_id = d.lot_id \
             WHERE d.hold_key = $1 AND d.charged_minor IS NULL \
             ORDER BY l.expires_at ASC NULLS LAST, d.lot_id \
             FOR UPDATE OF d, l",
        )
        .bind(hold_key)
        .fetch_all(&mut **tx)
        .await?;
        let mut burn = 0i64;
        for draw in &draws {
            let lot_id: Uuid = draw.try_get("lot_id")?;
            let reserved: i64 = draw.try_get("amount_minor")?;
            let charged = charge_left.min(reserved);
            charge_left -= charged;
            let released = reserved - charged;
            sqlx::query(
                "UPDATE oxsum.promo_draws SET charged_minor = $2, released_minor = $3 \
                 WHERE hold_key = $1 AND lot_id = $4",
            )
            .bind(hold_key)
            .bind(charged)
            .bind(released)
            .bind(lot_id)
            .execute(&mut **tx)
            .await?;
            if released > 0 {
                let closed: Option<OffsetDateTime> = draw.try_get("closed_at")?;
                if closed.is_none() {
                    // Open lot: the released share returns to the row it was
                    // pinned on — the deterministic un-draw.
                    sqlx::query(
                        "UPDATE oxsum.promo_lots SET remaining_minor = remaining_minor + $2 \
                         WHERE lot_id = $1",
                    )
                    .bind(lot_id)
                    .bind(released)
                    .execute(&mut **tx)
                    .await?;
                } else {
                    // Expired or cancelled: the released share can never be
                    // spent or refunded — the caller burns it to
                    // `Equity:PromoExpiry` once the release has landed.
                    burn += released;
                }
            }
        }
        Ok(burn)
    }
    /// The promo-leg grant a bundle fulfillment or an admin grant posts:
    /// `debit Adjustments / credit Promo`, booked inside the caller's
    /// transaction beside the `promo_lots` row it belongs to. `source` and
    /// `source_ref` are the lot's provenance and idempotency anchor — the
    /// unique pair makes a replayed grant a no-op; `expires_at` of `NULL`
    /// never expires.
    ///
    /// Returns `(lot_id, receipt)`; a replay finds the standing lot and its
    /// standing entry through the anchors rather than granting twice.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn grant_promo_lot_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization: Uuid,
        source: &str,
        source_ref: &str,
        ledger_key: &str,
        minor: i64,
        expires_at: Option<OffsetDateTime>,
        on: Date,
    ) -> Result<(Uuid, Receipt), WalletError> {
        let amt = positive(minor)?;
        // The grant inserts a promo_lots row; the append lock goes first so a
        // hold scanning the organization's lots cannot hold the append lock
        // while waiting on this grant's uncommitted row (issue #723).
        self.lock_append_in(tx).await?;
        // Replay first: the standing lot answers with its own id and the entry
        // id its grant landed under — the anchors make the pair a fact.
        if let Some(row) = sqlx::query(
            "SELECT lot_id FROM oxsum.promo_lots \
             WHERE organization_id = $1 AND source = $2 AND source_ref = $3",
        )
        .bind(organization)
        .bind(source)
        .bind(source_ref)
        .fetch_optional(&mut **tx)
        .await?
        {
            let lot_id: Uuid = row.try_get("lot_id")?;
            let receipt = match self.entry_in(tx, entry_id_for(ledger_key)).await? {
                Some(stored) => stored_receipt(&stored),
                None => {
                    return Err(WalletError::InvalidInput(
                        "the promo grant's ledger entry is missing".into(),
                    ));
                }
            };
            return Ok((lot_id, receipt));
        }
        let lot_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.promo_lots\
             (lot_id, organization_id, source, source_ref, amount_minor, remaining_minor, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $5, $6)",
        )
        .bind(lot_id)
        .bind(organization)
        .bind(source)
        .bind(source_ref)
        .bind(amt.to_minor())
        .bind(expires_at)
        .execute(&mut **tx)
        .await?;
        let receipt = self
            .append_in(
                tx,
                Entry::<Draft, SCALE>::new(entry_id_for(ledger_key), idem(ledger_key)?, on)
                    .with_description(description_of("promo lot grant")?)
                    .debit(self.adjustments, amt, currency())
                    .credit(self.promo, amt, currency()),
            )
            .await?;
        Ok((lot_id, receipt))
    }
    /// The burn posting an expiry or a cancellation writes: `debit Promo /
    /// credit PromoExpiry`, a net-zero move out of the spendable pool that can
    /// never return to refundable money (issue #609). `burn_key` names the
    /// event — `promo-expire:<lot>` or `promo-cancel:<lot>` — so a replayed
    /// pass posts the same entry once. Runs inside the caller's transaction
    /// beside the lot's own close, so the burn and the row update commit or
    /// roll back together.
    pub(crate) async fn burn_promo_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        burn_key: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        self.append_in(
            tx,
            Entry::<Draft, SCALE>::new(entry_id_for(burn_key), idem(burn_key)?, on)
                .with_description(description_of("promo lot expired")?)
                .debit(self.promo, amt, currency())
                .credit(self.promo_expiry, amt, currency()),
        )
        .await
    }
    /// The one-time pool migration (decision D1): moves the unspent remainder of
    /// historical grants out of `Liabilities:Wallet` into `Equity:Bonus` with a
    /// single `debit Wallet, credit Bonus` entry under `RECLASS_KEY`.
    ///
    /// `grants` counts the settled credits on Wallet that pair with an Adjustments
    /// debit — top-ups pair with Cash instead, so money the user paid in is never
    /// misread as granted. The amount moved is
    /// `clamp(grants - settled wallet outflow, 0, wallet balance)`: the
    /// draw-bonus-first convention applied retroactively, so granted money counts
    /// as spent before purchased money and what survives of it is grants minus
    /// everything the wallet ever paid out, bounded by what it still holds.
    ///
    /// Idempotent and self-marking: the entry's fixed key makes a second run a
    /// no-op, which is what makes a failed first pass safe to retry on the next
    /// `open`. Runs from `open` — the migration is lazy, each ledger reclassifying
    /// itself the first time it is opened under the two-pool model. Returns
    /// whether an entry was written. The move is a
    /// rebalancing between pools — the settled_spend_since read excludes the key,
    /// and the bills page never lists it (it has no pending, cash or adjustments
    /// posting, so `classify` has no kind for it).
    pub async fn reclassify_grants(&self) -> Result<bool, WalletError> {
        let schema = &self.tenant_id;
        let amount = {
            let mut conn = self.store.pool().acquire().await?;
            let done: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM ledger_{schema}.entries WHERE idempotency_key = $1)"
            ))
            .bind(RECLASS_KEY.as_bytes())
            .fetch_one(&mut *conn)
            .await?;
            if done {
                return Ok(false);
            }
            let grants: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint \
                 FROM ledger_{schema}.postings p \
                 JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = $1 AND p.direction = 'C' AND p.layer = 'settled' \
                   AND e.log_index IS NOT NULL \
                   AND EXISTS (SELECT 1 FROM ledger_{schema}.postings q \
                       WHERE q.entry_id = p.entry_id AND q.account_index = $2 \
                         AND q.direction = 'D' AND q.layer = 'settled')"
            ))
            .bind(self.wallet.index() as i32)
            .bind(self.adjustments.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
            let outflow: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint \
                 FROM ledger_{schema}.postings p \
                 JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = $1 AND p.direction = 'D' AND p.layer = 'settled' \
                   AND e.log_index IS NOT NULL AND e.idempotency_key <> $2"
            ))
            .bind(self.wallet.index() as i32)
            .bind(RECLASS_KEY.as_bytes())
            .fetch_one(&mut *conn)
            .await?;
            let balance: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(SUM(CASE WHEN p.direction = 'C' THEN p.amount_minor \
                                          ELSE -p.amount_minor END), 0)::bigint \
                 FROM ledger_{schema}.postings p \
                 JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = $1 AND p.layer = 'settled' AND e.log_index IS NOT NULL"
            ))
            .bind(self.wallet.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
            (grants - outflow).clamp(0, balance)
        };
        if amount == 0 {
            return Ok(false);
        }
        let amt = Credits::from_minor(amount);
        let appended = self
            .append(
                Entry::<Draft, SCALE>::new(
                    entry_id_for(RECLASS_KEY),
                    idem(RECLASS_KEY)?,
                    OffsetDateTime::now_utc().date(),
                )
                .with_description(description_of("bonus pool reclassification")?)
                .debit(self.wallet, amt, currency())
                .credit(self.bonus, amt, currency()),
            )
            .await;
        match appended {
            Ok(_) => Ok(true),
            // Two opens of one tenant can both pass the marker check and write at
            // once; the loser of the idempotent append sees the marker on re-read
            // and has nothing to do.
            Err(e) => {
                let mut conn = self.store.pool().acquire().await?;
                let done: bool = sqlx::query_scalar(&format!(
                    "SELECT EXISTS (SELECT 1 FROM ledger_{schema}.entries WHERE idempotency_key = $1)"
                ))
                .bind(RECLASS_KEY.as_bytes())
                .fetch_one(&mut *conn)
                .await?;
                if done { Ok(false) } else { Err(e) }
            }
        }
    }
    /// The one-time promo migration (issue #609): moves the aggregate bonus
    /// pool's *unreserved* balance into a `legacy_migration` promo lot, so the
    /// historical grants the two-pool reclassification parked in `Equity:Bonus`
    /// become a lot like every other — the ledger balance moves `debit Bonus /
    /// credit Promo`, a net-zero rebalancing between two spend-first pools.
    ///
    /// Only the *available* part of the bonus pool migrates: a hold taken
    /// before this build keeps its pending debit on Bonus, and its settlement
    /// still releases and charges through that pool — so the migration moves
    /// `settled + pending-net`, leaving the exact amount the in-flight
    /// reservations may still draw. The lot the remainder lands in never
    /// expires (`expires_at NULL`), and its `source_ref` is the tenant id, so
    /// the unique lot anchor and the fixed entry key both make a rerun a
    /// no-op. Returns whether a lot was written.
    pub(super) async fn migrate_legacy_bonus(&self) -> Result<bool, WalletError> {
        // The marker is the migration lot's own row plus the ledger entry's
        // fixed key: either alone proves the move ran, but checking the ledger
        // key first keeps the path identical to `reclassify_grants`.
        let schema = &self.tenant_id;
        let amount = {
            let mut conn = self.store.pool().acquire().await?;
            let done: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM ledger_{schema}.entries WHERE idempotency_key = $1)"
            ))
            .bind(LEGACY_PROMO_KEY.as_bytes())
            .fetch_one(&mut *conn)
            .await?;
            if done {
                return Ok(false);
            }
            // What the bonus pool can still fund: its settled balance minus the
            // reservations pending holds already carry. Migrating that figure
            // leaves every in-flight bonus hold the settled balance it may
            // settle against.
            self.pool_available_in(&mut conn, self.bonus).await?.max(0)
        };
        if amount == 0 {
            // Nothing to migrate — but the marker still lands, so a wallet that
            // earned bonus credit later (impossible under this build: `adjust`
            // credits Bonus only) or that reopens cannot re-run the move.
            return Ok(false);
        }
        let on = OffsetDateTime::now_utc().date();
        let mut tx = self.store.pool().begin().await?;
        // The lot row and the ledger move are one transaction: the grant
        // cannot exist without the projection that names it. The append lock
        // precedes the lot insert like every other promo writer (issue #723).
        self.lock_append_in(&mut tx).await?;
        let lot_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.promo_lots\
             (lot_id, organization_id, source, source_ref, amount_minor, remaining_minor, expires_at) \
             VALUES ($1, $2, 'legacy_migration', $3, $4, $4, NULL) \
             ON CONFLICT (organization_id, source, source_ref) DO NOTHING",
        )
        .bind(lot_id)
        .bind(self.organization_id_hint_in(&mut tx).await?)
        .bind(&self.tenant_id)
        .bind(amount)
        .execute(&mut *tx)
        .await?;
        let amt = Credits::from_minor(amount);
        let appended = self
            .append_in(
                &mut tx,
                Entry::<Draft, SCALE>::new(
                    entry_id_for(LEGACY_PROMO_KEY),
                    idem(LEGACY_PROMO_KEY)?,
                    on,
                )
                .with_description(description_of("legacy bonus migrates to a promo lot")?)
                .debit(self.bonus, amt, currency())
                .credit(self.promo, amt, currency()),
            )
            .await;
        match appended {
            Ok(_) => {
                tx.commit().await?;
                Ok(true)
            }
            Err(e) => {
                tx.rollback().await?;
                // A racing open may have committed while this one read; the
                // fixed key is the arbiter, like `reclassify_grants`.
                let mut conn = self.store.pool().acquire().await?;
                let done: bool = sqlx::query_scalar(&format!(
                    "SELECT EXISTS (SELECT 1 FROM ledger_{schema}.entries WHERE idempotency_key = $1)"
                ))
                .bind(LEGACY_PROMO_KEY.as_bytes())
                .fetch_one(&mut *conn)
                .await?;
                if done { Ok(false) } else { Err(e) }
            }
        }
    }
}

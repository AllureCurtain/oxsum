//! Settlement: discharging a hold into priced usage, day-anchor aware.

use crate::error::{WalletError, invalid};
use doubleentry::{Draft, Entry, Layer, Posting, Provenance};

use super::{
    Credits, Receipt, SCALE, Wallet, currency, description_of, entry_id_for, idem,
    settlement_key_for,
};
use doubleentry::LedgerStore;
use time::Date;

impl Wallet {
    /// Settle: one entry does two things — releases the hold (a reversal in the pending layer)
    /// and charges the actual usage (recorded in the settled layer).
    ///
    /// The settlement names the hold it releases: `hold_key` is the idempotency key the hold was
    /// taken under. The held amount is read from the hold entry in the ledger — the entry's
    /// pending-layer debit on the wallet account — so there is nothing for the caller to assert
    /// and nothing for a caller to get wrong. The hold's provenance actor is copied onto the
    /// settlement entry, so both the release and the charge attribute to the key that held.
    ///
    /// `actual` may be less than the held amount; the difference returns to the available
    /// balance. `actual` of zero amounts to a full release.
    ///
    /// One hold settles at most once. The settlement entry's idempotency key is
    /// [`settlement_key_for`] of the hold's key, so the ledger's idempotency gate — inside the
    /// append — refuses a second settlement of the same hold with [`WalletError::Conflict`]:
    /// concurrent settlements naming one hold cannot both release it, and retrying the
    /// identical settlement replays it. The wallet's limit still refuses a release the
    /// outstanding reservations cannot cover, as the backstop behind the pairing.
    ///
    /// Naming a hold that is not outstanding — no entry under the key, or an entry that is not
    /// a hold — is [`WalletError::HoldNotFound`], refused even when other holds would cover the
    /// amount.
    ///
    /// `description` is what the entry says it is, in the caller's words — for a gateway turn, the
    /// record built by [`crate::Settlement::description`], which carries the token counts and the
    /// prices so a bill proves the arithmetic and not only the total. It must be derived from the
    /// request alone: two settlements of one hold with different descriptions are different
    /// requests, and the ledger refuses the second.
    pub async fn settle(
        &self,
        hold_key: &str,
        description: &str,
        actual_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        // A call's own argument is bounded before the ledger is consulted.
        idem(hold_key)?;
        // A day the daily anchor sealed is closed to settlements (issue #340):
        // booking into it would rewrite a day leaf the anchor already fixed.
        if self.day_is_anchored(on).await? {
            return Err(WalletError::DaySealed);
        }
        if actual_minor < 0 {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let (
            promo_held,
            bonus_held,
            wallet_held,
            credit_held,
            outstanding_actor,
            outstanding_correlation,
        ) = self.outstanding_hold(hold_key).await?;
        if actual_minor > promo_held + bonus_held + wallet_held + credit_held {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let held = Credits::from_minor(promo_held + bonus_held + wallet_held + credit_held);
        let settle_key = settlement_key_for(hold_key);
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id_for(&settle_key), idem(&settle_key)?, on)
                .with_description(description_of(description)?);
        // The settlement attributes to the key the hold attributed to: the release side and
        // the settled charge side both count toward that key's committed spend, with no
        // caller input to get wrong. A hold taken before key attribution existed settles
        // unattributed, like it was held. The mandate id the hold carried as its provenance
        // correlation is copied the same way, so the settled charge lands inside the
        // mandate's committed sum once the hold's reservation releases (issue #262).
        if outstanding_actor.is_some() || outstanding_correlation.is_some() {
            let mut provenance = Provenance::none();
            if let Some(actor) = outstanding_actor {
                provenance = provenance.with_actor(&actor).map_err(invalid)?;
            }
            if let Some(correlation) = outstanding_correlation {
                provenance = provenance.with_correlation(&correlation).map_err(invalid)?;
            }
            draft = draft.with_provenance(provenance);
        }
        // The release returns exactly what the hold reserved in each pool.
        for (account, held_part) in [
            (self.promo, promo_held),
            (self.bonus, bonus_held),
            (self.wallet, wallet_held),
            (self.credit_line, credit_held),
        ] {
            if held_part > 0 {
                draft = draft.post(
                    Posting::credit(account, Credits::from_minor(held_part), currency())
                        .in_layer(Layer::Pending),
                );
            }
        }
        draft = draft.post(Posting::debit(self.revenue, held, currency()).in_layer(Layer::Pending));
        if actual_minor > 0 {
            // The charge draws the promo pool first — it is the money that can
            // expire — then the bonus pool, the purchased wallet, then the
            // credit line; never more than this hold reserved in each, because
            // taking another hold's reservation is exactly what the per-pool
            // split exists to prevent.
            let promo_take = actual_minor.min(promo_held);
            let bonus_take = (actual_minor - promo_take).min(bonus_held);
            let wallet_take = (actual_minor - promo_take - bonus_take).min(wallet_held);
            let credit_take = actual_minor - promo_take - bonus_take - wallet_take;
            for (account, take) in [
                (self.promo, promo_take),
                (self.bonus, bonus_take),
                (self.wallet, wallet_take),
                (self.credit_line, credit_take),
            ] {
                if take > 0 {
                    draft = draft.debit(account, Credits::from_minor(take), currency());
                }
            }
            draft = draft.credit(self.revenue, Credits::from_minor(actual_minor), currency());
        }
        // The pinned promo draws consume the same transaction the settlement
        // entry lands in: the uncharged share of a hold's draw returns to the
        // lot it was pinned on — or burns to `Equity:PromoExpiry` when the lot
        // already expired or was cancelled — never to refundable money. The
        // row updates run before the entry; the burn posts after it, because
        // the release's pending credits are what let the burn's settled debit
        // keep the pool's `FundedReservations` invariant.
        let mut tx = self.store.pool().begin().await?;
        // `consume_promo_draws_in` locks promo_draws and promo_lots rows; the
        // append lock goes first so the settlement cannot invert against a
        // hold pinning those rows under the same lock (issue #723). A hold
        // with no promo share has no promo rows to lock.
        if promo_held > 0 {
            self.lock_append_in(&mut tx).await?;
        }
        let burn = self
            .consume_promo_draws_in(&mut tx, hold_key, promo_held, actual_minor)
            .await?;
        match self.append_in(&mut tx, draft).await {
            Ok(receipt) => {
                if burn > 0 {
                    // Released into a closed lot: the burn posts after the
                    // release's pending credits, so the promo pool's settled
                    // balance still covers the debit it takes.
                    if let Err(error) = self
                        .burn_promo_in(&mut tx, &format!("{hold_key}:promo-burn"), burn, on)
                        .await
                    {
                        tx.rollback().await?;
                        return Err(error);
                    }
                }
                tx.commit().await?;
                Ok(receipt)
            }
            // The settlement's idempotency key is derived from the hold's key, so a key
            // conflict here means exactly one thing: the hold was already settled.
            Err(WalletError::Conflict(_)) => {
                tx.rollback().await?;
                Err(WalletError::Conflict("hold already settled".into()))
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }
    /// [`settle`](Self::settle) inside the caller's transaction: the same
    /// release-and-charge draft, appended through the engine's borrowed
    /// transaction so the settlement entry, the operation's terminal row, the
    /// subscription draw's consumption and the usage row commit as one unit
    /// (issue #602, D05). The caller owns the commit — a failure leaves nothing
    /// half-written: no release, no charge, no operation close.
    ///
    /// The outstanding hold, sealed-day check, calendar and append all use the
    /// caller's connection. A transaction therefore sees its own hold and never
    /// waits on a second pool slot. The settlement key derives from the hold key,
    /// so a raced second settle cannot write a different release.
    ///
    /// # Errors
    ///
    /// As [`settle`](Self::settle); the transaction is the caller's to commit
    /// or roll back.
    pub async fn settle_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        hold_key: &str,
        description: &str,
        actual_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        idem(hold_key)?;
        if self.day_is_anchored_in(tx, on).await? {
            return Err(WalletError::DaySealed);
        }
        if actual_minor < 0 {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let (
            promo_held,
            bonus_held,
            wallet_held,
            credit_held,
            outstanding_actor,
            outstanding_correlation,
        ) = self.outstanding_hold_in(tx, hold_key).await?;
        if actual_minor > promo_held + bonus_held + wallet_held + credit_held {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let held = Credits::from_minor(promo_held + bonus_held + wallet_held + credit_held);
        let settle_key = settlement_key_for(hold_key);
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id_for(&settle_key), idem(&settle_key)?, on)
                .with_description(description_of(description)?);
        if outstanding_actor.is_some() || outstanding_correlation.is_some() {
            let mut provenance = Provenance::none();
            if let Some(actor) = outstanding_actor {
                provenance = provenance.with_actor(&actor).map_err(invalid)?;
            }
            if let Some(correlation) = outstanding_correlation {
                provenance = provenance.with_correlation(&correlation).map_err(invalid)?;
            }
            draft = draft.with_provenance(provenance);
        }
        for (account, held_part) in [
            (self.promo, promo_held),
            (self.bonus, bonus_held),
            (self.wallet, wallet_held),
            (self.credit_line, credit_held),
        ] {
            if held_part > 0 {
                draft = draft.post(
                    Posting::credit(account, Credits::from_minor(held_part), currency())
                        .in_layer(Layer::Pending),
                );
            }
        }
        draft = draft.post(Posting::debit(self.revenue, held, currency()).in_layer(Layer::Pending));
        if actual_minor > 0 {
            let promo_take = actual_minor.min(promo_held);
            let bonus_take = (actual_minor - promo_take).min(bonus_held);
            let wallet_take = (actual_minor - promo_take - bonus_take).min(wallet_held);
            let credit_take = actual_minor - promo_take - bonus_take - wallet_take;
            for (account, take) in [
                (self.promo, promo_take),
                (self.bonus, bonus_take),
                (self.wallet, wallet_take),
                (self.credit_line, credit_take),
            ] {
                if take > 0 {
                    draft = draft.debit(account, Credits::from_minor(take), currency());
                }
            }
            draft = draft.credit(self.revenue, Credits::from_minor(actual_minor), currency());
        }
        // The pinned promo draws consume inside the caller's transaction: the
        // charged part debits the lots the hold named, the released part
        // returns to them — or burns to `Equity:PromoExpiry` for a closed lot.
        // The burn posts after the release's pending credits, like `settle`'s.
        // The append lock precedes the draw and lot row locks (issue #723);
        // it is re-entrant, so the append below takes it again.
        if promo_held > 0 {
            self.lock_append_in(tx).await?;
        }
        let burn = self
            .consume_promo_draws_in(tx, hold_key, promo_held, actual_minor)
            .await?;
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => {
                Err(WalletError::Conflict("hold already settled".into()))
            }
            other => match other {
                Ok(receipt) => {
                    if burn > 0 {
                        self.burn_promo_in(tx, &format!("{hold_key}:promo-burn"), burn, on)
                            .await?;
                    }
                    Ok(receipt)
                }
                Err(error) => Err(error),
            },
        }
    }
    /// Whether the daily usage anchor has sealed `day` for this ledger's
    /// organization (issue #340).
    ///
    /// A wallet opened over a bare ledger schema — the generative tests'
    /// model-only world — has no `oxsum` tables at all: the seal cannot exist
    /// there, so the check answers "not sealed" instead of failing the write.
    pub(super) async fn day_is_anchored(&self, day: Date) -> Result<bool, WalletError> {
        let table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('oxsum.usage_anchors')::text")
                .fetch_one(self.store.pool())
                .await?;
        if table.is_none() {
            return Ok(false);
        }
        let anchored: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM oxsum.usage_anchors a \
             JOIN oxsum.organizations o ON o.organization_id = a.organization_id \
             WHERE o.tenant_id = $1 AND a.day = $2)",
        )
        .bind(&self.tenant_id)
        .bind(day)
        .fetch_one(self.store.pool())
        .await?;
        Ok(anchored)
    }
    /// What the hold taken under `hold_key` reserved in each pool — the promo
    /// part first, then the bonus, the wallet and the credit-line parts — plus
    /// the provenance actor the hold attributed to: the API key whose spend it
    /// counts toward, if the hold named one.
    ///
    /// The hold entry is found by the id it was given when the hold was taken
    /// ([`entry_id_for`] of the key), and the amounts are the entry's pending-layer
    /// debits on the pool accounts — the ledger's own record of what was reserved,
    /// not a caller assertion. [`WalletError::HoldNotFound`] when no entry is stored
    /// under the key, or when the entry is not a hold.
    pub(super) async fn outstanding_hold(
        &self,
        hold_key: &str,
    ) -> Result<super::transaction_reads::HoldParts, WalletError> {
        let Some(stored) = self.store.get(entry_id_for(hold_key)).await? else {
            return Err(WalletError::HoldNotFound(format!(
                "no hold under key {hold_key:?}"
            )));
        };
        self.hold_parts(&stored, hold_key)
    }
    /// The typed zero-cost usage receipt (issue #602, D05): the settlement
    /// record a turn with no wallet hold still needs — a fully entitlement- or
    /// grant-covered call that served with zero wallet funds.
    ///
    /// Shaped like the statement- and usage-day anchors: a 1-minor balanced
    /// `Equity:Adjustments` debit/credit in the *settled* layer, so the marker
    /// is metadata, not money — no pool moves, no pending reservation, no
    /// revenue. The idempotency key is `settlement_key_for(hold_key)`, the same
    /// figure a wallet-hold settlement derives, so a replayed receipt and a
    /// raced settlement of the same turn cannot both land. The description is
    /// the ordinary [`crate::Settlement::description`] record — `charged: 0`,
    /// the subscription block naming what the entitlement paid — so a bill
    /// proves the turn ran, what it used, and that the wallet was never
    /// asked.
    ///
    /// `actor` and `correlation` are the turn's attribution — the key id and
    /// the mandate id it ran under — carried onto the receipt the way a hold's
    /// settlement carries them, so the key's request history and the mandate's
    /// record stay complete even though no money moved.
    ///
    /// # Errors
    ///
    /// [`WalletError::Conflict`] when the key is already taken — the turn
    /// settled, or a different entry claimed it. Storage failures surface as
    /// [`WalletError`].
    pub async fn usage_receipt(
        &self,
        hold_key: &str,
        description: &str,
        actor: Option<&str>,
        correlation: Option<&str>,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if self.day_is_anchored(on).await? {
            return Err(WalletError::DaySealed);
        }
        let draft = self.usage_receipt_draft(hold_key, description, actor, correlation, on)?;
        match self.append(draft).await {
            Err(WalletError::Conflict(_)) => {
                Err(WalletError::Conflict("hold already settled".into()))
            }
            other => other,
        }
    }
    /// [`usage_receipt`](Self::usage_receipt) inside the caller's transaction —
    /// the borrowed-append half of the zero-cost settle (issue #602). The
    /// receipt commits with whatever else the caller's transaction carries:
    /// the operation's terminal row, the entitlement's consumption, the usage
    /// row. `tx` is the caller's to commit or roll back.
    ///
    /// # Errors
    ///
    /// As [`usage_receipt`](Self::usage_receipt).
    pub async fn usage_receipt_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        hold_key: &str,
        description: &str,
        actor: Option<&str>,
        correlation: Option<&str>,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if self.day_is_anchored_in(tx, on).await? {
            return Err(WalletError::DaySealed);
        }
        let draft = self.usage_receipt_draft(hold_key, description, actor, correlation, on)?;
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => {
                Err(WalletError::Conflict("hold already settled".into()))
            }
            other => other,
        }
    }
    /// The receipt's draft: the 1-minor balanced adjustments marker carrying
    /// the settlement description and the turn's provenance. Shared by
    /// [`usage_receipt`](Self::usage_receipt) and
    /// [`usage_receipt_in`](Self::usage_receipt_in).
    pub(super) fn usage_receipt_draft(
        &self,
        hold_key: &str,
        description: &str,
        actor: Option<&str>,
        correlation: Option<&str>,
        on: Date,
    ) -> Result<Entry<Draft, SCALE>, WalletError> {
        idem(hold_key)?;
        let settle_key = settlement_key_for(hold_key);
        // The marker amount is immaterial — 1 minor like the other anchors —
        // and balanced on one account: the entry is the receipt, not a charge.
        let minor = Credits::from_minor(1);
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id_for(&settle_key), idem(&settle_key)?, on)
                .with_description(description_of(description)?)
                .debit(self.adjustments, minor, currency())
                .credit(self.adjustments, minor, currency());
        if actor.is_some() || correlation.is_some() {
            let mut provenance = Provenance::none();
            if let Some(actor) = actor {
                provenance = provenance.with_actor(actor).map_err(invalid)?;
            }
            if let Some(correlation) = correlation {
                provenance = provenance.with_correlation(correlation).map_err(invalid)?;
            }
            draft = draft.with_provenance(provenance);
        }
        Ok(draft)
    }
    /// The settlement record a hold's settlement committed, when it did: how a
    /// replayed settlement answers the original outcome — the settlement entry
    /// id derives from the hold's key, so the read is one probe (issue #172).
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settled_record(
        &self,
        hold_key: &str,
    ) -> Result<Option<crate::SettlementRecord>, WalletError> {
        let Some(stored) = self
            .store
            .get(entry_id_for(&settlement_key_for(hold_key)))
            .await?
        else {
            return Ok(None);
        };
        Ok(crate::SettlementRecord::parse(
            stored.entry.description().as_str(),
        ))
    }
}

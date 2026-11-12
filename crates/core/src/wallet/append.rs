//! The append path every ledger write goes through: seal a draft against the
//! ledger's current calendar, append it on the pool or inside a borrowed
//! transaction, and take the append advisory lock a promo-row writer needs
//! first.

use doubleentry::{Balanced, Draft, Entry, EntryBatch, LedgerStore, SealContext};

use super::{Receipt, SCALE, Wallet};
use crate::error::{WalletError, invalid};

impl Wallet {
    /// Seals a draft against the ledger's *current* calendar — read back from
    /// storage on every write, not held from `open`, so a month the operator closes
    /// stops new postings dated into it on the very next call rather than after a
    /// restart. This is where "a sealed month accepts no new entries" is enforced:
    /// `Entry::seal` refuses a booking date the calendar's sealed watermark covers.
    async fn seal(
        &self,
        draft: Entry<Draft, SCALE>,
    ) -> Result<Entry<Balanced, SCALE>, WalletError> {
        let calendar = self.store.calendar().await?;
        draft
            .seal(&SealContext {
                accounts: &self.registry,
                calendar: &calendar,
                policy: &self.policy,
            })
            .map_err(invalid)
    }

    pub(super) async fn append(&self, draft: Entry<Draft, SCALE>) -> Result<Receipt, WalletError> {
        let entry = self.seal(draft).await?;
        self.append_sealed(entry).await
    }

    async fn append_sealed(&self, entry: Entry<Balanced, SCALE>) -> Result<Receipt, WalletError> {
        let recorded = self.store.append(&EntryBatch::single(entry)).await?;
        let r = recorded
            .into_iter()
            .next()
            .ok_or_else(|| WalletError::InvalidInput("empty append result".into()))?;
        Ok(Receipt {
            entry_id: r.id,
            log_index: r.index.map(|i| i.get()),
            content_hash: r.content_hash,
            is_new: r.is_new,
        })
    }

    /// Takes the ledger's append advisory lock inside `tx`.
    ///
    /// Every transaction that writes `oxsum.promo_lots` or `oxsum.promo_draws`
    /// takes this lock before its first promo row lock (issue #723). The hold
    /// path already does — `hold_inner` pre-claims it, `hold_for_key` calls
    /// this helper before pinning — and expiry, cancellation, settlement,
    /// granting and the legacy migration call it for the same reason: the
    /// global order is append advisory lock first, promo row locks second, so
    /// a hold waiting on a lot row can never cycle with a writer waiting on
    /// the append lock.
    ///
    /// The lock is transaction-scoped and re-entrant: the engine's own
    /// `append_in_tx` takes it again once the row work is done.
    pub(crate) async fn lock_append_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), WalletError> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(self.store.append_lock())
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// [`append`](Self::append) inside the caller's transaction (issue #602):
    /// the draft seals against the calendar read on the same connection,
    /// then rides the borrowed-transaction append, so the
    /// entry commits with whatever else the caller's transaction carries.
    /// The caller owns the commit; a rolled-back transaction leaves no entry,
    /// no node and no idempotency claim.
    pub(super) async fn append_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        draft: Entry<Draft, SCALE>,
    ) -> Result<Receipt, WalletError> {
        let entry = self.seal_in(tx, draft).await?;
        let recorded = self
            .store
            .append_in_tx(tx, &EntryBatch::single(entry))
            .await?;
        let r = recorded
            .into_iter()
            .next()
            .ok_or_else(|| WalletError::InvalidInput("empty append result".into()))?;
        Ok(Receipt {
            entry_id: r.id,
            log_index: r.index.map(|i| i.get()),
            content_hash: r.content_hash,
            is_new: r.is_new,
        })
    }
}

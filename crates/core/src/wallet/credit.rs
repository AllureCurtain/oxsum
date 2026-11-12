//! Credit line: limit, draws, repayment against the oldest open statement.

use crate::error::WalletError;
use doubleentry::{Direction, Draft, Entry, Layer, StoredEntry};

use super::{
    Receipt, SCALE, Wallet, credit_lock_key, currency, entry_id_for, idem, positive, stored_receipt,
};
use time::Date;

impl Wallet {
    /// Repays the credit line and nothing else: cash in, credit-line balance
    /// restored, the wallet and bonus pools untouched.
    ///
    /// This is the posting a purpose-scoped invoice repayment makes — an order
    /// exists to settle a drawn receivable, so the whole amount books against
    /// `CreditLine` and none of it can become spendable wallet balance or a
    /// granted pool. A `top_up` cannot stand in for it: the top-up's own rule
    /// credits whatever exceeds the draw to the wallet, which is exactly the
    /// cash a repayment purpose must never grant.
    ///
    /// The amount is capped at what is drawn: a repayment larger than the
    /// outstanding debt has nothing to settle, so it is refused rather than
    /// booking the excess into a pool the purpose forbids. The repay figure is
    /// read under the credit lock, serialized with `set_credit_limit` and every
    /// other repaying write, so a stale read can under-pay, never over-credit
    /// the line past its limit.
    ///
    /// The entry replays under `key`: a crash between the append and the order's
    /// fulfillment write leaves a committed repayment the retry finds through
    /// `Self::repay_replay`, and a raced copy of the same repayment reads the
    /// drawn figure under the same lock.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount or one over the
    /// drawn debt; [`WalletError::Conflict`] when `key` is taken by a different
    /// entry; storage failures surface as [`WalletError`].
    pub async fn repay_credit(
        &self,
        key: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let mut tx = self.store.pool().begin().await?;
        let receipt = self.repay_credit_in(&mut tx, key, minor, on).await?;
        tx.commit().await?;
        Ok(receipt)
    }
    /// Serialize repayments and limit changes before acquiring the append lock.
    pub(crate) async fn lock_credit_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), WalletError> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(credit_lock_key(&self.tenant_id))
            .execute(&mut **tx)
            .await?;
        self.lock_append_in(tx).await
    }
    /// Verify the original repayment and raw key on the caller's connection.
    pub(crate) async fn repayment_receipt_in(
        &self,
        connection: &mut sqlx::PgConnection,
        key: &str,
        minor: i64,
    ) -> Result<Option<Receipt>, WalletError> {
        self.entry_in(connection, entry_id_for(key))
            .await?
            .as_ref()
            .map(|stored| self.repay_replay(stored, minor, key))
            .transpose()
    }
    /// Credit-only repayment in the caller's audited transaction.
    pub(crate) async fn repay_credit_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        let identity = idem(key)?;
        self.lock_credit_in(tx).await?;
        if let Some(receipt) = self.repayment_receipt_in(tx, key, minor).await? {
            return Ok(receipt);
        }
        if minor > self.credit_drawn_in(tx).await? {
            return Err(WalletError::InvalidInput(
                "a repayment cannot exceed the outstanding credit debt".into(),
            ));
        }
        let draft = Entry::<Draft, SCALE>::new(entry_id_for(key), identity, on)
            .debit(self.cash, amt, currency())
            .credit(self.credit_line, amt, currency());
        self.append_in(tx, draft).await
    }
    /// Answers for a key that already has a committed repayment entry: the
    /// stored receipt when it is the repayment being asked for — a cash debit
    /// and a credit-line credit of the same amount, nothing reaching the
    /// spendable pools — and the key-reuse conflict when the entry is anything
    /// else, including a top-up that split part of the same amount to wallet.
    fn repay_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let postings = stored.entry.postings();
        let matches = stored.entry.idempotency_key() == &idem(key)?
            && stored.entry.reverses().is_none()
            && stored.index.is_some()
            && postings.len() == 2
            && postings.iter().all(|p| {
                p.layer == Layer::Settled
                    && p.currency == currency()
                    && p.amount.to_minor() == minor
            })
            && postings
                .iter()
                .any(|p| p.account == self.cash && p.direction == Direction::Debit)
            && postings
                .iter()
                .any(|p| p.account == self.credit_line && p.direction == Direction::Credit);
        if matches {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }
}

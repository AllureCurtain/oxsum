//! Funding writes: top-ups, operator adjustments, credit notes and deposit
//! reversals — the entries that move money into or out of the pools outside a
//! hold's lifecycle — with the replay checks that answer a retried key.

use doubleentry::{
    AccountId, Description, Direction, Draft, Entry, Layer, LedgerStore, StoredEntry,
};
use time::Date;

use super::{
    Credits, POOL_SPLIT_ATTEMPTS, Receipt, SCALE, Wallet, credit_lock_key, currency,
    description_of, entry_id_for, idem, positive, stored_receipt,
};
use crate::error::WalletError;

impl Wallet {
    /// Top-up: cash and wallet balance increase together.
    ///
    /// When the organization owes on its credit line, the deposit repays the line
    /// first — `debit Cash, credit CreditLine` for the drawn part, the rest into
    /// the wallet — so paying in restores borrowed headroom before it adds own
    /// funds: "borrow first, settle monthly" needs the receipt to shrink the debt,
    /// not to stack on top of it. The repay split is computed under the credit
    /// lock, which serializes it with `set_credit_limit` and other repaying
    /// top-ups; a draw racing it only ever makes the drawn figure *larger*, so a
    /// stale read can under-repay, never over-credit the line past its limit.
    ///
    /// The entry's shape depends on what is drawn, so a retry could compute a
    /// different split than the one already committed under the same key. The
    /// derived entry id is looked up before anything is sealed: a committed
    /// entry under it answers with its own receipt, whatever shape it took.
    pub async fn top_up(&self, key: &str, minor: i64, on: Date) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        let entry_id = entry_id_for(key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return self.topup_replay(&stored, minor, key);
        }
        if self.credit_drawn().await? <= 0 {
            let receipt = self
                .append(
                    Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on)
                        .debit(self.cash, amt, currency())
                        .credit(self.wallet, amt, currency()),
                )
                .await;
            return match receipt {
                // The key was taken between our read and the append — by a
                // racing copy of this same request, whose repay split may
                // differ from ours, or by a different request under it.
                Err(WalletError::Conflict(_)) => match self.store.get(entry_id).await? {
                    Some(stored) => self.topup_replay(&stored, minor, key),
                    None => Err(WalletError::Conflict(format!(
                        "idempotency key {key:?} is already taken"
                    ))),
                },
                other => other,
            };
        }
        let mut tx = self.store.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(credit_lock_key(&self.tenant_id))
            .execute(&mut *tx)
            .await?;
        // Re-read under the lock: the figure the split is computed against is the
        // one serialized with every other write that restores the line.
        let drawn = self.credit_drawn_in(&mut tx).await?;
        let repay = minor.min(drawn.max(0));
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on).debit(self.cash, amt, currency());
        if repay > 0 {
            draft = draft.credit(self.credit_line, Credits::from_minor(repay), currency());
        }
        if minor - repay > 0 {
            draft = draft.credit(self.wallet, Credits::from_minor(minor - repay), currency());
        }
        let receipt = self.append_in(&mut tx, draft).await;
        match receipt {
            Ok(receipt) => {
                tx.commit().await?;
                Ok(receipt)
            }
            Err(WalletError::Conflict(_)) => {
                tx.rollback().await?;
                // A racing copy committed while this one waited on the lock:
                // its repay split need not be the shape this draft computed.
                match self.store.get(entry_id).await? {
                    Some(stored) => self.topup_replay(&stored, minor, key),
                    None => Err(WalletError::Conflict(format!(
                        "idempotency key {key:?} is already taken"
                    ))),
                }
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// Answers for a key that already has a committed top-up entry: the
    /// stored entry's receipt when it is the top-up being asked for — a cash
    /// debit of the full amount, pool credits adding to it, whatever the
    /// repay split (what is drawn moves after a top-up lands, so the
    /// committed shape is the answer, never the split a retry would compute
    /// now) — and the key-reuse conflict when the entry is anything else. A
    /// credit-limit grant also credits a pool; requiring the cash leg is what
    /// tells the two apart.
    fn topup_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let cash: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| p.account == self.cash && p.direction == Direction::Debit)
            .map(|p| p.amount.to_minor())
            .sum();
        let credited: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| self.is_pool(p.account) && p.direction == Direction::Credit)
            .map(|p| p.amount.to_minor())
            .sum();
        if cash == minor && credited == minor {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }

    /// Adjustment: a signed amount the operator books against the pools, with a
    /// `reason` that becomes the entry's description — covered by its content hash,
    /// so the reason is part of the proof.
    ///
    /// A positive `minor` grants credits into the bonus pool — a grant is the
    /// platform's contribution, never cash-backed. A negative one deducts, drawing
    /// the bonus pool first: what the platform granted is what it takes back before
    /// touching money the user paid for. Borrowed headroom is never a pool a
    /// deduction draws — shrinking what the organization may borrow is the credit
    /// limit's job, so a deduction deeper than granted plus purchased funds is
    /// refused even while undrawn credit remains. The pools' `FundedReservations` limit is
    /// what refuses a deduction the balance cannot carry —
    /// [`WalletError::InsufficientFunds`], the same refusal a hold gets — so the
    /// rule holds under concurrency rather than being a read-then-write check; the
    /// retry only re-reads the split after a racing append moved it.
    /// Zero is [`WalletError::InvalidInput`]: a no-op adjustment would only be a
    /// reason-looking entry that moved nothing.
    pub async fn adjust(
        &self,
        key: &str,
        reason: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if reason.trim().is_empty() {
            return Err(WalletError::InvalidInput(
                "an adjustment needs a reason".into(),
            ));
        }
        let amt = Credits::from_minor(
            minor
                .checked_abs()
                .ok_or(WalletError::InvalidInput("amount is out of range".into()))?,
        );
        if minor == 0 {
            return Err(WalletError::InvalidInput("amount must not be zero".into()));
        }
        if minor > 0 {
            let key = key.to_owned();
            let description = description_of(reason)?;
            match self
                .append(
                    Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                        .with_description(description.clone())
                        .debit(self.adjustments, amt, currency())
                        .credit(self.bonus, amt, currency()),
                )
                .await
            {
                // The key was taken between the read and the append — most often by
                // a racing copy of this same grant, whose stored entry answers the
                // retry rather than a bare conflict.
                Err(WalletError::Conflict(_)) => {
                    return match self.store.get(entry_id_for(&key)).await? {
                        Some(stored) => self.adjust_replay(&stored, &description, minor, &key),
                        None => Err(WalletError::Conflict(format!(
                            "idempotency key {key:?} is already taken"
                        ))),
                    };
                }
                other => return other,
            }
        }
        let description = description_of(reason)?;
        let mut last_err = None;
        for _ in 0..POOL_SPLIT_ATTEMPTS {
            // The deduction's split is a read-then-write choice: a racing append can
            // shrink a pool between the read and the write, which the append refuses
            // — re-read and try the split that fits now.
            let bonus_take = amt.to_minor().min(self.bonus_settled().await?.max(0));
            let wallet_take = amt.to_minor() - bonus_take;
            let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
                .with_description(description.clone());
            if bonus_take > 0 {
                draft = draft.debit(self.bonus, Credits::from_minor(bonus_take), currency());
            }
            if wallet_take > 0 {
                draft = draft.debit(self.wallet, Credits::from_minor(wallet_take), currency());
            }
            match self
                .append(draft.credit(self.adjustments, amt, currency()))
                .await
            {
                Err(error @ WalletError::InsufficientFunds) => last_err = Some(error),
                // The key was taken between our read and the append — by a
                // racing copy of this same deduction, whose pool split may
                // differ from ours, or by a different write under it.
                Err(WalletError::Conflict(_)) => {
                    return match self.store.get(entry_id_for(key)).await? {
                        Some(stored) => self.deduction_replay(&stored, &description, minor, key),
                        None => Err(WalletError::Conflict(format!(
                            "idempotency key {key:?} is already taken"
                        ))),
                    };
                }
                other => return other,
            }
        }
        Err(last_err.unwrap_or(WalletError::InsufficientFunds))
    }

    /// Issues a credit note: one ledger entry that gives back credit the
    /// customer already paid for, without moving money (issue #352, decision
    /// D12). The posting is the mirror of a settlement's charge — debit
    /// Revenue, credit Wallet — and the reason is the entry's description, so
    /// the note is a new fact beside the charge, never a rewrite of it.
    ///
    /// `pub(crate)`: only [`crate::Db::issue_credit_note`] calls it. A retried
    /// key replays the standing entry rather than crediting twice.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount, an empty
    /// reason, or a malformed key.
    pub(crate) async fn credit_note(
        &self,
        idempotency_key: &str,
        reason: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if reason.trim().is_empty() {
            return Err(WalletError::InvalidInput(
                "a credit note needs a reason".into(),
            ));
        }
        if minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a credit note is a positive amount".into(),
            ));
        }
        let key = format!("credit-note/{idempotency_key}");
        let amt = Credits::from_minor(minor);
        let description = description_of(reason)?;
        match self
            .append(
                Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                    .with_description(description.clone())
                    .debit(self.revenue, amt, currency())
                    .credit(self.wallet, amt, currency()),
            )
            .await
        {
            Err(WalletError::Conflict(_)) => match self.store.get(entry_id_for(&key)).await? {
                Some(stored) => self.credit_note_replay(&stored, &description, minor, &key),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other,
        }
    }

    /// Reverses a credited deposit: one ledger entry that takes back purchased
    /// credit the deposit put in — the exact mirror of a top-up, Wallet debited
    /// and Cash credited, the operator's reason as the entry's description
    /// (issue #356, decision D12). The deposit's own credit stays in history
    /// beside the reversal; nothing is rewritten.
    ///
    /// The draw is on the purchased pool alone — granted credit never stood
    /// behind a rail payment, so the bonus and credit-line pools are never
    /// touched. The pool's `FundedReservations` limit is what refuses a
    /// reversal the wallet cannot carry (the customer already spent the
    /// credit): [`WalletError::InsufficientFunds`], enforced inside the append
    /// against the balance the entry would leave, so it holds under
    /// concurrency rather than being a read-then-write check.
    ///
    /// `pub(crate)`: only [`crate::Db::reverse_deposit`] calls it, under a key
    /// derived from the deposit id — a retried reversal replays the standing
    /// entry rather than debiting twice.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount, an empty
    /// reason, or a malformed key; [`WalletError::InsufficientFunds`] when the
    /// purchased pool cannot cover the debit.
    pub(crate) async fn reverse_deposit(
        &self,
        idempotency_key: &str,
        reason: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if reason.trim().is_empty() {
            return Err(WalletError::InvalidInput(
                "a deposit reversal needs a reason".into(),
            ));
        }
        let amt = positive(minor)?;
        let key = format!("deposit-reversal/{idempotency_key}");
        let description = description_of(reason)?;
        match self
            .append(
                Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                    .with_description(description.clone())
                    .debit(self.wallet, amt, currency())
                    .credit(self.cash, amt, currency()),
            )
            .await
        {
            Err(WalletError::Conflict(_)) => match self.store.get(entry_id_for(&key)).await? {
                Some(stored) => self.reverse_deposit_replay(&stored, &description, minor, &key),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other,
        }
    }

    /// Answers for a key that already has a committed deduction entry: the
    /// stored entry's receipt when it drew the amount asked for, under the
    /// same reason — whatever its pool split (balances move after a
    /// deduction lands, so the committed shape is the answer, never the
    /// split a retry would compute now) — and the key-reuse conflict when
    /// the entry is anything else.
    fn deduction_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let drawn: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| {
                self.is_pool(p.account)
                    && p.layer == Layer::Settled
                    && p.direction == Direction::Debit
            })
            .map(|p| p.amount.to_minor())
            .sum();
        // A settlement draws the same settled debits a deduction does; the
        // adjustments credit is the leg only a deduction carries.
        let booked: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| {
                p.account == self.adjustments
                    && p.layer == Layer::Settled
                    && p.direction == Direction::Credit
            })
            .map(|p| p.amount.to_minor())
            .sum();
        if drawn == minor.abs()
            && booked == minor.abs()
            && stored.entry.description().as_str() == description.as_str()
        {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }

    /// The shared replay check for a two-leg entry: `debit` of `minor` on
    /// `debit_account`, `credit` of `minor` on `credit_account`, the reason as
    /// the description. Answers the stored receipt when the entry is exactly
    /// that, else the key-reuse conflict — the same shape `deduction_replay`
    /// uses, generalized so every crediting/debiting write replays its own key.
    fn two_leg_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        debit_account: AccountId,
        credit_account: AccountId,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let leg = |account: AccountId, direction: Direction| -> i64 {
            stored
                .entry
                .postings()
                .iter()
                .filter(|p| {
                    p.account == account && p.layer == Layer::Settled && p.direction == direction
                })
                .map(|p| p.amount.to_minor())
                .sum()
        };
        if leg(debit_account, Direction::Debit) == minor
            && leg(credit_account, Direction::Credit) == minor
            && stored.entry.description().as_str() == description.as_str()
        {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }

    /// Replay for a positive `adjust` (a grant): `debit Adjustments / credit
    /// Bonus`. The negative `adjust` arm goes through `deduction_replay`.
    fn adjust_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        self.two_leg_replay(
            stored,
            self.adjustments,
            self.bonus,
            description,
            minor,
            key,
        )
    }

    /// Replay for a `credit_note`: `debit Revenue / credit Wallet`.
    fn credit_note_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        self.two_leg_replay(stored, self.revenue, self.wallet, description, minor, key)
    }

    /// Replay for a `reverse_deposit`: `debit Wallet / credit Cash`.
    fn reverse_deposit_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        self.two_leg_replay(stored, self.wallet, self.cash, description, minor, key)
    }
}

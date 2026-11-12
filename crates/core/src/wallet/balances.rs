//! Balance reads and the credit line: available, reserved, drawn, the month close.

use super::{
    Credits, LEGACY_PROMO_KEY, RECLASS_KEY, Receipt, SCALE, Wallet, credit_lock_key, currency,
    description_of, entry_id_for, idem, stored_receipt,
};
use crate::error::{WalletError, invalid};
use doubleentry::{Draft, Entry, Layer, LedgerStore, Period, PeriodId, PeriodState, Seal};
use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

impl Wallet {
    /// Available balance in minor units = settled balance - unsettled holds.
    pub async fn available(&self) -> Result<i64, WalletError> {
        Ok(self.settled().await? - self.reserved().await?)
    }
    /// Credits that have settled across the pools: what top-ups and grants put there,
    /// less what settlements and deductions have charged.
    ///
    /// Separate from [`reserved`](Self::reserved) because a hold and a settlement are answered
    /// from different layers: a hold draws on the pools together, a settlement gives back part of
    /// the reserved one.
    pub async fn settled(&self) -> Result<i64, WalletError> {
        self.pool_net(Layer::Settled).await
    }
    /// Credits currently reserved by holds that have not been settled yet, across the pools.
    ///
    /// Never negative: each pool's limit keeps its pending layer on the hold side, which is what
    /// makes this the ceiling on what a settlement may release.
    pub async fn reserved(&self) -> Result<i64, WalletError> {
        Ok(-self.pool_net(Layer::Pending).await?)
    }
    /// [`available`](Self::available) on the caller's connection (issue #602):
    /// the atomic settle's `balance.low` read, taken on the settle's own
    /// transaction so the figure already counts the uncommitted settlement —
    /// a pool read would still see the pre-settle balance and judge the
    /// threshold against money the turn had already spent.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn available_in(&self, conn: &mut sqlx::PgConnection) -> Result<i64, WalletError> {
        let mut total = 0i64;
        for account in [self.promo, self.bonus, self.wallet, self.credit_line] {
            total += self.pool_available_in(conn, account).await?;
        }
        Ok(total)
    }
    /// The credit limit the operator granted, in minor units: the credit-facility
    /// account's settled debit balance, gross of draws. 0 for a wallet that has
    /// never been given a line.
    pub async fn credit_limit(&self) -> Result<i64, WalletError> {
        Ok(-self.account_net(self.facility, Layer::Settled).await?)
    }
    /// The part of the credit line in use, in minor units: the committed limit
    /// minus what the line still carries, so it counts settled draws and the
    /// credit reservations in flight alike. 0 for a wallet with no line at all.
    pub async fn credit_used(&self) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        let committed = self.committed_limit_in(&mut conn).await?;
        let headroom = self.pool_available_in(&mut conn, self.credit_line).await?;
        Ok((committed - headroom).max(0))
    }
    /// What the organization owes on its line right now, in minor units: limit
    /// minus the line's settled balance — settled draws only, not the holds
    /// still reserving it. 0 when nothing is drawn.
    pub(super) async fn credit_drawn(&self) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.credit_drawn_in(&mut conn).await
    }
    /// The [`credit_drawn`](Self::credit_drawn) read on the caller's connection:
    /// `top_up` runs it inside the transaction that holds the credit lock.
    pub(super) async fn credit_drawn_in(
        &self,
        conn: &mut sqlx::PgConnection,
    ) -> Result<i64, WalletError> {
        let committed = self.committed_limit_in(conn).await?;
        let line = self.settled_net_in(conn, self.credit_line).await?;
        Ok((committed - line).max(0))
    }
    /// The facility committed, on the caller's connection: the account is
    /// debit-normal, so its settled net — credits minus debits — reads negated.
    async fn committed_limit_in(&self, conn: &mut sqlx::PgConnection) -> Result<i64, WalletError> {
        Ok(-self.settled_net_in(conn, self.facility).await?)
    }
    /// What the credit line carried for settlements whose booking dates fall in
    /// `[from, to]` inclusive — a statement period's credit-drawn figure: the part
    /// of the month's charges the organization borrowed rather than paid from its
    /// own pools (issue #124).
    ///
    /// Settled debits on the credit line, entries carrying a facility leg excluded:
    /// a limit *shrink* debits the line too, and granting the organization less
    /// credit is not it spending.
    pub async fn credit_drawn_between(&self, from: Date, to: Date) -> Result<i64, WalletError> {
        self.credit_flow("D", Some(from), Some(to)).await
    }
    /// The same read cumulative through `to`: where the FIFO match of repayments
    /// against draws positions a statement's debt. Repayments cover the line's
    /// draws oldest first, so a statement is paid in the measure that all-time
    /// repaid exceeds the draws booked before its period (issue #124).
    pub async fn credit_drawn_through(&self, to: Date) -> Result<i64, WalletError> {
        self.credit_flow("D", None, Some(to)).await
    }
    /// What top-ups and payments have repaid to the credit line over all time —
    /// settled credits on it that carry no facility leg, which excludes the limit
    /// *grants* that credit the line without a payment behind them (issue #124).
    pub async fn credit_repaid(&self) -> Result<i64, WalletError> {
        self.credit_flow("C", None, None).await
    }
    /// Settled postings on the credit line of one `direction` (`D` or `C`),
    /// bounded by booking date, facility-leg entries excluded — those are the
    /// credit-limit changes, and a limit moving is neither a draw nor a repayment.
    async fn credit_flow(
        &self,
        direction: &str,
        from: Option<Date>,
        to: Option<Date>,
    ) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.credit_flow_in(&mut conn, direction, from, to).await
    }
    /// Credit movement on the caller's connection; excludes facility resizing.
    pub(crate) async fn credit_flow_in(
        &self,
        conn: &mut sqlx::PgConnection,
        direction: &str,
        from: Option<Date>,
        to: Option<Date>,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint AS flow \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = $1 AND p.direction = $2 AND p.layer = 'settled' \
               AND e.log_index IS NOT NULL \
               AND ($3::date IS NULL OR e.booking_date >= $3) \
               AND ($4::date IS NULL OR e.booking_date <= $4) \
               AND NOT EXISTS (SELECT 1 FROM ledger_{schema}.postings f \
                               WHERE f.entry_id = p.entry_id AND f.account_index = $5)",
            schema = self.tenant_id,
        );
        let flow: i64 = sqlx::query_scalar(&sql)
            .bind(self.credit_line.index() as i32)
            .bind(direction)
            .bind(from)
            .bind(to)
            .bind(self.facility.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
        Ok(flow)
    }
    /// The log-index window covering the named entries: `(min, max)` of the
    /// log indices the ids sit at, `None`s when none of them is in the log.
    /// A finalized statement stores the window over its settlement entries so
    /// the lines' proof window is a range the caller can walk (issue #124).
    pub async fn log_window_of(
        &self,
        entry_ids: &[Uuid],
    ) -> Result<(Option<i64>, Option<i64>), WalletError> {
        if entry_ids.is_empty() {
            return Ok((None, None));
        }

        let mut conn = self.store.pool().acquire().await?;
        let sql = format!(
            "SELECT min(log_index)::bigint, max(log_index)::bigint \
             FROM ledger_{schema}.entries \
             WHERE entry_id = ANY($1) AND log_index IS NOT NULL",
            schema = self.tenant_id,
        );
        let row = sqlx::query(&sql)
            .bind(entry_ids)
            .fetch_one(&mut *conn)
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }
    /// Grants or resizes the organization's credit line, booking the delta as a
    /// ledger entry: `debit CreditFacility, credit CreditLine` to grow the line,
    /// the reverse to shrink it.
    ///
    /// `limit_minor` is the whole limit, not a delta — the entry's amount is the
    /// difference from what the facility already commits, and an unchanged limit
    /// writes nothing and answers `None`. The computation runs under the credit
    /// lock so two calls cannot both read the same committed figure and double
    /// the line.
    ///
    /// Shrinking is bounded by the line's own `FundedReservations` limit: the
    /// shrink debits `CreditLine`, which refuses a debit beyond what it carries
    /// — the undrawn part — so a limit below the outstanding draw is
    /// [`WalletError::InvalidInput`], not a partial write. A draw racing the
    /// shrink can only make the refusal stricter, never permit a cut that should
    /// have failed.
    ///
    /// The idempotency key derives the entry's id like every other write: a
    /// retried call under one key replays, a second call under a fresh key is a
    /// second change.
    pub async fn set_credit_limit(
        &self,
        key: &str,
        limit_minor: i64,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        let mut tx = self.store.pool().begin().await?;
        let result = self
            .set_credit_limit_in(&mut tx, key, limit_minor, on)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Resize credit inside the caller's transaction so terms and actor audit
    /// can commit with the ledger effect. The caller owns commit and rollback.
    pub(crate) async fn set_credit_limit_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        key: &str,
        limit_minor: i64,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        if limit_minor < 0 {
            return Err(WalletError::InvalidInput(
                "a credit limit cannot be negative".into(),
            ));
        }
        idem(key)?;
        // The target rides in the description: the entry then says what it set,
        // not just what it moved, and the replay check can tell the same request
        // — same key, same limit — from a different one under a reused key.
        let description = description_of(&format!("credit limit set to {limit_minor} minor"))?;
        let entry_id = entry_id_for(key);
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(credit_lock_key(&self.tenant_id))
            .execute(&mut **tx)
            .await?;
        // A same-key call may have committed while this one waited on the
        // lock: a matching change replays its receipt, anything else is the
        // key-reuse conflict — rather than reporting a no-op for a change
        // that did write.
        if let Some(stored) = self.entry_in(tx, entry_id).await? {
            if stored.entry.description().as_str() == description.as_str() {
                return Ok(Some(stored_receipt(&stored)));
            }
            return Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )));
        }
        let committed = self.committed_limit_in(tx).await?;
        let delta = limit_minor.checked_sub(committed).ok_or_else(|| {
            WalletError::InvalidInput("credit limit delta exceeds integer range".into())
        })?;
        if delta == 0 {
            return Ok(None);
        }
        let amt = Credits::from_minor(delta.abs());
        let draft = if delta > 0 {
            Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on)
                .with_description(description)
                .debit(self.facility, amt, currency())
                .credit(self.credit_line, amt, currency())
        } else {
            Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on)
                .with_description(description)
                .debit(self.credit_line, amt, currency())
                .credit(self.facility, amt, currency())
        };
        let receipt = self.append_in(tx, draft).await;
        match receipt {
            // A shrink that would cut into the outstanding draw surfaces as the
            // credit line's `InsufficientFunds`; name what it actually is.
            Err(WalletError::InsufficientFunds) if delta < 0 => Err(WalletError::InvalidInput(
                "the credit limit cannot go below what is still drawn".into(),
            )),
            Ok(receipt) => {
                // The re-set line re-arms the organization's threshold latches
                // (issue #232), like the key PATCH clears its forecast date.
                sqlx::query(
                    "DELETE FROM oxsum.budget_threshold_fires \
                     WHERE scope = 'organization' AND scope_id = $1",
                )
                .bind(Uuid::parse_str(&self.tenant_id).unwrap_or_default())
                .execute(&mut **tx)
                .await?;
                Ok(Some(receipt))
            }
            Err(error) => Err(error),
        }
    }
    /// Monthly facility and drawn/reserved usage in the caller's snapshot.
    pub(crate) async fn credit_state_in(
        &self,
        conn: &mut sqlx::PgConnection,
    ) -> Result<(i64, i64), WalletError> {
        let committed = self.committed_limit_in(conn).await?;
        let headroom = self.pool_available_in(conn, self.credit_line).await?;
        Ok((committed, (committed - headroom).max(0)))
    }
    /// Credits the pools have been charged on or after `from`, in minor units: the
    /// settled-layer debits on the pool accounts, which is what a settlement or a
    /// deduction books when it draws the balance down.
    ///
    /// The gross debits, not the layer's net: a top-up credits the same layer, and
    /// subtracting the credits would read a month of top-ups as negative spend. Holds
    /// live in the pending layer, so an outstanding hold is not spend either, the
    /// pool reclassifications are excluded by their idempotency keys — moving money
    /// between pools is not spend — and a credit-limit change is excluded by the
    /// facility leg it always carries: shrinking the line debits the credit pool,
    /// but giving the organization less credit is not charging it. A window in
    /// which nothing settled is an empty sum: zero, not an error.
    pub async fn settled_spend_since(&self, from: Date) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        let sql = format!(
            "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint AS spend \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND p.direction = 'D' AND p.layer = 'settled' \
               AND e.log_index IS NOT NULL AND e.booking_date >= $2 \
               AND e.idempotency_key <> ALL($3) \
               AND NOT EXISTS (SELECT 1 FROM ledger_{schema}.postings f \
                               WHERE f.entry_id = p.entry_id AND f.account_index = $4)",
            schema = self.tenant_id,
        );
        let spend: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.promo.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(from)
            .bind(vec![
                RECLASS_KEY.as_bytes().to_vec(),
                LEGACY_PROMO_KEY.as_bytes().to_vec(),
            ])
            .bind(self.facility.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
        Ok(spend)
    }
    /// What the current UTC month has charged the wallet, in minor units — the dashboard's
    /// "spent this month".
    ///
    /// The month is the ledger's own. Entries carry whole days and every write is dated with
    /// the server's current UTC date (`today` in `crates/server/src/lib.rs`), so the first
    /// instant of the month is its first day as a booking date, and the window is inclusive
    /// of that day.
    pub async fn month_spend(&self) -> Result<i64, WalletError> {
        // The first of any month exists, so this cannot fail; it is mapped rather than
        // unwrapped because a panic on the way out of the domain layer is not an option.
        let first = OffsetDateTime::now_utc()
            .date()
            .replace_day(1)
            .map_err(invalid)?;
        self.settled_spend_since(first).await
    }
    /// Closes a month: defines the `YYYY-MM` period covering `first`'s month if the
    /// ledger does not know it, stops new postings into it and seals it — the seal
    /// the store appends is the month's closing record, chained onto the seals
    /// before it. From then on every write whose booking date falls in the month is
    /// refused at seal time (see `Wallet::seal`), which is product.md's "after
    /// closing, that month accepts no new entries" made structural rather than
    /// conventional.
    ///
    /// `first` must be the month's first day — the period id, `YYYY-MM`, is what
    /// callers and the admin page both say. Only a month that has fully ended can
    /// be closed: one that still contains the server's current UTC date would
    /// refuse the very next request the gateway writes, so it is rejected here.
    ///
    /// Closing an already-sealed month answers its existing record: the call is
    /// idempotent, because the admin page's button and a retried request must
    /// agree with what the seals table already holds.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a `first` that is not a month's first day
    /// or a month that has not ended; storage failures surface as
    /// [`WalletError::Storage`].
    pub async fn close_month(&self, first: Date) -> Result<Seal, WalletError> {
        if first.day() != 1 {
            return Err(invalid("the month's first day"));
        }
        let last = first
            .replace_day(first.month().length(first.year()))
            .map_err(invalid)?;
        if last >= OffsetDateTime::now_utc().date() {
            return Err(invalid("only a month that has fully ended can be closed"));
        }
        let period = PeriodId::new(format!(
            "{:04}-{:02}",
            first.year(),
            u8::from(first.month())
        ))
        .map_err(invalid)?;

        match self.store.calendar().await?.get(&period).map(|p| p.state) {
            // Already closed: answer the record rather than a conflict — a repeat
            // click and a retry must say the same thing.
            Some(PeriodState::Sealed) => {
                return self
                    .store
                    .seals()
                    .await?
                    .into_iter()
                    .find(|seal| seal.period == period)
                    .ok_or_else(|| invalid("a sealed period with no seal"));
            }
            Some(PeriodState::Closing) => {}
            Some(PeriodState::Open) => {
                self.store
                    .transition_period(&period, PeriodState::Closing)
                    .await?;
            }
            None => {
                self.store
                    .define_period(&Period::new(period.clone(), first, last).map_err(invalid)?)
                    .await?;
                self.store
                    .transition_period(&period, PeriodState::Closing)
                    .await?;
            }
        }
        self.store.seal_period(&period).await.map_err(Into::into)
    }
    /// The seals this ledger holds, in chain order — the closing records the admin's
    /// closing page lists.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError::Storage`].
    pub async fn seals(&self) -> Result<Vec<Seal>, WalletError> {
        self.store.seals().await.map_err(Into::into)
    }
}

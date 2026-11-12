//! Plan-fee journals and order financials: accruals, prepayments, refunds, corrections.

use crate::error::{WalletError, invalid};
use doubleentry::{AccountId, Draft, Entry, EntryId};

use super::{
    CASH, Credits, DISCOUNTS, RECEIVABLES, Receipt, SCALE, Wallet, currency, description_of,
    entry_id_for, find_account, idem, positive, stored_receipt,
};
use sqlx::Row;
use time::Date;
use uuid::Uuid;

impl Wallet {
    /// Retain complete refund Cash and its proven original Wallet allocation.
    /// The source owner holds the ledger append lock and commits its effect.
    pub(crate) async fn nonplan_refund_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        refund: Uuid,
        cash: i64,
        wallet: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amount = positive(cash)?;
        if !(0..=cash).contains(&wallet) {
            return Err(invalid("invalid non-plan refund allocation"));
        }
        let key = format!("order-refund/{refund}");
        let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on).credit(
            self.cash,
            amount,
            currency(),
        );
        if wallet > 0 {
            draft = draft.debit(self.wallet, Credits::from_minor(wallet), currency());
        }
        if cash > wallet {
            draft = draft.debit(
                self.adjustments,
                Credits::from_minor(cash - wallet),
                currency(),
            );
        }
        self.append_in(tx, draft).await
    }
    /// Restore a proven historical wrong-source debit without another Cash leg.
    pub(crate) async fn nonplan_refund_restitution_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        refund: Uuid,
        minor: i64,
        revision: &str,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amount = positive(minor)?;
        let key = format!("nonplan-refund-restitution/{refund}");
        self.append_in(
            tx,
            Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                .with_description(description_of(&format!("nonplan-refund-v1:{revision}"))?)
                .debit(self.adjustments, amount, currency())
                .credit(self.wallet, amount, currency()),
        )
        .await
    }
    /// Resolve an original-source account against this wallet's registered chart.
    /// Canonical entry hashes bind account IDs rather than database path labels.
    pub(crate) fn source_account(&self, path: &str) -> Result<AccountId, WalletError> {
        find_account(&self.registry, path)
    }
    /// Retain complete chargeback Cash outflow and only the proven Wallet part.
    /// The caller holds the source and ledger locks; no CreditLine debt is invented.
    pub(crate) async fn nonplan_dispute_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        dispute_id: Uuid,
        loss: i64,
        wallet_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amount = positive(loss)?;
        if !(0..=loss).contains(&wallet_minor) {
            return Err(invalid("invalid non-plan chargeback allocation"));
        }
        let key = format!("dispute-recharge/{dispute_id}");
        let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on).credit(
            self.cash,
            amount,
            currency(),
        );
        if wallet_minor > 0 {
            draft = draft.debit(self.wallet, Credits::from_minor(wallet_minor), currency());
        }
        if loss > wallet_minor {
            draft = draft.debit(
                self.adjustments,
                Credits::from_minor(loss - wallet_minor),
                currency(),
            );
        }
        self.append_in(tx, draft).await
    }
    /// Compensate old false Cash plus confirmed loss without reopening CreditLine.
    /// Original retained Wallet funds only the independently bounded actual loss.
    pub(crate) async fn nonplan_chargeback_correction_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        dispute: Uuid,
        cash_minor: i64,
        wallet_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amount = positive(cash_minor)?;
        if wallet_minor < 0 || wallet_minor >= cash_minor {
            return Err(invalid(
                "invalid historical chargeback correction allocation",
            ));
        }
        let key = format!("nonplan-chargeback-correction/{dispute}");
        let mut entry = Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on).credit(
            self.cash,
            amount,
            currency(),
        );
        if wallet_minor > 0 {
            entry = entry.debit(self.wallet, Credits::from_minor(wallet_minor), currency());
        }
        entry = entry.debit(
            self.adjustments,
            Credits::from_minor(cash_minor - wallet_minor),
            currency(),
        );
        self.append_in(tx, entry).await
    }
    /// The accrual a closing period's journal books: charge lines
    /// `debit Receivables / credit PlanFees`, discounts `debit Discounts /
    /// credit Receivables`, promo coverage `debit Promo / credit Receivables`
    /// — one entry per journal, keyed `plan-journal/<id>/accrual`, inside the
    /// caller's transaction so the journal row, its lines and the postings
    /// land or roll back together.
    ///
    /// `charges_minor` is the sum of the charge kinds (base fee, overage, tax);
    /// `discount_minor`/`promo_minor` are positive magnitudes of the credit
    /// legs. `promo_minor` must not exceed what the promo pool settles — the
    /// caller clamps it, `FundedReservations` is the backstop.
    ///
    /// A zero-leg journal (a free plan's 0-total close) posts nothing and
    /// returns `None` — an entry cannot carry an empty amount set.
    pub(crate) async fn plan_journal_accrual_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: &uuid::Uuid,
        charges_minor: i64,
        discount_minor: i64,
        promo_minor: i64,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        if charges_minor < 0 || discount_minor < 0 || promo_minor < 0 {
            return Err(WalletError::InvalidInput(
                "journal leg amounts must not be negative".into(),
            ));
        }
        if charges_minor + discount_minor + promo_minor == 0 {
            return Ok(None);
        }
        let key = format!("plan-journal/{journal_id}/accrual");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.receipt_in(tx, entry_id).await? {
            return Ok(Some(stored));
        }
        let mut draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on).with_description(
            description_of(&format!("plan fee journal {journal_id}: accrual"))?,
        );
        if charges_minor > 0 {
            let amt = Credits::from_minor(charges_minor);
            draft = draft.debit(self.receivables, amt, currency()).credit(
                self.plan_fees,
                amt,
                currency(),
            );
        }
        if discount_minor > 0 {
            let amt = Credits::from_minor(discount_minor);
            draft = draft.debit(self.discounts, amt, currency()).credit(
                self.receivables,
                amt,
                currency(),
            );
        }
        if promo_minor > 0 {
            let amt = Credits::from_minor(promo_minor);
            draft =
                draft
                    .debit(self.promo, amt, currency())
                    .credit(self.receivables, amt, currency());
        }
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => match self.receipt_in(tx, entry_id).await? {
                Some(stored) => Ok(Some(stored)),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other.map(Some),
        }
    }
    /// The prepayment a fulfilled `planPeriod` order books for the period it
    /// authorizes: `debit Cash / credit UnearnedPlanFees` — deferred revenue,
    /// not yet the receivable the period close will post. Keyed
    /// `payment-order/<order>/prepay`; replay-idempotent like every order leg.
    pub(crate) async fn plan_prepay_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        order_id: &uuid::Uuid,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        let key = format!("payment-order/{order_id}/prepay");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.entry_in(tx, entry_id).await? {
            return Ok(stored_receipt(&stored));
        }
        let draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on)
            .with_description(description_of(&format!(
                "plan period prepayment for order {order_id}"
            ))?)
            .debit(self.cash, amt, currency())
            .credit(self.unearned_plan_fees, amt, currency());
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => match self.entry_in(tx, entry_id).await? {
                Some(stored) => Ok(stored_receipt(&stored)),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other,
        }
    }
    /// Reclassifies prepayment into the receivable a closing journal opened:
    /// `debit UnearnedPlanFees / credit Receivables` up to what the journal
    /// still owes. Keyed `plan-journal/<id>/prepay-settle` — one journal
    /// consumes prepayment exactly once.
    pub(crate) async fn plan_prepay_settle_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: &uuid::Uuid,
        minor: i64,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        if minor <= 0 {
            return Ok(None);
        }
        let amt = Credits::from_minor(minor);
        let key = format!("plan-journal/{journal_id}/prepay-settle");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.receipt_in(tx, entry_id).await? {
            return Ok(Some(stored));
        }
        let draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on)
            .with_description(description_of(&format!(
                "plan fee journal {journal_id}: prepayment applied"
            ))?)
            .debit(self.unearned_plan_fees, amt, currency())
            .credit(self.receivables, amt, currency());
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => match self.receipt_in(tx, entry_id).await? {
                Some(stored) => Ok(Some(stored)),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other.map(Some),
        }
    }
    /// Reads the actual legacy refund legs, never inferring cash from a mutable
    /// journal state. Only the old Discounts/Cash-or-Receivables shape qualifies.
    pub(crate) async fn legacy_plan_refund_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        entry_id: Option<Uuid>,
    ) -> Result<Option<crate::plan_refunds::LegacyPlanRefund>, WalletError> {
        let Some(entry_id) = entry_id else {
            return Ok(None);
        };
        let sql = format!(
            "SELECT a.path,p.direction,p.amount_minor FROM ledger_{}.postings p JOIN ledger_{}.accounts a ON a.account_index=p.account_index JOIN ledger_{}.entries e ON e.entry_id=p.entry_id WHERE p.entry_id=$1 AND p.layer='settled' AND e.log_index IS NOT NULL",
            self.tenant_id, self.tenant_id, self.tenant_id
        );
        let rows = sqlx::query(&sql)
            .bind(entry_id)
            .fetch_all(&mut **tx)
            .await?;
        if rows.is_empty() {
            return Ok(None);
        }

        let mut old = crate::plan_refunds::LegacyPlanRefund {
            entry_id,
            cash: 0,
            receivable: 0,
            discount: 0,
        };
        for row in rows {
            let path: String = row.try_get("path")?;
            let direction: String = row.try_get("direction")?;
            let amount: i64 = row.try_get("amount_minor")?;
            let target = match (path.as_str(), direction.as_str()) {
                (CASH, "C") => &mut old.cash,
                (RECEIVABLES, "C") => &mut old.receivable,
                (DISCOUNTS, "D") => &mut old.discount,
                _ => {
                    return Err(WalletError::Conflict(
                        "legacy plan-refund entry has unsupported legs".into(),
                    ));
                }
            };
            *target = target
                .checked_add(amount)
                .ok_or_else(|| invalid("refund postings overflow"))?;
        }
        if old.cash.checked_add(old.receivable) != Some(old.discount) {
            return Err(WalletError::Conflict(
                "legacy plan-refund entry is not balanced".into(),
            ));
        }
        Ok(Some(old))
    }
    /// Original source credits, read through this transaction's snapshot.
    pub(crate) async fn plan_source_credit_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        path: &str,
    ) -> Result<i64, WalletError> {
        self.plan_source_minor_in(tx, key, path, "C").await
    }
    /// Actual debit allocation in a source-linked entry.
    pub(crate) async fn plan_source_debit_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        path: &str,
    ) -> Result<i64, WalletError> {
        self.plan_source_minor_in(tx, key, path, "D").await
    }
    async fn plan_source_minor_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        path: &str,
        direction: &str,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT COALESCE(SUM(p.amount_minor),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a ON a.account_index=p.account_index JOIN ledger_{}.entries e ON e.entry_id=p.entry_id WHERE p.entry_id=$1 AND a.path=$2 AND p.layer='settled' AND p.direction=$3 AND e.log_index IS NOT NULL",
            self.tenant_id, self.tenant_id, self.tenant_id
        );
        Ok(sqlx::query_scalar(&sql)
            .bind(entry_id_for(key).as_uuid())
            .bind(path)
            .bind(direction)
            .fetch_one(&mut **tx)
            .await?)
    }
    /// The original prepayment allocation, retained even after a buggy refund
    /// lowered a journal's mutable settled counter.
    pub(crate) async fn plan_journal_prepaid_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: Uuid,
    ) -> Result<i64, WalletError> {
        self.plan_source_credit_in(
            tx,
            &format!("plan-journal/{journal_id}/prepay-settle"),
            RECEIVABLES,
        )
        .await
    }
    /// Actual chargeback cash entries tied to this order, including a crash
    /// after posting and before its dispute row became terminal.
    pub(crate) async fn plan_order_chargebacks_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        order_id: Uuid,
    ) -> Result<i64, WalletError> {
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT dispute_id FROM oxsum.disputes WHERE order_id=$1")
                .bind(order_id)
                .fetch_all(&mut **tx)
                .await?;
        let mut total = 0i64;
        for id in ids {
            let amount = self
                .plan_source_credit_in(tx, &format!("dispute-recharge/{id}"), CASH)
                .await?;
            total = total
                .checked_add(amount)
                .ok_or_else(|| invalid("chargeback amount overflows"))?;
        }
        Ok(total)
    }
    /// Post the desired cash allocation minus its retained legacy legs. The
    /// exception debit is explicit reconciliation suspense, never customer debt.
    pub(crate) async fn plan_refund_cash_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        allocation: &crate::PlanRefundRecovery,
        old: Option<&crate::plan_refunds::LegacyPlanRefund>,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let key = format!("plan-refund-cash/{}", allocation.refund_id);
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.entry_in(tx, entry_id).await? {
            return Ok(stored_receipt(&stored));
        }
        let old_cash = old.map_or(0, |v| v.cash);
        let old_ar = old.map_or(0, |v| v.receivable);
        let old_discount = old.map_or(0, |v| v.discount);
        let legs = [
            (self.unearned_plan_fees, allocation.deferred_minor),
            (self.discounts, allocation.earned_minor - old_discount),
            (self.refund_payable, allocation.payable_minor),
            (self.adjustments, allocation.unallocated_minor),
            (self.receivables, old_ar),
            (self.cash, old_cash - allocation.cash_minor),
        ];
        if legs.iter().all(|(_, amount)| *amount == 0) {
            let previous = old.ok_or_else(|| invalid("refund allocation has no source legs"))?;
            let stored = self
                .entry_in(tx, EntryId::from_uuid(previous.entry_id))
                .await?
                .ok_or_else(|| invalid("legacy refund entry disappeared"))?;
            return Ok(stored_receipt(&stored));
        }
        let mut draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on).with_description(
            description_of(&format!(
                "confirmed plan refund {}: original order {}; unallocated {}",
                allocation.refund_id, allocation.order_id, allocation.unallocated_minor
            ))?,
        );
        for (account, signed) in legs {
            if signed > 0 {
                draft = draft.debit(account, Credits::from_minor(signed), currency());
            } else if signed < 0 {
                draft = draft.credit(account, Credits::from_minor(-signed), currency());
            }
        }
        self.append_in(tx, draft).await
    }
    /// Release one completed service day's original deferred fee to income.
    pub(crate) async fn plan_daily_recognition_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        minor: i64,
        on: Date,
    ) -> Result<Option<Uuid>, WalletError> {
        if minor == 0 {
            return Ok(None);
        }
        let amount = positive(minor)?;
        let entry = Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
            .with_description(description_of(
                "v2 plan fee: completed UTC service-day recognition",
            )?)
            .debit(self.unearned_plan_fees, amount, currency())
            .credit(self.plan_fees, amount, currency());
        self.append_in(tx, entry)
            .await
            .map(|r| Some(*r.entry_id.as_uuid()))
    }
    /// Open a corrected fee's receivable without consuming predecessor cash.
    pub(crate) async fn invoice_correction_accrual_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal: Uuid,
        earned: i64,
        deferred: i64,
        on: Date,
    ) -> Result<Option<Uuid>, WalletError> {
        let total = earned
            .checked_add(deferred)
            .filter(|_| earned >= 0 && deferred >= 0)
            .ok_or_else(|| invalid("corrected invoice accrual overflows"))?;
        if total == 0 {
            return Ok(None);
        }
        let key = format!("invoice-correction/{journal}/accrual");
        let mut entry = Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
            .with_description(description_of(
                "Source-linked corrected fee; original service and cash ownership retained",
            )?)
            .debit(self.receivables, Credits::from_minor(total), currency());
        for (account, amount) in [
            (self.plan_fees, earned),
            (self.unearned_plan_fees, deferred),
        ] {
            if amount > 0 {
                entry = entry.credit(account, Credits::from_minor(amount), currency());
            }
        }
        self.append_in(tx, entry)
            .await
            .map(|r| Some(*r.entry_id.as_uuid()))
    }
    /// Reverse exact v2 net earned/deferred amounts without moving Cash.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn plan_daily_void_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal: Uuid,
        earned: i64,
        deferred: i64,
        receivable: i64,
        collected: i64,
        on: Date,
    ) -> Result<Option<Uuid>, WalletError> {
        if [earned, deferred, receivable, collected]
            .iter()
            .any(|v| *v < 0)
            || earned.checked_add(deferred) != receivable.checked_add(collected)
        {
            return Err(invalid("daily void source amounts differ"));
        }
        if earned == 0 && deferred == 0 {
            return Ok(None);
        }
        let key = format!("plan-journal/{journal}/void");
        let mut entry = Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
            .with_description(description_of(
                "v2 plan journal: reverse net earned and deferred fees; cash unchanged",
            )?);
        for (account, minor, debit) in [
            (self.discounts, earned, true),
            (self.unearned_plan_fees, deferred, true),
            (self.receivables, receivable, false),
            (self.refund_payable, collected, false),
        ] {
            if minor > 0 {
                let amount = Credits::from_minor(minor);
                entry = if debit {
                    entry.debit(account, amount, currency())
                } else {
                    entry.credit(account, amount, currency())
                };
            }
        }
        self.append_in(tx, entry)
            .await
            .map(|r| Some(*r.entry_id.as_uuid()))
    }
    /// Remove proven duplicated plan revenue, reducing only original open debt
    /// and retaining already collected excess as payable. Cash never moves.
    pub(crate) async fn plan_usage_correction_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        correction: &crate::PlanUsageCorrection,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amount = positive(correction.duplicate_minor)?;
        if correction.receivable_minor < 0
            || correction.payable_minor < 0
            || correction
                .receivable_minor
                .checked_add(correction.payable_minor)
                != Some(correction.duplicate_minor)
        {
            return Err(invalid("invalid duplicate-usage allocation"));
        }
        let key = format!("plan-usage-correction/{}", correction.journal_id);
        let mut draft = Entry::<Draft,SCALE>::new(entry_id_for(&key),idem(&key)?,on)
            .with_description(description_of(&format!("historical duplicate usage correction {}; original accrual {}; receivable {}; refund payable {}; evidence sha256 {}; cash unchanged",correction.journal_id,correction.original_entry_id,correction.receivable_minor,correction.payable_minor,correction.evidence_sha256))?)
            .debit(self.plan_fees,amount,currency());
        for (account, minor) in [
            (self.receivables, correction.receivable_minor),
            (self.refund_payable, correction.payable_minor),
        ] {
            if minor > 0 {
                draft = draft.credit(account, Credits::from_minor(minor), currency());
            }
        }
        self.append_in(tx, draft).await
    }
    /// Record the full observed chargeback cash. Only original collected funds
    /// restore debt; a voided bill draws its payable and excess is review suspense.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn plan_journal_dispute_recharge_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: &Uuid,
        dispute_id: &Uuid,
        cash_minor: i64,
        receivable_minor: i64,
        payable_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(cash_minor)?;
        let suspense = cash_minor
            .checked_sub(receivable_minor)
            .and_then(|v| v.checked_sub(payable_minor))
            .filter(|v| *v >= 0)
            .ok_or_else(|| invalid("invalid chargeback source allocation"))?;
        let key = format!("dispute-recharge/{dispute_id}");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.entry_in(tx, entry_id).await? {
            return Ok(stored_receipt(&stored));
        }
        let mut draft=Entry::<Draft,SCALE>::new(entry_id,idem(&key)?,on)
            .with_description(description_of(&format!("plan fee journal {journal_id}: chargeback cash {cash_minor}, receivable {receivable_minor}, payable {payable_minor}, review suspense {suspense}"))?)
            .credit(self.cash,amt,currency());
        for (account, minor) in [
            (self.receivables, receivable_minor),
            (self.refund_payable, payable_minor),
            (self.adjustments, suspense),
        ] {
            if minor < 0 {
                return Err(invalid("negative chargeback allocation"));
            }
            if minor > 0 {
                draft = draft.debit(account, Credits::from_minor(minor), currency());
            }
        }
        self.append_in(tx, draft).await
    }
    /// Exact statement payment/return Cash and its original AR/payable allocation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn statement_cash_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        inflow: bool,
        cash: i64,
        receivable: i64,
        payable: i64,
        suspense: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if cash <= 0
            || receivable < 0
            || payable < 0
            || suspense < 0
            || receivable
                .checked_add(payable)
                .and_then(|v| v.checked_add(suspense))
                != Some(cash)
            || inflow && suspense != 0
        {
            return Err(invalid("invalid statement cash allocation"));
        }
        let mut draft=Entry::<Draft,SCALE>::new(entry_id_for(key),idem(key)?,on)
            .with_description(description_of(&format!("statement cash {key}; cash {cash}, receivable {receivable}, payable {payable}, suspense {suspense}"))?);
        draft = if inflow {
            draft.debit(self.cash, Credits::from_minor(cash), currency())
        } else {
            draft.credit(self.cash, Credits::from_minor(cash), currency())
        };
        for (account, amount) in [
            (self.receivables, receivable),
            (self.refund_payable, payable),
            (self.adjustments, suspense),
        ] {
            if amount > 0 {
                draft = if inflow {
                    draft.credit(account, Credits::from_minor(amount), currency())
                } else {
                    draft.debit(account, Credits::from_minor(amount), currency())
                };
            }
        }
        self.append_in(tx, draft).await
    }
    /// The reversal a staff void posts: every line's leg reversed in one entry
    /// — `debit PlanFees / credit Receivables` for the charge sums,
    /// `debit Receivables / credit Discounts` and `debit Receivables / credit
    /// Promo` for the credit legs, so the journal nets to zero and no history
    /// is rewritten. On a `paid` journal the receivable is already zero, so the
    /// settlement unwinds to `RefundPayable` instead — what the customer is
    /// owed back. `paid` selects between the two shapes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn plan_journal_void_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: &uuid::Uuid,
        charges_minor: i64,
        discount_minor: i64,
        promo_minor: i64,
        settled_minor: i64,
        journal_paid: bool,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        let key = format!("plan-journal/{journal_id}/void");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.entry_in(tx, entry_id).await? {
            return Ok(Some(stored_receipt(&stored)));
        }
        let mut draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on).with_description(
            description_of(&format!("plan fee journal {journal_id}: void reversal"))?,
        );
        let mut legs = 0i64;
        if journal_paid {
            // The receivable is zero; the collected money unwinds to a payable.
            if charges_minor > 0 {
                let amt = Credits::from_minor(charges_minor);
                draft = draft.debit(self.plan_fees, amt, currency()).credit(
                    self.refund_payable,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            // Credit legs the void backs out re-charge the payable — a
            // discounted bill voided owes back only what was paid, so the
            // payable nets to the journal's current total (prior cash refunds
            // already left through the refund corrections).
            if discount_minor > 0 {
                let amt = Credits::from_minor(discount_minor);
                draft = draft.debit(self.refund_payable, amt, currency()).credit(
                    self.discounts,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            if promo_minor > 0 {
                let amt = Credits::from_minor(promo_minor);
                draft = draft.debit(self.refund_payable, amt, currency()).credit(
                    self.promo,
                    amt,
                    currency(),
                );
                legs += 1;
            }
        } else {
            if charges_minor > 0 {
                let amt = Credits::from_minor(charges_minor);
                draft = draft.debit(self.plan_fees, amt, currency()).credit(
                    self.receivables,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            if discount_minor > 0 {
                let amt = Credits::from_minor(discount_minor);
                draft = draft.debit(self.receivables, amt, currency()).credit(
                    self.discounts,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            if promo_minor > 0 {
                let amt = Credits::from_minor(promo_minor);
                draft = draft.debit(self.receivables, amt, currency()).credit(
                    self.promo,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            // Any part the receivable already collected (partial settlement)
            // is owed back rather than reversed against the open receivable.
            if settled_minor > 0 {
                let amt = Credits::from_minor(settled_minor);
                draft = draft.debit(self.receivables, amt, currency()).credit(
                    self.refund_payable,
                    amt,
                    currency(),
                );
                legs += 1;
            }
        }
        if legs == 0 {
            return Ok(None);
        }
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => match self.entry_in(tx, entry_id).await? {
                Some(stored) => Ok(Some(stored_receipt(&stored))),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other.map(Some),
        }
    }
    /// The promo pool's settled balance — the bound a `promo_credit` journal
    /// line is clamped to before the accrual posts.
    pub(crate) async fn promo_settled_in(
        &self,
        conn: &mut sqlx::PgConnection,
    ) -> Result<i64, WalletError> {
        self.settled_net_in(conn, self.promo).await
    }
}

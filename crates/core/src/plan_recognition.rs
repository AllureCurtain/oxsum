//! Versioned plan accounting: immutable inputs and once-only service-day effects.

mod schedule;
#[cfg(test)]
mod tests;

use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{Db, InvoiceJournal, Wallet, WalletError, error};

pub(crate) struct AccountingPeriod {
    pub id: Uuid,
    pub organization: Uuid,
    pub fee: i64,
    pub start: OffsetDateTime,
    pub end: OffsetDateTime,
    pub payment_on: Date,
}

#[derive(Default)]
pub(crate) struct Effect {
    pub billed: i64,
    pub earned: i64,
    pub deferred: i64,
    pub collected: i64,
    pub receivable: i64,
    pub payable: i64,
    pub cash: i64,
}

pub(crate) async fn period_in(
    tx: &mut Transaction<'_, Postgres>,
    journal: Uuid,
) -> Result<Option<AccountingPeriod>, WalletError> {
    let row = sqlx::query("SELECT * FROM oxsum.plan_accounting_periods WHERE journal_id=$1")
        .bind(journal)
        .fetch_optional(&mut **tx)
        .await?;
    row.map(|r| {
        Ok(AccountingPeriod {
            id: r.try_get("plan_period_id")?,
            organization: r.try_get("organization_id")?,
            fee: r.try_get("fee_minor")?,
            start: r.try_get("service_start")?,
            end: r.try_get("service_end")?,
            payment_on: r.try_get("payment_on")?,
        })
    })
    .transpose()
}

pub(crate) async fn totals_in(
    tx: &mut Transaction<'_, Postgres>,
    period: Uuid,
) -> Result<Effect, WalletError> {
    let row=sqlx::query("SELECT COALESCE(SUM(billed_minor),0)::bigint AS billed,COALESCE(SUM(earned_minor),0)::bigint AS earned,COALESCE(SUM(deferred_minor),0)::bigint AS deferred,COALESCE(SUM(collected_minor),0)::bigint AS collected,COALESCE(SUM(receivable_minor),0)::bigint AS receivable,COALESCE(SUM(payable_minor),0)::bigint AS payable,COALESCE(SUM(cash_minor),0)::bigint AS cash FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1")
        .bind(period).fetch_one(&mut **tx).await?;
    Ok(Effect {
        billed: row.try_get("billed")?,
        earned: row.try_get("earned")?,
        deferred: row.try_get("deferred")?,
        collected: row.try_get("collected")?,
        receivable: row.try_get("receivable")?,
        payable: row.try_get("payable")?,
        cash: row.try_get("cash")?,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn effect_in(
    tx: &mut Transaction<'_, Postgres>,
    period: &AccountingPeriod,
    kind: &str,
    source: Uuid,
    service_day: Date,
    booked_on: Date,
    entry: Option<Uuid>,
    effect: &Effect,
) -> Result<(), WalletError> {
    sqlx::query("INSERT INTO oxsum.plan_accounting_effects(effect_id,plan_period_id,kind,source_ref,service_day,booked_on,payment_on,billed_minor,earned_minor,deferred_minor,collected_minor,receivable_minor,payable_minor,cash_minor,entry_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)")
        .bind(Uuid::new_v4()).bind(period.id).bind(kind).bind(source).bind(service_day).bind(booked_on).bind(if matches!(kind, "refund" | "dispute" | "statement_payment" | "statement_return") { booked_on } else { period.payment_on })
        .bind(effect.billed).bind(effect.earned).bind(effect.deferred).bind(effect.collected).bind(effect.receivable).bind(effect.payable).bind(effect.cash).bind(entry)
        .execute(&mut **tx).await?;
    Ok(())
}

impl Db {
    /// Snapshot one paid activation's fee and exact service interval beside its
    /// invoice. A complimentary zero-price period has no ledger entry.
    pub(crate) async fn initialize_plan_accounting_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        journal: &InvoiceJournal,
        version: Uuid,
        on: Date,
    ) -> Result<(), WalletError> {
        let period = journal
            .plan_period_id
            .ok_or_else(|| error::invalid("daily invoice lacks a period"))?;
        sqlx::query("INSERT INTO oxsum.plan_accounting_periods(plan_period_id,journal_id,customer_plan_id,organization_id,business_id,plan_version_id,accounting_policy_version,fee_minor,service_start,service_end,authorization_kind,authorization_ref,payment_on) SELECT p.plan_period_id,$2,p.customer_plan_id,$3,$4,$5,2,$6,p.period_start,p.period_end,p.authorization_kind,p.authorization_ref,COALESCE((SELECT (o.paid_at AT TIME ZONE 'UTC')::date FROM oxsum.payment_orders o WHERE o.order_id=p.authorization_ref AND p.authorization_kind='payment_order'),$7) FROM oxsum.plan_periods p WHERE p.plan_period_id=$1")
            .bind(period).bind(journal.journal_id).bind(journal.organization_id).bind(journal.business_id).bind(version).bind(journal.total_minor).bind(on)
            .execute(&mut **tx).await?;
        let period = period_in(tx, journal.journal_id)
            .await?
            .ok_or_else(|| error::invalid("daily period did not land"))?;
        let order: Uuid = sqlx::query_scalar(
            "SELECT authorization_ref FROM oxsum.plan_periods WHERE plan_period_id=$1",
        )
        .bind(period.id)
        .fetch_one(&mut **tx)
        .await?;
        let entry = (journal.total_minor > 0)
            .then(|| *crate::entry_id_for(&format!("payment-order/{order}/prepay")).as_uuid());
        effect_in(
            tx,
            &period,
            "activation",
            order,
            period.start.date(),
            on,
            entry,
            &Effect {
                billed: journal.total_minor,
                deferred: journal.total_minor,
                collected: journal.settled_minor,
                cash: journal.settled_minor,
                ..Effect::default()
            },
        )
        .await
    }

    /// Catch up completed service days under the caller's plan/journal ownership.
    /// The write date is today; the exact completed service day stays separate.
    pub(crate) async fn recognize_plan_period_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &Wallet,
        journal: Uuid,
        cutoff: OffsetDateTime,
        booked_on: Date,
    ) -> Result<usize, WalletError> {
        let Some(period) = period_in(tx, journal).await? else {
            return Ok(0);
        };
        let journal = sqlx::query("SELECT state,total_minor,settled_minor,accounting_policy_version FROM oxsum.invoice_journals WHERE journal_id=$1 FOR UPDATE")
            .bind(journal).fetch_one(&mut **tx).await?;
        if journal.try_get::<String, _>("state")? == "void" {
            return Ok(0);
        }
        wallet.lock_append_in(tx).await?;
        let mut totals = totals_in(tx, period.id).await?;
        if totals.billed < 0
            || totals.billed > period.fee
            || totals.earned < 0
            || totals.deferred < 0
            || totals.billed != journal.try_get::<i64, _>("total_minor")?
            || totals.collected != journal.try_get::<i64, _>("settled_minor")?
            || journal.try_get::<i16, _>("accounting_policy_version")? != 2
        {
            return Err(WalletError::Conflict(
                "daily recognition source differs from its original invoice".into(),
            ));
        }
        let last:Option<Date>=sqlx::query_scalar("SELECT MAX(service_day) FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 AND kind='recognition'").bind(period.id).fetch_one(&mut **tx).await?;
        let mut day = match last {
            Some(last) => last
                .next_day()
                .ok_or_else(|| error::invalid("recognition day overflows"))?,
            None => period.start.date(),
        };
        let mut count = 0;
        while day <= period.end.date() {
            let end = schedule::day_end(day, period.end)?;
            if end <= period.start || end > cutoff || end <= day.midnight().assume_utc() {
                break;
            }
            let target = schedule::earned_at(totals.billed, period.start, period.end, end)?;
            let amount = target
                .checked_sub(totals.earned)
                .filter(|v| *v >= 0)
                .ok_or_else(|| error::invalid("daily recognition target moved backwards"))?;
            if amount > totals.deferred {
                return Err(WalletError::Conflict(
                    "recognition exceeds original deferred fee".into(),
                ));
            }
            let key = format!("plan-recognition/{}/{day}", period.id);
            let entry = wallet
                .plan_daily_recognition_in(tx, &key, amount, booked_on)
                .await?;
            effect_in(
                tx,
                &period,
                "recognition",
                period.id,
                day,
                booked_on,
                entry,
                &Effect {
                    earned: amount,
                    deferred: -amount,
                    ..Effect::default()
                },
            )
            .await?;
            totals.earned = target;
            totals.deferred -= amount;
            count += 1;
            if end == period.end {
                break;
            }
            day = day
                .next_day()
                .ok_or_else(|| error::invalid("recognition day overflows"))?;
        }
        Ok(count)
    }

    /// Recognize completed UTC service days for a bounded set of v2 periods.
    /// Current database time is authoritative; callers cannot accelerate revenue.
    ///
    /// # Errors
    /// Invalid bounds or storage/source inconsistencies return [`WalletError`].
    pub async fn run_plan_recognition(&self, limit: i64) -> Result<usize, WalletError> {
        if !(1..=1000).contains(&limit) {
            return Err(error::invalid("recognition limit must be 1..1000"));
        }
        let mut cursor = Uuid::nil();
        let mut count = 0;
        for _ in 0..limit {
            let mut tx = self.pool().begin().await?;
            // Lock ownership before LIMIT: a busy prefix cannot starve later
            // customers. One candidate per transaction bounds each pass to
            // limit attempts and at most 32 completed day effects per period.
            let row=sqlx::query("SELECT a.plan_period_id,a.journal_id FROM oxsum.plan_accounting_periods a JOIN oxsum.invoice_journals j USING(journal_id) JOIN oxsum.customer_plans cp ON cp.customer_plan_id=a.customer_plan_id CROSS JOIN LATERAL (SELECT CASE WHEN a.service_end<=now() THEN a.service_end ELSE date_trunc('day',now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' END AS cutoff) c WHERE a.plan_period_id>$1 AND j.state<>'void' AND c.cutoff>a.service_start AND NOT EXISTS(SELECT 1 FROM oxsum.plan_accounting_effects e WHERE e.plan_period_id=a.plan_period_id AND e.kind='recognition' AND e.service_day=((c.cutoff-interval '1 microsecond') AT TIME ZONE 'UTC')::date) ORDER BY a.plan_period_id LIMIT 1 FOR UPDATE OF cp SKIP LOCKED")
                .bind(cursor).fetch_optional(&mut *tx).await?;
            let Some(row) = row else {
                tx.rollback().await?;
                break;
            };
            cursor = row.try_get("plan_period_id")?;
            let journal: Uuid = row.try_get("journal_id")?;
            let period = period_in(&mut tx, journal)
                .await?
                .ok_or_else(|| error::invalid("recognition period disappeared"))?;
            let tenant: String = sqlx::query_scalar(
                "SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1",
            )
            .bind(period.organization)
            .fetch_one(&mut *tx)
            .await?;
            let wallet = Wallet::open_existing(self.pool().clone(), &tenant).await?;
            let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *tx)
                .await?;
            count += self
                .recognize_plan_period_in(&mut tx, &wallet, journal, now, now.date())
                .await?;
            tx.commit().await?;
        }
        Ok(count)
    }
}

impl Db {
    pub(crate) async fn void_daily_plan_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &Wallet,
        journal: &InvoiceJournal,
        on: Date,
    ) -> Result<Option<Uuid>, WalletError> {
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut **tx)
            .await?;
        self.recognize_plan_period_in(tx, wallet, journal.journal_id, now, on)
            .await?;
        let period = period_in(tx, journal.journal_id)
            .await?
            .ok_or_else(|| error::invalid("daily void lacks accounting inputs"))?;
        let totals = totals_in(tx, period.id).await?;
        if totals.billed != journal.total_minor
            || totals.collected != journal.settled_minor
            || totals.receivable != journal.total_minor - journal.settled_minor
        {
            return Err(WalletError::Conflict(
                "daily void accounting sources differ from the journal".into(),
            ));
        }
        let entry = wallet
            .plan_daily_void_in(
                tx,
                journal.journal_id,
                totals.earned,
                totals.deferred,
                totals.receivable,
                totals.collected,
                on,
            )
            .await?;
        effect_in(
            tx,
            &period,
            "void",
            journal.journal_id,
            schedule::completed_boundary(period.start, period.end, now).date(),
            on,
            entry,
            &Effect {
                billed: -totals.billed,
                earned: -totals.earned,
                deferred: -totals.deferred,
                collected: -totals.collected,
                receivable: -totals.receivable,
                payable: totals.collected,
                ..Effect::default()
            },
        )
        .await?;
        Ok(entry)
    }

    pub(crate) async fn daily_refund_allocation_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &Wallet,
        journal: Uuid,
        amount: i64,
        on: Date,
    ) -> Result<Option<(i64, i64)>, WalletError> {
        let Some(period) = period_in(tx, journal).await? else {
            return Ok(None);
        };
        let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut **tx)
            .await?;
        self.recognize_plan_period_in(tx, wallet, journal, now, on)
            .await?;
        let totals = totals_in(tx, period.id).await?;
        let net = totals
            .billed
            .checked_sub(amount)
            .filter(|v| *v >= 0)
            .ok_or_else(|| error::invalid("daily refund exceeds original net fee"))?;
        let target = schedule::earned_at(
            net,
            period.start,
            period.end,
            schedule::completed_boundary(period.start, period.end, now),
        )?;
        let earned = totals
            .earned
            .checked_sub(target)
            .filter(|v| *v >= 0)
            .ok_or_else(|| error::invalid("daily refund recognition moved forward"))?;
        let deferred = amount
            .checked_sub(earned)
            .filter(|v| *v >= 0 && *v <= totals.deferred)
            .ok_or_else(|| error::invalid("daily refund deferred allocation differs"))?;
        Ok(Some((deferred, earned)))
    }

    pub(crate) async fn record_daily_refund_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        allocation: &crate::PlanRefundRecovery,
        on: Date,
    ) -> Result<(), WalletError> {
        let Some(journal) = allocation.journal_id else {
            return Ok(());
        };
        let Some(period) = period_in(tx, journal).await? else {
            return Ok(());
        };
        let cancelled = allocation
            .deferred_minor
            .checked_add(allocation.earned_minor)
            .ok_or_else(|| error::invalid("daily refund amount overflows"))?;
        effect_in(
            tx,
            &period,
            "refund",
            allocation.refund_id,
            on,
            on,
            allocation.entry_id,
            &Effect {
                billed: -cancelled,
                earned: -allocation.earned_minor,
                deferred: -allocation.deferred_minor,
                collected: -cancelled,
                payable: -allocation.payable_minor,
                cash: -allocation.cash_minor,
                ..Effect::default()
            },
        )
        .await
    }
}

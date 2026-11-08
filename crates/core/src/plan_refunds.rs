//! Source-bound plan refunds and explicit, append-only legacy recovery.

use serde::Serialize;
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use crate::{BusinessCapability, Db, Wallet, WalletError, business_staff as authority, error};

/// The exact cash allocation proposed or retained by a plan-refund repair.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRefundRecovery {
    /// Refund whose external outcome this record explains.
    pub refund_id: Uuid,
    /// Original paid order; never a renewal or another customer's funding.
    pub order_id: Uuid,
    /// Original authorized service period, absent before activation.
    pub plan_period_id: Option<Uuid>,
    /// Original closed journal, including a voided journal when applicable.
    pub journal_id: Option<Uuid>,
    /// Full external cash return, in ledger minor units.
    pub cash_minor: i64,
    /// Returned unearned prepayment.
    pub deferred_minor: i64,
    /// Contra recognized fee revenue.
    pub earned_minor: i64,
    /// Previously voided money payable to the customer.
    pub payable_minor: i64,
    /// Cash whose original funding is exhausted; retained suspense for review.
    pub unallocated_minor: i64,
    /// Entry proving the allocation, absent in a dry-run proposal.
    pub entry_id: Option<Uuid>,
    /// Retained legacy entry; correction never replaces or deletes it.
    pub original_entry_id: Option<Uuid>,
    /// Whether the allocation was already committed when read.
    pub already_applied: bool,
}

pub(crate) struct LegacyPlanRefund {
    pub entry_id: Uuid,
    pub cash: i64,
    pub receivable: i64,
    pub discount: i64,
}

struct RefundSource {
    order_id: Uuid,
    organization_id: Uuid,
    business_id: Uuid,
    note_id: Uuid,
    note_state: String,
    amount: i64,
    fee: i64,
    reason: String,
    original_entry: Option<Uuid>,
}

fn effect(row: &PgRow) -> Result<PlanRefundRecovery, WalletError> {
    Ok(PlanRefundRecovery {
        refund_id: row.try_get("refund_id")?,
        order_id: row.try_get("order_id")?,
        plan_period_id: row.try_get("plan_period_id")?,
        journal_id: row.try_get("journal_id")?,
        cash_minor: row.try_get("cash_minor")?,
        deferred_minor: row.try_get("deferred_minor")?,
        earned_minor: row.try_get("earned_minor")?,
        payable_minor: row.try_get("payable_minor")?,
        unallocated_minor: row.try_get("unallocated_minor")?,
        entry_id: Some(row.try_get("entry_id")?),
        original_entry_id: row.try_get("original_entry_id")?,
        already_applied: true,
    })
}

impl Db {
    /// Preview or explicitly apply one confirmed legacy plan refund. Requires
    /// personal FundsWrite authority and the organization's matching wallet.
    /// A dry run rolls its transaction back and never calls a payment provider.
    pub async fn recover_plan_refund(
        &self,
        token: &str,
        wallet: &Wallet,
        refund_id: Uuid,
        apply: bool,
        on: Date,
    ) -> Result<PlanRefundRecovery, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor =
            authority::require_capability(&mut tx, business, token, BusinessCapability::FundsWrite)
                .await?;
        let owner: Option<Uuid> =
            sqlx::query_scalar("SELECT business_id FROM oxsum.refunds WHERE refund_id=$1")
                .bind(refund_id)
                .fetch_optional(&mut *tx)
                .await?;
        if owner != Some(business) {
            return Err(WalletError::NotFound("unknown refund".into()));
        }
        let result = self
            .apply_plan_refund_in(&mut tx, wallet, refund_id, None, true, apply, on)
            .await?;
        if apply && !result.already_applied {
            sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES($1,$2,$3,'plan_refund.recover',$4,$5,$6)")
                .bind(Uuid::new_v4()).bind(business).bind(actor.user_id).bind(refund_id.to_string())
                .bind(serde_json::to_value(&result).map_err(error::invalid)?).bind(format!("plan-refund-recovery:{refund_id}"))
                .execute(&mut *tx).await?;
        }
        if apply {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(result)
    }

    /// Records the full cash fact and its bounded original-source allocation.
    /// The caller owns the refund-state transaction; all local effects share it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn apply_plan_refund_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &Wallet,
        refund_id: Uuid,
        event_receipt: Option<Uuid>,
        recovery: bool,
        apply: bool,
        on: Date,
    ) -> Result<PlanRefundRecovery, WalletError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("order-refund:{refund_id}"))
            .execute(&mut **tx)
            .await?;
        let source = source_in(tx, refund_id, event_receipt.is_some()).await?;
        let tenant: String = sqlx::query_scalar(
            "SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1",
        )
        .bind(source.organization_id)
        .fetch_one(&mut **tx)
        .await?;
        if tenant != wallet.tenant_id() {
            return Err(WalletError::Conflict(
                "refund wallet does not own the original order".into(),
            ));
        }
        // Match activation/rollover's order: subscription advisory, subscription
        // row, plan row, journal, ledger and statement. Never lock the order after
        // a refund row: activation already owns it before subscription locks.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum.subscription' || $1::text,0))",
        )
        .bind(source.organization_id.to_string())
        .execute(&mut **tx)
        .await?;
        sqlx::query("SELECT subscription_id FROM oxsum.org_subscriptions WHERE organization_id=$1 AND state='active' FOR UPDATE")
            .bind(source.organization_id).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=(SELECT purpose_ref FROM oxsum.payment_orders WHERE order_id=$1) FOR UPDATE")
            .bind(source.order_id).fetch_optional(&mut **tx).await?;
        if let Some(row) = sqlx::query("SELECT * FROM oxsum.plan_refund_effects WHERE refund_id=$1")
            .bind(refund_id)
            .fetch_optional(&mut **tx)
            .await?
        {
            return effect(&row);
        }
        let corrected_journals =
            crate::plan_usage_corrections::corrected_order_journals_in(tx, source.order_id).await?;
        let corrected_source = !corrected_journals.is_empty();
        let period: Option<Uuid> = sqlx::query_scalar("SELECT plan_period_id FROM oxsum.plan_periods WHERE authorization_kind='payment_order' AND authorization_ref=$1")
            .bind(source.order_id).fetch_optional(&mut **tx).await?;
        let journal = sqlx::query("SELECT journal_id,state,total_minor,settled_minor,accounting_policy_version FROM oxsum.invoice_journals WHERE plan_period_id=$1 ORDER BY created_at DESC LIMIT 1 FOR UPDATE")
            .bind(period).fetch_optional(&mut **tx).await?;
        let journal_id = journal
            .as_ref()
            .map(|r| r.try_get::<Uuid, _>("journal_id"))
            .transpose()?;
        let old = wallet
            .legacy_plan_refund_in(tx, source.original_entry)
            .await?;
        if let Some(old) = &old
            && (old.entry_id
                != *crate::entry_id_for(&format!("order-refund-plan/{refund_id}")).as_uuid()
                || old.discount > source.amount)
        {
            return Err(WalletError::Conflict(
                "legacy refund entry differs from its original source".into(),
            ));
        }
        let prepaid = wallet
            .plan_source_credit_in(
                tx,
                &format!("payment-order/{}/prepay", source.order_id),
                crate::wallet::UNEARNED_PLAN_FEES,
            )
            .await?;
        if !corrected_source
            && (period.is_some() || journal.is_some() || prepaid != 0)
            && prepaid != source.fee
        {
            return Err(WalletError::Conflict(
                "original plan prepayment requires finance review".into(),
            ));
        }
        if source.note_state == "applied" && old.is_none() {
            return Err(WalletError::Conflict(
                "applied legacy note has no provable plan-refund entry".into(),
            ));
        }
        // Other source-linked refunds, including pre-upgrade applied notes,
        // consume this order's money even before their recovery is requested.
        let prior: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(amount),0)::bigint FROM (SELECT cash_minor AS amount FROM oxsum.plan_refund_effects WHERE order_id=$1 UNION ALL SELECT n.amount_minor FROM oxsum.credit_notes n WHERE n.order_id=$1 AND n.refund_id<>$2 AND n.state='applied' AND NOT EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=n.refund_id)) r")
            .bind(source.order_id).bind(refund_id).fetch_one(&mut **tx).await?;
        let chargebacks = wallet
            .plan_order_chargebacks_in(tx, source.order_id)
            .await?;
        let remaining = source
            .fee
            .saturating_sub(prior)
            .saturating_sub(chargebacks)
            .max(0);
        let mut result = PlanRefundRecovery {
            refund_id,
            order_id: source.order_id,
            plan_period_id: if corrected_source { None } else { period },
            journal_id: if corrected_source { None } else { journal_id },
            cash_minor: source.amount,
            deferred_minor: 0,
            earned_minor: 0,
            payable_minor: 0,
            unallocated_minor: 0,
            entry_id: None,
            original_entry_id: old.as_ref().map(|o| o.entry_id),
            already_applied: false,
        };
        let mut old_discount = 0;
        let mut target_settled = None;
        if corrected_source {
            // A historical arrears order may also authorize another period.
            // Retain this external cash fact, but never borrow that period's
            // deferred balance or pay the corrected payable twice by guessing.
            result.unallocated_minor = source.amount;
        } else if let Some(journal) = &journal {
            let id: Uuid = journal.try_get("journal_id")?;
            let state: String = journal.try_get("state")?;
            let settled: i64 = journal.try_get("settled_minor")?;
            if state == "void" {
                if old.is_some() {
                    return Err(WalletError::Conflict(
                        "legacy refund followed by a void needs combined source review".into(),
                    ));
                }
                let prior_payable: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(payable_minor),0)::bigint FROM oxsum.plan_refund_effects WHERE journal_id=$1")
                    .bind(id).fetch_one(&mut **tx).await?;
                result.payable_minor = source
                    .amount
                    .min(remaining)
                    .min(settled.saturating_sub(prior_payable).max(0));
            } else {
                old_discount = sqlx::query_scalar::<_,i64>("SELECT -COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.invoice_journal_lines WHERE journal_id=$1 AND source_ref=$2 AND kind='discount'")
                    .bind(id).bind(source.note_id).fetch_one(&mut **tx).await?;
                let total: i64 = journal.try_get("total_minor")?;
                let before_total = total
                    .checked_add(old_discount)
                    .ok_or_else(|| error::invalid("refund journal total overflows"))?;
                let before_settled = if old.is_some() && old_discount > 0 {
                    // Recover payment allocation from the original ledger, not
                    // the legacy min(settled,total) projection that lost it.
                    let applied = wallet.plan_journal_prepaid_in(tx, id).await?;
                    let other: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(earned_minor),0)::bigint FROM oxsum.plan_refund_effects WHERE journal_id=$1")
                        .bind(id).fetch_one(&mut **tx).await?;
                    let ambiguous: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.credit_notes n JOIN oxsum.invoice_journal_lines l ON l.source_ref=n.credit_note_id AND l.journal_id=$1 WHERE n.refund_id<>$2 AND n.state='applied' AND NOT EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=n.refund_id))")
                        .bind(id).bind(refund_id).fetch_one(&mut **tx).await?;
                    if ambiguous {
                        return Err(WalletError::Conflict(
                            "multiple legacy refund allocations require explicit finance review"
                                .into(),
                        ));
                    }
                    applied
                        .saturating_sub(other)
                        .saturating_sub(chargebacks)
                        .max(0)
                } else {
                    settled
                };
                let allocated = source
                    .amount
                    .min(remaining)
                    .min(before_settled)
                    .min(before_total.max(0));
                if journal.try_get::<i16, _>("accounting_policy_version")? == 2 {
                    if old.is_some() {
                        return Err(WalletError::Conflict(
                            "daily policy cannot use a legacy refund correction".into(),
                        ));
                    }
                    let (deferred, earned) = self
                        .daily_refund_allocation_in(tx, wallet, id, allocated, on)
                        .await?
                        .ok_or_else(|| error::invalid("daily refund accounting inputs missing"))?;
                    result.deferred_minor = deferred;
                    result.earned_minor = earned;
                } else {
                    result.earned_minor = allocated;
                }
                target_settled = Some(before_settled.saturating_sub(allocated));
            }
        } else {
            result.deferred_minor = source.amount.min(remaining);
        }
        result.unallocated_minor =
            source.amount - result.deferred_minor - result.earned_minor - result.payable_minor;
        if !apply {
            return Ok(result);
        }
        // Paid-but-not-activated orders have no prepayment entry yet. Recording
        // the original cash fact is idempotent and never activates entitlement.
        if prepaid == 0 && !corrected_source {
            wallet
                .plan_prepay_in(tx, &source.order_id, source.fee, on)
                .await?;
        }
        let receipt = wallet
            .plan_refund_cash_in(tx, &result, old.as_ref(), on)
            .await?;
        result.entry_id = Some(*receipt.entry_id.as_uuid());
        if let (Some(id), Some(settled)) = (journal_id, target_settled) {
            self.correct_refunded_journal_in(
                tx,
                id,
                source.note_id,
                result.earned_minor + result.deferred_minor,
                old_discount,
                settled,
                &source.reason,
            )
            .await?;
        }
        self.record_daily_refund_in(tx, &result, on).await?;
        if source.note_state == "issued" {
            sqlx::query("UPDATE oxsum.credit_notes SET state='applied',entry_id=$2,updated_at=now() WHERE credit_note_id=$1 AND state='issued'")
                .bind(source.note_id).bind(result.entry_id).execute(&mut **tx).await?;
        }
        sqlx::query("INSERT INTO oxsum.plan_refund_effects(refund_id,order_id,organization_id,plan_period_id,journal_id,credit_note_id,event_receipt_id,cash_minor,deferred_minor,earned_minor,payable_minor,unallocated_minor,entry_id,original_entry_id,legacy_recovery) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)")
            .bind(refund_id).bind(source.order_id).bind(source.organization_id).bind(result.plan_period_id).bind(result.journal_id).bind(source.note_id).bind(event_receipt)
            .bind(result.cash_minor).bind(result.deferred_minor).bind(result.earned_minor).bind(result.payable_minor).bind(result.unallocated_minor)
            .bind(result.entry_id).bind(result.original_entry_id).bind(recovery).execute(&mut **tx).await?;
        if result.unallocated_minor > 0 || source.note_state == "reversed" {
            sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) SELECT $1,$2,COALESCE(resolved_by,$3),'refund.approve',$4,$5,$6 FROM oxsum.refunds WHERE refund_id=$7 ON CONFLICT DO NOTHING")
                .bind(Uuid::new_v4()).bind(source.business_id).bind(Uuid::nil()).bind(refund_id.to_string())
                .bind(serde_json::json!({"compensation":"bookedWithReview","allocation":result,"reversedNoteRetained":source.note_state=="reversed","correctedJournals":corrected_journals,"reviewReason":if corrected_source {Some("historical usage correction requires explicit payable allocation")} else {None}}))
                .bind(format!("plan-refund-review:{refund_id}")).bind(refund_id).execute(&mut **tx).await?;
        }
        Ok(result)
    }
}

async fn source_in(
    tx: &mut Transaction<'_, Postgres>,
    refund: Uuid,
    confirmed_event: bool,
) -> Result<RefundSource, WalletError> {
    let row = sqlx::query("SELECT r.*,o.amount_minor AS fee,o.purpose,o.status,o.paid_at,EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.refund_id=r.refund_id) AS has_effect,n.credit_note_id,n.state AS note_state,n.entry_id AS note_entry,n.amount_minor AS note_amount FROM oxsum.refunds r JOIN oxsum.payment_orders o ON o.order_id=r.order_id JOIN oxsum.credit_notes n ON n.refund_id=r.refund_id AND n.order_id=o.order_id AND n.organization_id=r.organization_id WHERE r.refund_id=$1 FOR UPDATE OF r,n")
        .bind(refund).fetch_optional(&mut **tx).await?.ok_or_else(|| WalletError::NotFound("refund has no matching credit note".into()))?;
    let purpose: String = row.try_get("purpose")?;
    let status: String = row.try_get("status")?;
    let state: String = row.try_get("state")?;
    if purpose != "plan_period"
        || !matches!(status.as_str(), "paid" | "fulfilled")
        || row
            .try_get::<Option<time::OffsetDateTime>, _>("paid_at")?
            .is_none()
        || (!confirmed_event && state != "succeeded" && !row.try_get::<bool, _>("has_effect")?)
    {
        return Err(WalletError::Conflict(
            "plan refund needs its confirmed paid-order cash outcome".into(),
        ));
    }
    let amount: i64 = row.try_get("amount_minor")?;
    if amount != row.try_get::<i64, _>("note_amount")? {
        return Err(error::invalid("refund and note amounts differ"));
    }
    let note_state: String = row.try_get("note_state")?;
    Ok(RefundSource {
        order_id: row.try_get("order_id")?,
        organization_id: row.try_get("organization_id")?,
        business_id: row.try_get("business_id")?,
        note_id: row.try_get("credit_note_id")?,
        original_entry: if note_state == "applied" {
            row.try_get("note_entry")?
        } else {
            None
        },
        note_state,
        amount,
        fee: row.try_get("fee")?,
        reason: row.try_get("reason")?,
    })
}

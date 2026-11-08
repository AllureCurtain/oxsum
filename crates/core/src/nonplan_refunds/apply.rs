//! Transaction-owned Cash, source allocation, note and automatic evidence.

use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::evidence::{self, ReturnEntry, Source};
use super::types::{NonplanRefundEffect, NonplanRefundRecovery};
use crate::{Db, Wallet, WalletError, error, nonplan_returns};

/// Revalidate a retained immutable effect against actual unreversed entry bytes.
pub(crate) async fn effect_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    refund: Uuid,
) -> Result<Option<NonplanRefundEffect>, WalletError> {
    let row = sqlx::query("SELECT e.*,o.tenant_id FROM oxsum.nonplan_refund_effects e JOIN oxsum.organizations o USING(organization_id) WHERE refund_id=$1")
        .bind(refund).fetch_optional(&mut **tx).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let effect: NonplanRefundEffect =
        serde_json::from_value(row.try_get("effect")?).map_err(error::invalid)?;
    if row.try_get::<String, _>("tenant_id")? != wallet.tenant_id()
        || effect.refund_id != refund
        || effect.policy != "nonplan-refund-v1"
        || effect.order_id != row.try_get::<Uuid, _>("order_id")?
        || effect.organization_id != row.try_get::<Uuid, _>("organization_id")?
        || effect.cash_minor != row.try_get::<i64, _>("cash_minor")?
        || effect.wallet_minor != row.try_get::<i64, _>("wallet_minor")?
        || effect.suspense_minor != row.try_get::<i64, _>("suspense_minor")?
        || effect.wallet_restored_minor != row.try_get::<i64, _>("wallet_restored_minor")?
        || effect.event_receipt_id != row.try_get::<Uuid, _>("event_receipt_id")?
        || effect.cash_entry_id != row.try_get::<Uuid, _>("cash_entry_id")?
        || effect.cash_entry_hash != row.try_get::<String, _>("cash_entry_hash")?
    {
        return Err(evidence::review("retained allocation metadata differs"));
    }
    let old = evidence::return_entry_in(tx, wallet, refund, effect.cash_minor)
        .await?
        .ok_or_else(|| evidence::review("retained effect has no Cash entry"))?;
    if old.id != effect.cash_entry_id
        || old.hash != effect.cash_entry_hash
        || old.date.to_string() != effect.cash_booking_date
        || old.wallet.checked_sub(effect.wallet_restored_minor) != Some(effect.wallet_minor)
        || old.suspense.checked_add(effect.wallet_restored_minor) != Some(effect.suspense_minor)
    {
        return Err(evidence::review(
            "retained allocation differs from Cash entry",
        ));
    }
    if effect.wallet_restored_minor > 0 {
        verify_restitution_in(tx, wallet, &effect).await?;
    } else if effect.correction_entry_id.is_some() || effect.correction_entry_hash.is_some() {
        return Err(evidence::review("unexpected restitution entry"));
    }
    Ok(Some(effect))
}

async fn verify_restitution_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    effect: &NonplanRefundEffect,
) -> Result<(), WalletError> {
    let key = format!("nonplan-refund-restitution/{}", effect.refund_id);
    let id = *crate::entry_id_for(&key).as_uuid();
    if effect.correction_entry_id != Some(id)
        || wallet.verified_source_hash(id).await? != effect.correction_entry_hash
    {
        return Err(evidence::review("restitution identity or hash differs"));
    }
    let tenant = wallet.tenant_id();
    let valid: bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ledger_{tenant}.entries e WHERE entry_id=$1 AND idempotency_key=$2 AND reverses IS NULL AND log_index IS NOT NULL AND NOT EXISTS(SELECT 1 FROM ledger_{tenant}.entries WHERE reverses=$1))"))
        .bind(id).bind(key.as_bytes()).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(evidence::review("restitution is missing or reversed"));
    }
    let rows=sqlx::query(&format!("SELECT a.path,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1"))
        .bind(id).fetch_all(&mut **tx).await?;
    if rows.len() != 2 {
        return Err(evidence::review("restitution posting shape differs"));
    }
    let mut debit = false;
    let mut credit = false;
    for row in rows {
        if row.try_get::<i64, _>("amount_minor")? != effect.wallet_restored_minor
            || row.try_get::<String, _>("currency")? != "USD"
            || row.try_get::<String, _>("layer")? != "settled"
        {
            return Err(evidence::review("restitution posting terms differ"));
        }
        match (
            row.try_get::<String, _>("path")?.as_str(),
            row.try_get::<String, _>("direction")?.as_str(),
        ) {
            ("Equity:Adjustments", "D") => debit = true,
            ("Liabilities:Wallet", "C") => credit = true,
            _ => {
                return Err(evidence::review(
                    "restitution touches an unsupported account",
                ));
            }
        }
    }
    if !debit || !credit {
        return Err(evidence::review("restitution is not balanced"));
    }
    Ok(())
}

pub(super) struct Inspection {
    pub report: NonplanRefundRecovery,
    pub source: Source,
    pub old: Option<ReturnEntry>,
}

pub(super) async fn inspect_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    refund: Uuid,
    receipt: Uuid,
    recovery: bool,
) -> Result<Inspection, WalletError> {
    let source = evidence::source_in(tx, wallet, refund).await?;
    let event = evidence::event_in(tx, &source, receipt, !recovery).await?;
    let old = evidence::return_entry_in(tx, wallet, refund, source.amount).await?;
    if !recovery
        && (old.is_some()
            || source.state == "succeeded"
            || source.note_state.as_deref() == Some("applied"))
    {
        return Err(evidence::review(
            "historical effect needs explicit preview/apply",
        ));
    }
    if source.note_entry.is_some() && old.as_ref().map(|entry| entry.id) != source.note_entry {
        return Err(evidence::review("old credit note points to another entry"));
    }
    if source.note_state.as_deref() == Some("applied") && old.is_none() {
        return Err(evidence::review(
            "applied credit note has no exact Cash entry",
        ));
    }
    if old
        .as_ref()
        .is_some_and(|entry| entry.wallet != source.amount || entry.suspense != 0)
    {
        return Err(evidence::review(
            "legacy return is not the original Wallet-only shape",
        ));
    }
    let mut allocation = nonplan_returns::inspect_refund_in(
        tx,
        wallet,
        source.order_id,
        source.organization_id,
        refund,
        source.amount,
        old.as_ref().map(|entry| entry.position),
    )
    .await?;
    // Unproven source cannot justify a historical restitution either. Zero is
    // merely conservative; it is not evidence that the old take-back was wrong.
    if old.is_some()
        && allocation
            .review_reasons
            .iter()
            .any(|reason| *reason != "originalWalletFundingNotRetained")
    {
        return Err(evidence::review(
            "historical original-source bound is unproven",
        ));
    }
    if old.is_some() && !source.approved {
        return Err(evidence::review(
            "historical Wallet restitution needs the original approval evidence",
        ));
    }
    if !source.approved {
        allocation.wallet_minor = 0;
        allocation.suspense_minor = source.amount;
        allocation.review_reasons.push("unapprovedRefund");
    }
    if source.note_id.is_none() {
        allocation.review_reasons.push("missingCreditNote");
    }
    if source.note_state.as_deref() == Some("reversed") {
        allocation.review_reasons.push("reversedCreditNoteRetained");
    }
    if matches!(source.state.as_str(), "declined" | "failed" | "released") {
        allocation.review_reasons.push("terminalLateSuccess");
    }
    let restore = old
        .as_ref()
        .map_or(0, |entry| entry.wallet - allocation.wallet_minor);
    let source_json = json!({"policy":"nonplan-refund-v1","refund":source,"event":event,"original":allocation,"oldCash":old});
    let revision = evidence::revision(&source_json)?;
    let report = NonplanRefundRecovery {
        refund_id: refund,
        status: "proposed".into(),
        refusal: None,
        source_revision: revision,
        source: source_json,
        effect: None,
        existing_cash_minor: old.as_ref().map_or(0, |entry| entry.cash),
        cash_to_book_minor: if old.is_some() { 0 } else { source.amount },
        wallet_minor: allocation.wallet_minor,
        suspense_minor: allocation.suspense_minor,
        wallet_to_restore_minor: restore,
        already_applied: false,
    };
    Ok(Inspection {
        report,
        source,
        old,
    })
}

pub(super) async fn commit_in(
    db: &Db,
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    inspection: &mut Inspection,
    receipt: Uuid,
    recovery: bool,
    on: Date,
) -> Result<NonplanRefundEffect, WalletError> {
    let source = &inspection.source;
    let report = &inspection.report;
    let (cash_id, cash_hash, cash_date) = if let Some(old) = &inspection.old {
        (old.id, old.hash.clone(), old.date)
    } else {
        let recorded = wallet
            .nonplan_refund_in(tx, source.refund_id, source.amount, report.wallet_minor, on)
            .await?;
        (
            *recorded.entry_id.as_uuid(),
            recorded.content_hash.to_hex(),
            on,
        )
    };
    let correction = if report.wallet_to_restore_minor > 0 {
        Some(
            wallet
                .nonplan_refund_restitution_in(
                    tx,
                    source.refund_id,
                    report.wallet_to_restore_minor,
                    &report.source_revision,
                    on,
                )
                .await?,
        )
    } else {
        None
    };
    if source.purpose == "bundle" {
        let lot:Option<Uuid>=sqlx::query_scalar("SELECT lot_id FROM oxsum.promo_lots WHERE organization_id=$1 AND source='bundle_promo' AND source_ref=$2")
            .bind(source.organization_id).bind(source.order_id.to_string()).fetch_optional(&mut **tx).await?;
        if let Some(lot) = lot {
            db.cancel_promo_lot_in(tx, wallet, lot, on).await?;
        }
    }
    // The approval state remains a separate fact. An unapproved request does not
    // turn into staff consent, but its effect prevents any later payout attempt.
    if source.state == "provider_pending" {
        sqlx::query("UPDATE oxsum.refunds SET state='succeeded',next_retry_at=NULL,updated_at=now() WHERE refund_id=$1")
            .bind(source.refund_id).execute(&mut **tx).await?;
    }
    if source.note_state.as_deref() == Some("issued") {
        sqlx::query("UPDATE oxsum.credit_notes SET state='applied',entry_id=$2,updated_at=now() WHERE credit_note_id=$1")
            .bind(source.note_id).bind(cash_id).execute(&mut **tx).await?;
    }
    let original = &report.source["original"];
    let reasons: Vec<String> =
        serde_json::from_value(original["reviewReasons"].clone()).map_err(error::invalid)?;
    let effect = NonplanRefundEffect {
        policy: "nonplan-refund-v1".into(),
        refund_id: source.refund_id,
        order_id: source.order_id,
        organization_id: source.organization_id,
        cash_minor: source.amount,
        wallet_minor: report.wallet_minor,
        suspense_minor: report.suspense_minor,
        wallet_restored_minor: report.wallet_to_restore_minor,
        cash_entry_id: cash_id,
        cash_entry_hash: cash_hash.clone(),
        cash_booking_date: cash_date.to_string(),
        correction_entry_id: correction.as_ref().map(|r| *r.entry_id.as_uuid()),
        correction_entry_hash: correction.as_ref().map(|r| r.content_hash.to_hex()),
        original_funding_entry_id: serde_json::from_value(original["entryId"].clone())
            .map_err(error::invalid)?,
        original_funding_entry_hash: serde_json::from_value(original["contentHash"].clone())
            .map_err(error::invalid)?,
        event_receipt_id: receipt,
        legacy_recovery: recovery,
        requires_review: !reasons.is_empty(),
        review_reasons: reasons,
        booked_at: OffsetDateTime::now_utc(),
    };
    sqlx::query("INSERT INTO oxsum.nonplan_refund_effects(refund_id,order_id,organization_id,business_id,credit_note_id,event_receipt_id,cash_minor,wallet_minor,suspense_minor,wallet_restored_minor,cash_entry_id,cash_entry_hash,cash_booking_date,correction_entry_id,correction_entry_hash,legacy_recovery,source_revision,source,effect,booked_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)")
        .bind(source.refund_id).bind(source.order_id).bind(source.organization_id).bind(source.business_id).bind(source.note_id).bind(receipt)
        .bind(effect.cash_minor).bind(effect.wallet_minor).bind(effect.suspense_minor).bind(effect.wallet_restored_minor)
        .bind(cash_id).bind(cash_hash).bind(cash_date).bind(effect.correction_entry_id).bind(&effect.correction_entry_hash)
        .bind(recovery).bind(&report.source_revision).bind(&report.source).bind(serde_json::to_value(&effect).map_err(error::invalid)?).bind(effect.booked_at)
        .execute(&mut **tx).await?;
    if source.approved
        && let Some(actor) = source.resolved_by
    {
        sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,note_id,idempotency_key) VALUES($1,$2,$3,'refund.approve',$4,$5,$6,$7)")
            .bind(Uuid::new_v4()).bind(source.business_id).bind(actor).bind(source.refund_id.to_string())
            .bind(json!({"automaticCashFact":true,"effect":effect,"previousState":source.state,"sourceRevision":report.source_revision}))
            .bind(source.note_id).bind(format!("nonplan-refund-cash:{}",source.refund_id)).execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE oxsum.merchant_event_inbox SET state='applied',updated_at=now() WHERE receipt_id=$1 AND state='accepted'")
        .bind(receipt).execute(&mut **tx).await?;
    inspection.report.status = "applied".into();
    inspection.report.effect = Some(effect.clone());
    Ok(effect)
}

impl Db {
    pub(crate) async fn settle_nonplan_refund(
        &self,
        wallet: &Wallet,
        refund: Uuid,
        receipt: Uuid,
        on: Date,
    ) -> Result<(), WalletError> {
        let mut tx = self.pool().begin().await?;
        let order: Uuid =
            sqlx::query_scalar("SELECT order_id FROM oxsum.refunds WHERE refund_id=$1")
                .bind(refund)
                .fetch_one(&mut *tx)
                .await?;
        nonplan_returns::lock_source_in(&mut tx, order).await?;
        let source = evidence::source_in(&mut tx, wallet, refund).await?;
        wallet.lock_append_in(&mut tx).await?;
        if let Some(effect) = effect_in(&mut tx, wallet, refund).await? {
            if effect.event_receipt_id != receipt {
                evidence::event_in(&mut tx, &source, receipt, false).await?;
            }
            tx.rollback().await?;
            return Ok(());
        }
        let mut inspection = inspect_in(&mut tx, wallet, refund, receipt, false).await?;
        commit_in(self, &mut tx, wallet, &mut inspection, receipt, false, on).await?;
        tx.commit().await?;
        Ok(())
    }
}

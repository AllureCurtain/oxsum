//! Verified retained corrections and atomic append-only compensation.

use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::evidence::{self, Source};
use super::{NonplanChargebackCorrection, NonplanChargebackRecovery};
use crate::{Wallet, WalletError, error, nonplan_returns};

/// Verify immutable effect metadata, exact terminal/audit/receipt and both entries.
/// This does not rerun allocation against later activity, which cannot rewrite it.
pub(crate) async fn effect_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    dispute: Uuid,
) -> Result<Option<NonplanChargebackCorrection>, WalletError> {
    let row=sqlx::query("SELECT e.*,o.tenant_id FROM oxsum.nonplan_chargeback_corrections e JOIN oxsum.organizations o USING(organization_id) WHERE dispute_id=$1")
        .bind(dispute).fetch_optional(&mut **tx).await?;
    let Some(row) = row else { return Ok(None) };
    let effect: NonplanChargebackCorrection =
        serde_json::from_value(row.try_get("effect")?).map_err(error::invalid)?;
    let retained: serde_json::Value = row.try_get("source")?;
    if row.try_get::<String, _>("tenant_id")? != wallet.tenant_id()
        || effect.policy != "nonplan-chargeback-recovery-v1"
        || effect.dispute_id != dispute
        || effect.order_id != row.try_get::<Uuid, _>("order_id")?
        || effect.organization_id != row.try_get::<Uuid, _>("organization_id")?
        || effect.event_receipt_id != row.try_get::<Uuid, _>("event_receipt_id")?
        || effect.false_receipt_minor != row.try_get::<i64, _>("false_receipt_minor")?
        || effect.actual_loss_minor != row.try_get::<i64, _>("actual_loss_minor")?
        || effect.cash_correction_minor != row.try_get::<i64, _>("cash_correction_minor")?
        || effect.wallet_minor != row.try_get::<i64, _>("wallet_minor")?
        || effect.suspense_minor != row.try_get::<i64, _>("suspense_minor")?
        || effect.original_entry_id != row.try_get::<Uuid, _>("original_entry_id")?
        || effect.original_entry_hash != row.try_get::<String, _>("original_entry_hash")?
        || effect.correction_entry_id != row.try_get::<Uuid, _>("correction_entry_id")?
        || effect.correction_entry_hash != row.try_get::<String, _>("correction_entry_hash")?
        || effect.correction_booking_date
            != row
                .try_get::<Date, _>("correction_booking_date")?
                .to_string()
        || effect.source_revision != row.try_get::<String, _>("source_revision")?
        || effect.booked_at != row.try_get::<OffsetDateTime, _>("booked_at")?
        || effect.source_revision != evidence::revision(&retained)?
        || effect.preserved_credit_benefit_minor != effect.false_receipt_minor
        || !effect.requires_review
        || effect.review_reasons.is_empty()
        || effect
            .false_receipt_minor
            .checked_add(effect.actual_loss_minor)
            != Some(effect.cash_correction_minor)
        || effect.wallet_minor.checked_add(effect.suspense_minor)
            != Some(effect.cash_correction_minor)
        || effect.wallet_minor < 0
        || effect.wallet_minor > effect.actual_loss_minor
        || retained["reviewReasons"] != json!(effect.review_reasons)
        || retained["originalFunding"]["walletMinor"] != effect.wallet_minor
    {
        return Err(evidence::review("retained correction metadata differs"));
    }
    let source = evidence::source_in(tx, wallet, dispute).await?;
    let event = evidence::event_in(tx, &source, effect.event_receipt_id).await?;
    if retained["dispute"] != serde_json::to_value(&source).map_err(error::invalid)?
        || retained["event"] != event
        || source.business_id != row.try_get::<Uuid, _>("business_id")?
        || source.original_audit_id != row.try_get::<Uuid, _>("original_audit_id")?
        || source.order_id != effect.order_id
        || source.organization_id != effect.organization_id
        || source.original.cash != effect.false_receipt_minor
        || source.loss != effect.actual_loss_minor
        || source.original.id != effect.original_entry_id
        || source.original.hash != effect.original_entry_hash
    {
        return Err(evidence::review("retained original case evidence differs"));
    }
    let funding = *crate::entry_id_for(&format!("payment-order:{}", effect.order_id)).as_uuid();
    if retained["originalFunding"]["entryId"] != json!(funding)
        || wallet
            .verified_source_hash_in(tx, funding)
            .await?
            .as_deref()
            != retained["originalFunding"]["contentHash"].as_str()
    {
        return Err(evidence::review("retained original funding hash differs"));
    }
    let funding_valid:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ledger_{}.entries e WHERE entry_id=$1 AND idempotency_key=$2 AND reverses IS NULL AND log_index IS NOT NULL AND log_index<$3 AND NOT EXISTS(SELECT 1 FROM ledger_{}.entries r WHERE r.reverses=e.entry_id))",wallet.tenant_id(),wallet.tenant_id()))
        .bind(funding).bind(format!("payment-order:{}",effect.order_id).as_bytes()).bind(source.original.position).fetch_one(&mut **tx).await?;
    if !funding_valid {
        return Err(evidence::review(
            "retained original funding is reversed or unsequenced",
        ));
    }
    let correction = evidence::entry_in(tx, wallet, dispute, true)
        .await?
        .ok_or_else(|| evidence::review("retained correction entry is missing"))?;
    if correction.id != effect.correction_entry_id
        || correction.hash != effect.correction_entry_hash
        || correction.date.to_string() != effect.correction_booking_date
        || correction.position <= source.original.position
        || correction.cash != effect.cash_correction_minor
        || correction.wallet != effect.wallet_minor
        || correction.suspense != effect.suspense_minor
        || correction.credit != 0
    {
        return Err(evidence::review(
            "retained correction differs from its exact ledger postings",
        ));
    }
    Ok(Some(effect))
}

pub(super) struct Inspection {
    pub source: Source,
    pub report: NonplanChargebackRecovery,
    pub reasons: Vec<String>,
}

pub(super) async fn inspect_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    dispute: Uuid,
    receipt: Uuid,
) -> Result<Inspection, WalletError> {
    let source = evidence::source_in(tx, wallet, dispute).await?;
    let event = evidence::event_in(tx, &source, receipt).await?;
    if evidence::entry_in(tx, wallet, dispute, true)
        .await?
        .is_some()
    {
        return Err(evidence::review(
            "orphan correction entry requires explicit investigation",
        ));
    }
    let allocation = nonplan_returns::inspect_in(
        tx,
        wallet,
        source.order_id,
        source.organization_id,
        dispute,
        source.loss,
    )
    .await?;
    // An explicit historical operation requires proven original funding and all
    // prior sources. A conservative fresh-cash fallback is not proof of history.
    if allocation
        .review_reasons
        .iter()
        .any(|reason| *reason != "originalWalletFundingNotRetained")
    {
        return Err(evidence::review(
            "original funding, activity or prior returns are unproven",
        ));
    }
    let funding_position: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT log_index FROM ledger_{}.entries WHERE entry_id=$1",
        wallet.tenant_id()
    ))
    .bind(allocation.entry_id)
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    if funding_position.is_none_or(|position| position >= source.original.position) {
        return Err(evidence::review(
            "original wrong entry precedes its proven funding",
        ));
    }
    let mut reasons: Vec<String> = allocation
        .review_reasons
        .iter()
        .map(|reason| (*reason).to_owned())
        .collect();
    reasons.push("historicalCreditBenefitRetained".into());
    if source.purpose == "bundle" {
        reasons.push("bundlePromoRetained".into());
    }
    let cash = source
        .original
        .cash
        .checked_add(source.loss)
        .ok_or_else(|| evidence::review("correction amount overflows"))?;
    let suspense = cash
        .checked_sub(allocation.wallet_minor)
        .ok_or_else(|| evidence::review("allocation amount overflows"))?;
    let value = json!({"policy":"nonplan-chargeback-recovery-v1","dispute":source,"event":event,"originalFunding":allocation,"reviewReasons":reasons});
    let report = NonplanChargebackRecovery {
        dispute_id: dispute,
        status: "proposed".into(),
        refusal: None,
        source_revision: evidence::revision(&value)?,
        source: value,
        false_receipt_minor: source.original.cash,
        actual_loss_minor: source.loss,
        cash_to_book_minor: cash,
        wallet_minor: allocation.wallet_minor,
        suspense_minor: suspense,
        effect: None,
        already_applied: false,
    };
    Ok(Inspection {
        source,
        report,
        reasons,
    })
}

pub(super) async fn commit_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    inspected: &mut Inspection,
    receipt: Uuid,
    on: Date,
) -> Result<(), WalletError> {
    let source = &inspected.source;
    let report = &mut inspected.report;
    let result = wallet
        .nonplan_chargeback_correction_in(
            tx,
            source.dispute_id,
            report.cash_to_book_minor,
            report.wallet_minor,
            on,
        )
        .await?;
    let correction = evidence::entry_in(tx, wallet, source.dispute_id, true)
        .await?
        .ok_or_else(|| evidence::review("appended correction is missing"))?;
    if correction.id != *result.entry_id.as_uuid() {
        return Err(evidence::review("appended correction identity differs"));
    }
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let effect = NonplanChargebackCorrection {
        policy: "nonplan-chargeback-recovery-v1".into(),
        dispute_id: source.dispute_id,
        order_id: source.order_id,
        organization_id: source.organization_id,
        false_receipt_minor: report.false_receipt_minor,
        actual_loss_minor: report.actual_loss_minor,
        cash_correction_minor: report.cash_to_book_minor,
        wallet_minor: report.wallet_minor,
        suspense_minor: report.suspense_minor,
        preserved_credit_benefit_minor: report.false_receipt_minor,
        original_entry_id: source.original.id,
        original_entry_hash: source.original.hash.clone(),
        correction_entry_id: correction.id,
        correction_entry_hash: correction.hash,
        correction_booking_date: correction.date.to_string(),
        event_receipt_id: receipt,
        source_revision: report.source_revision.clone(),
        requires_review: true,
        review_reasons: inspected.reasons.clone(),
        booked_at: now,
    };
    sqlx::query("INSERT INTO oxsum.nonplan_chargeback_corrections(dispute_id,order_id,organization_id,business_id,event_receipt_id,original_audit_id,false_receipt_minor,actual_loss_minor,cash_correction_minor,wallet_minor,suspense_minor,original_entry_id,original_entry_hash,correction_entry_id,correction_entry_hash,correction_booking_date,source_revision,source,effect,booked_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)")
        .bind(source.dispute_id).bind(source.order_id).bind(source.organization_id).bind(source.business_id).bind(receipt).bind(source.original_audit_id)
        .bind(effect.false_receipt_minor).bind(effect.actual_loss_minor).bind(effect.cash_correction_minor).bind(effect.wallet_minor).bind(effect.suspense_minor)
        .bind(effect.original_entry_id).bind(&effect.original_entry_hash).bind(effect.correction_entry_id).bind(&effect.correction_entry_hash).bind(correction.date)
        .bind(&report.source_revision).bind(&report.source).bind(serde_json::to_value(&effect).map_err(error::invalid)?).bind(now).execute(&mut **tx).await?;
    report.status = "applied".into();
    report.effect = Some(effect);
    Ok(())
}

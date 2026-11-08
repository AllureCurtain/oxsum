//! Original-source evidence and serialization for non-plan cash returns.

use std::time::Duration;

use doubleentry::storage::postgres::PostgresError;
use doubleentry::{Direction, Layer};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use tokio::time::Instant;
use uuid::Uuid;

use crate::{VerifiedMerchantEvent, Wallet, WalletError};

// Bounded source inspection while holding append ownership. A larger history
// retains Cash in suspense instead of guessing an unverified take-back.
const ACTIVITY_LIMIT: usize = 20_000;

/// Claim a source without parking a pooled connection behind independent grants.
/// The caller rolls back on `PoolBusy`; retained event/request retries remain safe.
pub(crate) async fn lock_source_in(
    tx: &mut Transaction<'_, Postgres>,
    order: Uuid,
) -> Result<(), WalletError> {
    let claimed: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("oxsum.nonplan-return:{order}"))
            .fetch_one(&mut **tx)
            .await?;
    if claimed {
        Ok(())
    } else {
        Err(WalletError::PoolBusy)
    }
}

/// A fulfillment may wait off the pool, within the same five-second pool budget.
pub(crate) async fn fulfillment_guard(
    db: &crate::Db,
    order: Uuid,
) -> Result<Transaction<'_, Postgres>, WalletError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() >= deadline {
            return Err(WalletError::PoolBusy);
        }
        let mut tx = tokio::time::timeout_at(deadline, db.pool().begin())
            .await
            .map_err(|_| WalletError::PoolBusy)??;
        match tokio::time::timeout_at(deadline, lock_source_in(&mut tx, order))
            .await
            .map_err(|_| WalletError::PoolBusy)?
        {
            Ok(()) => return Ok(tx),
            Err(WalletError::PoolBusy) => {
                tx.rollback().await?;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// A lost charge cannot subsequently grant funds or authorize another payout.
pub(crate) async fn guard_lost_in(
    tx: &mut Transaction<'_, Postgres>,
    order: Uuid,
) -> Result<(), WalletError> {
    let lost: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.disputes d JOIN oxsum.payment_orders p USING(order_id) WHERE d.order_id=$1 AND d.state='lost' AND p.purpose<>'plan_period')")
        .bind(order).fetch_one(&mut **tx).await?;
    if lost {
        return Err(WalletError::Conflict(
            "non-plan order has lost chargeback cash; explicit source review is required".into(),
        ));
    }
    Ok(())
}

/// Actual refund Cash before fulfillment prevents an unperformed purchase grant.
pub(crate) async fn guard_grant_in(
    tx: &mut Transaction<'_, Postgres>,
    order: Uuid,
) -> Result<(), WalletError> {
    guard_lost_in(tx, order).await?;
    let returned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM oxsum.nonplan_refund_effects WHERE order_id=$1)",
    )
    .bind(order)
    .fetch_one(&mut **tx)
    .await?;
    if returned {
        return Err(WalletError::Conflict(
            "non-plan order has returned Cash before fulfillment; source review is required".into(),
        ));
    }
    Ok(())
}

/// Convert authenticated fiat into credit minor units without rounding or fallback.
pub(crate) fn disputed_minor(
    amount: i64,
    fiat: i64,
    currency: &str,
    event: &VerifiedMerchantEvent,
) -> Result<i64, &'static str> {
    if event
        .refund_id
        .as_deref()
        .is_none_or(|id| id.trim().is_empty())
    {
        return Err("missingDisputeId");
    }
    if event.currency.as_deref() != Some(currency) {
        return Err("disputeCurrencyMismatch");
    }
    let Some(disputed) = event.amount_minor else {
        return Err("missingDisputeAmount");
    };
    if amount <= 0 || fiat <= 0 || disputed <= 0 || disputed > fiat {
        return Err("disputeAmountMismatch");
    }
    let numerator = i128::from(amount) * i128::from(disputed);
    if numerator % i128::from(fiat) != 0 {
        return Err("disputeAmountNotIntegral");
    }
    i64::try_from(numerator / i128::from(fiat)).map_err(|_| "disputeAmountMismatch")
}

/// Exact immutable source and conservative take-back bound retained in finance audit.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceEvidence {
    policy: &'static str,
    order_id: Uuid,
    pub(crate) entry_id: Option<Uuid>,
    pub(crate) content_hash: Option<String>,
    fulfillment_id: Option<Uuid>,
    source_wallet_minor: i64,
    source_repayment_minor: i64,
    subsequent_wallet_debit_minor: i64,
    pending_wallet_minor: i64,
    prior_chargeback_cash_minor: i64,
    prior_refund_cash_minor: i64,
    before_log_index: Option<i64>,
    ledger_boundary: Option<i64>,
    wallet_activity_sha256: Option<String>,
    prior_returns: Vec<serde_json::Value>,
    /// Only the retained original Wallet amount may be reclaimed.
    pub(crate) wallet_minor: i64,
    /// The full known loss less the proven Wallet amount requires review.
    pub(crate) suspense_minor: i64,
    /// Explicit uncertainty is evidence, never an inferred old debt repair.
    pub(crate) review_reasons: Vec<&'static str>,
}

impl SourceEvidence {
    fn uncertain(mut self, reason: &'static str) -> Self {
        self.review_reasons.push(reason);
        self
    }
}

/// Inspect the original source while source and append locks exclude new writers.
/// Later Wallet credits never refill the original source's conservative bound.
pub(crate) async fn inspect_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    order: Uuid,
    organization: Uuid,
    dispute: Uuid,
    loss: i64,
) -> Result<SourceEvidence, WalletError> {
    inspect_source_in(
        tx,
        wallet,
        order,
        organization,
        Some(dispute),
        None,
        loss,
        None,
    )
    .await
}

/// Inspect a refund at the current boundary or immediately before its old entry.
pub(crate) async fn inspect_refund_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    order: Uuid,
    organization: Uuid,
    refund: Uuid,
    loss: i64,
    before: Option<i64>,
) -> Result<SourceEvidence, WalletError> {
    inspect_source_in(
        tx,
        wallet,
        order,
        organization,
        None,
        Some(refund),
        loss,
        before,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn inspect_source_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    order: Uuid,
    organization: Uuid,
    dispute: Option<Uuid>,
    refund: Option<Uuid>,
    loss: i64,
    before: Option<i64>,
) -> Result<SourceEvidence, WalletError> {
    let mut proof = SourceEvidence {
        policy: "nonplan-cash-v1",
        order_id: order,
        suspense_minor: loss,
        before_log_index: before,
        ..SourceEvidence::default()
    };
    let row = sqlx::query("SELECT p.organization_id,p.purpose,p.status,p.amount_minor,p.paid_at,f.fulfillment_id,f.purpose AS fulfilled_purpose,f.purpose_payload FROM oxsum.payment_orders p LEFT JOIN oxsum.payment_fulfillments f USING(order_id) WHERE p.order_id=$1")
        .bind(order).fetch_one(&mut **tx).await?;
    let purpose: String = row.try_get("purpose")?;
    let total: i64 = row.try_get("amount_minor")?;
    proof.fulfillment_id = row.try_get("fulfillment_id")?;
    if row.try_get::<Uuid, _>("organization_id")? != organization
        || row.try_get::<String, _>("status")? != "fulfilled"
        || row
            .try_get::<Option<time::OffsetDateTime>, _>("paid_at")?
            .is_none()
        || proof.fulfillment_id.is_none()
        || row
            .try_get::<Option<String>, _>("fulfilled_purpose")?
            .as_deref()
            != Some(purpose.as_str())
        || !matches!(
            purpose.as_str(),
            "wallet_topup" | "bundle" | "invoice_repayment"
        )
    {
        return Ok(proof.uncertain("originalFulfillmentUnproven"));
    }
    let key = format!("payment-order:{order}");
    let id = *crate::entry_id_for(&key).as_uuid();
    let payload: serde_json::Value = row.try_get("purpose_payload")?;
    let amount_field = if purpose == "invoice_repayment" {
        "repaidMinor"
    } else {
        "creditedMinor"
    };
    if payload["arm"] != purpose
        || payload["walletEntryId"] != serde_json::json!(id)
        || payload[amount_field] != total
    {
        return Ok(proof.uncertain("originalFulfillmentUnproven"));
    }
    let tenant = wallet.tenant_id();
    let rows = sqlx::query(&format!("SELECT entry_id,idempotency_key,encode(content_hash,'hex') AS hash,reverses,log_index FROM ledger_{tenant}.entries WHERE entry_id=$1 OR idempotency_key=$2"))
        .bind(id).bind(key.as_bytes()).fetch_all(&mut **tx).await?;
    if rows.len() != 1
        || rows[0].try_get::<Uuid, _>("entry_id")? != id
        || rows[0].try_get::<Vec<u8>, _>("idempotency_key")? != key.as_bytes()
        || rows[0].try_get::<Option<Uuid>, _>("reverses")?.is_some()
        || rows[0].try_get::<Option<i64>, _>("log_index")?.is_none()
    {
        return Ok(proof.uncertain("originalEntryUnproven"));
    }
    proof.entry_id = Some(id);
    let hash: String = rows[0].try_get("hash")?;
    if wallet.verified_source_hash_in(tx, id).await?.as_deref() != Some(hash.as_str()) {
        return Ok(proof.uncertain("originalEntryHashUnproven"));
    }
    proof.content_hash = Some(hash);
    let reversed: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM ledger_{tenant}.entries WHERE reverses=$1)"
    ))
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if reversed {
        return Ok(proof.uncertain("originalEntryReversed"));
    }
    let postings = sqlx::query(&format!("SELECT a.path,p.account_index,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1"))
        .bind(id).fetch_all(&mut **tx).await?;
    let mut cash = 0i64;
    for posting in postings {
        let minor: i64 = posting.try_get("amount_minor")?;
        let path: String = posting.try_get("path")?;
        let direction: String = posting.try_get("direction")?;
        if wallet
            .source_account(&path)
            .map(|account| account.index())
            .ok()
            != u32::try_from(posting.try_get::<i32, _>("account_index")?).ok()
        {
            return Ok(proof.uncertain("originalAccountIdentityUnproven"));
        }
        if minor <= 0
            || posting.try_get::<String, _>("currency")? != "USD"
            || posting.try_get::<String, _>("layer")? != "settled"
        {
            return Ok(proof.uncertain("originalEntryShapeUnproven"));
        }
        let target = match (path.as_str(), direction.as_str()) {
            ("Assets:Cash", "D") => &mut cash,
            ("Liabilities:Wallet", "C") if purpose != "invoice_repayment" => {
                &mut proof.source_wallet_minor
            }
            ("Liabilities:CreditLine", "C") => &mut proof.source_repayment_minor,
            _ => return Ok(proof.uncertain("originalEntryShapeUnproven")),
        };
        let Some(sum) = target.checked_add(minor) else {
            return Ok(proof.uncertain("originalEntryShapeUnproven"));
        };
        *target = sum;
    }
    if cash != total
        || total <= 0
        || proof
            .source_wallet_minor
            .checked_add(proof.source_repayment_minor)
            != Some(total)
    {
        return Ok(proof.uncertain("originalEntryAmountUnproven"));
    }
    // An unknown old refund may have an interrupted Cash leg. Only a verified
    // immutable effect makes its source consumption safe to count automatically.
    let refunds = sqlx::query("SELECT refund_id,amount_minor,state,cash_policy_version FROM oxsum.refunds WHERE order_id=$1 AND ($2::uuid IS NULL OR refund_id<>$2) ORDER BY refund_id")
        .bind(order).bind(refund).fetch_all(&mut **tx).await?;
    for previous in refunds {
        let id: Uuid = previous.try_get("refund_id")?;
        // A later return cannot consume source at the old return's boundary,
        // regardless of its later allocation or recovery state.
        let return_id = *crate::entry_id_for(&format!("order-refund/{id}")).as_uuid();
        let position: Option<i64> = sqlx::query_scalar(&format!(
            "SELECT log_index FROM ledger_{tenant}.entries WHERE entry_id=$1"
        ))
        .bind(return_id)
        .fetch_optional(&mut **tx)
        .await?
        .flatten();
        if before.is_some_and(|upper| position.is_some_and(|position| position >= upper)) {
            match crate::nonplan_refunds::return_entry_in(
                tx,
                wallet,
                id,
                previous.try_get("amount_minor")?,
            )
            .await
            {
                Ok(Some(_)) => continue,
                Ok(None) | Err(WalletError::Conflict(_)) => {
                    return Ok(proof.uncertain("priorRefundEffectUnproven"));
                }
                Err(error) => return Err(error),
            }
        }
        let effect = match crate::nonplan_refunds::effect_in(tx, wallet, id).await {
            Ok(effect) => effect,
            Err(WalletError::Conflict(_)) => {
                return Ok(proof.uncertain("priorRefundEffectUnproven"));
            }
            Err(error) => return Err(error),
        };
        let Some(effect) = effect else {
            if refund.is_some()
                && previous.try_get::<i16, _>("cash_policy_version")? == 1
                && previous.try_get::<String, _>("state")? != "succeeded"
            {
                match crate::nonplan_refunds::return_entry_in(
                    tx,
                    wallet,
                    id,
                    previous.try_get("amount_minor")?,
                )
                .await
                {
                    Ok(None) => continue,
                    Ok(Some(_)) | Err(WalletError::Conflict(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            return Ok(proof.uncertain("priorRefundHistory"));
        };
        let position: i64 = sqlx::query_scalar(&format!(
            "SELECT log_index FROM ledger_{tenant}.entries WHERE entry_id=$1"
        ))
        .bind(effect.cash_entry_id)
        .fetch_one(&mut **tx)
        .await?;
        if before.is_none_or(|upper| position < upper) {
            proof.prior_returns.push(serde_json::json!({"refundId":id,"entryId":effect.cash_entry_id,"contentHash":effect.cash_entry_hash,"cashMinor":effect.cash_minor,"logIndex":position}));
            proof.prior_refund_cash_minor = proof
                .prior_refund_cash_minor
                .checked_add(effect.cash_minor)
                .ok_or_else(|| WalletError::Conflict("prior refund amount overflows".into()))?;
        }
    }
    let previous = sqlx::query("SELECT dispute_id,amount_minor,state,recharge_entry_id FROM oxsum.disputes WHERE order_id=$1 AND ($2::uuid IS NULL OR dispute_id<>$2) ORDER BY dispute_id")
        .bind(order).bind(dispute).fetch_all(&mut **tx).await?;
    for prior in previous {
        let prior_id: Uuid = prior.try_get("dispute_id")?;
        let expected: i64 = prior.try_get("amount_minor")?;
        let Some(prior_cash) = prior_cash_in(tx, wallet, prior_id, expected).await? else {
            return Ok(proof.uncertain("priorChargebackUnproven"));
        };
        let cash = prior_cash.cash;
        if cash > 0
            && before.is_some_and(|upper| {
                prior_cash
                    .position
                    .is_some_and(|position| position >= upper)
            })
        {
            continue;
        }
        if cash > 0 {
            proof.prior_returns.push(prior_cash.evidence);
        }
        if cash == 0
            && (prior.try_get::<String, _>("state")? == "lost"
                || prior
                    .try_get::<Option<Uuid>, _>("recharge_entry_id")?
                    .is_some())
        {
            return Ok(proof.uncertain("priorChargebackUnproven"));
        }
        proof.prior_chargeback_cash_minor = proof
            .prior_chargeback_cash_minor
            .checked_add(cash)
            .ok_or_else(|| WalletError::Conflict("prior chargeback amount overflows".into()))?;
    }
    let position: i64 = rows[0].try_get("log_index")?;
    if before.is_some_and(|upper| position >= upper) {
        return Ok(proof.uncertain("originalEntryAfterReturn"));
    }
    proof.ledger_boundary=sqlx::query_scalar(&format!("SELECT MAX(log_index) FROM ledger_{tenant}.entries WHERE $1::bigint IS NULL OR log_index<$1"))
        .bind(before).fetch_one(&mut **tx).await?;
    let wallet_account = wallet.source_account("Liabilities:Wallet")?;
    // Verify every entry before the boundary: selecting only current Wallet
    // postings would miss a corrupted posting whose account ID was changed.
    let ids: Vec<Uuid> = sqlx::query_scalar(&format!("SELECT e.entry_id FROM ledger_{tenant}.entries e WHERE e.log_index IS NOT NULL AND ($1::bigint IS NULL OR e.log_index<$1) ORDER BY e.log_index LIMIT $2"))
        .bind(before).bind((ACTIVITY_LIMIT + 1) as i64).fetch_all(&mut **tx).await?;
    if ids.len() > ACTIVITY_LIMIT {
        return Ok(proof.uncertain("walletActivityReviewLimit"));
    }
    let mut activity = Vec::new();
    let mut pending = 0i64;
    for chunk in ids.chunks(ACTIVITY_LIMIT) {
        let entries = match crate::ledger_source::entries_in(tx, tenant, chunk).await {
            Ok(entries) => entries,
            Err(PostgresError::Integrity(_) | PostgresError::Malformed(_)) => {
                return Ok(proof.uncertain("walletActivityUnproven"));
            }
            Err(error) => return Err(error.into()),
        };
        if entries.len() != chunk.len() {
            return Ok(proof.uncertain("walletActivityUnproven"));
        }
        for source in entries {
            let stored = match source.verification {
                Ok(stored) => stored,
                Err(PostgresError::Integrity(_) | PostgresError::Malformed(_)) => {
                    return Ok(proof.uncertain("walletActivityUnproven"));
                }
                Err(error) => return Err(error.into()),
            };
            let Some(index) = stored.index else {
                return Ok(proof.uncertain("walletActivityUnproven"));
            };
            let index = i64::try_from(index.get()).map_err(crate::error::invalid)?;
            let mut entry_debit = 0i64;
            let mut entry_pending = 0i64;
            for posting in stored.entry.postings() {
                if posting.account != wallet_account {
                    continue;
                }
                let amount = posting.amount.to_minor();
                if amount <= 0 || posting.currency.code() != "USD" {
                    return Ok(proof.uncertain("walletActivityUnproven"));
                }
                if posting.layer == Layer::Settled
                    && posting.direction == Direction::Debit
                    && index > position
                {
                    entry_debit = entry_debit
                        .checked_add(amount)
                        .ok_or_else(|| crate::error::invalid("Wallet debit total overflows"))?;
                }
                if posting.layer == Layer::Pending {
                    entry_pending = entry_pending
                        .checked_add(if posting.direction == Direction::Debit {
                            amount
                        } else {
                            -amount
                        })
                        .ok_or_else(|| {
                            crate::error::invalid("Wallet reservation total overflows")
                        })?;
                }
            }
            proof.subsequent_wallet_debit_minor = proof
                .subsequent_wallet_debit_minor
                .checked_add(entry_debit)
                .ok_or_else(|| crate::error::invalid("Wallet debit total overflows"))?;
            pending = pending
                .checked_add(entry_pending)
                .ok_or_else(|| crate::error::invalid("Wallet reservation total overflows"))?;
            activity.push(serde_json::json!({"id":source.id,"hash":stored.content_hash.to_hex(),"position":index,"debit":entry_debit,"pending":entry_pending}));
        }
    }
    proof.wallet_activity_sha256 = Some(
        Sha256::digest(serde_json::to_vec(&activity).map_err(crate::error::invalid)?)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    );
    proof.pending_wallet_minor = pending.max(0);
    proof.wallet_minor = proof
        .source_wallet_minor
        .saturating_sub(proof.subsequent_wallet_debit_minor)
        .saturating_sub(proof.pending_wallet_minor)
        .max(0)
        .min(
            total
                .saturating_sub(proof.prior_chargeback_cash_minor)
                .saturating_sub(proof.prior_refund_cash_minor)
                .max(0),
        )
        .min(loss);
    proof.suspense_minor = loss - proof.wallet_minor;
    if proof.suspense_minor > 0 {
        proof
            .review_reasons
            .push("originalWalletFundingNotRetained");
    }
    Ok(proof)
}

struct PriorCash {
    cash: i64,
    position: Option<i64>,
    evidence: serde_json::Value,
}

/// Count actual complete prior Cash returns, irrespective of interrupted state.
/// Legacy wrong-direction or incompatible effects cannot authorize pool debits.
async fn prior_cash_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    dispute: Uuid,
    expected: i64,
) -> Result<Option<PriorCash>, WalletError> {
    let correction = match crate::nonplan_chargebacks::effect_in(tx, wallet, dispute).await {
        Ok(value) => value,
        Err(WalletError::Conflict(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if let Some(effect) = correction {
        let position: Option<i64> = sqlx::query_scalar(&format!(
            "SELECT log_index FROM ledger_{}.entries WHERE entry_id=$1",
            wallet.tenant_id()
        ))
        .bind(effect.original_entry_id)
        .fetch_one(&mut **tx)
        .await?;
        return Ok((effect.actual_loss_minor == expected).then_some(PriorCash {
            cash:effect.actual_loss_minor,position,
            evidence:serde_json::json!({"disputeId":dispute,"cashMinor":effect.actual_loss_minor,"originalWrongEntryId":effect.original_entry_id,"originalWrongHash":effect.original_entry_hash,"correctionEntryId":effect.correction_entry_id,"correctionHash":effect.correction_entry_hash,"falseReceiptMinor":effect.false_receipt_minor,"sourceRevision":effect.source_revision,"logIndex":position}),
        }));
    }
    let tenant = wallet.tenant_id();
    let key = format!("dispute-recharge/{dispute}");
    let id = *crate::entry_id_for(&key).as_uuid();
    let rows = sqlx::query(&format!("SELECT entry_id,idempotency_key,reverses,log_index FROM ledger_{tenant}.entries WHERE entry_id=$1 OR idempotency_key=$2"))
        .bind(id).bind(key.as_bytes()).fetch_all(&mut **tx).await?;
    if rows.is_empty() {
        return Ok(Some(PriorCash {
            cash: 0,
            position: None,
            evidence: serde_json::Value::Null,
        }));
    }
    if rows.len() != 1
        || rows[0].try_get::<Uuid, _>("entry_id")? != id
        || rows[0].try_get::<Vec<u8>, _>("idempotency_key")? != key.as_bytes()
        || rows[0].try_get::<Option<Uuid>, _>("reverses")?.is_some()
        || rows[0].try_get::<Option<i64>, _>("log_index")?.is_none()
        || wallet.verified_source_hash_in(tx, id).await?.is_none()
    {
        return Ok(None);
    }
    let rows_position: Option<i64> = rows[0].try_get("log_index")?;
    let reversed: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM ledger_{tenant}.entries WHERE reverses=$1)"
    ))
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if reversed {
        return Ok(None);
    }
    let rows=sqlx::query(&format!("SELECT a.path,p.account_index,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1"))
        .bind(id).fetch_all(&mut **tx).await?;
    let mut cash = 0i64;
    let mut allocated = 0i64;
    for row in rows {
        let path: String = row.try_get("path")?;
        let direction: String = row.try_get("direction")?;
        if wallet
            .source_account(&path)
            .map(|account| account.index())
            .ok()
            != u32::try_from(row.try_get::<i32, _>("account_index")?).ok()
        {
            return Ok(None);
        }
        let minor: i64 = row.try_get("amount_minor")?;
        if minor <= 0
            || row.try_get::<String, _>("currency")? != "USD"
            || row.try_get::<String, _>("layer")? != "settled"
        {
            return Ok(None);
        }
        let target = match (path.as_str(), direction.as_str()) {
            ("Assets:Cash", "C") => &mut cash,
            ("Liabilities:Wallet" | "Equity:Adjustments", "D") => &mut allocated,
            _ => return Ok(None),
        };
        let Some(sum) = target.checked_add(minor) else {
            return Ok(None);
        };
        *target = sum;
    }
    Ok((cash == expected && allocated == cash && cash > 0).then_some(PriorCash {
        cash,position:rows_position,
        evidence:serde_json::json!({"disputeId":dispute,"entryId":id,"contentHash":wallet.verified_source_hash_in(tx,id).await?,"cashMinor":cash,"logIndex":rows_position}),
    }))
}

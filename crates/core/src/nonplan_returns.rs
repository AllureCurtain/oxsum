//! Original-source evidence and serialization for non-plan cash returns.

use std::time::Duration;

use serde::Serialize;
use sqlx::{Postgres, Row, Transaction};
use tokio::time::Instant;
use uuid::Uuid;

use crate::{VerifiedMerchantEvent, Wallet, WalletError};

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
    entry_id: Option<Uuid>,
    content_hash: Option<String>,
    fulfillment_id: Option<Uuid>,
    source_wallet_minor: i64,
    source_repayment_minor: i64,
    subsequent_wallet_debit_minor: i64,
    pending_wallet_minor: i64,
    prior_chargeback_cash_minor: i64,
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
    let mut proof = SourceEvidence {
        policy: "nonplan-cash-v1",
        order_id: order,
        suspense_minor: loss,
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
    if wallet.verified_source_hash(id).await?.as_deref() != Some(hash.as_str()) {
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
    let postings = sqlx::query(&format!("SELECT a.path,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1"))
        .bind(id).fetch_all(&mut **tx).await?;
    let mut cash = 0i64;
    for posting in postings {
        let minor: i64 = posting.try_get("amount_minor")?;
        let path: String = posting.try_get("path")?;
        let direction: String = posting.try_get("direction")?;
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
    // Existing refund writers are not transaction-owned yet (#841). Any refund
    // history excludes a take-back, including an already-retained orphan effect.
    let refunds: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.refunds WHERE order_id=$1)")
            .bind(order)
            .fetch_one(&mut **tx)
            .await?;
    if refunds {
        return Ok(proof.uncertain("priorRefundHistory"));
    }
    let previous = sqlx::query("SELECT dispute_id,amount_minor,state,recharge_entry_id FROM oxsum.disputes WHERE order_id=$1 AND dispute_id<>$2 ORDER BY dispute_id")
        .bind(order).bind(dispute).fetch_all(&mut **tx).await?;
    for prior in previous {
        let prior_id: Uuid = prior.try_get("dispute_id")?;
        let expected: i64 = prior.try_get("amount_minor")?;
        let Some(cash) = prior_cash_in(tx, wallet, prior_id, expected).await? else {
            return Ok(proof.uncertain("priorChargebackUnproven"));
        };
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
    let used = sqlx::query(&format!("SELECT COALESCE(SUM(p.amount_minor) FILTER(WHERE p.layer='settled' AND p.direction='D' AND e.log_index>$1),0)::bigint AS debits,COALESCE(SUM(CASE WHEN p.direction='D' THEN p.amount_minor ELSE -p.amount_minor END) FILTER(WHERE p.layer='pending'),0)::bigint AS reserved FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.entries e USING(entry_id) JOIN ledger_{tenant}.accounts a USING(account_index) WHERE e.log_index IS NOT NULL AND a.path='Liabilities:Wallet'"))
        .bind(position).fetch_one(&mut **tx).await?;
    proof.subsequent_wallet_debit_minor = used.try_get("debits")?;
    proof.pending_wallet_minor = used.try_get::<i64, _>("reserved")?.max(0);
    proof.wallet_minor = proof
        .source_wallet_minor
        .saturating_sub(proof.subsequent_wallet_debit_minor)
        .saturating_sub(proof.pending_wallet_minor)
        .max(0)
        .min(
            total
                .saturating_sub(proof.prior_chargeback_cash_minor)
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

/// Count actual complete prior Cash returns, irrespective of interrupted state.
/// Legacy wrong-direction or incompatible effects cannot authorize pool debits.
async fn prior_cash_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    dispute: Uuid,
    expected: i64,
) -> Result<Option<i64>, WalletError> {
    let tenant = wallet.tenant_id();
    let key = format!("dispute-recharge/{dispute}");
    let id = *crate::entry_id_for(&key).as_uuid();
    let rows = sqlx::query(&format!("SELECT entry_id,idempotency_key,reverses,log_index FROM ledger_{tenant}.entries WHERE entry_id=$1 OR idempotency_key=$2"))
        .bind(id).bind(key.as_bytes()).fetch_all(&mut **tx).await?;
    if rows.is_empty() {
        return Ok(Some(0));
    }
    if rows.len() != 1
        || rows[0].try_get::<Uuid, _>("entry_id")? != id
        || rows[0].try_get::<Vec<u8>, _>("idempotency_key")? != key.as_bytes()
        || rows[0].try_get::<Option<Uuid>, _>("reverses")?.is_some()
        || rows[0].try_get::<Option<i64>, _>("log_index")?.is_none()
        || wallet.verified_source_hash(id).await?.is_none()
    {
        return Ok(None);
    }
    let reversed: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM ledger_{tenant}.entries WHERE reverses=$1)"
    ))
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if reversed {
        return Ok(None);
    }
    let rows=sqlx::query(&format!("SELECT a.path,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1"))
        .bind(id).fetch_all(&mut **tx).await?;
    let mut cash = 0i64;
    let mut allocated = 0i64;
    for row in rows {
        let path: String = row.try_get("path")?;
        let direction: String = row.try_get("direction")?;
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
    Ok((cash == expected && allocated == cash && cash > 0).then_some(cash))
}

//! Exact immutable case, audit, receipt and canonical ledger source checks.

use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use crate::{MerchantEventKind, VerifiedMerchantEvent, Wallet, WalletError, error};

pub(super) fn review(reason: &str) -> WalletError {
    WalletError::Conflict(format!("historical chargeback source review: {reason}"))
}

pub(super) fn revision(source: &serde_json::Value) -> Result<String, WalletError> {
    Ok(
        Sha256::digest(serde_json::to_vec(source).map_err(error::invalid)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Source {
    pub dispute_id: Uuid,
    pub order_id: Uuid,
    pub purpose: String,
    pub organization_id: Uuid,
    pub business_id: Uuid,
    pub loss: i64,
    pub order_amount: i64,
    pub order_fiat: i64,
    pub currency: String,
    pub merchant_id: Uuid,
    pub version_id: Uuid,
    pub provider_account: String,
    pub merchant_mode: String,
    pub provider_payment_id: String,
    pub provider_dispute_id: String,
    pub event_receipt_id: Uuid,
    pub original_audit_id: Uuid,
    pub actor_id: Uuid,
    pub audit: serde_json::Value,
    pub original: ExactEntry,
}

/// Only verified canonical bytes and registered account meaning can prove a leg.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ExactEntry {
    pub id: Uuid,
    pub hash: String,
    pub position: i64,
    pub date: Date,
    pub cash: i64,
    pub wallet: i64,
    pub suspense: i64,
    pub credit: i64,
}

/// Validate the old wrong entry or the new compensation, never a general reversal.
pub(super) async fn entry_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    dispute: Uuid,
    correction: bool,
) -> Result<Option<ExactEntry>, WalletError> {
    let key = format!(
        "{}/{dispute}",
        if correction {
            "nonplan-chargeback-correction"
        } else {
            "dispute-recharge"
        }
    );
    let id = *crate::entry_id_for(&key).as_uuid();
    let tenant = wallet.tenant_id();
    let rows = sqlx::query(&format!("SELECT entry_id,idempotency_key,reverses,log_index,booking_date FROM ledger_{tenant}.entries WHERE entry_id=$1 OR idempotency_key=$2"))
        .bind(id).bind(key.as_bytes()).fetch_all(&mut **tx).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() != 1
        || rows[0].try_get::<Uuid, _>("entry_id")? != id
        || rows[0].try_get::<Vec<u8>, _>("idempotency_key")? != key.as_bytes()
        || rows[0].try_get::<Option<Uuid>, _>("reverses")?.is_some()
        || rows[0].try_get::<Option<i64>, _>("log_index")?.is_none()
    {
        return Err(review("entry identity, raw key or sequencing differs"));
    }
    let reversed: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM ledger_{tenant}.entries WHERE reverses=$1)"
    ))
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    let hash = wallet.verified_source_hash_in(tx, id).await?;
    if reversed || hash.is_none() {
        return Err(review("entry is reversed or canonical bytes are unproven"));
    }
    let mut entry = ExactEntry {
        id,
        hash: hash.ok_or_else(|| review("missing entry hash"))?,
        position: rows[0].try_get("log_index")?,
        date: rows[0].try_get("booking_date")?,
        cash: 0,
        wallet: 0,
        suspense: 0,
        credit: 0,
    };
    let postings=sqlx::query(&format!("SELECT a.path,p.account_index,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1"))
        .bind(id).fetch_all(&mut **tx).await?;
    if (!correction && postings.len() != 2) || (correction && !(2..=3).contains(&postings.len())) {
        return Err(review("entry posting count differs"));
    }
    for posting in postings {
        let path: String = posting.try_get("path")?;
        let minor: i64 = posting.try_get("amount_minor")?;
        if minor <= 0
            || posting.try_get::<String, _>("currency")? != "USD"
            || posting.try_get::<String, _>("layer")? != "settled"
            || wallet
                .source_account(&path)
                .map(|account| account.index())
                .ok()
                != u32::try_from(posting.try_get::<i32, _>("account_index")?).ok()
        {
            return Err(review(
                "entry account identity, amount, layer or currency differs",
            ));
        }
        let direction: String = posting.try_get("direction")?;
        let target = match (correction, path.as_str(), direction.as_str()) {
            (false, "Assets:Cash", "D") | (true, "Assets:Cash", "C") => &mut entry.cash,
            (false, "Liabilities:CreditLine", "C") => &mut entry.credit,
            (true, "Liabilities:Wallet", "D") => &mut entry.wallet,
            (true, "Equity:Adjustments", "D") => &mut entry.suspense,
            _ => return Err(review("entry touches an unsupported account or direction")),
        };
        if *target != 0 {
            return Err(review("duplicate account leg"));
        }
        *target = minor;
    }
    if entry.cash <= 0
        || if correction {
            entry.wallet.checked_add(entry.suspense) != Some(entry.cash) || entry.suspense <= 0
        } else {
            entry.cash != entry.credit
        }
    {
        return Err(review("entry does not conserve the exact supported legs"));
    }
    Ok(Some(entry))
}

/// Check current immutable terminal ownership and the original approval audit.
pub(super) async fn source_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    dispute: Uuid,
) -> Result<Source, WalletError> {
    let row=sqlx::query("SELECT d.*,p.business_id,p.organization_id AS order_customer,p.merchant_id AS order_merchant,p.merchant_version_id,p.provider_payment_id,p.purpose,p.status,p.paid_at,p.amount_minor AS order_amount,p.display_amount_minor,p.display_currency,m.business_id AS merchant_business,m.account_id AS provider_account,m.mode AS merchant_mode,o.tenant_id FROM oxsum.disputes d JOIN oxsum.payment_orders p USING(order_id) JOIN oxsum.merchant_accounts m ON m.merchant_id=d.merchant_id JOIN oxsum.organizations o ON o.organization_id=d.organization_id WHERE d.dispute_id=$1 FOR UPDATE OF d")
        .bind(dispute).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::NotFound("unknown dispute".into()))?;
    let business: Uuid = row.try_get("business_id")?;
    let organization: Uuid = row.try_get("organization_id")?;
    let order: Uuid = row.try_get("order_id")?;
    let merchant: Uuid = row.try_get("merchant_id")?;
    let loss: i64 = row.try_get("amount_minor")?;
    let order_amount: i64 = row.try_get("order_amount")?;
    if row.try_get::<String, _>("tenant_id")? != wallet.tenant_id()
        || row.try_get::<Uuid, _>("order_customer")? != organization
        || row.try_get::<Uuid, _>("merchant_business")? != business
        || row.try_get::<Option<Uuid>, _>("order_merchant")? != Some(merchant)
    {
        return Err(review("wallet, order or merchant ownership differs"));
    }
    if row.try_get::<String, _>("state")? != "lost"
        || row.try_get::<String, _>("status")? != "fulfilled"
        || row
            .try_get::<Option<time::OffsetDateTime>, _>("paid_at")?
            .is_none()
        || !matches!(
            row.try_get::<String, _>("purpose")?.as_str(),
            "wallet_topup" | "bundle" | "invoice_repayment"
        )
        || loss <= 0
        || loss > order_amount
        || row.try_get::<String, _>("currency")? != row.try_get::<String, _>("display_currency")?
    {
        return Err(review(
            "only exact paid, fulfilled and terminal lost non-plan sources are supported",
        ));
    }
    let original = entry_in(tx, wallet, dispute, false)
        .await?
        .ok_or_else(|| review("original wrong-direction entry is missing"))?;
    if original.cash > loss
        || row.try_get::<Option<Uuid>, _>("recharge_entry_id")? != Some(original.id)
    {
        return Err(review(
            "original wrong amount or terminal entry pointer differs",
        ));
    }
    let audit_id = row
        .try_get::<Option<Uuid>, _>("audit_ref")?
        .ok_or_else(|| review("original resolution audit is missing"))?;
    let actor = row
        .try_get::<Option<Uuid>, _>("resolved_by")?
        .ok_or_else(|| review("original resolution actor is missing"))?;
    let audit=sqlx::query("SELECT actor_user_id,action,target,detail,note_id FROM oxsum.finance_audit WHERE audit_id=$1 AND business_id=$2")
        .bind(audit_id).bind(business).fetch_optional(&mut **tx).await?.ok_or_else(||review("original resolution audit is missing"))?;
    let detail: serde_json::Value = audit.try_get("detail")?;
    if audit.try_get::<Uuid, _>("actor_user_id")? != actor
        || actor.is_nil()
        || audit.try_get::<String, _>("action")? != "dispute.resolve"
        || audit.try_get::<String, _>("target")? != dispute.to_string()
        || audit.try_get::<Option<Uuid>, _>("note_id")?.is_some()
        || detail["orderId"] != json!(order)
        || detail["resolution"] != "lost"
        || detail["contestedMinor"] != loss
        || detail["rechargedMinor"] != original.cash
        || detail["currency"] != row.try_get::<String, _>("currency")?
        || detail["rechargeEntryId"] != json!(original.id)
        || ["payableMinor", "unallocatedMinor"]
            .iter()
            .any(|field| detail.get(*field).is_some_and(|value| value != 0))
    {
        return Err(review(
            "original terminal audit does not prove both false receipt and actual loss",
        ));
    }
    Ok(Source {
        dispute_id: dispute,
        order_id: order,
        purpose: row.try_get("purpose")?,
        organization_id: organization,
        business_id: business,
        loss,
        order_amount,
        order_fiat: row.try_get("display_amount_minor")?,
        currency: row.try_get("currency")?,
        merchant_id: merchant,
        version_id: row
            .try_get::<Option<Uuid>, _>("merchant_version_id")?
            .ok_or_else(|| review("pinned merchant version is missing"))?,
        provider_account: row.try_get("provider_account")?,
        merchant_mode: row.try_get("merchant_mode")?,
        provider_payment_id: row
            .try_get::<Option<String>, _>("provider_payment_id")?
            .ok_or_else(|| review("provider payment is missing"))?,
        provider_dispute_id: row.try_get("provider_dispute_id")?,
        event_receipt_id: row
            .try_get::<Option<Uuid>, _>("inbox_receipt_id")?
            .ok_or_else(|| review("original authenticated receipt is missing"))?,
        original_audit_id: audit_id,
        actor_id: actor,
        audit: detail,
        original,
    })
}

pub(super) async fn event_in(
    tx: &mut Transaction<'_, Postgres>,
    source: &Source,
    receipt: Uuid,
) -> Result<serde_json::Value, WalletError> {
    if receipt != source.event_receipt_id {
        return Err(review("receipt is not the original exact case receipt"));
    }
    let row=sqlx::query("SELECT e.*,b.payment_id AS bound_payment,b.organization_id AS bound_customer,b.amount_minor AS bound_amount,b.currency AS bound_currency,b.purpose AS bound_purpose FROM oxsum.merchant_event_inbox e LEFT JOIN oxsum.merchant_payment_refs r ON r.merchant_id=e.merchant_id AND r.provider_payment_id=e.normalized->>'providerPaymentId' LEFT JOIN oxsum.merchant_payment_bindings b ON b.payment_id=r.payment_id AND b.merchant_id=e.merchant_id AND b.version_id=e.version_id WHERE e.receipt_id=$1")
        .bind(receipt).fetch_optional(&mut **tx).await?.ok_or_else(||review("exact authenticated receipt is missing"))?;
    let normalized: serde_json::Value = row.try_get("normalized")?;
    let mut event: VerifiedMerchantEvent =
        serde_json::from_value(normalized.clone()).map_err(error::invalid)?;
    let payment: Option<Uuid> = row.try_get("bound_payment")?;
    let customer: Option<Uuid> = row.try_get("bound_customer")?;
    if event.payment_id.is_none() {
        event.payment_id = payment;
    }
    if event.organization_id.is_none() {
        event.organization_id = customer;
    }
    let amount = crate::nonplan_returns::disputed_minor(
        source.order_amount,
        source.order_fiat,
        &source.currency,
        &event,
    )
    .map_err(review)?;
    if !matches!(
        row.try_get::<String, _>("state")?.as_str(),
        "accepted" | "applied"
    ) || row.try_get::<Uuid, _>("merchant_id")? != source.merchant_id
        || row.try_get::<Uuid, _>("version_id")? != source.version_id
        || row.try_get::<String, _>("event_id")? != event.event_id
        || event.kind != MerchantEventKind::Dispute
        || event
            .provider_account_id
            .as_deref()
            .is_some_and(|value| value != source.provider_account)
        || event
            .mode
            .is_some_and(|value| value.as_str() != source.merchant_mode)
        || row
            .try_get::<Option<String>, _>("bound_purpose")?
            .as_deref()
            != Some("order")
        || amount != source.loss
        || event.payment_id != Some(source.order_id)
        || payment != Some(source.order_id)
        || event.organization_id != Some(source.organization_id)
        || customer != Some(source.organization_id)
        || event.provider_payment_id.as_deref() != Some(source.provider_payment_id.as_str())
        || event.refund_id.as_deref() != Some(source.provider_dispute_id.as_str())
        || row.try_get::<Option<i64>, _>("bound_amount")? != Some(source.order_fiat)
        || row
            .try_get::<Option<String>, _>("bound_currency")?
            .as_deref()
            != Some(source.currency.as_str())
    {
        return Err(review(
            "authenticated event does not bind the exact original case and loss",
        ));
    }
    Ok(
        json!({"receiptId":receipt,"eventId":event.event_id,"merchantId":source.merchant_id,"versionId":source.version_id,
        "payloadDigest":row.try_get::<Vec<u8>,_>("payload_digest")?,"normalized":normalized,"boundPaymentId":payment,"boundOrganizationId":customer}),
    )
}

//! Verify exact authenticated refund and immutable ledger source evidence.

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use crate::{MerchantEventKind, VerifiedMerchantEvent, Wallet, WalletError, error};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Source {
    pub refund_id: Uuid,
    pub order_id: Uuid,
    pub organization_id: Uuid,
    pub business_id: Uuid,
    pub amount: i64,
    pub display_amount: i64,
    pub currency: String,
    pub state: String,
    pub purpose: String,
    pub status: String,
    pub merchant_id: Option<Uuid>,
    pub merchant_version_id: Option<Uuid>,
    pub provider_payment_id: Option<String>,
    pub provider_refund_id: Option<String>,
    pub body_id: bool,
    pub resolved_by: Option<Uuid>,
    pub note_id: Option<Uuid>,
    pub note_state: Option<String>,
    pub note_entry: Option<Uuid>,
    pub approved: bool,
}

pub(super) fn review(message: &str) -> WalletError {
    WalletError::Conflict(format!("non-plan refund requires source review: {message}"))
}

pub(super) fn revision(value: &serde_json::Value) -> Result<String, WalletError> {
    Ok(
        Sha256::digest(serde_json::to_vec(value).map_err(error::invalid)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

pub(super) async fn source_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    refund: Uuid,
) -> Result<Source, WalletError> {
    let row = sqlx::query("SELECT r.*,p.purpose,p.status,p.paid_at,p.organization_id AS order_organization,p.business_id AS order_business,p.merchant_id,p.merchant_version_id,p.provider_payment_id AS order_provider,o.tenant_id,n.credit_note_id,n.state AS note_state,n.entry_id AS note_entry,n.amount_minor AS note_amount,n.order_id AS note_order,n.organization_id AS note_organization,EXISTS(SELECT 1 FROM oxsum.finance_audit a WHERE a.audit_id=r.audit_ref AND a.business_id=r.business_id AND a.actor_user_id=r.resolved_by AND a.action='refund.approve' AND a.target=r.refund_id::text AND a.note_id=n.credit_note_id AND a.detail->>'orderId'=r.order_id::text AND (a.detail->>'amountMinor')::numeric=r.amount_minor) AS approved FROM oxsum.refunds r JOIN oxsum.payment_orders p USING(order_id) JOIN oxsum.organizations o ON o.organization_id=r.organization_id LEFT JOIN oxsum.credit_notes n ON n.refund_id=r.refund_id WHERE r.refund_id=$1 FOR UPDATE OF r")
        .bind(refund).fetch_optional(&mut **tx).await?
        .ok_or_else(|| WalletError::NotFound("unknown refund".into()))?;
    let organization: Uuid = row.try_get("organization_id")?;
    let business: Uuid = row.try_get("business_id")?;
    let amount: i64 = row.try_get("amount_minor")?;
    let order: Uuid = row.try_get("order_id")?;
    if row.try_get::<String, _>("tenant_id")? != wallet.tenant_id()
        || row.try_get::<Uuid, _>("order_organization")? != organization
        || row.try_get::<Uuid, _>("order_business")? != business
    {
        return Err(review("wallet or order ownership differs"));
    }
    let purpose: String = row.try_get("purpose")?;
    if !matches!(
        purpose.as_str(),
        "wallet_topup" | "bundle" | "invoice_repayment"
    ) {
        return Err(review("unsupported payment purpose"));
    }
    let status: String = row.try_get("status")?;
    if !matches!(status.as_str(), "paid" | "fulfilled")
        || row
            .try_get::<Option<time::OffsetDateTime>, _>("paid_at")?
            .is_none()
    {
        return Err(review("original payment is not confirmed"));
    }
    let note_id: Option<Uuid> = row.try_get("credit_note_id")?;
    if note_id.is_some()
        && (row.try_get::<Option<i64>, _>("note_amount")? != Some(amount)
            || row.try_get::<Option<Uuid>, _>("note_order")? != Some(order)
            || row.try_get::<Option<Uuid>, _>("note_organization")? != Some(organization))
    {
        return Err(review("credit note differs from refund"));
    }
    Ok(Source {
        refund_id: refund,
        order_id: order,
        organization_id: organization,
        business_id: business,
        amount,
        display_amount: row.try_get("display_amount_minor")?,
        currency: row.try_get("display_currency")?,
        state: row.try_get("state")?,
        purpose,
        status,
        merchant_id: row.try_get("merchant_id")?,
        merchant_version_id: row.try_get("merchant_version_id")?,
        provider_payment_id: row.try_get("order_provider")?,
        provider_refund_id: row.try_get("provider_refund_id")?,
        body_id: row.try_get("hyperswitch_body_id")?,
        resolved_by: row.try_get("resolved_by")?,
        note_id,
        note_state: row.try_get("note_state")?,
        note_entry: row.try_get("note_entry")?,
        approved: row.try_get("approved")?,
    })
}

pub(super) async fn event_in(
    tx: &mut Transaction<'_, Postgres>,
    source: &Source,
    receipt: Uuid,
    allow_legacy: bool,
) -> Result<serde_json::Value, WalletError> {
    let row = sqlx::query("SELECT e.*,b.payment_id AS bound_payment,b.organization_id AS bound_organization FROM oxsum.merchant_event_inbox e LEFT JOIN oxsum.merchant_payment_refs r ON r.merchant_id=e.merchant_id AND r.provider_payment_id=e.normalized->>'providerPaymentId' LEFT JOIN oxsum.merchant_payment_bindings b ON b.payment_id=r.payment_id AND b.merchant_id=e.merchant_id AND b.version_id=e.version_id WHERE e.receipt_id=$1")
        .bind(receipt).fetch_optional(&mut **tx).await?.ok_or_else(|| review("exact success receipt is missing"))?;
    let normalized: serde_json::Value = row.try_get("normalized")?;
    let mut event: VerifiedMerchantEvent =
        serde_json::from_value(normalized.clone()).map_err(error::invalid)?;
    let payment: Option<Uuid> = row.try_get("bound_payment")?;
    let customer: Option<Uuid> = row.try_get("bound_organization")?;
    if event.payment_id.is_none() {
        event.payment_id = payment;
    }
    if event.organization_id.is_none() {
        event.organization_id = customer;
    }
    if !matches!(
        row.try_get::<String, _>("state")?.as_str(),
        "accepted" | "applied"
    ) || Some(row.try_get::<Uuid, _>("merchant_id")?) != source.merchant_id
        || Some(row.try_get::<Uuid, _>("version_id")?) != source.merchant_version_id
        || row.try_get::<String, _>("event_id")? != event.event_id
        || event.kind != MerchantEventKind::RefundSucceeded
        || event.payment_id != Some(source.order_id)
        || event.organization_id != Some(source.organization_id)
        || payment != Some(source.order_id)
        || customer != Some(source.organization_id)
        || event.provider_payment_id != source.provider_payment_id
        || event.amount_minor != Some(source.display_amount)
        || event.currency.as_deref() != Some(source.currency.as_str())
    {
        return Err(review("receipt does not bind the exact confirmed refund"));
    }
    if let Some(id) = event.refund_id.as_deref() {
        if source.provider_refund_id.as_deref() != Some(id)
            && !(source.provider_refund_id.is_none()
                && source.body_id
                && source.refund_id.to_string() == id)
        {
            return Err(review("provider refund identity differs"));
        }
    } else {
        let retained: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.nonplan_refund_effects WHERE refund_id=$1 AND event_receipt_id=$2)")
            .bind(source.refund_id).bind(receipt).fetch_one(&mut **tx).await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.refunds WHERE order_id=$1 AND state='provider_pending'",
        )
        .bind(source.order_id)
        .fetch_one(&mut **tx)
        .await?;
        if !allow_legacy || (!retained && (source.state != "provider_pending" || count != 1)) {
            return Err(review(
                "historical payment-only receipt is not an exact refund identity",
            ));
        }
    }
    Ok(
        serde_json::json!({"receiptId":receipt,"eventId":event.event_id,"merchantId":source.merchant_id,
        "versionId":source.merchant_version_id,"payloadDigest":row.try_get::<Vec<u8>,_>("payload_digest")?,
        "normalized":normalized,"boundPaymentId":payment,"boundOrganizationId":customer}),
    )
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReturnEntry {
    pub id: Uuid,
    pub hash: String,
    pub position: i64,
    pub date: Date,
    pub wallet: i64,
    pub suspense: i64,
    pub cash: i64,
}

/// Read an exact unreversed Cash return through the engine's hash verifier.
pub(crate) async fn return_entry_in(
    tx: &mut Transaction<'_, Postgres>,
    wallet: &Wallet,
    refund: Uuid,
    expected: i64,
) -> Result<Option<ReturnEntry>, WalletError> {
    let key = format!("order-refund/{refund}");
    let id = *crate::entry_id_for(&key).as_uuid();
    let tenant = wallet.tenant_id();
    let rows = sqlx::query(&format!("SELECT entry_id,idempotency_key,encode(content_hash,'hex') AS hash,reverses,log_index,booking_date FROM ledger_{tenant}.entries WHERE entry_id=$1 OR idempotency_key=$2"))
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
        return Err(review("retained Cash entry identity is unproven"));
    }
    let row = &rows[0];
    let hash: String = row.try_get("hash")?;
    if wallet
        .verified_source_hash_in(&mut **tx, id)
        .await?
        .as_deref()
        != Some(hash.as_str())
    {
        return Err(review("retained Cash content hash differs"));
    }
    let reversed: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM ledger_{tenant}.entries WHERE reverses=$1)"
    ))
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if reversed {
        return Err(review("retained Cash entry was reversed"));
    }
    let mut result = ReturnEntry {
        id,
        hash,
        position: row.try_get("log_index")?,
        date: row.try_get("booking_date")?,
        wallet: 0,
        suspense: 0,
        cash: 0,
    };
    let postings = sqlx::query(&format!("SELECT a.path,p.account_index,p.direction,p.amount_minor,p.currency,p.layer FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) WHERE p.entry_id=$1"))
        .bind(id).fetch_all(&mut **tx).await?;
    for posting in postings {
        let amount: i64 = posting.try_get("amount_minor")?;
        if amount <= 0
            || posting.try_get::<String, _>("currency")? != "USD"
            || posting.try_get::<String, _>("layer")? != "settled"
        {
            return Err(review("retained Cash entry shape differs"));
        }
        let path: String = posting.try_get("path")?;
        let direction: String = posting.try_get("direction")?;
        if wallet
            .source_account(&path)
            .map(|account| account.index())
            .ok()
            != u32::try_from(posting.try_get::<i32, _>("account_index")?).ok()
        {
            return Err(review("retained Cash account identity differs"));
        }
        let target = match (path.as_str(), direction.as_str()) {
            ("Assets:Cash", "C") => &mut result.cash,
            ("Liabilities:Wallet", "D") => &mut result.wallet,
            ("Equity:Adjustments", "D") => &mut result.suspense,
            _ => return Err(review("retained Cash entry uses an unsupported account")),
        };
        *target = target
            .checked_add(amount)
            .ok_or_else(|| review("retained Cash amount overflows"))?;
    }
    if result.cash != expected || result.wallet.checked_add(result.suspense) != Some(expected) {
        return Err(review("retained Cash amount differs"));
    }
    Ok(Some(result))
}

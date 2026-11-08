//! Durable authenticated merchant inbox and pre-request expected payment terms.
use crate::{
    BusinessCapability, Db, MerchantMode, MerchantSigningContext, WalletError,
    business_staff as authority,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;
/// Normalized kind of a verified merchant event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MerchantEventKind {
    /// The payment completed successfully.
    Succeeded,
    /// The payment attempt failed.
    Failed,
    /// The payment expired before completing.
    Expired,
    /// A refund completed successfully.
    RefundSucceeded,
    /// A refund attempt failed.
    RefundFailed,
    /// A dispute was opened or updated.
    Dispute,
    /// An event type this service does not handle; recorded as ignored.
    Unsupported,
}
/// A merchant event whose origin has already been authenticated by the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifiedMerchantEvent {
    /// Provider event identifier, unique per merchant.
    pub event_id: String,
    /// Normalized event kind.
    pub kind: MerchantEventKind,
    /// Local payment identifier the event refers to, if known.
    pub payment_id: Option<Uuid>,
    /// Provider-side payment identifier.
    pub provider_payment_id: Option<String>,
    /// Customer organization expected to own the payment.
    pub organization_id: Option<Uuid>,
    /// Provider account that produced the event.
    pub provider_account_id: Option<String>,
    /// Live or test mode reported by the provider.
    pub mode: Option<MerchantMode>,
    /// Event amount in integer minor units.
    pub amount_minor: Option<i64>,
    /// ISO 4217 currency code.
    pub currency: Option<String>,
    /// Provider refund identifier for refund events.
    pub refund_id: Option<String>,
}
/// What a bound merchant payment is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MerchantPaymentPurpose {
    /// An order fulfilled through the purpose-specific order path.
    Order,
    /// A legacy wallet deposit credited through the compatibility adapter.
    LegacyDeposit,
}
impl MerchantPaymentPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Order => "order",
            Self::LegacyDeposit => "legacyDeposit",
        }
    }
}
/// Expected payment terms pinned before any provider request is sent.
#[derive(Debug, Clone)]
pub struct MerchantPaymentBinding {
    /// Local payment identifier.
    pub payment_id: Uuid,
    /// Merchant account the payment belongs to.
    pub merchant_id: Uuid,
    /// Merchant configuration version in force when the terms were pinned.
    pub version_id: Uuid,
    /// Customer organization that owns the payment.
    pub organization_id: Uuid,
    /// Expected amount in integer minor units.
    pub amount_minor: i64,
    /// Expected ISO 4217 currency code.
    pub currency: String,
    /// Whether this is an order or a legacy deposit.
    pub purpose: MerchantPaymentPurpose,
    /// Whether a new Hyperswitch operation pins its body identity before I/O.
    /// False preserves historical operations whose outcome may be unknown.
    pub hyperswitch_body_id: bool,
    /// Provider payment identifier once one has been observed.
    pub provider_payment_id: Option<String>,
}
/// Durable receipt for a merchant event stored in the inbox.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MerchantEventReceipt {
    /// Inbox receipt identifier.
    pub id: Uuid,
    /// Merchant that received the event.
    pub merchant_id: Uuid,
    /// Merchant configuration version that classified the event.
    pub version_id: Uuid,
    /// Provider event identifier.
    pub event_id: String,
    /// Normalized event kind.
    pub kind: MerchantEventKind,
    /// Local payment identifier, if the event resolved to one.
    pub payment_id: Option<Uuid>,
    /// Processing state, such as `"pending"`, `"accepted"` or `"rejected"`.
    pub state: String,
    /// Machine-readable rejection or deferral reason, when applicable.
    pub reason: Option<String>,
}
/// A cursor-paginated page of merchant event receipts.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MerchantEventPage {
    /// Receipts in ascending receipt identifier order.
    pub events: Vec<MerchantEventReceipt>,
    /// Identifier to pass as `after` for the next page; `None` at the end.
    pub next_cursor: Option<Uuid>,
}
fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}
fn binding(row: &PgRow) -> Result<MerchantPaymentBinding, WalletError> {
    Ok(MerchantPaymentBinding {
        payment_id: row.try_get("payment_id")?,
        merchant_id: row.try_get("merchant_id")?,
        version_id: row.try_get("version_id")?,
        organization_id: row.try_get("organization_id")?,
        amount_minor: row.try_get("amount_minor")?,
        currency: row.try_get("currency")?,
        purpose: match row.try_get::<String, _>("purpose")?.as_str() {
            "order" => MerchantPaymentPurpose::Order,
            "legacyDeposit" => MerchantPaymentPurpose::LegacyDeposit,
            _ => return Err(invalid("invalid stored merchant payment purpose")),
        },
        hyperswitch_body_id: row.try_get("hyperswitch_body_id")?,
        provider_payment_id: row.try_get("provider_payment_id")?,
    })
}
// The received normalization remains immutable. Trusted coordinates are a
// derived read over immutable payment bindings, never rewritten event data.
const EVENT_SELECT: &str = "SELECT e.*,b.payment_id AS correlated_payment_id,b.organization_id AS correlated_organization_id FROM oxsum.merchant_event_inbox e LEFT JOIN oxsum.merchant_payment_refs r ON r.merchant_id=e.merchant_id AND r.provider_payment_id=e.normalized->>'providerPaymentId' LEFT JOIN oxsum.merchant_payment_bindings b ON b.payment_id=r.payment_id AND b.merchant_id=e.merchant_id AND b.version_id=e.version_id";

fn event_data(row: &PgRow) -> Result<VerifiedMerchantEvent, WalletError> {
    let mut event: VerifiedMerchantEvent = serde_json::from_value(row.try_get("normalized")?)
        .map_err(|_| invalid("invalid stored merchant event"))?;
    let payment: Option<Uuid> = row.try_get("correlated_payment_id")?;
    let organization: Option<Uuid> = row.try_get("correlated_organization_id")?;
    if payment.is_some()
        && event.payment_id.is_none_or(|id| Some(id) == payment)
        && event
            .organization_id
            .is_none_or(|id| Some(id) == organization)
    {
        event.payment_id = payment;
        event.organization_id = organization;
    }
    Ok(event)
}

fn receipt(row: &PgRow) -> Result<MerchantEventReceipt, WalletError> {
    let event = event_data(row)?;
    Ok(MerchantEventReceipt {
        id: row.try_get("receipt_id")?,
        merchant_id: row.try_get("merchant_id")?,
        version_id: row.try_get("version_id")?,
        event_id: event.event_id,
        kind: event.kind,
        payment_id: event.payment_id,
        state: row.try_get("state")?,
        reason: row.try_get("reason")?,
    })
}
async fn payment_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<MerchantPaymentBinding>, WalletError> {
    let row=sqlx::query("SELECT b.*,r.provider_payment_id FROM oxsum.merchant_payment_bindings b LEFT JOIN oxsum.merchant_payment_refs r USING(payment_id) WHERE b.payment_id=$1 FOR UPDATE OF b").bind(id).fetch_optional(&mut **tx).await?;
    row.as_ref().map(binding).transpose()
}
async fn reference_in(
    tx: &mut Transaction<'_, Postgres>,
    payment: &MerchantPaymentBinding,
    provider_id: &str,
) -> Result<(), WalletError> {
    if !identifier(provider_id) {
        return Err(invalid("invalid provider payment reference"));
    }
    if payment
        .provider_payment_id
        .as_deref()
        .is_some_and(|existing| existing != provider_id)
    {
        return Err(WalletError::Conflict(
            "provider payment reference changed".into(),
        ));
    }
    let inserted=sqlx::query("INSERT INTO oxsum.merchant_payment_refs(payment_id,merchant_id,provider_payment_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(payment.payment_id).bind(payment.merchant_id).bind(provider_id).execute(&mut **tx).await?.rows_affected();
    if inserted == 0 {
        let matches:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_payment_refs WHERE payment_id=$1 AND merchant_id=$2 AND provider_payment_id=$3)").bind(payment.payment_id).bind(payment.merchant_id).bind(provider_id).fetch_one(&mut **tx).await?;
        if !matches {
            return Err(WalletError::Conflict(
                "provider payment already belongs to another payment".into(),
            ));
        }
    }
    Ok(())
}
async fn classify(
    tx: &mut Transaction<'_, Postgres>,
    merchant: Uuid,
    version: Uuid,
    event: &mut VerifiedMerchantEvent,
) -> Result<(&'static str, Option<&'static str>), WalletError> {
    if event.kind == MerchantEventKind::Unsupported {
        return Ok(("ignored", Some("unsupportedEvent")));
    }
    let identity = sqlx::query(
        "SELECT rail,mode,account_id FROM oxsum.merchant_accounts WHERE merchant_id=$1",
    )
    .bind(merchant)
    .fetch_one(&mut **tx)
    .await?;
    let account: String = identity.try_get("account_id")?;
    let mode: String = identity.try_get("mode")?;
    if event
        .provider_account_id
        .as_deref()
        .is_some_and(|v| v != account)
    {
        return Ok(("rejected", Some("merchantMismatch")));
    }
    if event.mode.is_some_and(|v| v.as_str() != mode) {
        return Ok(("rejected", Some("modeMismatch")));
    }
    // A switch's refund object need not copy the payment's metadata. Resolve
    // only missing coordinates from our already-bound reference, within the
    // signed route's merchant/version. Never repair a supplied contradiction.
    if (event.payment_id.is_none() || event.organization_id.is_none())
        && let Some(provider) = event.provider_payment_id.as_deref()
    {
        let row = sqlx::query("SELECT b.*,r.provider_payment_id FROM oxsum.merchant_payment_refs r JOIN oxsum.merchant_payment_bindings b USING(payment_id) WHERE r.merchant_id=$1 AND r.provider_payment_id=$2")
            .bind(merchant).bind(provider).fetch_optional(&mut **tx).await?;
        if let Some(row) = row {
            let bound = binding(&row)?;
            if bound.version_id != version {
                return Ok(("rejected", Some("versionMismatch")));
            }
            if event.payment_id.is_some_and(|id| id != bound.payment_id) {
                return Ok(("rejected", Some("providerPaymentMismatch")));
            }
            if event
                .organization_id
                .is_some_and(|id| id != bound.organization_id)
            {
                return Ok(("rejected", Some("customerMismatch")));
            }
            event.payment_id = Some(bound.payment_id);
            event.organization_id = Some(bound.organization_id);
        } else {
            return Ok(("pending", Some("paymentReferencePending")));
        }
    }
    let Some(id) = event.payment_id else {
        return Ok(("rejected", Some("missingLocalPayment")));
    };
    let Some(payment) = payment_in(tx, id).await? else {
        return Ok(("pending", Some("unknownPayment")));
    };
    if payment.merchant_id != merchant {
        return Ok(("rejected", Some("merchantMismatch")));
    }
    if payment.version_id != version {
        return Ok(("rejected", Some("versionMismatch")));
    }
    if event.organization_id != Some(payment.organization_id) {
        return Ok(("rejected", Some("customerMismatch")));
    }
    let Some(provider_id) = event.provider_payment_id.as_deref() else {
        return Ok(("rejected", Some("missingProviderPayment")));
    };
    if payment
        .provider_payment_id
        .as_deref()
        .is_some_and(|p| p != provider_id)
    {
        return Ok(("rejected", Some("providerPaymentMismatch")));
    }
    if event.kind == MerchantEventKind::Succeeded {
        if event.amount_minor != Some(payment.amount_minor) {
            return Ok(("rejected", Some("amountMismatch")));
        }
        if event.currency.as_deref() != Some(payment.currency.as_str()) {
            return Ok(("rejected", Some("currencyMismatch")));
        }
    }
    // Refunds/disputes remain authenticated intake for T17's own expected terms.
    // They cannot establish a new payment reference or invoke deposit credit.
    if matches!(
        event.kind,
        MerchantEventKind::RefundSucceeded
            | MerchantEventKind::RefundFailed
            | MerchantEventKind::Dispute
    ) {
        if payment.provider_payment_id.is_none() {
            return Ok(("pending", Some("paymentReferencePending")));
        }
    } else if let Err(error) = reference_in(tx, &payment, provider_id).await {
        if matches!(error, WalletError::Conflict(_)) {
            return Ok(("rejected", Some("providerPaymentMismatch")));
        }
        return Err(error);
    }
    Ok(("accepted", None))
}
impl Db {
    /// Trusted order/deposit service: immutable expected terms commit before I/O.
    /// Existing identical bindings can be recovered after disablement or rotation.
    pub async fn pin_merchant_payment(
        &self,
        payment: &MerchantPaymentBinding,
    ) -> Result<(), WalletError> {
        if payment.amount_minor <= 0
            || payment.currency.len() != 3
            || !payment.currency.bytes().all(|c| c.is_ascii_uppercase())
            || payment.provider_payment_id.is_some()
        {
            return Err(invalid("invalid expected payment terms"));
        }
        let mut tx = self.pool().begin().await?;
        // Same local identity serializes even before its binding row exists.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("merchant-payment:{}", payment.payment_id))
            .execute(&mut *tx)
            .await?;
        if let Some(existing) = payment_in(&mut tx, payment.payment_id).await? {
            if existing.merchant_id != payment.merchant_id
                || existing.version_id != payment.version_id
                || existing.organization_id != payment.organization_id
                || existing.amount_minor != payment.amount_minor
                || existing.currency != payment.currency
                || existing.purpose != payment.purpose
                || existing.hyperswitch_body_id != payment.hyperswitch_body_id
            {
                return Err(WalletError::Conflict(
                    "expected payment terms changed".into(),
                ));
            }
            return Ok(());
        }
        let allowed=sqlx::query("SELECT m.merchant_id FROM oxsum.merchant_accounts m JOIN oxsum.customer_profiles c ON c.business_id=m.business_id WHERE m.merchant_id=$1 AND m.current_version_id=$2 AND m.enabled AND c.organization_id=$3 FOR SHARE OF m").bind(payment.merchant_id).bind(payment.version_id).bind(payment.organization_id).fetch_optional(&mut *tx).await?;
        if allowed.is_none() {
            return Err(WalletError::Forbidden(
                "merchant is not enabled for this customer's new payments".into(),
            ));
        }
        sqlx::query("INSERT INTO oxsum.merchant_payment_bindings(payment_id,merchant_id,version_id,organization_id,amount_minor,currency,purpose,hyperswitch_body_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(payment.payment_id).bind(payment.merchant_id).bind(payment.version_id).bind(payment.organization_id).bind(payment.amount_minor).bind(&payment.currency).bind(payment.purpose.as_str()).bind(payment.hyperswitch_body_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Loads the pinned terms for a local payment, or `None` if never pinned.
    pub async fn merchant_payment_binding(
        &self,
        id: Uuid,
    ) -> Result<Option<MerchantPaymentBinding>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let found = payment_in(&mut tx, id).await?;
        tx.commit().await?;
        Ok(found)
    }
    /// Looks up a pinned payment by the merchant and provider payment reference.
    pub async fn merchant_payment_by_provider(
        &self,
        merchant: Uuid,
        provider: &str,
    ) -> Result<Option<MerchantPaymentBinding>, WalletError> {
        let row=sqlx::query("SELECT b.*,r.provider_payment_id FROM oxsum.merchant_payment_refs r JOIN oxsum.merchant_payment_bindings b USING(payment_id) WHERE r.merchant_id=$1 AND r.provider_payment_id=$2").bind(merchant).bind(provider).fetch_optional(self.pool()).await?;
        row.as_ref().map(binding).transpose()
    }
    /// Records the provider payment reference for an already pinned payment.
    /// A changed or already-claimed reference is a conflict.
    pub async fn record_merchant_payment_reference(
        &self,
        id: Uuid,
        provider: &str,
    ) -> Result<(), WalletError> {
        let mut tx = self.pool().begin().await?;
        let payment = payment_in(&mut tx, id)
            .await?
            .ok_or_else(|| WalletError::NotFound("unknown expected payment".into()))?;
        reference_in(&mut tx, &payment, provider).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Only verified raw bytes may enter this trusted API. Store a raw digest
    /// and normalized fields, never a payload containing customer/secret data.
    pub async fn receive_merchant_event(
        &self,
        context: &MerchantSigningContext,
        event: &VerifiedMerchantEvent,
        payload_digest: &[u8; 32],
    ) -> Result<MerchantEventReceipt, WalletError> {
        if !identifier(&event.event_id)
            || event
                .provider_payment_id
                .as_deref()
                .is_some_and(|v| !identifier(v))
            || event.refund_id.as_deref().is_some_and(|v| !identifier(v))
        {
            return Err(invalid("invalid merchant event identifiers"));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "merchant-event:{}:{}",
                context.merchant_id, event.event_id
            ))
            .execute(&mut *tx)
            .await?;
        let existing = sqlx::query(&format!(
            "{EVENT_SELECT} WHERE e.merchant_id=$1 AND e.event_id=$2 FOR UPDATE OF e"
        ))
        .bind(context.merchant_id)
        .bind(&event.event_id)
        .fetch_optional(&mut *tx)
        .await?;
        let id = if let Some(row) = existing {
            let digest: Vec<u8> = row.try_get("payload_digest")?;
            if digest != payload_digest {
                return Err(WalletError::Conflict(
                    "merchant event ID was reused with different content".into(),
                ));
            }
            let existing = receipt(&row)?;
            if existing.state != "pending" {
                return Ok(existing);
            }
            existing.id
        } else {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO oxsum.merchant_event_inbox(receipt_id,merchant_id,version_id,event_id,payload_digest,normalized) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(context.merchant_id).bind(context.version.id).bind(&event.event_id).bind(payload_digest.as_slice()).bind(json!(event)).execute(&mut *tx).await?;
            id
        };
        let stored = sqlx::query(
            "SELECT version_id,normalized FROM oxsum.merchant_event_inbox WHERE receipt_id=$1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let mut original: VerifiedMerchantEvent =
            serde_json::from_value(stored.try_get("normalized")?)
                .map_err(|_| invalid("invalid stored merchant event"))?;
        let (state, reason) = classify(
            &mut tx,
            context.merchant_id,
            stored.try_get("version_id")?,
            &mut original,
        )
        .await?;
        sqlx::query("UPDATE oxsum.merchant_event_inbox SET state=$2,reason=$3,updated_at=now() WHERE receipt_id=$1").bind(id).bind(state).bind(reason).execute(&mut *tx).await?;
        let row = sqlx::query(&format!("{EVENT_SELECT} WHERE e.receipt_id=$1"))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let result = receipt(&row)?;
        tx.commit().await?;
        Ok(result)
    }
    /// Re-evaluates pending verified data without requiring provider redelivery.
    pub async fn retry_merchant_event(
        &self,
        id: Uuid,
    ) -> Result<MerchantEventReceipt, WalletError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(&format!(
            "{EVENT_SELECT} WHERE e.receipt_id=$1 FOR UPDATE OF e"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("unknown merchant event".into()))?;
        let result = receipt(&row)?;
        if result.state != "pending" {
            return Ok(result);
        }
        let mut event: VerifiedMerchantEvent =
            serde_json::from_value(row.try_get("normalized")?)
                .map_err(|_| invalid("invalid stored merchant event"))?;
        let (state, reason) =
            classify(&mut tx, result.merchant_id, result.version_id, &mut event).await?;
        sqlx::query("UPDATE oxsum.merchant_event_inbox SET state=$2,reason=$3,updated_at=now() WHERE receipt_id=$1").bind(id).bind(state).bind(reason).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(MerchantEventReceipt {
            state: state.into(),
            reason: reason.map(str::to_owned),
            payment_id: event.payment_id,
            ..result
        })
    }
    /// Marks an accepted event applied once its effects are durable. Events in
    /// any other state are left untouched.
    pub async fn mark_merchant_event_applied(&self, id: Uuid) -> Result<(), WalletError> {
        sqlx::query("UPDATE oxsum.merchant_event_inbox SET state='applied',updated_at=now() WHERE receipt_id=$1 AND state='accepted'").bind(id).execute(self.pool()).await?;
        Ok(())
    }
    /// Recovery selects retained pending/accepted work, least recently tried
    /// first. Retrying updates `updated_at`, so unresolved events cannot
    /// permanently starve later customers behind the bounded page.
    pub async fn recoverable_merchant_events(
        &self,
        limit: i64,
    ) -> Result<Vec<MerchantEventReceipt>, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let rows = sqlx::query(&format!(
            "{EVENT_SELECT} WHERE e.state IN ('pending','accepted') ORDER BY e.updated_at,e.receipt_id LIMIT $1"
        ))
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(receipt).collect()
    }
    /// Returns the verified event with missing coordinates derived from its
    /// immutable merchant/version binding, or `None` for an unknown receipt.
    /// The original normalized event and payload digest remain unchanged.
    pub async fn merchant_event_data(
        &self,
        id: Uuid,
    ) -> Result<Option<VerifiedMerchantEvent>, WalletError> {
        let row = sqlx::query(&format!("{EVENT_SELECT} WHERE e.receipt_id=$1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(event_data).transpose()
    }
    /// Lists a merchant's event inbox for business reports. Requires the
    /// reports-read capability; `limit` must be between 1 and 100.
    pub async fn business_merchant_events(
        &self,
        token: &str,
        merchant: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<MerchantEventPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("limit must be 1..100"));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::ReportsRead)
            .await?;
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.merchant_accounts WHERE business_id=$1 AND merchant_id=$2)").bind(business).bind(merchant).fetch_one(&mut *tx).await?;
        if !exists {
            return Err(WalletError::NotFound("unknown merchant".into()));
        }
        let rows=sqlx::query(&format!("{EVENT_SELECT} WHERE e.merchant_id=$1 AND ($2::uuid IS NULL OR e.receipt_id>$2) ORDER BY e.receipt_id LIMIT $3")).bind(merchant).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut events = rows.iter().map(receipt).collect::<Result<Vec<_>, _>>()?;
        let more = events.len() > limit as usize;
        events.truncate(limit as usize);
        let next_cursor = if more {
            events.last().map(|e| e.id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(MerchantEventPage {
            events,
            next_cursor,
        })
    }
}

impl Db {
    /// The legacy adapter's local target; provider IDs are deliberately not
    /// consulted here, so configured merchant events cannot cross businesses.
    pub async fn legacy_payment_target(
        &self,
        payment_id: Uuid,
    ) -> Result<Option<(Uuid, String, String)>, WalletError> {
        let row=sqlx::query("SELECT d.organization_id,o.tenant_id,d.rail FROM oxsum.deposits d JOIN oxsum.organizations o USING(organization_id) WHERE d.deposit_id=$1").bind(payment_id).fetch_optional(self.pool()).await?;
        row.map(|r| {
            Ok((
                r.try_get("organization_id")?,
                r.try_get("tenant_id")?,
                r.try_get("rail")?,
            ))
        })
        .transpose()
    }
}

impl Db {
    /// Apply only a validated legacy deposit event. T15 purpose-specific order
    /// fulfillment deliberately never enters this adapter.
    pub async fn apply_legacy_merchant_event(
        &self,
        wallet: &crate::Wallet,
        receipt: &MerchantEventReceipt,
        on: time::Date,
    ) -> Result<(), WalletError> {
        if receipt.state != "accepted" || receipt.kind != MerchantEventKind::Succeeded {
            return Ok(());
        }
        let Some(payment_id) = receipt.payment_id else {
            return Ok(());
        };
        let Some(payment) = self.merchant_payment_binding(payment_id).await? else {
            return Ok(());
        };
        if payment.purpose != MerchantPaymentPurpose::LegacyDeposit {
            return Ok(());
        }
        let Some(event) = self.merchant_event_data(receipt.id).await? else {
            return Ok(());
        };
        if event.payment_id != Some(payment_id)
            || event.amount_minor != Some(payment.amount_minor)
            || event.currency.as_deref() != Some(payment.currency.as_str())
        {
            return Err(invalid(
                "merchant event does not match expected deposit terms",
            ));
        }
        let Some(provider) = event.provider_payment_id.as_deref() else {
            return Err(invalid("missing merchant payment reference"));
        };
        let Some((organization, _, rail)) = self.legacy_payment_target(payment_id).await? else {
            return Ok(());
        };
        if organization != payment.organization_id || payment.currency != "USD" {
            return Err(invalid("merchant payment does not match legacy deposit"));
        }
        let expected_rail: String =
            sqlx::query_scalar("SELECT rail FROM oxsum.merchant_accounts WHERE merchant_id=$1")
                .bind(payment.merchant_id)
                .fetch_one(self.pool())
                .await?;
        if rail != expected_rail {
            return Err(invalid("merchant rail does not match deposit"));
        }
        // Confirmed is retryable after a crash; authoritative paid evidence can
        // also follow an earlier expiration notification. The credit operation
        // owns the row lock and stable merchant-scoped ledger key.
        sqlx::query("UPDATE oxsum.deposits SET status='confirmed',updated_at=now() WHERE deposit_id=$1 AND status IN ('pending','expired')").bind(payment_id).execute(self.pool()).await?;
        match rail.as_str() {
            "hyperswitch" => {
                self.credit_hyperswitch_deposit(
                    wallet,
                    payment_id,
                    provider,
                    Some(payment.amount_minor),
                    on,
                )
                .await?;
            }
            "stripe" => {
                self.credit_stripe_deposit(
                    wallet,
                    payment_id,
                    provider,
                    Some(payment.amount_minor),
                    on,
                )
                .await?;
            }
            "airwallex" => {
                self.credit_airwallex_deposit(
                    wallet,
                    payment_id,
                    provider,
                    Some(payment.amount_minor),
                    on,
                )
                .await?;
            }
            _ => return Err(invalid("unsupported merchant deposit rail")),
        }
        self.reconcile_statements(organization, wallet, on).await?;
        let credited:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.deposits WHERE deposit_id=$1 AND status IN ('credited','reversed'))").bind(payment_id).fetch_one(self.pool()).await?;
        if credited {
            self.mark_merchant_event_applied(receipt.id).await?;
        }
        Ok(())
    }
}

/// Compatibility checkout snapshot. Browser handles remain customer-scoped;
/// callers must never serialize this into business settings or event history.
pub struct LegacyMerchantCheckout {
    /// Pinned expected terms for the deposit payment.
    pub payment: MerchantPaymentBinding,
    /// Provider client secret for completing checkout, when the rail uses one.
    pub client_secret: Option<String>,
    /// Hosted checkout URL, when the rail uses one.
    pub checkout_url: Option<String>,
}
impl Db {
    /// Starts or resumes a legacy deposit checkout. Repeating the same
    /// organization, rail and key returns the existing checkout rather than
    /// creating another payment.
    pub async fn begin_merchant_deposit(
        &self,
        context: &MerchantSigningContext,
        organization: Uuid,
        key: &str,
        amount: i64,
    ) -> Result<LegacyMerchantCheckout, WalletError> {
        authority::validate(key, None)?;
        if amount <= 0 {
            return Err(invalid("amountMinor must be positive"));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "merchant-deposit:{organization}:{}:{key}",
                context.rail.as_str()
            ))
            .execute(&mut *tx)
            .await?;
        let row=sqlx::query("SELECT deposit_id,amount_minor,meta FROM oxsum.deposits WHERE organization_id=$1 AND rail=$2 AND payment_ref=$3 FOR UPDATE").bind(organization).bind(context.rail.as_str()).bind(key).fetch_optional(&mut *tx).await?;
        if let Some(row) = row {
            if row.try_get::<i64, _>("amount_minor")? != amount {
                return Err(WalletError::Conflict(
                    "checkout amount changed for the same idempotency key".into(),
                ));
            }
            let payment = payment_in(&mut tx, row.try_get("deposit_id")?)
                .await?
                .ok_or_else(|| {
                    WalletError::Conflict(
                        "historical checkout has no merchant binding; reconcile it before retrying"
                            .into(),
                    )
                })?;
            let meta: serde_json::Value = row.try_get("meta")?;
            let (secret, url) = match context.rail {
                crate::MerchantRail::Hyperswitch => ("hyperswitchClientSecret", "unused"),
                crate::MerchantRail::Stripe => ("unused", "stripeCheckoutUrl"),
                crate::MerchantRail::Airwallex => ("airwallexClientSecret", "airwallexCheckoutUrl"),
            };
            let result = LegacyMerchantCheckout {
                payment,
                client_secret: meta[secret].as_str().map(str::to_owned),
                checkout_url: meta[url].as_str().map(str::to_owned),
            };
            tx.commit().await?;
            return Ok(result);
        }
        let allowed=sqlx::query("SELECT m.merchant_id FROM oxsum.merchant_accounts m JOIN oxsum.customer_profiles c ON c.business_id=m.business_id WHERE m.merchant_id=$1 AND m.current_version_id=$2 AND m.enabled AND c.organization_id=$3 FOR SHARE OF m").bind(context.merchant_id).bind(context.version.id).bind(organization).fetch_optional(&mut *tx).await?;
        if allowed.is_none() {
            return Err(WalletError::Forbidden(
                "merchant is not enabled for this customer's new payments".into(),
            ));
        }
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.deposits(deposit_id,organization_id,rail,payment_ref,amount_minor,status) VALUES($1,$2,$3,$4,$5,'pending')").bind(id).bind(organization).bind(context.rail.as_str()).bind(key).bind(amount).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO oxsum.merchant_payment_bindings(payment_id,merchant_id,version_id,organization_id,amount_minor,currency,purpose,hyperswitch_body_id) VALUES($1,$2,$3,$4,$5,'USD','legacyDeposit',true)").bind(id).bind(context.merchant_id).bind(context.version.id).bind(organization).bind(amount).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(LegacyMerchantCheckout {
            payment: MerchantPaymentBinding {
                payment_id: id,
                merchant_id: context.merchant_id,
                version_id: context.version.id,
                organization_id: organization,
                amount_minor: amount,
                currency: "USD".into(),
                purpose: MerchantPaymentPurpose::LegacyDeposit,
                hyperswitch_body_id: true,
                provider_payment_id: None,
            },
            client_secret: None,
            checkout_url: None,
        })
    }
}

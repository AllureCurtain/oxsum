//! Durable recovery of external refunds for locally reversed legacy deposits.
use crate::{Db, MerchantEventKind, MerchantEventReceipt, VerifiedMerchantEvent, WalletError};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Pinned external operation; absent merchant coordinates fence old unbound data.
#[derive(Debug, Clone)]
pub struct DepositRefund {
    /// Reversed deposit owning this one refund.
    pub deposit_id: Uuid,
    /// Immutable local request identity, used in new Hyperswitch bodies.
    pub request_id: Uuid,
    /// Customer that owned the deposit.
    pub organization_id: Uuid,
    /// Merchant pinned by the original payment, when present.
    pub merchant_id: Option<Uuid>,
    /// Original immutable credential version, when present.
    pub version_id: Option<Uuid>,
    /// Provider payment pinned by the original checkout.
    pub provider_payment_id: Option<String>,
    /// Exact provider minor units, separate from the wallet's ledger amount.
    pub amount_minor: i64,
    /// Provider currency pinned by the original payment.
    pub currency: String,
    /// New operations can create with their body ID; historical rows cannot.
    pub body_identity: bool,
    /// Known provider refund handle, immutable once observed.
    pub provider_refund_id: Option<String>,
    /// Original local reversal reason, unchanged on retry.
    pub reason: String,
    /// Unknown, acknowledged pending, or authenticated terminal outcome.
    pub state: String,
    /// Number of pre-I/O claims, bounded to five.
    pub attempt_count: i32,
    /// Next due time; absent when fenced, terminal or exhausted.
    pub next_attempt_at: Option<OffsetDateTime>,
}
fn from_row(row: &PgRow) -> Result<DepositRefund, WalletError> {
    Ok(DepositRefund {
        deposit_id: row.try_get("deposit_id")?,
        request_id: row.try_get("request_id")?,
        organization_id: row.try_get("organization_id")?,
        merchant_id: row.try_get("merchant_id")?,
        version_id: row.try_get("version_id")?,
        provider_payment_id: row.try_get("provider_payment_id")?,
        amount_minor: row.try_get("amount_minor")?,
        currency: row.try_get("currency")?,
        body_identity: row.try_get("body_identity")?,
        provider_refund_id: row.try_get("provider_refund_id")?,
        reason: row.try_get("reason")?,
        state: row.try_get("state")?,
        attempt_count: row.try_get("attempt_count")?,
        next_attempt_at: row.try_get("next_attempt_at")?,
    })
}
impl Db {
    /// Queues inside the local reversal transaction. Replayed historical
    /// reversals can gain a fenced record but never a fresh external identity.
    pub(crate) async fn queue_deposit_refund_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        deposit: Uuid,
        reason: &str,
        new: bool,
    ) -> Result<(), WalletError> {
        sqlx::query("INSERT INTO oxsum.deposit_refunds(deposit_id,request_id,organization_id,merchant_id,version_id,provider_payment_id,amount_minor,currency,body_identity,provider_refund_id,reason,next_attempt_at) \
            SELECT d.deposit_id,$2,d.organization_id,b.merchant_id,b.version_id,r.provider_payment_id,COALESCE(d.received_minor,d.amount_minor),COALESCE(b.currency,'USD'), \
            ($4 AND b.payment_id IS NOT NULL AND r.provider_payment_id IS NOT NULL AND b.amount_minor=COALESCE(d.received_minor,d.amount_minor) AND b.amount_minor>0 AND b.organization_id=d.organization_id), \
            NULLIF(d.meta->>'hyperswitchRefundId',''),$3, \
            CASE WHEN b.payment_id IS NOT NULL AND r.provider_payment_id IS NOT NULL AND b.amount_minor=COALESCE(d.received_minor,d.amount_minor) AND b.amount_minor>0 AND b.organization_id=d.organization_id AND ($4 OR NULLIF(d.meta->>'hyperswitchRefundId','') IS NOT NULL) THEN now() ELSE NULL END \
            FROM oxsum.deposits d LEFT JOIN oxsum.merchant_payment_bindings b ON b.payment_id=d.deposit_id AND b.purpose='legacyDeposit' \
            LEFT JOIN oxsum.merchant_payment_refs r ON r.payment_id=d.deposit_id \
            WHERE d.deposit_id=$1 AND d.rail='hyperswitch' AND d.status='reversed' ON CONFLICT(deposit_id) DO NOTHING")
            .bind(deposit).bind(Uuid::new_v4()).bind(reason).bind(new).execute(&mut **tx).await?;
        Ok(())
    }
    /// Loads the standing operation without exposing credentials.
    pub async fn deposit_refund(
        &self,
        deposit: Uuid,
    ) -> Result<Option<DepositRefund>, WalletError> {
        sqlx::query("SELECT * FROM oxsum.deposit_refunds WHERE deposit_id=$1")
            .bind(deposit)
            .fetch_optional(self.pool())
            .await?
            .as_ref()
            .map(from_row)
            .transpose()
    }
    /// Returns a bounded page; callers must claim before network I/O.
    pub async fn due_deposit_refunds(&self, limit: i64) -> Result<Vec<DepositRefund>, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput("limit must be 1..100".into()));
        }
        sqlx::query("SELECT * FROM oxsum.deposit_refunds WHERE state IN ('unknown','pending') AND next_attempt_at<=now() AND attempt_count<5 ORDER BY next_attempt_at,deposit_id LIMIT $1")
            .bind(limit).fetch_all(self.pool()).await?.iter().map(from_row).collect()
    }
    /// Durably spends one leased attempt before I/O. A crash cannot reset it.
    pub async fn claim_deposit_refund(
        &self,
        deposit: Uuid,
    ) -> Result<Option<DepositRefund>, WalletError> {
        sqlx::query("UPDATE oxsum.deposit_refunds SET attempt_count=attempt_count+1,next_attempt_at=CASE WHEN attempt_count+1<5 THEN now()+((ARRAY[60,120,300,900])[attempt_count+1]*interval '1 second') ELSE NULL END,updated_at=now() \
            WHERE deposit_id=$1 AND state IN ('unknown','pending') AND next_attempt_at<=now() AND attempt_count<5 RETURNING *")
            .bind(deposit).fetch_optional(self.pool()).await?.as_ref().map(from_row).transpose()
    }
    /// Records a verified matching read/create handle, never an HTTP terminal
    /// status. A callback that won the race retains its terminal outcome.
    pub async fn note_deposit_refund(
        &self,
        deposit: Uuid,
        handle: &str,
    ) -> Result<(), WalletError> {
        if handle.is_empty() || handle.len() > 200 || handle.chars().any(char::is_control) {
            return Err(WalletError::InvalidInput("invalid refund handle".into()));
        }
        let changed = sqlx::query("UPDATE oxsum.deposit_refunds SET provider_refund_id=$2,state=CASE WHEN state IN ('unknown','pending') THEN 'pending' ELSE state END,updated_at=now() \
            WHERE deposit_id=$1 AND (provider_refund_id IS NULL OR provider_refund_id=$2)")
            .bind(deposit).bind(handle).execute(self.pool()).await?;
        if changed.rows_affected() != 1 {
            return Err(WalletError::Conflict(
                "refund handle differs from the durable reversal".into(),
            ));
        }
        Ok(())
    }
    /// Applies exact signed evidence within the payment's original merchant
    /// version. Unknown or mismatched IDs cannot select another refund.
    pub async fn apply_deposit_refund_event(
        &self,
        receipt: &MerchantEventReceipt,
        event: &VerifiedMerchantEvent,
    ) -> Result<(), WalletError> {
        if receipt.state != "accepted" {
            return Ok(());
        }
        let Some(deposit) = receipt.payment_id else {
            return Ok(());
        };
        let mut tx = self.pool().begin().await?;
        let Some(row) =
            sqlx::query("SELECT * FROM oxsum.deposit_refunds WHERE deposit_id=$1 FOR UPDATE")
                .bind(deposit)
                .fetch_optional(&mut *tx)
                .await?
        else {
            return Err(WalletError::Conflict(
                "refund event has no durable reversal; reconcile it".into(),
            ));
        };
        let target = from_row(&row)?;
        let id = event
            .refund_id
            .as_deref()
            .ok_or_else(|| WalletError::Conflict("refund event has no identity".into()))?;
        let expected = target
            .provider_refund_id
            .clone()
            .or_else(|| target.body_identity.then(|| target.request_id.to_string()));
        if expected.as_deref() != Some(id)
            || target.merchant_id != Some(receipt.merchant_id)
            || target.version_id != Some(receipt.version_id)
            || event.payment_id != Some(deposit)
            || event.organization_id != Some(target.organization_id)
            || event.provider_payment_id != target.provider_payment_id
            || event.amount_minor != Some(target.amount_minor)
            || event.currency.as_deref() != Some(target.currency.as_str())
        {
            return Err(WalletError::Conflict(
                "refund event differs from pinned reversal terms".into(),
            ));
        }
        let state = match event.kind {
            MerchantEventKind::RefundSucceeded => "succeeded",
            MerchantEventKind::RefundFailed => "failed",
            _ => return Ok(()),
        };
        sqlx::query("UPDATE oxsum.deposit_refunds SET provider_refund_id=$2,state=$3,next_attempt_at=NULL,updated_at=now() WHERE deposit_id=$1 AND state<>'succeeded'")
            .bind(deposit).bind(id).bind(state).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

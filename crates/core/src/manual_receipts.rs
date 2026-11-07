//! Authorized out-of-band receipts. Persist immutable terms and audit before
//! any ledger effect; retries recover using the intent UUID's stable ledger key.

use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

use crate::audit_action::MANUAL_RECEIPT_AUTHORIZED;
use crate::{Db, Receipt, Tenants, WalletError};

/// The trusted caller, supplied only after deployment authentication. Customer
/// principals have no conversion into this authority. Staff capabilities will
/// extend this type when the business authority surface is introduced.
#[derive(Clone, Copy)]
pub enum FundingAuthority {
    Deployment,
}

/// Recorded money, denominated in existing credit minor units. This is not a
/// currency conversion API; merchant orders pin that conversion separately.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManualReceiptRequest {
    pub idempotency_key: String,
    pub amount_minor: i64,
    pub reference: String,
    pub reason: String,
}

impl Db {
    /// Records a verified payment under trusted authority, preserving its terms
    /// across retries. A crash after authorization is recovered by the identical
    /// request; a crash after the ledger append replays the same entry. Audit is
    /// durable before money moves, including interrupted fulfillment attempts.
    pub async fn manual_receipt(
        &self,
        _authority: FundingAuthority,
        tenants: &Tenants,
        organization_id: Uuid,
        request: &ManualReceiptRequest,
    ) -> Result<Receipt, WalletError> {
        for (name, value, max) in [
            ("idempotencyKey", request.idempotency_key.as_str(), 200),
            ("reference", request.reference.as_str(), 200),
            ("reason", request.reason.as_str(), 1000),
        ] {
            if value.trim().is_empty() || value.chars().count() > max {
                return Err(WalletError::InvalidInput(format!(
                    "{name} must contain 1 to {max} characters"
                )));
            }
        }
        if request.amount_minor <= 0 {
            return Err(WalletError::InvalidInput(
                "amountMinor must be positive".into(),
            ));
        }
        let org = self.organization_by_id(organization_id).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO oxsum.manual_receipts \
             (receipt_id, organization_id, idempotency_key, reference, reason, amount_minor) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(organization_id)
        .bind(&request.idempotency_key)
        .bind(&request.reference)
        .bind(&request.reason)
        .bind(request.amount_minor)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(
            "SELECT receipt_id, reference, reason, amount_minor FROM oxsum.manual_receipts \
             WHERE organization_id = $1 AND idempotency_key = $2",
        )
        .bind(organization_id)
        .bind(&request.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::Conflict("payment reference is already recorded".into()))?;
        if row.try_get::<String, _>("reference")? != request.reference
            || row.try_get::<String, _>("reason")? != request.reason
            || row.try_get::<i64, _>("amount_minor")? != request.amount_minor
        {
            return Err(WalletError::Conflict(
                "manual receipt terms do not match".into(),
            ));
        }
        let id: Uuid = row.try_get("receipt_id")?;
        let key = format!("manual-receipt:{id}");
        sqlx::query(
            "INSERT INTO oxsum.admin_audit (action, target, detail, idempotency_key) \
             VALUES ($4, $1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(organization_id.to_string())
        .bind(serde_json::json!({
            "receiptId": id, "reference": request.reference, "reason": request.reason,
            "amountMinor": request.amount_minor,
        }))
        .bind(&key)
        .bind(MANUAL_RECEIPT_AUTHORIZED)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let wallet = tenants.get(&org.tenant_id).await?;
        let today = time::OffsetDateTime::now_utc().date();
        let receipt = wallet.top_up(&key, request.amount_minor, today).await?;
        self.record_manual_deposit(
            organization_id,
            &key,
            request.amount_minor,
            *receipt.entry_id.as_uuid(),
            wallet.available().await.ok(),
        )
        .await?;
        self.reconcile_statements(organization_id, &wallet, today)
            .await?;
        Ok(receipt)
    }
}

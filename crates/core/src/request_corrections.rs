//! Atomic, source-linked per-request credits under current Business authority.

use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{BusinessCapability, Db, Wallet, WalletError, business_staff as authority, customers};

/// The immutable source and current remaining bound shown before a correction.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestCorrectionPreview {
    /// Original settlement entry.
    pub source_entry_id: Uuid,
    /// Verified original hash.
    pub source_hash: String,
    /// Original gateway request.
    pub request_id: String,
    /// Entire original purchased-credit charge.
    pub charged_minor: i64,
    /// Credits already appended against this source.
    pub corrected_minor: i64,
    /// Maximum remaining credit.
    pub remaining_minor: i64,
    /// Human-readable funding and budget effects.
    pub effect: String,
}

/// Exact reviewed correction intent; changed retries conflict.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestCorrectionInput {
    /// Actor-scoped exact retry identity.
    pub idempotency_key: String,
    /// Original immutable entry.
    pub source_entry_id: Uuid,
    /// Original hash seen in preview.
    pub source_hash: String,
    /// Total credits seen in preview; rejects stale confirmation.
    pub expected_corrected_minor: i64,
    /// Positive purchased-credit amount.
    pub amount_minor: i64,
    /// Staff reason retained with the effect.
    pub reason: String,
}

/// A retained correction, separate from the original immutable settlement.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestCorrection {
    /// Immutable mutation identity.
    pub id: Uuid,
    /// Original entry.
    pub source_entry_id: Uuid,
    /// Verified original hash.
    pub source_hash: String,
    /// Appended credit entry.
    pub entry_id: Uuid,
    /// Appended entry hash.
    pub content_hash: String,
    /// Purchased credit restored.
    pub amount_minor: i64,
    /// Staff reason.
    pub reason: String,
    /// Authenticated staff user, not the original consumption key.
    pub actor_id: Uuid,
    /// Creation timestamp.
    pub created_at: String,
}

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
    organization: Uuid,
) -> Result<(Uuid, Uuid, String), WalletError> {
    let business = authority::lock_business(tx).await?;
    let actor =
        authority::require_capability(tx, business, token, BusinessCapability::FundsWrite).await?;
    customers::customer(tx, business, organization).await?;
    let tenant =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(organization)
            .fetch_one(&mut **tx)
            .await?;
    Ok((business, actor.user_id, tenant))
}

async fn correction_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<RequestCorrection, WalletError> {
    let row = sqlx::query("SELECT * FROM oxsum.request_corrections WHERE correction_id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(RequestCorrection {
        id,
        source_entry_id: row.try_get("source_entry_id")?,
        source_hash: row.try_get("source_hash")?,
        entry_id: row.try_get("entry_id")?,
        content_hash: row.try_get("content_hash")?,
        amount_minor: row.try_get("amount_minor")?,
        reason: row.try_get("reason")?,
        actor_id: row.try_get("actor_id")?,
        created_at: row.try_get::<OffsetDateTime, _>("created_at")?.to_string(),
    })
}

impl Db {
    async fn correction_wallet(
        &self,
        token: &str,
        organization: Uuid,
    ) -> Result<Wallet, WalletError> {
        let mut tx = self.pool().begin().await?;
        let (_, _, tenant) = authorize(&mut tx, token, organization).await?;
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("ledger_{tenant}.entries"))
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Err(WalletError::NotFound("source charge not found".into()));
        }
        tx.commit().await?;
        Wallet::open_existing(self.pool().clone(), &tenant).await
    }

    /// Preview source funding and remaining correction capacity without writing.
    pub async fn preview_request_correction(
        &self,
        token: &str,
        organization: Uuid,
        source: Uuid,
    ) -> Result<RequestCorrectionPreview, WalletError> {
        let wallet = self.correction_wallet(token, organization).await?;
        let mut tx = self.pool().begin().await?;
        authorize(&mut tx, token, organization).await?;
        wallet.lock_append_in(&mut tx).await?;
        wallet
            .correction_source_in(&mut tx, organization, source)
            .await
    }

    /// Append one bounded credit, retained source link and audit atomically.
    pub async fn apply_request_correction(
        &self,
        token: &str,
        organization: Uuid,
        request: &RequestCorrectionInput,
    ) -> Result<RequestCorrection, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        if !(1..=9_007_199_254_740_991).contains(&request.amount_minor)
            || request.expected_corrected_minor < 0
        {
            return Err(WalletError::InvalidInput(
                "amount must be a positive exact integer and expected total nonnegative".into(),
            ));
        }
        let wallet = self.correction_wallet(token, organization).await?;
        let detail = serde_json::json!({"organizationId":organization,"request":request});
        let mut tx = self.pool().begin().await?;
        let (business, actor, _) = authorize(&mut tx, token, organization).await?;
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor,
            &request.idempotency_key,
            "business.request.correct",
            &detail,
        )
        .await?
        {
            return correction_in(&mut tx, id).await;
        }
        wallet.lock_append_in(&mut tx).await?;
        let source = wallet
            .correction_source_in(&mut tx, organization, request.source_entry_id)
            .await?;
        if source.source_hash != request.source_hash
            || source.corrected_minor != request.expected_corrected_minor
            || request.amount_minor > source.remaining_minor
        {
            return Err(WalletError::Conflict(
                "source or correction balance changed; preview the charge again".into(),
            ));
        }
        let mutation = authority::record(
            &mut tx,
            business,
            actor,
            &request.idempotency_key,
            "business.request.correct",
            &detail,
        )
        .await?;
        let booked_on = OffsetDateTime::now_utc().date();
        let receipt = wallet
            .append_request_correction_in(&mut tx, mutation.id, &source, request, actor, booked_on)
            .await?;
        sqlx::query("INSERT INTO oxsum.request_corrections (correction_id,organization_id,source_entry_id,source_hash,entry_id,content_hash,amount_minor,reason,actor_id,booked_on) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(mutation.id).bind(organization).bind(request.source_entry_id).bind(&source.source_hash).bind(receipt.entry_id.as_uuid()).bind(receipt.content_hash.to_hex()).bind(request.amount_minor).bind(&request.reason).bind(actor).bind(booked_on).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO oxsum.finance_audit (audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES ($1,$2,$3,'request.correct',$4,$5,$6)")
            .bind(mutation.id).bind(business).bind(actor).bind(request.source_entry_id.to_string()).bind(&detail).bind(format!("request-correction:{}",mutation.id)).execute(&mut *tx).await?;
        let result = correction_in(&mut tx, mutation.id).await?;
        tx.commit().await?;
        Ok(result)
    }
}

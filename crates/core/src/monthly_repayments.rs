//! Verified staff monthly repayments under current personal Business authority.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    BusinessCapability, Db, Tenants, WalletError, business_staff as authority, customers,
    manual_receipts,
};

/// Exact verified payment intent. The server owns the posting date and FIFO allocation.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessMonthlyPaymentRequest {
    /// Positive exact credit minor units within the JSON safe integer range.
    pub amount_minor: i64,
    /// Globally unique verified bank/payment reference.
    pub reference: String,
    /// Staff justification retained with the original actor audit.
    pub reason: String,
    /// Stable actor-bound identity for exact recovery.
    pub idempotency_key: String,
}

/// Immutable original receipt; current standing is available through document reads.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessMonthlyPaymentResult {
    /// Actor mutation receipt.
    pub id: Uuid,
    /// Customer whose debt was repaid.
    pub organization_id: Uuid,
    /// Selected issued monthly document.
    pub statement_id: Uuid,
    /// Original credit amount, including earlier FIFO debt.
    pub amount_minor: i64,
    /// Verified source reference.
    pub reference: String,
    /// Original credit-only ledger entry.
    pub entry_id: Uuid,
    /// Hash committed by that entry.
    pub content_hash: String,
}

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
    customer: Uuid,
    statement: Uuid,
) -> Result<(Uuid, Uuid, String, String), WalletError> {
    let business = authority::lock_business(tx).await?;
    let actor =
        authority::require_capability(tx, business, token, BusinessCapability::FundsWrite).await?;
    let account = customers::customer(tx, business, customer).await?;
    let tenant: Option<String> = sqlx::query_scalar("SELECT o.tenant_id FROM oxsum.organizations o JOIN oxsum.statements s USING(organization_id) WHERE o.organization_id=$1 AND s.statement_id=$2")
        .bind(customer).bind(statement).fetch_optional(&mut **tx).await?;
    Ok((
        business,
        actor.user_id,
        tenant.ok_or_else(|| WalletError::NotFound("monthly statement not found".into()))?,
        account.state,
    ))
}

async fn result_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<BusinessMonthlyPaymentResult, WalletError> {
    let row = sqlx::query("SELECT * FROM oxsum.monthly_repayments WHERE mutation_id=$1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(BusinessMonthlyPaymentResult {
        id,
        organization_id: row.try_get("organization_id")?,
        statement_id: row.try_get("statement_id")?,
        amount_minor: row.try_get("amount_minor")?,
        reference: row.try_get("reference")?,
        entry_id: row.try_get("entry_id")?,
        content_hash: row.try_get("content_hash")?,
    })
}

impl Db {
    /// Record a verified receipt, credit-only cash and staff audit atomically.
    /// Current authority gates every replay; archival refuses only new payments.
    pub async fn record_business_monthly_payment(
        &self,
        token: &str,
        tenants: &Tenants,
        customer: Uuid,
        statement: Uuid,
        request: &BusinessMonthlyPaymentRequest,
    ) -> Result<BusinessMonthlyPaymentResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        if !(1..=9_007_199_254_740_991).contains(&request.amount_minor) {
            return Err(WalletError::InvalidInput(
                "amountMinor must be a positive safe integer".into(),
            ));
        }
        if request.reference.is_empty()
            || request.reference.chars().count() > 200
            || request.reference.trim() != request.reference
            || request.reference.chars().any(char::is_control)
        {
            return Err(WalletError::InvalidInput("reference must contain 1 to 200 characters without surrounding whitespace or control characters".into()));
        }
        let action = "business.customer.monthly_payment";
        let detail = json!({"organizationId":customer,"statementId":statement,"request":request});
        let mut preflight = self.pool().begin().await?;
        let (business, actor, tenant, state) =
            authorize(&mut preflight, token, customer, statement).await?;
        if let Some(id) = authority::replay(
            &mut preflight,
            business,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return result_in(&mut preflight, id).await;
        }
        if state != "active" {
            return Err(WalletError::Conflict(
                "archived customer monthly payments cannot change".into(),
            ));
        }
        preflight.commit().await?;
        let wallet = tenants.get(&tenant).await?;
        let mut tx = self.pool().begin().await?;
        let (business, actor, _, state) = authorize(&mut tx, token, customer, statement).await?;
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return result_in(&mut tx, id).await;
        }
        if state != "active" {
            return Err(WalletError::Conflict(
                "archived customer monthly payments cannot change".into(),
            ));
        }
        // A fresh unguessable receipt owns its ledger key. Audit/reference failure
        // rolls back both the mutation and the later cash/standing effects.
        let mutation = authority::record(
            &mut tx,
            business,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let (_, receipt) = self
            .apply_monthly_payment_in(
                &mut tx,
                &wallet,
                statement,
                &format!("business-monthly-payment:{}", mutation.id),
                request.amount_minor,
                OffsetDateTime::now_utc().date(),
            )
            .await?;
        sqlx::query("INSERT INTO oxsum.monthly_repayments(mutation_id,organization_id,statement_id,amount_minor,reference,entry_id,content_hash) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(mutation.id).bind(customer).bind(statement).bind(request.amount_minor).bind(&request.reference)
            .bind(receipt.entry_id.as_uuid()).bind(receipt.content_hash.to_hex()).execute(&mut *tx).await
            .map_err(manual_receipts::payment_reference_error)?;
        let result = result_in(&mut tx, mutation.id).await?;
        tx.commit().await?;
        Ok(result)
    }
}

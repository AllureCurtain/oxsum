//! Reviewed monthly draft/issuance writes under current personal Business authority.

use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::{
    BusinessCapability, Db, Statement, StatementLine, Tenants, WalletError,
    business_staff as authority, customers, statements,
};

/// Exact staff intent, with no client-controlled financial dates.
#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BusinessMonthlyStatementAction {
    /// Generate or refresh the working document for a closed UTC month.
    Generate {
        /// Billed month, YYYY-MM.
        period: String,
        /// Stable identity for an exact retry.
        idempotency_key: String,
        /// Human reason retained with the actor audit.
        reason: String,
    },
    /// Issue only the exact figures and terms reviewed in the current UTC day.
    Finalize {
        /// Customer-bound draft identity.
        statement_id: Uuid,
        /// Latest draft detail commitment.
        review_digest: String,
        /// Stable identity for an exact retry.
        idempotency_key: String,
        /// Human reason retained with the actor audit.
        reason: String,
    },
}

/// Immutable outcome coordinates; later changes never alter a retry's answer.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessMonthlyStatementActionResult {
    /// Actor mutation receipt, linked to the immutable audit.
    pub id: Uuid,
    /// Original resulting document, absent for an empty unbilled month.
    pub statement_id: Option<Uuid>,
}

pub(crate) fn review_digest(
    statement: &Statement,
    lines: &[StatementLine],
    on: Date,
) -> Result<String, WalletError> {
    let bytes = serde_json::to_vec(&json!({
        "v":1,"statementId":statement.id,"organizationId":statement.organization_id,
        "period":statement.period,"totalMinor":statement.total_minor,
        "creditDrawnMinor":statement.credit_drawn_minor,"entryCount":statement.entry_count,
        "paymentTermsDays":statement.payment_terms_days,"reviewDay":on.to_string(),"lines":lines
    }))
    .map_err(|error| WalletError::InvalidInput(error.to_string()))?;
    Ok(doubleentry::Hash::digest(b"oxsum/monthly-review/v1", &bytes).to_hex())
}

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
    customer: Uuid,
) -> Result<(Uuid, Uuid, String, String), WalletError> {
    let business = authority::lock_business(tx).await?;
    let actor =
        authority::require_capability(tx, business, token, BusinessCapability::FundsWrite).await?;
    let account = customers::customer(tx, business, customer).await?;
    let tenant =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(customer)
            .fetch_one(&mut **tx)
            .await?;
    Ok((business, actor.user_id, tenant, account.state))
}

async fn result_in(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<BusinessMonthlyStatementActionResult, WalletError> {
    let statement_id = sqlx::query_scalar(
        "SELECT statement_id FROM oxsum.monthly_statement_actions WHERE mutation_id=$1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(BusinessMonthlyStatementActionResult { id, statement_id })
}

impl Db {
    /// Generate or issue one reviewed monthly statement with atomic actor audit.
    pub async fn mutate_business_monthly_statement(
        &self,
        token: &str,
        tenants: &Tenants,
        customer: Uuid,
        request: &BusinessMonthlyStatementAction,
    ) -> Result<BusinessMonthlyStatementActionResult, WalletError> {
        let (key, reason) = match request {
            BusinessMonthlyStatementAction::Generate {
                idempotency_key,
                reason,
                ..
            }
            | BusinessMonthlyStatementAction::Finalize {
                idempotency_key,
                reason,
                ..
            } => (idempotency_key, reason),
        };
        authority::validate(key, Some(reason))?;
        match request {
            BusinessMonthlyStatementAction::Generate { period, .. } => {
                statements::statement_period(period)?;
            }
            BusinessMonthlyStatementAction::Finalize { review_digest, .. } => {
                if review_digest.len() != 64
                    || !review_digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(WalletError::InvalidInput(
                        "reviewDigest must be 64 lowercase hexadecimal characters".into(),
                    ));
                }
            }
        }
        let action = "business.customer.monthly_statement";
        let detail = json!({"organizationId":customer,"request":request});
        let mut preflight = self.pool().begin().await?;
        let (business, actor, tenant, state) = authorize(&mut preflight, token, customer).await?;
        if let Some(id) =
            authority::replay(&mut preflight, business, actor, key, action, &detail).await?
        {
            return result_in(&mut preflight, id).await;
        }
        if state != "active" {
            return Err(WalletError::Conflict(
                "archived customer monthly statements cannot change".into(),
            ));
        }
        if let BusinessMonthlyStatementAction::Finalize { statement_id, .. } = request {
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.statements WHERE organization_id=$1 AND statement_id=$2)")
                .bind(customer).bind(statement_id).fetch_one(&mut *preflight).await?;
            if !exists {
                return Err(WalletError::NotFound("monthly statement not found".into()));
            }
        }
        preflight.commit().await?;
        // Wallet initialization cannot retain a business transaction connection;
        // all authority, identity and replay checks run again before any effect.
        let wallet = tenants.get(&tenant).await?;
        let mut tx = self.pool().begin().await?;
        let (business, actor, _, state) = authorize(&mut tx, token, customer).await?;
        if let Some(id) = authority::replay(&mut tx, business, actor, key, action, &detail).await? {
            return result_in(&mut tx, id).await;
        }
        if state != "active" {
            return Err(WalletError::Conflict(
                "archived customer monthly statements cannot change".into(),
            ));
        }
        self.lock_monthly_statement_in(&mut tx, customer, &wallet)
            .await?;
        let document = match request {
            BusinessMonthlyStatementAction::Generate { period, .. } => {
                self.generate_statement_in(
                    &mut tx,
                    customer,
                    &wallet,
                    &statements::statement_period(period)?,
                )
                .await?
            }
            BusinessMonthlyStatementAction::Finalize {
                statement_id,
                review_digest: expected,
                ..
            } => {
                let row=sqlx::query("SELECT status FROM oxsum.statements WHERE statement_id=$1 AND organization_id=$2")
                    .bind(statement_id).bind(customer).fetch_optional(&mut *tx).await?
                    .ok_or_else(||WalletError::NotFound("monthly statement not found".into()))?;
                if row.try_get::<String, _>("status")? != "draft" {
                    return Err(WalletError::Conflict(
                        "monthly statement is already finalized; replay its original action".into(),
                    ));
                }
                let on = OffsetDateTime::now_utc().date();
                let issued = self
                    .finalize_statement_in(&mut tx, *statement_id, &wallet, on)
                    .await?;
                let lines = self.monthly_lines_in(&mut tx, *statement_id).await?;
                if review_digest(&issued, &lines, on)? != *expected {
                    return Err(WalletError::Conflict("monthly statement changed since review; regenerate and review current figures and terms".into()));
                }
                Some(issued)
            }
        };
        // Deliberately after the ledger/document writes: enqueue or audit failure
        // rolls the whole operation back, including the idempotency receipt.
        let mutation = authority::record(&mut tx, business, actor, key, action, &detail).await?;
        let statement_id = document.map(|document| document.id);
        sqlx::query(
            "INSERT INTO oxsum.monthly_statement_actions(mutation_id,statement_id) VALUES($1,$2)",
        )
        .bind(mutation.id)
        .bind(statement_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(BusinessMonthlyStatementActionResult {
            id: mutation.id,
            statement_id,
        })
    }
}

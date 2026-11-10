//! Source-bound monthly repayments: cap, cash, FIFO standings and audit are atomic.

use serde_json::json;
use sqlx::{Postgres, Row, Transaction};
use time::Date;
use uuid::Uuid;

use crate::{
    Db, Receipt, Statement, StatementStatus, Wallet, WalletError, audit_action, statements,
};

impl Db {
    /// Apply a new bounded payment on the caller's financial/audit transaction.
    pub(crate) async fn apply_monthly_payment_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        wallet: &Wallet,
        statement_id: Uuid,
        key: &str,
        minor: i64,
        today: Date,
    ) -> Result<(Statement, Receipt), WalletError> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM oxsum.statements WHERE statement_id=$1",
            statements::STATEMENT_COLS
        ))
        .bind(statement_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("monthly statement not found".into()))?;
        let document = statements::statement_from_row(&row)?;
        self.lock_monthly_organization_in(tx, document.organization_id, wallet)
            .await?;
        wallet.lock_credit_in(tx).await?;
        let row = sqlx::query(&format!(
            "SELECT {} FROM oxsum.statements WHERE statement_id=$1 FOR UPDATE",
            statements::STATEMENT_COLS
        ))
        .bind(statement_id)
        .fetch_one(&mut **tx)
        .await?;
        let document = statements::statement_from_row(&row)?;
        if document.status != StatementStatus::Finalized {
            return Err(WalletError::InvalidInput(
                "a draft statement is not issued yet; finalize it first".into(),
            ));
        }
        let period = statements::statement_period(&document.period)?;
        let drawn = wallet
            .credit_flow_in(tx, "D", None, Some(period.last))
            .await?;
        let repaid = wallet.credit_flow_in(tx, "C", None, None).await?;
        if minor <= 0 || minor > (drawn - repaid).max(0) {
            return Err(WalletError::InvalidInput(
                "the payment exceeds the billed debt outstanding through this statement".into(),
            ));
        }
        let receipt = wallet.repay_credit_in(tx, key, minor, today).await?;
        self.reconcile_statements_in(tx, document.organization_id, wallet, today)
            .await?;
        Ok((document, receipt))
    }

    /// Record verified deployment money against one issued monthly statement.
    ///
    /// The caller must authenticate deployment authority. Repayment covers the
    /// oldest credit draws through the selected month, including unbilled months.
    /// Exact retries replay their original ledger entry after full payment; an
    /// ambiguous historical entry without matching source audit fails closed.
    /// Uses one connection for cap, posting, standings, events and audit.
    pub async fn record_monthly_statement_payment(
        &self,
        wallet: &Wallet,
        statement_id: Uuid,
        key: &str,
        minor: i64,
        today: Date,
    ) -> Result<(Statement, Receipt), WalletError> {
        crate::business_staff::validate(key, None)?;
        if minor <= 0 {
            return Err(WalletError::InvalidInput(
                "amountMinor must be positive".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        // Admin audit keys are global for this action, unlike tenant ledger keys.
        // Claim their namespace first so a cross-tenant retry cannot race its audit.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum/monthly-payment/v1:' || $1,0))",
        )
        .bind(key)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(&format!(
            "SELECT {} FROM oxsum.statements WHERE statement_id=$1",
            statements::STATEMENT_COLS
        ))
        .bind(statement_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("monthly statement not found".into()))?;
        let statement = statements::statement_from_row(&row)?;
        self.lock_monthly_organization_in(&mut tx, statement.organization_id, wallet)
            .await?;
        wallet.lock_credit_in(&mut tx).await?;
        // Lock order is organization -> credit -> append -> statement. Re-read
        // document standing only after waiting for any concurrent issuer.
        let row = sqlx::query(&format!(
            "SELECT {} FROM oxsum.statements WHERE statement_id=$1 FOR UPDATE",
            statements::STATEMENT_COLS
        ))
        .bind(statement_id)
        .fetch_one(&mut *tx)
        .await?;
        let statement = statements::statement_from_row(&row)?;
        let audit = sqlx::query(
            "SELECT target,detail FROM oxsum.admin_audit WHERE action=$1 AND idempotency_key=$2",
        )
        .bind(audit_action::STATEMENT_PAYMENT)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        let original = wallet.repayment_receipt_in(&mut tx, key, minor).await?;
        let receipt = match (audit, original) {
            (Some(audit), Some(receipt)) => {
                let detail: serde_json::Value = audit.try_get("detail")?;
                if audit.try_get::<Option<String>, _>("target")?.as_deref() != Some(&statement_id.to_string())
                    || detail.get("amountMinor") != Some(&json!(minor))
                    || detail.get("entryId") != Some(&json!(receipt.entry_id)) {
                    return Err(WalletError::Conflict("monthly payment source or amount differs".into()));
                }
                receipt
            }
            (None, None) => {
                let (_, receipt) = self.apply_monthly_payment_in(
                    &mut tx, wallet, statement_id, key, minor, today,
                ).await?;
                // No conflict-ignore: an unexpected audit collision must roll back cash.
                sqlx::query("INSERT INTO oxsum.admin_audit(action,target,detail,idempotency_key) VALUES($1,$2,$3,$4)")
                    .bind(audit_action::STATEMENT_PAYMENT).bind(statement_id.to_string())
                    .bind(json!({"amountMinor":minor,"entryId":receipt.entry_id})).bind(key)
                    .execute(&mut *tx).await?;
                receipt
            }
            _ => return Err(WalletError::Conflict("monthly payment entry and source audit do not agree; reconcile the original payment".into())),
        };
        self.reconcile_statements_in(&mut tx, statement.organization_id, wallet, today)
            .await?;
        let row = sqlx::query(&format!(
            "SELECT {} FROM oxsum.statements WHERE statement_id=$1",
            statements::STATEMENT_COLS
        ))
        .bind(statement_id)
        .fetch_one(&mut *tx)
        .await?;
        let statement = statements::statement_from_row(&row)?;
        tx.commit().await?;
        Ok((statement, receipt))
    }
}

//! Bounded staff inspection of monthly statements, without ledger or document writes.

use serde::Serialize;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    BusinessCapability, Db, PaymentStatus, Statement, StatementLine, StatementStatus, WalletError,
    business_staff as authority, customers, statements,
};

/// One bounded page of monthly wallet-credit statements, ordered by immutable ID.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessMonthlyStatementPage {
    /// Original recorded documents, with time-dependent display standing.
    pub statements: Vec<Statement>,
    /// Exclusive cursor; absent when no further records exist in this snapshot.
    pub next_cursor: Option<Uuid>,
}

/// The original monthly statement and its itemized usage lines.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessMonthlyStatementDetail {
    /// Recorded money, issue terms and proof-window identifiers; no proof is minted.
    pub statement: Statement,
    /// Stored itemization, never silently truncated.
    pub lines: Vec<StatementLine>,
}

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
    customer: Uuid,
) -> Result<(), WalletError> {
    let business = authority::lock_business(tx).await?;
    authority::require_capability(tx, business, token, BusinessCapability::ReportsRead).await?;
    customers::customer(tx, business, customer).await?;
    Ok(())
}

fn display_standing(mut statement: Statement) -> Statement {
    // Expose elapsed due dates without persisting a read-triggered transition.
    // Paid/suspended states and all recorded money/timestamps remain unchanged.
    if statement.status == StatementStatus::Finalized
        && statement.payment_status == PaymentStatus::Pending
        && statement.outstanding_minor > 0
        && statement
            .due_date
            .is_some_and(|date| date < OffsetDateTime::now_utc().date())
    {
        statement.payment_status = PaymentStatus::Overdue;
    }
    statement
}

impl Db {
    /// Inspect a customer's recorded monthly statements without initializing a wallet.
    pub async fn business_monthly_statements(
        &self,
        token: &str,
        customer: Uuid,
        period: Option<&str>,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<BusinessMonthlyStatementPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput(
                "limit must be between 1 and 100".into(),
            ));
        }
        if let Some(period) = period {
            statements::statement_period(period)?;
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        authorize(&mut tx, token, customer).await?;
        let rows=sqlx::query(&format!("SELECT {} FROM oxsum.statements WHERE organization_id=$1 AND ($2::text IS NULL OR period=$2) AND ($3::uuid IS NULL OR statement_id>$3) ORDER BY statement_id LIMIT $4",statements::STATEMENT_COLS))
            .bind(customer).bind(period).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut documents = rows
            .iter()
            .map(statements::statement_from_row)
            .map(|row| row.map(display_standing))
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if documents.len() > limit as usize {
            documents.truncate(limit as usize);
            documents.last().map(|row| row.id)
        } else {
            None
        };
        tx.commit().await?;
        // A snapshot that waited behind revocation must not expose old authority.
        let mut current = self.pool().begin().await?;
        authorize(&mut current, token, customer).await?;
        Ok(BusinessMonthlyStatementPage {
            statements: documents,
            next_cursor,
        })
    }

    /// Inspect original lines/anchor identifiers with bounded output and live authority.
    pub async fn business_monthly_statement(
        &self,
        token: &str,
        customer: Uuid,
        statement: Uuid,
    ) -> Result<BusinessMonthlyStatementDetail, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await?;
        authorize(&mut tx, token, customer).await?;
        let row = sqlx::query(&format!(
            "SELECT {} FROM oxsum.statements WHERE organization_id=$1 AND statement_id=$2",
            statements::STATEMENT_COLS
        ))
        .bind(customer)
        .bind(statement)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("monthly statement not found".into()))?;
        let document = display_standing(statements::statement_from_row(&row)?);
        let bytes:i64=sqlx::query_scalar("SELECT COALESCE(SUM(octet_length(row_to_json(l)::text)),0)::bigint FROM (SELECT * FROM oxsum.statement_lines WHERE statement_id=$1 ORDER BY line_no LIMIT 1001) l").bind(statement).fetch_one(&mut *tx).await?;
        if bytes > 2 * 1024 * 1024 {
            return Err(WalletError::InvalidInput(
                "monthly statement exceeds the 2 MiB detail limit".into(),
            ));
        }
        let rows=sqlx::query("SELECT channel,model,turns,input_tokens,output_tokens,cached_tokens,reasoning_tokens,amount_minor FROM oxsum.statement_lines WHERE statement_id=$1 ORDER BY line_no LIMIT 1001").bind(statement).fetch_all(&mut *tx).await?;
        if rows.len() > 1000 {
            return Err(WalletError::InvalidInput(
                "monthly statement exceeds the 1000-line detail limit".into(),
            ));
        }
        let lines = rows
            .iter()
            .map(|row| {
                Ok(StatementLine {
                    channel: row.try_get("channel")?,
                    model: row.try_get("model")?,
                    turns: row.try_get("turns")?,
                    input_tokens: row.try_get("input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    cached_tokens: row.try_get("cached_tokens")?,
                    reasoning_tokens: row.try_get("reasoning_tokens")?,
                    amount_minor: row.try_get("amount_minor")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        tx.commit().await?;
        let mut current = self.pool().begin().await?;
        authorize(&mut current, token, customer).await?;
        Ok(BusinessMonthlyStatementDetail {
            statement: document,
            lines,
        })
    }
}

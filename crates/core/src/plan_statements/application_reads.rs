//! Complete bounded statement snapshots for explicitly authorized applications.
use super::{PlanStatementDetail, PlanStatementPage, STATEMENT_COLS, statement_from_row};
use crate::WalletError;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const MAX_BYTES: i64 = 4 * 1024 * 1024;

fn oversized() -> WalletError {
    WalletError::Conflict(
        "statement export exceeds application bounds; request operator recovery".into(),
    )
}

/// Read an entire bounded page only after measuring its source rows in PostgreSQL.
pub(crate) async fn application_page_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    after: Option<Uuid>,
    limit: i64,
) -> Result<PlanStatementPage, WalletError> {
    let cursor = if let Some(id) = after {
        Some(sqlx::query_as::<_, (time::OffsetDateTime, Uuid)>(
            "SELECT created_at,statement_id FROM oxsum.plan_statements WHERE organization_id=$1 AND statement_id=$2 AND state<>'open'",
        ).bind(organization).bind(id).fetch_optional(&mut **tx).await?
            .ok_or_else(|| WalletError::NotFound("statement cursor not found".into()))?)
    } else {
        None
    };
    let condition = "organization_id=$1 AND state<>'open' AND ($2::timestamptz IS NULL OR (created_at,statement_id)<($2,$3)) ORDER BY created_at DESC,statement_id DESC LIMIT $4";
    // The extra 512 bytes per row cover wire/envelope formatting differences.
    // Measure the lookahead too: no variable-sized source reaches the client first.
    let bytes: i64 = sqlx::query_scalar(&format!(
        "SELECT COALESCE(sum(octet_length(to_jsonb(s)::text)+512),0)::bigint FROM (SELECT {STATEMENT_COLS} FROM oxsum.plan_statements WHERE {condition}) s"
    )).bind(organization).bind(cursor.map(|v|v.0)).bind(cursor.map(|v|v.1)).bind(limit+1).fetch_one(&mut **tx).await?;
    if bytes + 128 > MAX_BYTES {
        return Err(oversized());
    }
    let rows = sqlx::query(&format!(
        "SELECT {STATEMENT_COLS} FROM oxsum.plan_statements WHERE {condition}"
    ))
    .bind(organization)
    .bind(cursor.map(|v| v.0))
    .bind(cursor.map(|v| v.1))
    .bind(limit + 1)
    .fetch_all(&mut **tx)
    .await?;
    let mut statements = rows
        .iter()
        .map(statement_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let more = statements.len() > limit as usize;
    statements.truncate(limit as usize);
    let next_cursor = if more {
        statements.last().map(|s| s.statement_id)
    } else {
        None
    };
    Ok(PlanStatementPage {
        statements,
        next_cursor,
    })
}

/// Read complete header/lines from the caller's repeatable snapshot after bounds.
pub(crate) async fn application_detail_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    statement: Uuid,
) -> Result<PlanStatementDetail, WalletError> {
    let header_bytes: i64 = sqlx::query_scalar(
        "SELECT octet_length(to_jsonb(s)::text)::bigint FROM oxsum.plan_statements s WHERE organization_id=$1 AND statement_id=$2 AND state<>'open'",
    ).bind(organization).bind(statement).fetch_optional(&mut **tx).await?
        .ok_or_else(|| WalletError::NotFound("statement not found".into()))?;
    if header_bytes + 640 > MAX_BYTES {
        return Err(oversized());
    }
    // Count only the first 1001 sources. Never transfer a large line body to Rust
    // before the complete accepted set has a measured, bounded serialized size.
    let (count, bytes): (i64,i64) = sqlx::query_as(
        "SELECT count(*),COALESCE(sum(octet_length(to_jsonb(l)::text)+512),0)::bigint FROM (SELECT * FROM oxsum.plan_statement_lines WHERE statement_id=$1 ORDER BY created_at,line_id LIMIT 1001) l",
    ).bind(statement).fetch_one(&mut **tx).await?;
    if count > 1000 || header_bytes + bytes + 640 > MAX_BYTES {
        return Err(oversized());
    }
    super::statement_snapshot_in(tx, organization, statement)
        .await?
        .ok_or_else(|| WalletError::NotFound("statement not found".into()))
}

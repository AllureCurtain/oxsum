//! Bounded collection using the existing deterministic scope publication engine.
use super::{Collector, EvidenceKind};
use crate::{Db, EvidenceBundleRow, HeadSigningKey, Wallet, WalletError};
use sqlx::Row;
use time::Date;
use uuid::Uuid;
const MAX_BYTES: usize = 4 * 1024 * 1024;
fn oversized() -> WalletError {
    WalletError::Conflict(
        "evidence exceeds bounded application capture; request a smaller scope or operator export"
            .into(),
    )
}
pub(super) fn check_collector(collector: &Collector) -> Result<(), WalletError> {
    if collector.include_upstream_cost
        || collector.manifest.len() > 1000
        || collector.entry_ids.len() > 256
        || serde_json::to_vec(collector)
            .map_err(|_| oversized())?
            .len()
            > MAX_BYTES
    {
        return Err(oversized());
    }
    Ok(())
}
impl Db {
    /// Existing scope publication with hard application capture limits and no costs.
    pub(crate) async fn mint_bounded_application_evidence(
        &self,
        wallet: &Wallet,
        org: Uuid,
        span: Option<(Date, Date)>,
        statement: Option<Uuid>,
        key: Option<&HeadSigningKey>,
    ) -> Result<EvidenceBundleRow, WalletError> {
        let (kind, scope) = match (span, statement) {
            (Some((from, to)), None) => (EvidenceKind::Period, format!("period/{org}/{from}/{to}")),
            (None, Some(id)) => (EvidenceKind::Statement, format!("statement/{org}/{id}")),
            _ => return Err(oversized()),
        };
        let id = *crate::entry_id_for(&format!("evidence-bundle/{scope}")).as_uuid();
        if let Some(existing) = self.evidence_bundle(org, id).await? {
            return Ok(existing);
        }
        // Inspect bytes before transferring a retained capture. Private captures
        // share the same deterministic scope and cannot be silently replaced.
        let frozen=sqlx::query("SELECT CASE WHEN octet_length(collector::text)<=$3 THEN collector END AS collector FROM oxsum.evidence_mint_inputs WHERE scope_key=$1 AND organization_id=$2")
            .bind(&scope).bind(org).bind(MAX_BYTES as i64).fetch_optional(self.pool()).await?;
        if let Some(row) = frozen {
            let value: serde_json::Value = row
                .try_get::<Option<serde_json::Value>, _>("collector")?
                .ok_or_else(oversized)?;
            let mut collector: Collector =
                serde_json::from_value(value).map_err(|_| oversized())?;
            check_collector(&collector)?;
            collector.bounded = true;
            return self
                .commit_bundle(
                    wallet,
                    collector,
                    kind,
                    span.map(|s| s.0),
                    span.map(|s| s.1),
                    statement,
                    key,
                )
                .await;
        }
        let mut collector = Collector::new(org, false);
        collector.bounded = true;
        if let Some((from, to)) = span {
            self.collect_usage_records(&mut collector, wallet.tenant_id(), from, to)
                .await?;
            self.collect_plan_statements(&mut collector, org).await?;
            self.collect_orders_and_adjustments(&mut collector, org)
                .await?;
            self.collect_day_seals(&mut collector, org, wallet.tenant_id(), from, to)
                .await?;
        } else if let Some(id) = statement {
            // collect_one_statement resolves the current header/lines itself in
            // one snapshot; this bounded wrapper avoids an unbounded initial read.
            let detail = self.bounded_evidence_statement(org, id).await?;
            self.collect_one_statement(&mut collector, &detail).await?;
            self.collect_statement_relations(&mut collector, &detail)
                .await?;
            self.collect_day_seals_for_statement(&mut collector, wallet.tenant_id(), &detail)
                .await?;
        }
        check_collector(&collector)?;
        self.commit_bundle(
            wallet,
            collector,
            kind,
            span.map(|s| s.0),
            span.map(|s| s.1),
            statement,
            key,
        )
        .await
    }
    pub(super) async fn bounded_evidence_order(
        &self,
        org: Uuid,
        id: Uuid,
    ) -> Result<
        Option<(
            crate::PaymentOrder,
            Option<crate::PaymentFulfillment>,
            Vec<crate::OrderRefund>,
        )>,
        WalletError,
    > {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        for query in [
            "SELECT octet_length(to_jsonb(r)::text)::bigint FROM oxsum.payment_orders r WHERE r.organization_id=$1 AND r.order_id=$2",
            "SELECT octet_length(to_jsonb(r)::text)::bigint FROM oxsum.payment_fulfillments r JOIN oxsum.payment_orders o USING(order_id) WHERE o.organization_id=$1 AND o.order_id=$2",
            "SELECT (octet_length(to_jsonb(r)::text)+COALESCE(octet_length(e.effect::text),0))::bigint FROM oxsum.refunds r LEFT JOIN oxsum.nonplan_refund_effects e USING(refund_id) WHERE r.organization_id=$1 AND r.order_id=$2 LIMIT 1001",
        ] {
            let sizes: Vec<i64> = sqlx::query_scalar(query)
                .bind(org)
                .bind(id)
                .fetch_all(&mut *tx)
                .await?;
            check_sizes(&sizes)?;
        }
        let row = sqlx::query(
            "SELECT * FROM oxsum.payment_orders WHERE organization_id=$1 AND order_id=$2",
        )
        .bind(org)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else { return Ok(None) };
        let order = crate::payment_orders::order(&row)?;
        let fulfilled = sqlx::query("SELECT * FROM oxsum.payment_fulfillments WHERE order_id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let fulfilled = fulfilled
            .as_ref()
            .map(crate::payment_orders::fulfillment)
            .transpose()?;
        let rows = sqlx::query(&format!(
            "{} WHERE r.organization_id=$1 AND r.order_id=$2 ORDER BY r.created_at LIMIT 1001",
            crate::refunds::REFUND_SELECT
        ))
        .bind(org)
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        if rows.len() > 1000 {
            return Err(oversized());
        }
        let refunds = rows
            .iter()
            .map(crate::refunds::refund_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(Some((order, fulfilled, refunds)))
    }
    pub(super) async fn bounded_evidence_statement(
        &self,
        org: Uuid,
        id: Uuid,
    ) -> Result<crate::PlanStatementDetail, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        check_statement(&mut tx, org, id).await?;
        let detail = crate::plan_statements::statement_snapshot_in(&mut tx, org, id)
            .await?
            .ok_or_else(|| WalletError::NotFound("issued statement not found".into()))?;
        tx.commit().await?;
        Ok(detail)
    }
}
/// Bound nested statement/journal/effect rows in the same snapshot used to read them.
pub(super) async fn check_statement(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: Uuid,
    id: Uuid,
) -> Result<(), WalletError> {
    for query in [
        "SELECT octet_length(to_jsonb(r)::text)::bigint FROM oxsum.plan_statements r WHERE r.organization_id=$1 AND r.statement_id=$2",
        "SELECT octet_length(to_jsonb(r)::text)::bigint FROM oxsum.invoice_journals r JOIN oxsum.plan_statements s USING(journal_id) WHERE s.organization_id=$1 AND s.statement_id=$2",
        "SELECT octet_length(to_jsonb(r)::text)::bigint FROM oxsum.plan_accounting_periods r JOIN oxsum.plan_statements s USING(journal_id) WHERE s.organization_id=$1 AND s.statement_id=$2",
        "SELECT octet_length(to_jsonb(r)::text)::bigint AS bytes FROM oxsum.plan_statement_lines r JOIN oxsum.plan_statements s USING(statement_id) WHERE s.organization_id=$1 AND s.statement_id=$2 LIMIT 1001",
        "SELECT octet_length(to_jsonb(r)::text)::bigint AS bytes FROM oxsum.invoice_journal_lines r JOIN oxsum.plan_statements s USING(journal_id) WHERE s.organization_id=$1 AND s.statement_id=$2 LIMIT 1001",
        "SELECT octet_length(to_jsonb(r)::text)::bigint AS bytes FROM oxsum.plan_accounting_effects r JOIN oxsum.plan_accounting_periods a USING(plan_period_id) JOIN oxsum.plan_statements s ON s.journal_id=a.journal_id WHERE s.organization_id=$1 AND s.statement_id=$2 LIMIT 257",
    ] {
        let sizes: Vec<i64> = sqlx::query_scalar(query)
            .bind(org)
            .bind(id)
            .fetch_all(&mut **tx)
            .await?;
        if sizes.len() > 256 || sizes.iter().sum::<i64>() > MAX_BYTES as i64 {
            return Err(oversized());
        }
    }
    Ok(())
}

pub(super) fn check_sizes(sizes: &[i64]) -> Result<(), WalletError> {
    if sizes.len() > 1000 || sizes.iter().sum::<i64>() > MAX_BYTES as i64 {
        return Err(oversized());
    }
    Ok(())
}

/// Ledger rows are immutable; inspect bounded retained entry size before proofs.
pub(super) async fn check_entries(
    db: &Db,
    wallet: &Wallet,
    collector: &Collector,
) -> Result<(), WalletError> {
    let ids: Vec<Uuid> = collector.entry_ids.iter().map(|(id, _)| *id).collect();
    let tenant = wallet.tenant_id();
    for table in ["entries", "postings", "posting_dimensions"] {
        let sizes:Vec<i64>=sqlx::query_scalar(&format!("SELECT octet_length(to_jsonb(r)::text)::bigint FROM ledger_{tenant}.{table} r WHERE entry_id=ANY($1) LIMIT 4097"))
            .bind(&ids).fetch_all(db.pool()).await?;
        if sizes.len() > 4096 || sizes.iter().sum::<i64>() > 4 * 1024 * 1024 {
            return Err(oversized());
        }
    }
    Ok(())
}

//! Bounded business assertions captured on the reconciliation transaction.

use std::collections::BTreeMap;
use std::sync::Arc;

use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::types::{BYTE_LIMIT, SOURCE_LIMIT};
use crate::{WalletError, error};

#[derive(Clone)]
pub(super) struct Source {
    pub row: Arc<PgRow>,
    pub evidence: serde_json::Value,
}

impl Source {
    pub fn id(&self, field: &str) -> Result<Uuid, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn optional_id(&self, field: &str) -> Result<Option<Uuid>, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn text(&self, field: &str) -> Result<String, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn optional_text(&self, field: &str) -> Result<Option<String>, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn boolean(&self, field: &str) -> Result<bool, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn amount(&self, field: &str) -> Result<i64, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn date(&self, field: &str) -> Result<Date, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn instant(&self, field: &str) -> Result<OffsetDateTime, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn optional_day(&self, field: &str) -> Result<Option<Date>, WalletError> {
        Ok(self
            .row
            .try_get::<Option<OffsetDateTime>, _>(field)?
            .map(|time| time.date()))
    }
    pub fn json(&self, field: &str) -> Result<serde_json::Value, WalletError> {
        Ok(self.row.try_get(field)?)
    }
    pub fn policy(&self) -> Result<i16, WalletError> {
        Ok(self.row.try_get("accounting_policy_version")?)
    }
}

pub(super) struct Sources {
    pub tables: BTreeMap<&'static str, BTreeMap<Uuid, Source>>,
    pub count: usize,
    pub bytes: usize,
}

impl Sources {
    pub fn table(&self, table: &str) -> &BTreeMap<Uuid, Source> {
        self.tables
            .get(table)
            .expect("reconciliation selects every declared source table")
    }
    pub fn get(&self, table: &str, id: Uuid) -> Option<&Source> {
        self.table(table).get(&id)
    }
}

pub(super) async fn capture(
    connection: &mut PgConnection,
    org: Uuid,
) -> Result<Sources, WalletError> {
    let mut sources = Sources {
        tables: BTreeMap::new(),
        count: 0,
        bytes: 0,
    };
    for (table, identity, ownership) in [
        ("invoice_journals", "journal_id", "t.organization_id=$1"),
        (
            "invoice_journal_lines",
            "line_id",
            "EXISTS(SELECT 1 FROM oxsum.invoice_journals j WHERE j.journal_id=t.journal_id AND j.organization_id=$1)",
        ),
        ("plan_statements", "statement_id", "t.organization_id=$1"),
        (
            "plan_statement_lines",
            "line_id",
            "EXISTS(SELECT 1 FROM oxsum.plan_statements s WHERE s.statement_id=t.statement_id AND s.organization_id=$1)",
        ),
        ("customer_plans", "customer_plan_id", "t.organization_id=$1"),
        (
            "plan_periods",
            "plan_period_id",
            "EXISTS(SELECT 1 FROM oxsum.customer_plans p WHERE p.customer_plan_id=t.customer_plan_id AND p.organization_id=$1)",
        ),
        (
            "sold_plans",
            "plan_version_id",
            "(EXISTS(SELECT 1 FROM oxsum.customer_plans p WHERE p.plan_version_id=t.plan_version_id AND p.organization_id=$1) OR EXISTS(SELECT 1 FROM oxsum.plan_accounting_periods a WHERE a.plan_version_id=t.plan_version_id AND a.organization_id=$1))",
        ),
        ("payment_orders", "order_id", "t.organization_id=$1"),
        (
            "merchant_payment_bindings",
            "payment_id",
            "t.organization_id=$1",
        ),
        (
            "merchant_payment_refs",
            "payment_id",
            "EXISTS(SELECT 1 FROM oxsum.merchant_payment_bindings b WHERE b.payment_id=t.payment_id AND b.organization_id=$1)",
        ),
        ("refunds", "refund_id", "t.organization_id=$1"),
        ("credit_notes", "credit_note_id", "t.organization_id=$1"),
        (
            "merchant_event_inbox",
            "receipt_id",
            "EXISTS(SELECT 1 FROM oxsum.plan_refund_effects e WHERE e.event_receipt_id=t.receipt_id AND e.organization_id=$1) OR EXISTS(SELECT 1 FROM oxsum.nonplan_refund_effects e WHERE e.event_receipt_id=t.receipt_id AND e.organization_id=$1)",
        ),
        ("disputes", "dispute_id", "t.organization_id=$1"),
        ("plan_refund_effects", "refund_id", "t.organization_id=$1"),
        (
            "plan_accounting_periods",
            "plan_period_id",
            "t.organization_id=$1",
        ),
        (
            "plan_accounting_effects",
            "effect_id",
            "EXISTS(SELECT 1 FROM oxsum.plan_accounting_periods p WHERE p.plan_period_id=t.plan_period_id AND p.organization_id=$1)",
        ),
        (
            "plan_usage_corrections",
            "journal_id",
            "t.organization_id=$1",
        ),
        (
            "nonplan_refund_effects",
            "refund_id",
            "t.organization_id=$1",
        ),
    ] {
        let bytes:i64=sqlx::query_scalar(&format!("SELECT COALESCE(SUM(pg_column_size(t)),0)::bigint FROM (SELECT * FROM oxsum.{table} t WHERE {ownership} ORDER BY {identity} LIMIT $2) t"))
            .bind(org).bind((SOURCE_LIMIT+1) as i64).fetch_one(&mut *connection).await?;
        sources.bytes = sources
            .bytes
            .checked_add(usize::try_from(bytes).map_err(error::invalid)?)
            .ok_or_else(|| error::invalid("reconciliation captured bytes overflow"))?;
        if sources.bytes > BYTE_LIMIT {
            return Err(error::invalid(
                "reconciliation captured source byte limit exceeded",
            ));
        }
        let rows=sqlx::query(&format!("SELECT t.*,to_jsonb(t) AS evidence_row FROM oxsum.{table} t WHERE {ownership} ORDER BY {identity} LIMIT $2"))
            .bind(org).bind((SOURCE_LIMIT+1) as i64).fetch_all(&mut *connection).await?;
        sources.count += rows.len();
        if sources.count > SOURCE_LIMIT {
            return Err(error::invalid("reconciliation source limit exceeded"));
        }
        let mut selected = BTreeMap::new();
        for row in rows {
            let raw: serde_json::Value = row.try_get("evidence_row")?;
            let allowed = [
                "journal_id",
                "line_id",
                "statement_id",
                "customer_plan_id",
                "plan_period_id",
                "plan_version_id",
                "organization_id",
                "business_id",
                "order_id",
                "refund_id",
                "credit_note_id",
                "dispute_id",
                "effect_id",
                "event_receipt_id",
                "receipt_id",
                "payment_id",
                "merchant_id",
                "merchant_version_id",
                "plan_payment",
                "version_id",
                "voided_at",
                "correction_id",
                "kind",
                "source_ref",
                "entry_id",
                "original_entry_id",
                "cash_entry_id",
                "cash_entry_hash",
                "cash_booking_date",
                "correction_entry_id",
                "correction_entry_hash",
                "recharge_entry_id",
                "authorization_kind",
                "authorization_ref",
                "state",
                "status",
                "purpose",
                "purpose_ref",
                "accounting_policy_version",
                "cash_policy_version",
                "period_index",
                "period_start",
                "period_end",
                "service_start",
                "service_end",
                "service_day",
                "booked_on",
                "payment_on",
                "issued_at",
                "posted_at",
                "paid_at",
                "created_at",
                "booked_at",
                "amount_minor",
                "fee_minor",
                "price_minor",
                "total_minor",
                "settled_minor",
                "paid_minor",
                "billed_minor",
                "earned_minor",
                "deferred_minor",
                "collected_minor",
                "receivable_minor",
                "payable_minor",
                "cash_minor",
                "discount_minor",
                "unallocated_minor",
                "wallet_minor",
                "wallet_restored_minor",
                "suspense_minor",
                "duplicate_minor",
                "currency",
                "legacy_recovery",
                "paid_order_id",
                "replaces_statement_id",
                "replaces_journal_id",
            ];
            let evidence = serde_json::Value::Object(
                allowed
                    .into_iter()
                    .filter_map(|name| raw.get(name).map(|value| (name.to_owned(), value.clone())))
                    .collect(),
            );
            selected.insert(
                row.try_get(identity)?,
                Source {
                    evidence,
                    row: Arc::new(row),
                },
            );
        }
        sources.tables.insert(table, selected);
    }
    Ok(sources)
}

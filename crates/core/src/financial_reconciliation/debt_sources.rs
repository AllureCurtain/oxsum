//! Exact per-journal debt slices after the shared canonical financial comparison.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::compare::{self, Customer};
use super::sources::Sources;
use super::types::{self, Comparison, Scope};
use super::{ledger, sources};
use crate::{CustomerCapability, Db, Principal, TimeBasis, WalletError};

/// One exact journal receivable presented by the selected customer statement.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementDebtSlice {
    /// Original journal whose receivable would be repaid.
    pub journal_id: Uuid,
    /// Original owner plan, unaffected by repayment or cancellation.
    pub customer_plan_id: Uuid,
    /// Unpaid credit-minor from this journal on the selected statement.
    pub remaining_minor: i64,
    /// Immutable statement charge lines forming this slice.
    pub statement_line_ids: Vec<Uuid>,
    /// Chargebacks restoring the original receivable, if any.
    pub dispute_ids: Vec<Uuid>,
}

/// A source proof prerequisite, never a payment quote or reservation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementDebtSources {
    /// Customer statement inspected under current personal authority.
    pub statement_id: Uuid,
    /// Owning customer account.
    pub organization_id: Uuid,
    /// Current database snapshot time, excluded from the stable revision.
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    /// Exact displayed unpaid amount, preserved even when source review is required.
    pub outstanding_minor: i64,
    /// Every required source and canonical entry passed comparison and mapping.
    pub verified: bool,
    /// Explicit review class; absent only for an exact verified map.
    pub reason: Option<&'static str>,
    /// Stable fingerprint of the selected and cross-document source evidence.
    pub revision: Option<String>,
    /// Exact unpaid slices; empty on review-required history rather than guessed.
    pub slices: Vec<StatementDebtSlice>,
}

impl Db {
    /// Map one customer's issued statement to verified original receivable sources.
    /// Uses the shared bounded financial source capture in one read-only snapshot;
    /// takes no locks, creates no ledger and performs no financial or provider action.
    /// Cancellation does not hide old debt. Members retain the existing personal
    /// plan-statement authority; foreign statements are hidden before capture.
    pub async fn statement_debt_sources(
        &self,
        principal: &Principal,
        statement: Uuid,
    ) -> Result<StatementDebtSources, WalletError> {
        principal.require_customer(CustomerCapability::Personal)?;
        let organization = principal.organization();
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        let header = sqlx::query("SELECT total_minor,paid_minor,state FROM oxsum.plan_statements WHERE statement_id=$1 AND organization_id=$2 AND state<>'open'")
            .bind(statement).bind(organization.id).fetch_optional(&mut *tx).await?
            .ok_or_else(||WalletError::NotFound("customer statement not found".into()))?;
        let as_of = sqlx::query_scalar("SELECT now()")
            .fetch_one(&mut *tx)
            .await?;
        let state: String = header.try_get("state")?;
        let outstanding = types::sub(
            header.try_get("total_minor")?,
            header.try_get("paid_minor")?,
        )?;
        let mut answer = StatementDebtSources {
            statement_id: statement,
            organization_id: organization.id,
            as_of,
            outstanding_minor: outstanding.max(0),
            verified: false,
            reason: Some("sourceReviewRequired"),
            revision: None,
            slices: Vec::new(),
        };
        if !matches!(state.as_str(), "issued" | "past_due") || outstanding <= 0 {
            answer.reason = Some("statementNotPayable");
            return Ok(answer);
        }
        let captured = sources::capture(&mut tx, organization.id).await?;
        let official = ledger::capture(&mut tx, &organization.tenant_id).await?;
        if captured
            .bytes
            .checked_add(official.bytes)
            .is_none_or(|total| total > types::BYTE_LIMIT)
        {
            return Err(WalletError::InvalidInput(
                "statement source evidence byte limit exceeded".into(),
            ));
        }
        let mut comparison = Comparison::new(TimeBasis::Booking);
        let mut customer = Customer {
            organization: organization.id,
            sources: captured,
            ledger: official,
            scope: Scope {
                start: Date::MIN,
                end: Date::MAX,
                basis: TimeBasis::Booking,
            },
            captured_at: as_of,
            result: &mut comparison,
        };
        compare::check(&mut customer)?;
        if !customer.result.coverage.complete || !customer.result.differences.is_empty() {
            return Ok(answer);
        }
        // Old binaries could invoice usage already settled by the wallet. A
        // balanced accrual proves the recorded AR, not that collecting it is
        // justified. Require the source-linked correction before quoting debt.
        if customer
            .sources
            .table("invoice_journals")
            .iter()
            .any(|(id, journal)| {
                journal.text("state").ok().as_deref() == Some("posted")
                    && customer
                        .sources
                        .get("plan_usage_corrections", *id)
                        .is_none()
                    && customer
                        .sources
                        .table("invoice_journal_lines")
                        .values()
                        .any(|line| {
                            line.id("journal_id").ok() == Some(*id)
                                && line.text("kind").ok().as_deref() == Some("usage_overage")
                                && line.amount("amount_minor").is_ok_and(|amount| amount > 0)
                        })
            })
        {
            answer.reason = Some("historicalUsageReviewRequired");
            return Ok(answer);
        }
        let mapping = map_documents(&customer.sources, &customer.ledger)?;
        let Some(mapping) = mapping else {
            answer.reason = Some("ambiguousDocumentAllocation");
            return Ok(answer);
        };
        let slices = mapping.get(&statement).cloned().unwrap_or_default();
        let total = slices
            .iter()
            .try_fold(0, |sum, slice| types::add(sum, slice.remaining_minor))?;
        if total != outstanding {
            answer.reason = Some("unmappedStatementDebt");
            return Ok(answer);
        }
        // Stable sorted source captures and official hashes bind cross-document
        // allocation as well as this statement; snapshot time is not a quote field.
        let source_values:Vec<_>=customer.sources.tables.iter().map(|(table,rows)|
            serde_json::json!({"table":table,"rows":rows.values().map(|row|&row.evidence).collect::<Vec<_>>()})).collect();
        let hashes:Vec<_>=customer.ledger.entries.iter().map(|(id,entry)|
            serde_json::json!({"entryId":id,"hash":entry.stored.as_ref().ok().map(|entry|entry.content_hash.to_hex())})).collect();
        let bytes=serde_json::to_vec(&serde_json::json!({"statementId":statement,"sources":source_values,"entries":hashes,"slices":slices}))
            .map_err(crate::error::invalid)?;
        let mut hash = Sha256::new();
        hash.update(b"oxsum/statement-debt-sources/v1\0");
        hash.update(bytes);
        answer.revision = Some(
            hash.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        );
        answer.verified = true;
        answer.reason = None;
        answer.slices = slices;
        tx.commit().await?;
        Ok(answer)
    }
}

fn map_documents(
    sources: &Sources,
    ledger: &ledger::Ledger,
) -> Result<Option<BTreeMap<Uuid, Vec<StatementDebtSlice>>>, WalletError> {
    let mut result = BTreeMap::new();
    let mut displayed = BTreeMap::<Uuid, i64>::new();
    let mut seen_disputes = BTreeSet::new();
    for (id, statement) in sources.table("plan_statements") {
        if !matches!(statement.text("state")?.as_str(), "issued" | "past_due") {
            continue;
        }
        let mut parts = BTreeMap::<Uuid, StatementDebtSlice>::new();
        let mut original = 0;
        let mut payment = 0;
        let mut original_lines = Vec::new();
        for line in sources
            .table("plan_statement_lines")
            .values()
            .filter(|line| line.id("statement_id").ok() == Some(*id))
        {
            let amount = line.amount("amount_minor")?;
            match line.text("kind")?.as_str() {
                "payment_applied" => {
                    payment = types::sub(payment, amount)?;
                }
                "dispute_chargeback" => {
                    let Some(dispute) = line.optional_id("source_ref")? else {
                        return Ok(None);
                    };
                    if amount <= 0 || !seen_disputes.insert(dispute) {
                        return Ok(None);
                    }
                    let Some(source) = sources.get("disputes", dispute) else {
                        return Ok(None);
                    };
                    if source.text("state")? != "lost" {
                        return Ok(None);
                    }
                    let entry =
                        *crate::entry_id_for(&format!("dispute-recharge/{dispute}")).as_uuid();
                    if source.optional_id("recharge_entry_id")? != Some(entry)
                        || ledger
                            .net(&BTreeSet::from([entry]), ledger::RECEIVABLE)?
                            .checked_neg()
                            != Some(amount)
                    {
                        return Ok(None);
                    }
                    let order = source.id("order_id")?;
                    let periods: Vec<_> = sources
                        .table("plan_periods")
                        .iter()
                        .filter(|(_, period)| {
                            period.text("authorization_kind").ok().as_deref()
                                == Some("payment_order")
                                && period.id("authorization_ref").ok() == Some(order)
                        })
                        .map(|(id, _)| *id)
                        .collect();
                    if periods.len() != 1 {
                        return Ok(None);
                    }
                    let journals: Vec<_> = sources
                        .table("invoice_journals")
                        .iter()
                        .filter(|(_, journal)| {
                            journal.optional_id("plan_period_id").ok() == Some(Some(periods[0]))
                                && journal.text("state").ok().as_deref() != Some("void")
                        })
                        .collect();
                    if journals.len() != 1 {
                        return Ok(None);
                    }
                    let (journal, source) = journals[0];
                    let part = parts.entry(*journal).or_insert(StatementDebtSlice {
                        journal_id: *journal,
                        customer_plan_id: source.id("customer_plan_id")?,
                        remaining_minor: 0,
                        statement_line_ids: Vec::new(),
                        dispute_ids: Vec::new(),
                    });
                    part.remaining_minor = types::add(part.remaining_minor, amount)?;
                    part.statement_line_ids.push(line.id("line_id")?);
                    part.dispute_ids.push(dispute);
                }
                "plan_fee" | "usage" | "discount" | "refund_credit" => {
                    original = types::add(original, amount)?;
                    original_lines.push(line.id("line_id")?);
                }
                _ => return Ok(None),
            }
        }
        let original = types::sub(original, payment)?;
        if original < 0 {
            return Ok(None);
        }
        if original > 0 {
            let Some(journal) = statement.optional_id("journal_id")? else {
                return Ok(None);
            };
            let Some(source) = sources.get("invoice_journals", journal) else {
                return Ok(None);
            };
            let part = parts.entry(journal).or_insert(StatementDebtSlice {
                journal_id: journal,
                customer_plan_id: source.id("customer_plan_id")?,
                remaining_minor: 0,
                statement_line_ids: Vec::new(),
                dispute_ids: Vec::new(),
            });
            part.remaining_minor = types::add(part.remaining_minor, original)?;
            part.statement_line_ids.extend(original_lines);
        }
        let total = parts
            .values()
            .try_fold(0, |sum, part| types::add(sum, part.remaining_minor))?;
        if total
            != types::sub(
                statement.amount("total_minor")?,
                statement.amount("paid_minor")?,
            )?
        {
            return Ok(None);
        }
        for part in parts.values_mut() {
            part.statement_line_ids.sort();
            part.dispute_ids.sort();
            let previous = displayed.entry(part.journal_id).or_default();
            *previous = types::add(*previous, part.remaining_minor)?;
        }
        result.insert(*id, parts.into_values().collect());
    }
    for (id, journal) in sources.table("invoice_journals") {
        let expected = if journal.text("state")? == "posted" {
            types::sub(
                journal.amount("total_minor")?,
                journal.amount("settled_minor")?,
            )?
        } else {
            0
        };
        if displayed.get(id).copied().unwrap_or(0) != expected {
            return Ok(None);
        }
    }
    Ok(Some(result))
}

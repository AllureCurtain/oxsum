//! Customer-facing statement lines compared with original journal and dispute sources.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::Row;
use uuid::Uuid;

use super::Customer;
use crate::WalletError;
use crate::financial_reconciliation::types;

pub(super) fn compare(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    let statements: Vec<_> = customer
        .sources
        .table("plan_statements")
        .iter()
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    let mut displayed = BTreeMap::<Uuid, i64>::new();
    for (statement, header) in statements {
        let before = customer.result.differences.len();
        let state = header.text("state")?;
        if state == "open" {
            customer.unsupported(
                "plan_statements",
                statement,
                "unfinishedDocument",
                "Unissued statement requires explicit source review",
            );
            continue;
        }
        let lines: Vec<_> = customer
            .sources
            .table("plan_statement_lines")
            .values()
            .filter(|line| line.id("statement_id").ok() == Some(statement))
            .cloned()
            .collect();
        let mut reversed = BTreeSet::new();
        let mut reversal_lines = BTreeSet::new();
        if state == "void" {
            for line in &lines {
                if let Some(original) = line.optional_id("source_ref")?.and_then(|id| {
                    lines
                        .iter()
                        .find(|source| source.id("line_id").ok() == Some(id))
                }) {
                    let original_id = original.id("line_id")?;
                    if line.text("kind")? != original.text("kind")?
                        || line.amount("amount_minor")?.checked_neg()
                            != Some(original.amount("amount_minor")?)
                        || !reversed.insert(original_id)
                    {
                        customer.unsupported(
                            "plan_statements",
                            statement,
                            "voidReversal",
                            "Void must retain exactly one opposite line per original source",
                        );
                    }
                    reversal_lines.insert(line.id("line_id")?);
                }
            }
            if reversed.len() * 2 != lines.len() {
                customer.unsupported(
                    "plan_statements",
                    statement,
                    "voidReversalCoverage",
                    "Void does not reverse every original statement line exactly once",
                );
            }
        }
        let mut charges = BTreeMap::<(String, Option<Uuid>), i64>::new();
        let mut payments = 0;
        let mut dispute_amount = 0;
        let mut origins = BTreeMap::<Uuid, i64>::new();
        for line in &lines {
            if state == "void"
                && (reversed.contains(&line.id("line_id")?)
                    || reversal_lines.contains(&line.id("line_id")?))
            {
                continue;
            }
            let kind = line.text("kind")?;
            let amount = line.amount("amount_minor")?;
            let source = line.optional_id("source_ref")?;
            if kind == "payment_applied" {
                payments = types::sub(payments, amount)?;
                continue;
            }
            if kind == "dispute_chargeback" {
                dispute_amount = types::add(dispute_amount, amount)?;
                if let Some(dispute) = source
                    .and_then(|source| customer.sources.get("disputes", source))
                    .cloned()
                {
                    let order = dispute.id("order_id")?;
                    let original = customer
                        .sources
                        .table("plan_periods")
                        .iter()
                        .find(|(_, period)| {
                            period.text("authorization_kind").ok().as_deref()
                                == Some("payment_order")
                                && period.id("authorization_ref").ok() == Some(order)
                        })
                        .map(|(id, _)| *id);
                    let original_journal = customer
                        .sources
                        .table("invoice_journals")
                        .iter()
                        .find(|(_, journal)| {
                            journal.optional_id("plan_period_id").ok() == Some(original)
                                && journal.text("state").ok().as_deref() != Some("void")
                        })
                        .map(|(id, _)| *id);
                    if let Some(journal) = original_journal {
                        let old = origins.get(&journal).copied().unwrap_or(0);
                        origins.insert(journal, types::add(old, amount)?);
                    } else {
                        customer.unsupported(
                            "plan_statements",
                            statement,
                            "disputeSource",
                            "Chargeback line cannot be mapped to its exact original journal",
                        );
                    }
                } else {
                    customer.unsupported(
                        "plan_statements",
                        statement,
                        "disputeSource",
                        "Chargeback line lacks a matching customer dispute",
                    );
                }
                continue;
            }
            let key = (category(&kind).to_owned(), source);
            let current = charges.get(&key).copied().unwrap_or(0);
            charges.insert(key, types::add(current, amount)?);
        }
        let charge_total = charges
            .values()
            .try_fold(dispute_amount, |total, amount| types::add(total, *amount))?;
        customer.compare_amount(
            "plan_statements",
            statement,
            "lineTotalMinor",
            header.amount("total_minor")?,
            charge_total,
        );
        customer.compare_amount(
            "plan_statements",
            statement,
            "paymentLinesMinor",
            header.amount("paid_minor")?,
            payments,
        );
        let journal_id = header.optional_id("journal_id")?;
        if let Some(journal) = journal_id
            .and_then(|id| customer.sources.get("invoice_journals", id))
            .cloned()
        {
            let id = journal.id("journal_id")?;
            if journal.id("organization_id")? != customer.organization
                || header.optional_id("customer_plan_id")? != Some(journal.id("customer_plan_id")?)
                || header.id("business_id")? != journal.id("business_id")?
            {
                customer.unsupported(
                    "plan_statements",
                    statement,
                    "journalOwnership",
                    "Statement does not retain its original customer, business and plan journal",
                );
            }
            // A chargeback-only follow-up has no original period charge lines.
            let original_document = header
                .row
                .try_get::<Option<i32>, _>("period_index")?
                .is_some();
            if state != "void" && original_document {
                let mut expected = BTreeMap::<(String, Option<Uuid>), i64>::new();
                for line in customer.sources.table("invoice_journal_lines").values() {
                    if line.id("journal_id")? != id {
                        continue;
                    }
                    let kind = line.text("kind")?;
                    let kind = match kind.as_str() {
                        "base_fee" | "tax" => "plan_fee",
                        "usage_overage" => "usage",
                        "discount" | "promo_credit" => "discount",
                        _ => "unsupported",
                    };
                    let source = line
                        .optional_id("source_ref")?
                        .or(Some(line.id("line_id")?));
                    let key = (kind.to_owned(), source);
                    let current = expected.get(&key).copied().unwrap_or(0);
                    expected.insert(key, types::add(current, line.amount("amount_minor")?)?);
                }
                expected.retain(|_, amount| *amount != 0);
                charges.retain(|_, amount| *amount != 0);
                if charges != expected {
                    customer.unsupported("plan_statements",statement,"journalLines","Statement charge lines differ from the exact signed source lines of its original journal");
                }
            }
        } else if !charges.is_empty() {
            customer.unsupported(
                "plan_statements",
                statement,
                "missingJournal",
                "Statement charges have no original journal",
            );
        }
        if matches!(state.as_str(), "issued" | "past_due") {
            let mut remaining =
                types::sub(header.amount("total_minor")?, header.amount("paid_minor")?)?.max(0);
            // Mixed-policy chargebacks retain the original source, not the policy
            // of the unrelated invoice whose statement received the line.
            for (journal, amount) in origins {
                let source = customer
                    .sources
                    .get("invoice_journals", journal)
                    .expect("captured source");
                let debt = types::sub(
                    source.amount("total_minor")?,
                    source.amount("settled_minor")?,
                )?
                .max(0);
                let take = amount.max(0).min(debt).min(remaining);
                remaining -= take;
                let prior = displayed.get(&journal).copied().unwrap_or(0);
                displayed.insert(journal, types::add(prior, take)?);
            }
            if let Some(journal) = journal_id {
                let prior = displayed.get(&journal).copied().unwrap_or(0);
                displayed.insert(journal, types::add(prior, remaining)?);
            } else if remaining > 0 {
                customer.unsupported(
                    "plan_statements",
                    statement,
                    "unmappedDebt",
                    "Customer document contains debt without an original journal source",
                );
            }
        }
        customer.result.coverage.sources += 1;
        customer.result.coverage.verified_sources +=
            usize::from(customer.result.differences.len() == before);
        customer.result.evidence.push(serde_json::json!({"organizationId":customer.organization,"sourceTable":"plan_statements","sourceId":statement,"source":header.evidence,"lines":lines.iter().map(|line|&line.evidence).collect::<Vec<_>>(),"lineTotalMinor":charge_total,"paymentLinesMinor":payments}));
    }
    let journals: Vec<_> = customer
        .sources
        .table("invoice_journals")
        .iter()
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    for (journal, source) in journals {
        if source.text("state")? == "draft" {
            continue;
        }
        let debt = if source.text("state")? == "posted" {
            types::sub(
                source.amount("total_minor")?,
                source.amount("settled_minor")?,
            )?
            .max(0)
        } else {
            0
        };
        let active = customer.sources.table("plan_statements").values().any(|s| {
            s.optional_id("journal_id").ok() == Some(Some(journal))
                && s.text("state").ok().as_deref() != Some("void")
        });
        let document = displayed
            .get(&journal)
            .copied()
            .unwrap_or(if active { 0 } else { debt });
        customer.compare_amount(
            "invoice_journals",
            journal,
            "customerDocumentDebtMinor",
            debt,
            document,
        );
    }
    Ok(())
}

fn category(kind: &str) -> &str {
    if kind == "refund_credit" {
        "discount"
    } else {
        kind
    }
}

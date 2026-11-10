//! Verify exact statement-line corrections against their original journal void.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::Row;
use uuid::Uuid;

use super::ledger::{self, Ledger};
use super::sources::{Source, Sources};
use crate::{WalletError, error};

pub(super) fn owner(sources: &Sources, line: &Source) -> Result<Uuid, WalletError> {
    let statement = sources
        .get("plan_statements", line.id("statement_id")?)
        .ok_or_else(|| error::invalid("statement line owner missing"))?;
    let source = line.optional_id("source_ref")?;
    match line.text("kind")?.as_str() {
        "payment_applied" => {
            if let Some(effect) = source.and_then(|id| sources.get("statement_payment_effects", id))
            {
                return effect
                    .optional_id("journal_id")?
                    .ok_or_else(|| error::invalid("payment journal missing"));
            }
        }
        "dispute_chargeback" => {
            let dispute = source
                .and_then(|id| sources.get("disputes", id))
                .ok_or_else(|| error::invalid("chargeback source missing"))?;
            let order = dispute.id("order_id")?;
            let periods: BTreeSet<_> = sources
                .table("plan_periods")
                .iter()
                .filter(|(_, p)| {
                    p.text("authorization_kind").ok().as_deref() == Some("payment_order")
                        && p.id("authorization_ref").ok() == Some(order)
                })
                .map(|(id, _)| *id)
                .collect();
            let journals: Vec<_> = sources
                .table("invoice_journals")
                .iter()
                .filter(|(_, j)| {
                    j.optional_id("plan_period_id")
                        .ok()
                        .flatten()
                        .is_some_and(|id| periods.contains(&id))
                })
                .map(|(id, _)| *id)
                .collect();
            if journals.len() == 1 {
                return Ok(journals[0]);
            }
            return Err(error::invalid("chargeback journal is ambiguous"));
        }
        "plan_fee" | "usage" | "discount" | "refund_credit" => (),
        _ => return Err(error::invalid("unsupported statement source")),
    }
    statement
        .optional_id("journal_id")?
        .ok_or_else(|| error::invalid("statement journal missing"))
}

/// Every ignored line must have a retained exact opposite and canonical source void.
pub(super) fn excluded(sources: &Sources, ledger: &Ledger) -> Result<BTreeSet<Uuid>, WalletError> {
    let mut ignored = BTreeSet::new();
    let mut documents = BTreeSet::new();
    for (id, correction) in sources.table("statement_source_voids") {
        let original = sources
            .get("plan_statement_lines", *id)
            .ok_or_else(|| error::invalid("void original line missing"))?;
        let reversal_id = correction.id("reversal_line_id")?;
        let reversal = sources
            .get("plan_statement_lines", reversal_id)
            .ok_or_else(|| error::invalid("void reversal missing"))?;
        let journal = correction.id("journal_id")?;
        let source = sources
            .get("invoice_journals", journal)
            .ok_or_else(|| error::invalid("void source journal missing"))?;
        let entry_id = correction.id("entry_id")?;
        let entry = ledger
            .entries
            .get(&entry_id)
            .ok_or_else(|| error::invalid("source void entry missing"))?;
        let statement = sources
            .get("plan_statements", original.id("statement_id")?)
            .ok_or_else(|| error::invalid("void statement missing"))?;
        let origin = original.optional_id("source_ref")?;
        let amount = original.amount("amount_minor")?;
        let kind = original.text("kind")?;
        let source_valid = match kind.as_str() {
            "payment_applied"
                if origin
                    .and_then(|id| sources.get("statement_payment_effects", id))
                    .is_some() =>
            {
                let effect = sources
                    .get(
                        "statement_payment_effects",
                        origin.ok_or_else(|| error::invalid("payment source missing"))?,
                    )
                    .ok_or_else(|| error::invalid("payment effect missing"))?;
                effect.id("statement_id")? == original.id("statement_id")?
                    && amount
                        == effect.amount("receivable_minor")?
                            * if effect.text("kind")? == "payment" {
                                -1
                            } else {
                                1
                            }
            }
            "dispute_chargeback" => {
                let dispute = origin
                    .and_then(|id| sources.get("disputes", id))
                    .ok_or_else(|| error::invalid("void dispute missing"))?;
                let entry = *crate::entry_id_for(&format!(
                    "dispute-recharge/{}",
                    dispute.id("dispute_id")?
                ))
                .as_uuid();
                dispute.text("state")? == "lost"
                    && amount > 0
                    && ledger
                        .net(&BTreeSet::from([entry]), ledger::RECEIVABLE)?
                        .checked_neg()
                        == Some(amount)
            }
            _ => {
                statement
                    .row
                    .try_get::<Option<i32>, _>("period_index")?
                    .is_some()
                    && statement.optional_id("journal_id")? == Some(journal)
            }
        };
        if statement
            .row
            .try_get::<Option<i32>, _>("period_index")?
            .is_some()
            && statement.optional_id("journal_id")? == Some(journal)
        {
            documents.insert((original.id("statement_id")?, journal));
        }
        if !source_valid
            || original.id("statement_id")? != correction.id("statement_id")?
            || reversal.id("statement_id")? != original.id("statement_id")?
            || reversal.optional_id("source_ref")? != Some(*id)
            || reversal.text("kind")? != original.text("kind")?
            || original.amount("amount_minor")?.checked_neg()
                != Some(reversal.amount("amount_minor")?)
            || owner(sources, original)? != journal
            || source.text("state")? != "void"
            || !entry.same_key(&format!("plan-journal/{journal}/void"))
            || !entry.stored.as_ref().is_ok_and(|entry| {
                entry.content_hash.to_hex() == correction.text("entry_hash").unwrap_or_default()
            })
            || !ignored.insert(*id)
            || !ignored.insert(reversal_id)
        {
            return Err(error::invalid(
                "statement source void differs from its retained proof",
            ));
        }
    }
    for (statement, journal) in documents {
        validate_original_document(sources, ledger, statement, journal)?;
    }
    Ok(ignored)
}

fn validate_original_document(
    sources: &Sources,
    ledger: &Ledger,
    statement: Uuid,
    journal: Uuid,
) -> Result<(), WalletError> {
    let source = sources
        .get("invoice_journals", journal)
        .ok_or_else(|| error::invalid("void journal missing"))?;
    let mut charges = BTreeMap::<(String, Option<Uuid>), i64>::new();
    let mut payments = BTreeMap::<Option<Uuid>, i64>::new();
    let mut expected = BTreeMap::<(String, Option<Uuid>), i64>::new();
    for line in sources
        .table("invoice_journal_lines")
        .values()
        .filter(|l| l.id("journal_id").ok() == Some(journal))
    {
        if let Some(original) = line
            .optional_id("source_ref")?
            .and_then(|id| sources.get("invoice_journal_lines", id))
            && original.id("journal_id")? == journal
            && original.text("kind")? == line.text("kind")?
            && original.amount("amount_minor")?.checked_neg() == Some(line.amount("amount_minor")?)
        {
            continue;
        }
        let kind = match line.text("kind")?.as_str() {
            "base_fee" | "tax" => "plan_fee",
            "usage_overage" => "usage",
            "discount" | "promo_credit" => "discount",
            _ => return Err(error::invalid("unknown void journal charge")),
        };
        let value = expected
            .entry((
                kind.into(),
                line.optional_id("source_ref")?
                    .or(Some(line.id("line_id")?)),
            ))
            .or_default();
        *value = super::types::add(*value, line.amount("amount_minor")?)?;
    }
    for correction in sources
        .table("statement_source_voids")
        .values()
        .filter(|v| {
            v.id("statement_id").ok() == Some(statement) && v.id("journal_id").ok() == Some(journal)
        })
    {
        let line = sources
            .get("plan_statement_lines", correction.id("original_line_id")?)
            .ok_or_else(|| error::invalid("void original missing"))?;
        let source = line.optional_id("source_ref")?;
        let amount = line.amount("amount_minor")?;
        match line.text("kind")?.as_str() {
            "dispute_chargeback" => (),
            "payment_applied" => {
                if source
                    .and_then(|id| sources.get("statement_payment_effects", id))
                    .is_none()
                {
                    let value = payments.entry(source).or_default();
                    *value = super::types::add(*value, amount)?;
                }
            }
            kind => {
                let value = charges
                    .entry((
                        if kind == "refund_credit" {
                            "discount"
                        } else {
                            kind
                        }
                        .into(),
                        source,
                    ))
                    .or_default();
                *value = super::types::add(*value, amount)?;
            }
        }
    }
    let mut expected_payments =
        super::compare::documents::payment_sources_for(sources, ledger, source)?;
    expected.retain(|_, v| *v != 0);
    charges.retain(|_, v| *v != 0);
    expected_payments.retain(|_, v| *v != 0);
    payments.retain(|_, v| *v != 0);
    if charges != expected || payments != expected_payments {
        return Err(error::invalid(
            "cancelled statement sources differ from original journal charges or payments",
        ));
    }
    Ok(())
}

//! Reconstruct corrections from predecessor lines and original sold service.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::Row;
use uuid::Uuid;

use super::Customer;
use crate::financial_reconciliation::sources::Source;
use crate::financial_reconciliation::types::{self, Amounts};
use crate::{InvoiceCorrectionRequest, InvoiceCorrectionResult, WalletError, error};

pub(super) fn compare(c: &mut Customer<'_>) -> Result<(), WalletError> {
    let generations: Vec<_> = c
        .sources
        .table("invoice_accounting_generations")
        .values()
        .cloned()
        .collect();
    for g in generations {
        let journal = g.id("journal_id")?;
        let before = c.result.differences.len();
        let valid = validate(c, &g)?;
        if !valid {
            c.unsupported("invoice_accounting_generations", journal, "originalCorrectionSources",
                "Correction differs from its exact predecessor lines, original service or retained operator intent");
        }
        c.result.coverage.sources += 1;
        c.result.coverage.verified_sources +=
            usize::from(valid && before == c.result.differences.len());
        c.result
            .evidence
            .push(serde_json::json!({"organizationId": c.organization,
            "sourceTable": "invoice_accounting_generations", "sourceId": journal,
            "source": g.evidence, "includedInTotals": false}));
    }
    let corrections: Vec<_> = c
        .sources
        .table("invoice_corrections")
        .values()
        .cloned()
        .collect();
    for correction in corrections {
        let id = correction.id("correction_id")?;
        if !c
            .sources
            .get(
                "invoice_accounting_generations",
                correction.id("journal_id")?,
            )
            .is_some_and(|g| g.id("correction_id").ok() == Some(id))
        {
            c.unsupported(
                "invoice_corrections",
                id,
                "missingGeneration",
                "Correction has no matching immutable accounting context",
            );
        }
    }
    let missing: Vec<_> = c
        .sources
        .table("invoice_journals")
        .values()
        .filter(|j| {
            j.optional_id("replaces_journal_id")
                .ok()
                .flatten()
                .is_some()
                && c.sources
                    .get(
                        "invoice_accounting_generations",
                        j.id("journal_id").unwrap_or_default(),
                    )
                    .is_none()
        })
        .map(|j| j.id("journal_id"))
        .collect::<Result<_, _>>()?;
    for id in missing {
        c.unsupported(
            "invoice_journals",
            id,
            "missingCorrection",
            "Reissued invoice has no source-linked correction receipt",
        );
    }
    Ok(())
}

fn validate(c: &mut Customer<'_>, g: &Source) -> Result<bool, WalletError> {
    let journal = g.id("journal_id")?;
    let predecessor = g.id("predecessor_journal_id")?;
    let correction_id = g.id("correction_id")?;
    let Some(correction) = c.sources.get("invoice_corrections", correction_id).cloned() else {
        return Ok(false);
    };
    let Some(old) = c.sources.get("invoice_journals", predecessor).cloned() else {
        return Ok(false);
    };
    let Some(new) = c.sources.get("invoice_journals", journal).cloned() else {
        return Ok(false);
    };
    let Some(period) = c
        .sources
        .get("plan_periods", g.id("plan_period_id")?)
        .cloned()
    else {
        return Ok(false);
    };
    let Some(sold) = c
        .sources
        .get("sold_plans", g.id("plan_version_id")?)
        .cloned()
    else {
        return Ok(false);
    };
    let Some(statement) = c
        .sources
        .get("plan_statements", correction.id("statement_id")?)
        .cloned()
    else {
        return Ok(false);
    };
    let Some(old_statement) = c
        .sources
        .get(
            "plan_statements",
            correction.id("predecessor_statement_id")?,
        )
        .cloned()
    else {
        return Ok(false);
    };
    let raw = correction.json("request")?;
    let request: InvoiceCorrectionRequest = match serde_json::from_value(raw["request"].clone()) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let result: InvoiceCorrectionResult = match serde_json::from_value(correction.json("result")?) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    let mut valid = correction.id("predecessor_journal_id")? == predecessor
        && correction.id("journal_id")? == journal
        && old.text("state")? == "void"
        && old.id("organization_id")? == c.organization
        && new.id("organization_id")? == c.organization
        && new.id("business_id")? == old.id("business_id")?
        && new.id("customer_plan_id")? == old.id("customer_plan_id")?
        && new.optional_id("plan_period_id")?.is_none()
        && new.optional_id("replaces_journal_id")? == Some(predecessor)
        && g.id("organization_id")? == c.organization
        && g.id("business_id")? == old.id("business_id")?
        && g.id("customer_plan_id")? == old.id("customer_plan_id")?
        && period.id("customer_plan_id")? == g.id("customer_plan_id")?
        && g.instant("service_start")? == period.instant("period_start")?
        && g.instant("service_end")? == period.instant("period_end")?
        && g.policy()? == old.policy()?
        && g.policy()? == new.policy()?
        && g.policy()? == sold.policy()?
        && sold.id("business_id")? == g.id("business_id")?
        && sold.text("currency")? == old.text("currency")?
        && old.text("currency")? == new.text("currency")?
        && statement.optional_id("journal_id")? == Some(journal)
        && statement.optional_id("replaces_statement_id")?
            == Some(old_statement.id("statement_id")?)
        && old_statement.optional_id("journal_id")? == Some(predecessor)
        && statement.instant("period_start")? == g.instant("service_start")?
        && statement.instant("period_end")? == g.instant("service_end")?
        && statement
            .row
            .try_get::<Option<time::OffsetDateTime>, _>("due_at")?
            == old_statement
                .row
                .try_get::<Option<time::OffsetDateTime>, _>("due_at")?
        && raw["journalId"] == serde_json::json!(predecessor)
        && request.source_revision == correction.text("source_revision")?
        && request.reason == correction.text("reason")?
        && request.idempotency_key == correction.text("idempotency_key")?
        && result.correction_id == correction_id
        && result.journal_id == journal
        && result.statement_id == statement.id("statement_id")?
        && result.original_journal_id == predecessor
        && result.original_statement_id == old_statement.id("statement_id")?;
    let original_journal =
        if let Some(previous) = c.sources.get("invoice_accounting_generations", predecessor) {
            valid &= previous.id("plan_period_id")? == g.id("plan_period_id")?
                && previous.id("plan_version_id")? == g.id("plan_version_id")?
                && previous.instant("recognition_from")? <= g.instant("recognition_from")?;
            previous.id("original_journal_id")?
        } else {
            valid &= old.optional_id("plan_period_id")? == Some(g.id("plan_period_id")?);
            predecessor
        };
    valid &= original_journal == g.id("original_journal_id")?;
    // Trace to the immutable original authorization, never the current plan version.
    if g.policy()? == 2 {
        let Some(snapshot) = c
            .sources
            .get("plan_accounting_periods", g.id("plan_period_id")?)
        else {
            return Ok(false);
        };
        valid &= snapshot.id("journal_id")? == original_journal
            && snapshot.id("plan_version_id")? == g.id("plan_version_id")?;
    } else {
        let Some(order) = c
            .sources
            .get("payment_orders", period.id("authorization_ref")?)
        else {
            return Ok(false);
        };
        let terms = order.json("plan_payment")?;
        valid &= period.text("authorization_kind")? == "payment_order"
            && terms["planVersionId"] == serde_json::json!(g.id("plan_version_id")?)
            && terms["feeMinor"] == serde_json::json!(sold.amount("price_minor")?);
    }
    let originals = original_lines(c, predecessor)?;
    let links: Vec<_> = c
        .sources
        .table("invoice_correction_lines")
        .values()
        .filter(|l| l.id("correction_id").ok() == Some(correction_id))
        .cloned()
        .collect();
    let mut selected = BTreeMap::new();
    for selection in &request.lines {
        valid &= selected
            .insert(selection.source_line_id, selection.amount_minor)
            .is_none();
    }
    valid &= !selected.is_empty() && selected.len() <= 100 && links.len() == originals.len();
    let mut seen = BTreeSet::new();
    let mut preview_lines = BTreeMap::new();
    for line in &result.preview.lines {
        valid &= preview_lines.insert(line.source_line_id, line).is_none();
    }
    let mut original_total = 0i128;
    let mut corrected_total = 0i128;
    for link in links {
        let source_id = link.id("source_line_id")?;
        let Some(source) = originals.get(&source_id) else {
            valid = false;
            continue;
        };
        let Some(line) = c.sources.get("invoice_journal_lines", link.id("line_id")?) else {
            valid = false;
            continue;
        };
        let amount = source.amount("amount_minor")?;
        let requested = selected.remove(&source_id);
        let corrected = requested.unwrap_or(amount);
        valid &= preview_lines.remove(&source_id).is_some_and(|p| {
            p.kind.as_str() == source.text("kind").unwrap_or_default()
                && p.original_minor == amount
                && p.corrected_minor == corrected
                && source
                    .text("description")
                    .is_ok_and(|description| description == p.description)
        });
        valid &= seen.insert(source_id)
            && line.id("journal_id")? == journal
            && line.optional_id("source_ref")? == Some(source_id)
            && line.text("kind")? == source.text("kind")?
            && link.amount("original_minor")? == amount
            && link.amount("corrected_minor")? == corrected
            && line.amount("amount_minor")? == corrected
            && requested.is_none_or(|n| {
                source.text("kind").ok().as_deref() == Some("base_fee") && n >= 0 && n <= amount
            });
        original_total += i128::from(amount);
        corrected_total += i128::from(corrected);
    }
    valid &= preview_lines.is_empty()
        && selected.is_empty()
        && original_total == i128::from(old.amount("total_minor")?)
        && corrected_total >= 0
        && corrected_total <= original_total
        && corrected_total == i128::from(g.amount("fee_minor")?);
    let p = &result.preview;
    valid &= p.journal_id == predecessor
        && p.statement_id == old_statement.id("statement_id")?
        && p.organization_id == c.organization
        && p.customer_plan_id == g.id("customer_plan_id")?
        && p.plan_period_id == g.id("plan_period_id")?
        && p.plan_version_id == g.id("plan_version_id")?
        && p.currency == sold.text("currency")?
        && p.accounting_policy_version == g.policy()?
        && p.service_start == g.instant("service_start")?
        && p.service_end == g.instant("service_end")?
        && p.recognition_boundary == g.instant("recognition_from")?
        && p.source_revision == request.source_revision
        && p.amounts.original_total_minor == old.amount("total_minor")?
        && p.amounts.corrected_total_minor == g.amount("fee_minor")?
        && p.amounts.successor_receivable_minor == g.amount("fee_minor")?
        && p.amounts.successor_collected_minor == 0
        && p.amounts.cash_movement_minor == 0;
    let start = g.instant("service_start")?;
    let end = g.instant("service_end")?;
    let boundary = g.instant("recognition_from")?;
    let issued_at = new.instant("posted_at")?;
    let completed = if issued_at >= end {
        end
    } else {
        issued_at.date().midnight().assume_utc().max(start)
    };
    valid &= boundary == completed
        && issued_at <= c.captured_at
        && g.row.try_get::<time::Date, _>("payment_on")? == issued_at.date()
        && correction.instant("created_at")? <= issued_at;
    let elapsed = (boundary - start).whole_microseconds();
    let duration = (end - start).whole_microseconds();
    if duration <= 0 || elapsed < 0 || elapsed > duration {
        return Ok(false);
    }
    let earned = if g.policy()? == 1 {
        g.amount("fee_minor")?
    } else {
        i64::try_from(i128::from(g.amount("fee_minor")?) * elapsed / duration)
            .map_err(error::invalid)?
    };
    valid &= p.amounts.successor_earned_minor == earned
        && p.amounts.successor_deferred_minor == g.amount("fee_minor")? - earned
        && boundary <= c.captured_at;
    let ledger_boundary =
        u64::try_from(correction.amount("ledger_boundary")?).map_err(error::invalid)?;
    let captured_boundary = c
        .ledger
        .entries
        .values()
        .filter_map(|entry| {
            entry
                .stored
                .as_ref()
                .ok()
                .and_then(|stored| stored.index.map(|index| index.get()))
        })
        .max()
        .unwrap_or(0);
    valid &= ledger_boundary <= captured_boundary;
    let payable = crate::financial_reconciliation::correction_sources::payable_at(
        c,
        predecessor,
        Some(ledger_boundary),
    )?;
    valid &= p.amounts.original_payable_minor == payable;
    if let Some(entry) = correction.optional_id("entry_id")? {
        let ids = BTreeSet::from([entry]);
        valid &= c.ledger.entries.get(&entry).is_some_and(|e| {
            e.same_key(&format!("invoice-correction/{journal}/accrual"))
                && e.booked_on() == Some(issued_at.date())
                && e.stored.as_ref().is_ok_and(|e| {
                    e.content_hash.to_hex() == correction.text("entry_hash").unwrap_or_default()
                        && e.index.is_some_and(|index| index.get() == ledger_boundary)
                })
        });
        valid &= c.shape(
            "invoice_corrections",
            correction_id,
            &ids,
            &[
                super::ledger::RECEIVABLE,
                super::ledger::REVENUE,
                super::ledger::DEFERRED,
            ],
        );
        let actual = c.ledger.financial_amounts(&ids, false)?;
        let expected = Amounts {
            billed_minor: g.amount("fee_minor")?,
            earned_minor: earned,
            deferred_minor: g.amount("fee_minor")? - earned,
            receivable_minor: g.amount("fee_minor")?,
            ..Amounts::default()
        };
        for ((field, value), actual) in types::FIELDS
            .into_iter()
            .zip(expected.fields())
            .zip(actual.fields())
        {
            c.compare_amount("invoice_corrections", correction_id, field, value, actual);
        }
    } else {
        valid &= g.amount("fee_minor")? == 0;
    }
    Ok(valid)
}

/// Retained pre-void lines, with exactly one opposite reversal per original.
fn original_lines(c: &Customer<'_>, journal: Uuid) -> Result<BTreeMap<Uuid, Source>, WalletError> {
    let lines: BTreeMap<_, _> = c
        .sources
        .table("invoice_journal_lines")
        .iter()
        .filter(|(_, l)| l.id("journal_id").ok() == Some(journal))
        .map(|(id, l)| (*id, l.clone()))
        .collect();
    let mut originals = lines.clone();
    let mut reversed = BTreeSet::new();
    for (id, line) in &lines {
        if let Some(source) = line
            .optional_id("source_ref")?
            .and_then(|id| lines.get(&id))
        {
            if line.text("kind")? != source.text("kind")?
                || line.amount("amount_minor")?.checked_neg()
                    != Some(source.amount("amount_minor")?)
                || !reversed.insert(source.id("line_id")?)
            {
                return Err(error::invalid(
                    "correction predecessor reversal is inconsistent",
                ));
            }
            originals.remove(id);
        }
    }
    if originals.keys().copied().collect::<BTreeSet<_>>() != reversed {
        return Err(error::invalid(
            "correction predecessor reversals are incomplete",
        ));
    }
    Ok(originals)
}

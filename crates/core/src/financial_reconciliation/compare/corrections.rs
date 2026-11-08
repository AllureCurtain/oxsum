//! Recheck the retained original sources bound into historical usage corrections.

use std::collections::BTreeSet;

use doubleentry::{Direction, Layer};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Customer;
use crate::financial_reconciliation::sources::Source;
use crate::financial_reconciliation::types;
use crate::{PlanUsageCorrection, WalletError, error};

pub(super) fn verify(
    customer: &mut Customer<'_>,
    journal: Uuid,
    correction: &Source,
    header: &Source,
) -> Result<bool, WalletError> {
    let report = serde_json::from_value::<PlanUsageCorrection>(correction.json("report")?);
    let Ok(report) = report else {
        customer.unsupported(
            "plan_usage_corrections",
            journal,
            "retainedReport",
            "Historical usage correction has an invalid retained source report",
        );
        return Ok(false);
    };
    let digest: String =
        Sha256::digest(serde_json::to_vec(&report.evidence).map_err(error::invalid)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
    let entry_id = correction.id("entry_id")?;
    let description = format!(
        "historical duplicate usage correction {journal}; original accrual {}; receivable {}; refund payable {}; evidence sha256 {digest}; cash unchanged",
        report.original_entry_id, report.receivable_minor, report.payable_minor,
    );
    let original = *crate::entry_id_for(&format!("plan-journal/{journal}/accrual")).as_uuid();
    let mut valid = report.journal_id == journal
        && report.organization_id == customer.organization
        && correction.id("business_id")? == header.id("business_id")?
        && report.status == "applied"
        && report.refusal.is_none()
        && report.correction_id == entry_id
        && report.correction_id == correction.id("correction_id")?
        && report.entry_id == Some(entry_id)
        && report.original_entry_id == original
        && report.original_entry_id == correction.id("original_entry_id")?
        && report.statement_id == correction.optional_id("statement_id")?
        && report.duplicate_minor == correction.amount("duplicate_minor")?
        && report.receivable_minor == correction.amount("receivable_minor")?
        && report.payable_minor == correction.amount("payable_minor")?
        && report.duplicate_minor > 0
        && report.before_settled_minor >= 0
        && report.before_settled_minor <= report.before_total_minor
        && report.receivable_minor
            == report.duplicate_minor.min(types::sub(
                report.before_total_minor,
                report.before_settled_minor,
            )?)
        && report.payable_minor == types::sub(report.duplicate_minor, report.receivable_minor)?
        && report.after_total_minor
            == types::sub(report.before_total_minor, report.duplicate_minor)?
        && report.after_settled_minor
            == types::sub(report.before_settled_minor, report.payable_minor)?
        && report.evidence_sha256 == digest
        && customer.ledger.entries.get(&entry_id).is_some_and(|entry| {
            entry.valid()
                && entry
                    .stored
                    .as_ref()
                    .is_ok_and(|stored| stored.entry.description().as_str() == description)
        });
    let evidence = &report.evidence;
    let period = header.id("plan_period_id")?;
    valid &= evidence["planPeriodId"] == serde_json::json!(period)
        && evidence["customerPlanId"] == serde_json::json!(header.id("customer_plan_id")?);
    if let Some(source) = customer.sources.get("plan_periods", period) {
        valid &= evidence["periodStart"]
            == source
                .instant("period_start")?
                .unix_timestamp_nanos()
                .to_string()
            && evidence["periodEnd"]
                == source
                    .instant("period_end")?
                    .unix_timestamp_nanos()
                    .to_string();
    } else {
        valid = false;
    }
    let position = customer
        .ledger
        .entries
        .get(&entry_id)
        .and_then(|entry| entry.stored.as_ref().ok())
        .and_then(|stored| stored.index)
        .map(|index| index.get());
    let mut retained = vec![&evidence["accrual"]];
    let mut usage_total = 0;
    let mut usage_ids = BTreeSet::new();
    if let Some(usage) = evidence["usage"].as_array() {
        for row in usage {
            let fact = &row["entry"];
            let id = fact["entry_id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok());
            let record = fact["description"]
                .as_str()
                .and_then(crate::SettlementRecord::parse);
            let amount = row["chargedMinor"].as_i64().filter(|amount| *amount > 0);
            valid &= id.is_some_and(|id| usage_ids.insert(id))
                && record.is_some_and(|record| {
                    row["requestId"] == record.request && Some(record.charged) == amount
                });
            valid &= id
                .zip(amount)
                .is_some_and(|(id, amount)| usage_amount(customer, id) == Some(amount));
            usage_total = types::add(usage_total, amount.unwrap_or(0))?;
            retained.push(fact);
        }
    } else {
        valid = false;
    }
    valid &= usage_total == report.duplicate_minor && !usage_ids.is_empty();
    if let Some(payments) = evidence["payments"].as_array() {
        for payment in payments {
            retained.push(&payment["allocation"]);
            if let Some(cash) = payment.get("originalCash") {
                retained.push(cash);
            }
        }
    } else {
        valid = false;
    }
    if let Some(orders) = evidence["orderCashSources"].as_array() {
        valid &= !orders.is_empty();
        for order in orders {
            if let Some(entries) = order["entries"].as_array() {
                valid &= !entries.is_empty();
                retained.extend(entries);
            } else {
                valid = false;
            }
        }
    } else {
        valid = false;
    }
    let mut source_ids = BTreeSet::new();
    for fact in retained {
        if let Some(id) = fact["entry_id"]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            source_ids.insert(id);
        }
        valid &= exact_entry(customer, fact, position);
    }
    if !valid {
        customer.unsupported(
            "plan_usage_corrections", journal, "retainedEvidence",
            "Historical correction identity, source revision or original entries differ from the evidence bound into its official entry",
        );
    }
    customer.result.evidence.push(serde_json::json!({
        "organizationId": customer.organization, "sourceTable": "plan_usage_corrections", "sourceId": journal,
        "source": correction.evidence, "complete": valid, "entries": customer.ledger.evidence(&source_ids),
    }));
    Ok(valid)
}

fn exact_entry(customer: &Customer<'_>, retained: &serde_json::Value, before: Option<u64>) -> bool {
    let Some(id) = retained["entry_id"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return false;
    };
    let Some(entry) = customer.ledger.entries.get(&id) else {
        return false;
    };
    let Ok(stored) = &entry.stored else {
        return false;
    };
    let key = std::str::from_utf8(stored.entry.idempotency_key().as_bytes()).ok();
    if !key.is_some_and(|key| entry.same_key(key))
        || stored.entry.reverses().is_some()
        || !stored
            .index
            .zip(before)
            .is_some_and(|(position, before)| position.get() < before)
        || customer.ledger.entries.values().any(|source| {
            source.stored.as_ref().is_ok_and(|source| {
                source
                    .entry
                    .reverses()
                    .is_some_and(|reverses| *reverses.as_uuid() == id)
            })
        })
    {
        return false;
    }
    let postings: Vec<_> = stored.entry.postings().iter().map(|posting| serde_json::json!({
        "path": customer.ledger.accounts.iter().find(|(_, account)| **account == posting.account).map(|(path, _)| path),
        "direction": match posting.direction { Direction::Debit => "D", Direction::Credit => "C" },
        "amount_minor": posting.amount.to_minor(), "currency": posting.currency.to_string(),
        "layer": posting.layer.to_string(),
    })).collect();
    *retained
        == serde_json::json!({
            "entry_id": id, "content_hash": stored.content_hash.to_hex(), "description": stored.entry.description().as_str(),
            "reverses": null, "postings": postings,
        })
}

fn usage_amount(customer: &Customer<'_>, id: Uuid) -> Option<i64> {
    let entry = customer.ledger.entries.get(&id)?.stored.as_ref().ok()?;
    let mut debit = 0i64;
    let mut credit = 0i64;
    for posting in entry
        .entry
        .postings()
        .iter()
        .filter(|posting| posting.layer == Layer::Settled)
    {
        if posting.currency != doubleentry::Currency::USD || posting.amount.to_minor() <= 0 {
            return None;
        }
        let path = customer
            .ledger
            .accounts
            .iter()
            .find(|(_, account)| **account == posting.account)?
            .0
            .as_str();
        match (path, posting.direction) {
            ("Income:Usage", Direction::Credit) => {
                credit = credit.checked_add(posting.amount.to_minor())?
            }
            (
                "Liabilities:Wallet" | "Liabilities:CreditLine" | "Equity:Bonus" | "Equity:Promo",
                Direction::Debit,
            ) => debit = debit.checked_add(posting.amount.to_minor())?,
            _ => return None,
        }
    }
    (debit == credit && credit > 0).then_some(credit)
}

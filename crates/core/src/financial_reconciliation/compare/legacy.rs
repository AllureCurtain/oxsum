//! Retained close-time journals: captured net source positions, including corrections.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use super::{Customer, corrections, refunds};
use crate::WalletError;
use crate::financial_reconciliation::ledger;
use crate::financial_reconciliation::types::{self, Amounts};

pub(super) fn compare(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    let journals: Vec<_> = customer
        .sources
        .table("invoice_journals")
        .iter()
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    for (journal, header) in journals {
        if header.policy()? != 1 || header.text("state")? == "draft" {
            continue;
        }
        let generation = customer
            .sources
            .get("invoice_accounting_generations", journal)
            .cloned();
        let owner = format!("journal/{journal}");
        let mut ids = BTreeSet::new();
        let mut valid = true;
        let prefix = format!("plan-journal/{journal}/");
        let suffix = format!("/journal/{journal}");
        let found: Vec<_> = customer
            .ledger
            .entries
            .iter()
            .filter_map(|(id, entry)| {
                entry
                    .key()
                    .and_then(|key| std::str::from_utf8(key).ok())
                    .filter(|key| key.starts_with(&prefix) || key.ends_with(&suffix))
                    .map(|key| (*id, key.to_owned()))
            })
            .collect();
        let mut allocations = BTreeMap::<Uuid, i64>::new();
        for (entry, key) in found {
            let recognized = matches!(
                key.strip_prefix(&prefix),
                Some("accrual" | "prepay-settle" | "void")
            ) || key.starts_with("payment-order/") && key.ends_with(&suffix);
            if !recognized {
                customer.unsupported(
                    "invoice_journals",
                    journal,
                    "entryKind",
                    "Unsupported journal entry key requires source review",
                );
                valid = false;
                continue;
            }
            valid &= customer.require_entry("invoice_journals", journal, entry, &key, &owner);
            ids.insert(entry);
            if let Some(order) = key
                .strip_prefix("payment-order/")
                .and_then(|key| key.strip_suffix(&suffix))
                .and_then(|value| Uuid::parse_str(value).ok())
            {
                if let Some(payment) = customer.sources.get("payment_orders", order).cloned() {
                    valid &= matches!(payment.text("status")?.as_str(), "paid" | "fulfilled")
                        && payment.text("purpose")? == "plan_period"
                        && payment.optional_id("purpose_ref")?
                            == Some(header.id("customer_plan_id")?);
                    let applied = customer
                        .ledger
                        .net(&BTreeSet::from([entry]), ledger::RECEIVABLE)?;
                    allocations.insert(order, applied);
                    if applied > payment.amount("amount_minor")? {
                        customer.unsupported(
                            "invoice_journals",
                            journal,
                            "originalPayment",
                            "Original journal allocation exceeds its paid order",
                        );
                        valid = false;
                    }
                } else {
                    customer.unsupported(
                        "invoice_journals",
                        journal,
                        "originalPayment",
                        "Journal allocation has no matching customer payment order",
                    );
                    valid = false;
                }
            }
        }
        let period = generation
            .as_ref()
            .map(|g| g.id("plan_period_id"))
            .transpose()?
            .or(header.optional_id("plan_period_id")?)
            .and_then(|id| customer.sources.get("plan_periods", id))
            .cloned();
        if let Some(period) = period.as_ref() {
            if period.id("customer_plan_id")? != header.id("customer_plan_id")? {
                customer.unsupported(
                    "invoice_journals",
                    journal,
                    "periodOwnership",
                    "Journal period belongs to another customer plan",
                );
                valid = false;
            }
        } else {
            customer.unsupported(
                "invoice_journals",
                journal,
                "missingPeriod",
                "Legacy journal has no captured original authorized period",
            );
            valid = false;
        }
        let original_order = period
            .as_ref()
            .filter(|_| generation.is_none())
            .filter(|period| {
                period.text("authorization_kind").ok().as_deref() == Some("payment_order")
            })
            .map(|period| period.id("authorization_ref"))
            .transpose()?;
        let prepay = crate::entry_id_for(&format!("plan-journal/{journal}/prepay-settle"));
        if ids.contains(prepay.as_uuid()) {
            let applied = customer
                .ledger
                .net(&BTreeSet::from([*prepay.as_uuid()]), ledger::RECEIVABLE)?;
            if let Some(order) = original_order {
                allocations.insert(order, applied);
            } else {
                customer.unsupported(
                    "invoice_journals",
                    journal,
                    "originalPrepayment",
                    "Prepayment allocation has no original authorized order",
                );
                valid = false;
            }
        }
        let refunds: Vec<_> = customer
            .sources
            .table("plan_refund_effects")
            .iter()
            .filter(|(_, effect)| effect.optional_id("journal_id").ok() == Some(Some(journal)))
            .map(|(id, _)| *id)
            .collect();
        for refund in refunds {
            let (entries, okay) = refunds::plan_entries(customer, refund)?;
            ids.extend(entries);
            valid &= okay;
        }
        let disputes: Vec<_> = customer
            .sources
            .table("disputes")
            .iter()
            .filter(|(_, source)| {
                source
                    .id("order_id")
                    .ok()
                    .is_some_and(|id| Some(id) == original_order || allocations.contains_key(&id))
            })
            .map(|(id, source)| (*id, source.clone()))
            .collect();
        for (dispute, source) in disputes {
            if let Some(entry) = source.optional_id("recharge_entry_id")? {
                let key = format!("dispute-recharge/{dispute}");
                valid &= customer.require_entry("disputes", dispute, entry, &key, &owner);
                customer.compare_amount(
                    "disputes",
                    dispute,
                    "cashMinor",
                    source.amount("amount_minor")?,
                    customer
                        .ledger
                        .net(&BTreeSet::from([entry]), ledger::CASH)?,
                );
                ids.insert(entry);
            } else if source.text("state")? == "lost" {
                customer.unsupported(
                    "disputes",
                    dispute,
                    "missingEntry",
                    "Lost plan dispute has no official cash entry",
                );
                valid = false;
            }
        }
        if let Some(correction) = customer
            .sources
            .get("plan_usage_corrections", journal)
            .cloned()
        {
            let entry = correction.id("entry_id")?;
            valid &= customer.require_entry(
                "plan_usage_corrections",
                journal,
                entry,
                &format!("plan-usage-correction/{journal}"),
                &owner,
            );
            valid &= correction.id("original_entry_id")?
                == *crate::entry_id_for(&format!("plan-journal/{journal}/accrual")).as_uuid();
            valid &= corrections::verify(customer, journal, &correction, &header)?;
            let singleton = BTreeSet::from([entry]);
            customer.compare_amount(
                "plan_usage_corrections",
                journal,
                "duplicateMinor",
                correction.amount("duplicate_minor")?,
                types::sub(0, customer.ledger.net(&singleton, ledger::REVENUE)?)?,
            );
            customer.compare_amount(
                "plan_usage_corrections",
                journal,
                "receivableMinor",
                correction.amount("receivable_minor")?,
                customer.ledger.net(&singleton, ledger::RECEIVABLE)?,
            );
            customer.compare_amount(
                "plan_usage_corrections",
                journal,
                "payableMinor",
                correction.amount("payable_minor")?,
                customer.ledger.net(&singleton, ledger::PAYABLE)?,
            );
            ids.insert(entry);
        }
        for effect in customer.sources.table("statement_payment_effects").values() {
            if effect.optional_id("journal_id")? == Some(journal) {
                ids.insert(effect.id("entry_id")?);
            }
        }
        let mut line_total = 0;
        let mut charged = false;
        for line in customer.sources.table("invoice_journal_lines").values() {
            if line.id("journal_id")? == journal {
                let amount = line.amount("amount_minor")?;
                line_total = types::add(line_total, amount)?;
                charged |= amount > 0;
            }
        }
        if let Some(generation) = &generation {
            if generation.amount("fee_minor")? > 0 {
                let key = format!("invoice-correction/{journal}/accrual");
                let entry = *crate::entry_id_for(&key).as_uuid();
                valid &= customer.require_entry("invoice_journals", journal, entry, &key, &owner);
                ids.insert(entry);
            }
        } else if charged {
            let accrual =
                *crate::entry_id_for(&format!("plan-journal/{journal}/accrual")).as_uuid();
            valid &= customer.require_entry(
                "invoice_journals",
                journal,
                accrual,
                &format!("plan-journal/{journal}/accrual"),
                &owner,
            );
            ids.insert(accrual);
        }
        let voided = header.text("state")? == "void";
        customer.compare_amount(
            "invoice_journals",
            journal,
            "lineTotalMinor",
            if voided {
                0
            } else {
                header.amount("total_minor")?
            },
            line_total,
        );
        valid &= customer.shape(
            "invoice_journals",
            journal,
            &ids,
            &[
                ledger::CASH,
                ledger::RECEIVABLE,
                ledger::REVENUE,
                ledger::DISCOUNTS,
                ledger::DEFERRED,
                ledger::PAYABLE,
                ledger::SUSPENSE,
                "Equity:Promo",
            ],
        );
        let billed = if voided {
            0
        } else {
            header.amount("total_minor")?
        };
        let collected = if voided {
            0
        } else {
            header.amount("settled_minor")?
        };
        let expected = Amounts {
            billed_minor: billed,
            earned_minor: billed,
            collected_minor: collected,
            receivable_minor: types::sub(billed, collected)?,
            ..Amounts::default()
        };
        let actual = customer.ledger.financial_amounts(&ids, true)?;
        if customer.scope.contains(header.optional_day("posted_at")?) {
            customer.result.expected.add(&expected)?;
            customer.result.actual.add(&actual)?;
        }
        customer.record("invoice_journals",journal,&expected,&actual,&ids,valid,serde_json::json!({"period":period.map(|source|source.evidence),"originalOrder":original_order,"paymentAllocations":allocations,"payableMinor":customer.ledger.net(&ids,ledger::PAYABLE)?,"suspenseMinor":types::sub(0,customer.ledger.net(&ids,ledger::SUSPENSE)?)?}));
    }
    original_payments(customer)?;
    Ok(())
}

fn original_payments(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    let orders: Vec<_> = customer
        .sources
        .table("payment_orders")
        .iter()
        .filter(|(_, source)| {
            source.text("purpose").ok().as_deref() == Some("plan_period")
                && matches!(
                    source.text("status").ok().as_deref(),
                    Some("paid" | "fulfilled")
                )
        })
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    for (order, source) in orders {
        let prefix = format!("payment-order/{order}/");
        let found: Vec<_> = customer
            .ledger
            .entries
            .iter()
            .filter_map(|(id, entry)| {
                entry
                    .key()
                    .and_then(|key| std::str::from_utf8(key).ok())
                    .filter(|key| key.starts_with(&prefix))
                    .map(|key| (*id, key.to_owned()))
            })
            .collect();
        let mut ids = BTreeSet::new();
        let mut valid = true;
        for (id, key) in found {
            let supported = key == format!("{prefix}prepay")
                || key.strip_prefix(&prefix).is_some_and(|tail| {
                    tail.starts_with("journal/") && tail[8..].parse::<Uuid>().is_ok()
                });
            if !supported {
                customer.unsupported(
                    "payment_orders",
                    order,
                    "paymentShape",
                    "Unsupported original payment allocation key",
                );
                valid = false;
            }
            // Funding validation can reuse an already claimed journal/effect leg;
            // it is evidence only and does not add that cash a second time.
            if !customer
                .ledger
                .entries
                .get(&id)
                .is_some_and(|entry| entry.same_key(&key))
            {
                customer.unsupported(
                    "payment_orders",
                    order,
                    "originalCash",
                    "Original payment entry is unsequenced, corrupt or key-mismatched",
                );
                valid = false;
            }
            customer
                .ledger
                .claims
                .entry(id)
                .or_insert_with(|| format!("funding/{order}"));
            ids.insert(id);
        }
        let cash = types::sub(0, customer.ledger.net(&ids, ledger::CASH)?)?;
        // Paid is an external fact that may precede asynchronous fulfillment.
        // A still-pending posting makes coverage incomplete, never false matched.
        if ids.is_empty() {
            customer.unsupported(
                "payment_orders",
                order,
                "missingCash",
                "Confirmed plan payment has no official original cash allocation",
            );
            valid = false;
        }
        customer.compare_amount(
            "payment_orders",
            order,
            "originalCashMinor",
            source.amount("amount_minor")?,
            cash,
        );
        valid &= customer.shape(
            "payment_orders",
            order,
            &ids,
            &[ledger::CASH, ledger::DEFERRED, ledger::RECEIVABLE],
        );
        customer.record("payment_orders",order,&Amounts::default(),&Amounts::default(),&ids,valid,serde_json::json!({"confirmedCashMinor":source.amount("amount_minor")?,"officialCashMinor":cash,"includedInTotals":false}));
    }
    Ok(())
}

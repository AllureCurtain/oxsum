//! Independently verify each statement payment and source-bound return.

use std::collections::{BTreeMap, BTreeSet};

use super::Customer;
use crate::financial_reconciliation::{ledger, types};
use crate::{StatementPaymentTerms, WalletError, error};

pub(super) fn compare(c: &mut Customer<'_>) -> Result<(), WalletError> {
    let effects: Vec<_> = c
        .sources
        .table("statement_payment_effects")
        .iter()
        .map(|(id, e)| (*id, e.clone()))
        .collect();
    let mut paid = BTreeMap::<uuid::Uuid, i64>::new();
    let mut returned = BTreeMap::<uuid::Uuid, i64>::new();
    for (id, e) in effects {
        let order = e.id("order_id")?;
        let payment = c
            .sources
            .get("payment_orders", order)
            .cloned()
            .ok_or_else(|| error::invalid("statement payment source missing"))?;
        let terms: StatementPaymentTerms =
            serde_json::from_value(payment.json("statement_payment")?).map_err(error::invalid)?;
        let journal = e.optional_id("journal_id")?;
        let kind = e.text("kind")?;
        let source = e.id("source_ref")?;
        let entry = e.id("entry_id")?;
        let cash = e.amount("cash_minor")?;
        let ar = e.amount("receivable_minor")?;
        let payable = e.amount("payable_minor")?;
        let suspense = e.amount("suspense_minor")?;
        let sign = if kind == "payment" { 1 } else { -1 };
        let mut valid = payment.text("purpose")? == "statement_repayment"
            && payment.id("organization_id")? == c.organization
            && terms.organization_id == c.organization
            && terms.statement_id == e.id("statement_id")?
            && terms.amount_minor == payment.amount("amount_minor")?
            && terms.business_id == payment.id("business_id")?
            && cash > 0
            && ar >= 0
            && payable >= 0
            && suspense >= 0
            && types::add(types::add(ar, payable)?, suspense)? == cash;
        let retained = e.json("source")?;
        valid &= crate::statement_payments::evidence::validate(&crate::payment_orders::order(
            &payment.row,
        )?)
        .is_ok()
            && retained["terms"] == serde_json::json!(terms)
            && retained["originalEffectId"]
                == serde_json::json!(e.optional_id("original_effect_id")?);
        let key = crate::statement_payments::evidence::key(&kind, source, journal);
        valid &= id == entry
            && c.require_entry(
                "statement_payment_effects",
                id,
                entry,
                &key,
                &format!("statement/{id}"),
            );
        let ids = BTreeSet::from([entry]);
        valid &= c.shape(
            "statement_payment_effects",
            id,
            &ids,
            &[
                ledger::CASH,
                ledger::RECEIVABLE,
                ledger::PAYABLE,
                ledger::SUSPENSE,
            ],
        );
        for (path, expected) in [
            (ledger::CASH, -sign * cash),
            (ledger::RECEIVABLE, sign * ar),
            (ledger::PAYABLE, sign * payable),
            (ledger::SUSPENSE, -suspense),
        ] {
            c.compare_amount(
                "statement_payment_effects",
                id,
                path,
                expected,
                c.ledger.net(&ids, path)?,
            );
        }
        valid &= c.ledger.entries.get(&entry).is_some_and(|v| {
            v.stored
                .as_ref()
                .is_ok_and(|v| v.content_hash.to_hex() == e.text("entry_hash").unwrap_or_default())
        });
        if kind == "payment" {
            valid &= source == order
                && suspense == 0
                && e.optional_id("original_effect_id")?.is_none()
                && terms
                    .allocations
                    .iter()
                    .any(|a| Some(a.journal_id) == journal && a.amount_minor == cash);
            if let Some(receipt) = e
                .optional_id("event_receipt_id")?
                .and_then(|id| c.sources.get("merchant_event_inbox", id))
            {
                let mut event: crate::VerifiedMerchantEvent =
                    serde_json::from_value(receipt.json("normalized")?).map_err(error::invalid)?;
                if let (Some(binding), Some(reference)) = (
                    c.sources.get("merchant_payment_bindings", order),
                    c.sources.get("merchant_payment_refs", order),
                ) && reference.id("merchant_id")? == receipt.id("merchant_id")?
                    && reference.text("provider_payment_id")?
                        == event.provider_payment_id.as_deref().unwrap_or("")
                    && binding.id("merchant_id")? == receipt.id("merchant_id")?
                    && binding.id("version_id")? == receipt.id("version_id")?
                {
                    event.payment_id = event.payment_id.or(Some(order));
                    event.organization_id = event
                        .organization_id
                        .or(Some(binding.id("organization_id")?));
                }
                valid &= event.kind == crate::MerchantEventKind::Succeeded
                    && event.payment_id == Some(order)
                    && event.organization_id == Some(c.organization)
                    && event.amount_minor == Some(payment.amount("display_amount_minor")?)
                    && event.currency.as_deref()
                        == Some(payment.text("display_currency")?.as_str())
                    && receipt.id("merchant_id")? == payment.id("merchant_id")?
                    && receipt.id("version_id")? == payment.id("merchant_version_id")?
                    && matches!(receipt.text("state")?.as_str(), "accepted" | "applied");
            } else {
                valid = false;
            }
            let total = paid.entry(order).or_default();
            *total = types::add(*total, cash)?;
        } else if matches!(kind.as_str(), "refund" | "dispute") {
            if let Some(original) = e.optional_id("original_effect_id")? {
                let original_source = c
                    .sources
                    .get("statement_payment_effects", original)
                    .ok_or_else(|| error::invalid("statement return original missing"))?;
                valid &= original_source.text("kind")? == "payment"
                    && original_source.id("order_id")? == order
                    && original_source.optional_id("journal_id")? == journal;
                let total = returned.entry(original).or_default();
                *total = types::add(*total, cash)?;
            } else {
                valid &= ar == 0 && payable == 0 && suspense == cash;
            }
            let table = if kind == "refund" {
                "refunds"
            } else {
                "disputes"
            };
            let original = c.sources.get(table, source);
            let receipt = e
                .optional_id("event_receipt_id")?
                .and_then(|r| c.sources.get("merchant_event_inbox", r));
            if let (Some(original), Some(receipt)) = (original, receipt) {
                valid &= original.id("order_id")? == order
                    && original.id("organization_id")? == c.organization
                    && receipt.id("merchant_id")? == payment.id("merchant_id")?
                    && receipt.id("version_id")? == payment.id("merchant_version_id")?
                    && matches!(receipt.text("state")?.as_str(), "accepted" | "applied");
                let mut event: crate::VerifiedMerchantEvent =
                    serde_json::from_value(receipt.json("normalized")?).map_err(error::invalid)?;
                if let (Some(binding), Some(reference)) = (
                    c.sources.get("merchant_payment_bindings", order),
                    c.sources.get("merchant_payment_refs", order),
                ) {
                    let bound = reference.id("merchant_id")? == receipt.id("merchant_id")?
                        && reference.text("provider_payment_id")?
                            == event.provider_payment_id.as_deref().unwrap_or("")
                        && binding.id("merchant_id")? == receipt.id("merchant_id")?
                        && binding.id("version_id")? == receipt.id("version_id")?
                        && binding.id("organization_id")? == c.organization;
                    if bound {
                        event.payment_id = event.payment_id.or(Some(order));
                        event.organization_id = event.organization_id.or(Some(c.organization));
                    }
                }
                valid &= event.payment_id == Some(order)
                    && event.organization_id == Some(c.organization);
                let total = c
                    .sources
                    .table("statement_payment_effects")
                    .values()
                    .filter(|v| {
                        v.text("kind").ok().as_deref() == Some(kind.as_str())
                            && v.id("source_ref").ok() == Some(source)
                    })
                    .try_fold(0, |sum, v| types::add(sum, v.amount("cash_minor")?))?;
                valid &= total == original.amount("amount_minor")?;
                if kind == "refund" {
                    let reference = original.optional_text("provider_refund_id")?;
                    valid &= event.kind == crate::MerchantEventKind::RefundSucceeded
                        && event.amount_minor == Some(original.amount("display_amount_minor")?)
                        && event.currency.as_deref()
                            == Some(original.text("display_currency")?.as_str())
                        && event.refund_id.as_deref().is_none_or(|id| {
                            Some(id) == reference.as_deref()
                                || original.boolean("hyperswitch_body_id").unwrap_or(false)
                                    && id == source.to_string()
                        });
                } else {
                    valid &= original.text("state")? == "lost"
                        && event.kind == crate::MerchantEventKind::Dispute
                        && crate::nonplan_returns::disputed_minor(
                            payment.amount("amount_minor")?,
                            payment.amount("display_amount_minor")?,
                            &payment.text("display_currency")?,
                            &event,
                        )
                        .ok()
                            == Some(total)
                        && event.refund_id.as_deref()
                            == Some(original.text("provider_dispute_id")?.as_str());
                }
            } else {
                valid = false;
            }
        } else {
            valid = false;
        }
        // Every receivable allocation has one exact signed customer line, even
        // when that document is fully paid and no longer in the debt map.
        if ar > 0 {
            let lines: Vec<_> = c
                .sources
                .table("plan_statement_lines")
                .values()
                .filter(|l| l.optional_id("source_ref").ok() == Some(Some(id)))
                .collect();
            valid &= lines.len() == 1
                && lines[0].id("statement_id")? == terms.statement_id
                && lines[0].text("kind")? == "payment_applied"
                && lines[0].amount("amount_minor")? == -sign * ar;
        }
        if !valid {
            c.unsupported(
                "statement_payment_effects",
                id,
                "repaymentSource",
                "Statement cash allocation or original authority differs",
            );
        }
        let amounts = types::Amounts {
            refund_minor: if kind == "refund" { cash } else { 0 },
            ..Default::default()
        };
        let actual = types::Amounts {
            refund_minor: if kind == "refund" {
                c.ledger.net(&ids, ledger::CASH)?
            } else {
                0
            },
            ..Default::default()
        };
        if kind == "refund" && c.scope.contains(Some(e.date("booked_on")?)) {
            c.result.expected.add(&amounts)?;
            c.result.actual.add(&actual)?;
        }
        c.record(
            "statement_payment_effects",
            id,
            &amounts,
            &actual,
            &ids,
            valid,
            serde_json::json!({"source":e.evidence}),
        );
    }
    for (original, total) in returned {
        let limit = c
            .sources
            .get("statement_payment_effects", original)
            .ok_or_else(|| error::invalid("statement return source missing"))?
            .amount("cash_minor")?;
        if total > limit {
            c.unsupported(
                "statement_payment_effects",
                original,
                "retainedCash",
                "Returns exceed their original payment allocation",
            );
        }
    }
    // Paid but unfulfilled orders still reserve debt. Fulfilled orders must
    // retain the complete immutable cash set, including a missing-all case.
    let orders: Vec<_> = c
        .sources
        .table("payment_orders")
        .iter()
        .filter(|(_, p)| p.text("purpose").ok().as_deref() == Some("statement_repayment"))
        .map(|(id, p)| (*id, p.clone()))
        .collect();
    for (order, payment) in orders {
        let total = paid.remove(&order).unwrap_or(0);
        if payment.text("status")? == "fulfilled" {
            c.compare_amount(
                "payment_orders",
                order,
                "statementCashMinor",
                payment.amount("amount_minor")?,
                total,
            );
        } else if total != 0 {
            c.unsupported(
                "payment_orders",
                order,
                "uncommittedFulfillment",
                "Statement cash exists without its atomic fulfillment",
            );
        }
    }
    let allocations: Vec<_> = c
        .sources
        .table("statement_void_allocations")
        .iter()
        .map(|(id, v)| (*id, v.clone()))
        .collect();
    for (id, allocation) in allocations {
        let payment = c
            .sources
            .get("statement_payment_effects", id)
            .ok_or_else(|| error::invalid("void allocation original missing"))?;
        let journal = allocation.id("journal_id")?;
        let entry = allocation.id("entry_id")?;
        let index = |id| {
            c.ledger
                .entries
                .get(&id)
                .and_then(|e| e.stored.as_ref().ok())
                .and_then(|s| s.index)
                .map(|i| i.get())
        };
        let void_index = index(entry);
        let mut expected = payment.amount("receivable_minor")?;
        let mut paid_back = 0i64;
        for effect in c.sources.table("statement_payment_effects").values() {
            if effect.optional_id("original_effect_id")? == Some(id) {
                if index(effect.id("entry_id")?)
                    .zip(void_index)
                    .is_some_and(|(a, b)| a < b)
                {
                    expected = types::sub(expected, effect.amount("receivable_minor")?)?;
                }
                paid_back = types::add(paid_back, effect.amount("payable_minor")?)?;
            }
        }
        let valid = payment.text("kind")? == "payment"
            && payment.optional_id("journal_id")? == Some(journal)
            && void_index.is_some()
            && index(payment.id("entry_id")?)
                .zip(void_index)
                .is_some_and(|(a, b)| a < b)
            && c.sources
                .get("invoice_journals", journal)
                .is_some_and(|j| j.text("state").ok().as_deref() == Some("void"))
            && entry == *crate::entry_id_for(&format!("plan-journal/{journal}/void")).as_uuid()
            && c.ledger.entries.get(&entry).is_some_and(|e| {
                e.stored.as_ref().is_ok_and(|v| {
                    v.content_hash.to_hex() == allocation.text("entry_hash").unwrap_or_default()
                })
            })
            && allocation.amount("amount_minor")? == expected
            && paid_back <= expected;
        if !valid {
            c.unsupported(
                "statement_void_allocations",
                id,
                "voidSource",
                "Void payable differs from the original payment's retained allocation",
            );
        }
    }
    Ok(())
}

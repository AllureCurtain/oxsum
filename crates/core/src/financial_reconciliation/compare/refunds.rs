//! Refund cash and original source ownership, including retained correction legs.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Customer;
use crate::WalletError;
use crate::financial_reconciliation::ledger;
use crate::financial_reconciliation::types::{self, Amounts};

pub(super) fn plan_entries(
    customer: &mut Customer<'_>,
    refund: Uuid,
) -> Result<(BTreeSet<Uuid>, bool), WalletError> {
    let Some(effect) = customer.sources.get("plan_refund_effects", refund).cloned() else {
        customer.unsupported(
            "refunds",
            refund,
            "allocation",
            "Confirmed plan refund has no immutable cash allocation",
        );
        return Ok((BTreeSet::new(), false));
    };
    let entry = effect.id("entry_id")?;
    let original = effect.optional_id("original_entry_id")?;
    let mut ids = BTreeSet::from([entry]);
    if let Some(original) = original {
        ids.insert(original);
    }
    let owner = format!("refund/{refund}");
    let mut valid = true;
    for id in &ids {
        let key = if Some(*id) == original {
            format!("order-refund-plan/{refund}")
        } else {
            format!("plan-refund-cash/{refund}")
        };
        valid &= customer.require_entry("plan_refund_effects", refund, *id, &key, &owner);
    }
    valid &= customer.shape(
        "plan_refund_effects",
        refund,
        &ids,
        &[
            ledger::CASH,
            ledger::DEFERRED,
            ledger::DISCOUNTS,
            ledger::PAYABLE,
            ledger::RECEIVABLE,
            ledger::SUSPENSE,
        ],
    );
    Ok((ids, valid))
}

pub(super) fn compare(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    let selected: Vec<_> = customer
        .sources
        .table("plan_refund_effects")
        .iter()
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    for (refund, effect) in selected {
        let expected_cash = effect.amount("cash_minor")?;
        let order = effect.id("order_id")?;
        let mut valid = source_owner(customer, refund, order)?;
        valid &= allocation_source(customer, "plan_refund_effects", refund, &effect)?;
        let (ids, entries_valid) = plan_entries(customer, refund)?;
        valid &= entries_valid;
        for (field, path, sign) in [
            ("cash_minor", ledger::CASH, 1),
            ("deferred_minor", ledger::DEFERRED, -1),
            ("earned_minor", ledger::DISCOUNTS, -1),
            ("payable_minor", ledger::PAYABLE, -1),
            ("unallocated_minor", ledger::SUSPENSE, -1),
        ] {
            let actual = customer
                .ledger
                .net(&ids, path)?
                .checked_mul(sign)
                .ok_or_else(|| crate::error::invalid("refund amount overflows"))?;
            customer.compare_amount(
                "plan_refund_effects",
                refund,
                field,
                effect.amount(field)?,
                actual,
            );
        }
        customer.compare_amount(
            "plan_refund_effects",
            refund,
            "receivableMinor",
            0,
            customer.ledger.net(&ids, ledger::RECEIVABLE)?,
        );
        let expected = Amounts {
            refund_minor: expected_cash,
            ..Amounts::default()
        };
        let actual = Amounts {
            refund_minor: customer.ledger.net(&ids, ledger::CASH)?,
            ..Amounts::default()
        };
        // The retained plan allocation cohort uses its original booked_at clock,
        // matching financial-facts. A historical correction is not a second refund.
        if customer.scope.contains(effect.optional_day("booked_at")?) {
            customer.result.expected.add(&expected)?;
            customer.result.actual.add(&actual)?;
        }
        customer.record("plan_refund_effects",refund,&expected,&actual,&ids,valid,serde_json::json!({"orderId":order,"cashCohort":"allocation.bookedAt","payableMinor":types::sub(0,customer.ledger.net(&ids,ledger::PAYABLE)?)?,"suspenseMinor":types::sub(0,customer.ledger.net(&ids,ledger::SUSPENSE)?)?}));
    }
    let selected: Vec<_> = customer
        .sources
        .table("nonplan_refund_effects")
        .iter()
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    for (refund, effect) in selected {
        let metadata_valid = nonplan_metadata(customer, refund, &effect)?;
        let cash_id = effect.id("cash_entry_id")?;
        let mut ids = BTreeSet::from([cash_id]);
        let mut valid = metadata_valid && source_owner(customer, refund, effect.id("order_id")?)?;
        valid &= allocation_source(customer, "nonplan_refund_effects", refund, &effect)?;
        let owner = format!("refund/{refund}");
        valid &= customer.require_entry(
            "nonplan_refund_effects",
            refund,
            cash_id,
            &format!("order-refund/{refund}"),
            &owner,
        );
        if let Some(correction) = effect.optional_id("correction_entry_id")? {
            ids.insert(correction);
            valid &= customer.require_entry(
                "nonplan_refund_effects",
                refund,
                correction,
                &format!("nonplan-refund-restitution/{refund}"),
                &owner,
            );
        }
        valid &= customer.shape(
            "nonplan_refund_effects",
            refund,
            &ids,
            &[ledger::CASH, "Liabilities:Wallet", ledger::SUSPENSE],
        );
        let restored = effect.amount("wallet_restored_minor")?;
        let original = BTreeSet::from([cash_id]);
        customer.compare_amount(
            "nonplan_refund_effects",
            refund,
            "originalWalletMinor",
            types::add(effect.amount("wallet_minor")?, restored)?,
            types::sub(0, customer.ledger.net(&original, "Liabilities:Wallet")?)?,
        );
        customer.compare_amount(
            "nonplan_refund_effects",
            refund,
            "originalSuspenseMinor",
            types::sub(effect.amount("suspense_minor")?, restored)?,
            types::sub(0, customer.ledger.net(&original, ledger::SUSPENSE)?)?,
        );
        if let Some(correction) = effect.optional_id("correction_entry_id")? {
            let correction = BTreeSet::from([correction]);
            valid &= customer.shape(
                "nonplan_refund_effects",
                refund,
                &correction,
                &["Liabilities:Wallet", ledger::SUSPENSE],
            );
            customer.compare_amount(
                "nonplan_refund_effects",
                refund,
                "restoredWalletMinor",
                restored,
                customer.ledger.net(&correction, "Liabilities:Wallet")?,
            );
            customer.compare_amount(
                "nonplan_refund_effects",
                refund,
                "restoredSuspenseMinor",
                restored,
                types::sub(0, customer.ledger.net(&correction, ledger::SUSPENSE)?)?,
            );
            customer.compare_amount(
                "nonplan_refund_effects",
                refund,
                "restitutionCashMinor",
                0,
                customer.ledger.net(&correction, ledger::CASH)?,
            );
        } else {
            customer.compare_amount(
                "nonplan_refund_effects",
                refund,
                "restoredWalletMinor",
                restored,
                0,
            );
        }
        for (field, entry) in [
            ("cash_entry_hash", Some(cash_id)),
            (
                "correction_entry_hash",
                effect.optional_id("correction_entry_id")?,
            ),
        ] {
            if let Some(entry) = entry {
                let expected = effect.text(field)?;
                if !customer.ledger.entries.get(&entry).is_some_and(|entry| {
                    entry
                        .stored
                        .as_ref()
                        .is_ok_and(|stored| stored.content_hash.to_hex() == expected)
                }) {
                    customer.unsupported(
                        "nonplan_refund_effects",
                        refund,
                        field,
                        "Captured effect hash differs from its verified official entry",
                    );
                    valid = false;
                }
            }
        }
        if customer
            .ledger
            .entries
            .get(&cash_id)
            .and_then(|entry| entry.booked_on())
            != Some(effect.date("cash_booking_date")?)
        {
            customer.unsupported(
                "nonplan_refund_effects",
                refund,
                "cashBookingDate",
                "Cash allocation date differs from its verified original entry",
            );
            valid = false;
        }
        let cash = customer.ledger.net(&ids, ledger::CASH)?;
        customer.compare_amount(
            "nonplan_refund_effects",
            refund,
            "walletMinor",
            effect.amount("wallet_minor")?,
            types::sub(0, customer.ledger.net(&ids, "Liabilities:Wallet")?)?,
        );
        customer.compare_amount(
            "nonplan_refund_effects",
            refund,
            "suspenseMinor",
            effect.amount("suspense_minor")?,
            types::sub(0, customer.ledger.net(&ids, ledger::SUSPENSE)?)?,
        );
        let expected = Amounts {
            refund_minor: effect.amount("cash_minor")?,
            ..Amounts::default()
        };
        let actual = Amounts {
            refund_minor: cash,
            ..Amounts::default()
        };
        if customer
            .scope
            .contains(Some(effect.date("cash_booking_date")?))
        {
            customer.result.expected.add(&expected)?;
        }
        if customer.scope.contains(
            customer
                .ledger
                .entries
                .get(&cash_id)
                .and_then(|entry| entry.booked_on()),
        ) {
            customer.result.actual.add(&actual)?;
        }
        customer.record("nonplan_refund_effects",refund,&expected,&actual,&ids,valid,serde_json::json!({"cashBookingDate":effect.date("cash_booking_date")?.to_string(),"restoredWalletMinor":effect.amount("wallet_restored_minor")?}));
    }
    legacy_notes(customer)?;
    Ok(())
}

fn source_owner(
    customer: &mut Customer<'_>,
    refund: Uuid,
    order: Uuid,
) -> Result<bool, WalletError> {
    let original = customer.sources.get("refunds", refund).cloned();
    let payment = customer.sources.get("payment_orders", order).cloned();
    let valid = match (original, payment) {
        (Some(original), Some(payment)) => {
            original.id("order_id")? == order
                && original.id("organization_id")? == customer.organization
                && payment.id("organization_id")? == customer.organization
                && original.id("business_id")? == payment.id("business_id")?
                && matches!(payment.text("status")?.as_str(), "paid" | "fulfilled")
        }
        _ => false,
    };
    if !valid {
        customer.unsupported(
            "refunds",
            refund,
            "sourceOwnership",
            "Refund lacks its exact paid original customer order",
        );
    }
    Ok(valid)
}

fn allocation_source(
    customer: &mut Customer<'_>,
    table: &str,
    refund: Uuid,
    effect: &super::super::sources::Source,
) -> Result<bool, WalletError> {
    let original = customer.sources.get("refunds", refund).cloned();
    let mut valid = true;
    if let Some(original) = original.as_ref() {
        customer.compare_amount(
            table,
            refund,
            "confirmedRefundMinor",
            original.amount("amount_minor")?,
            effect.amount("cash_minor")?,
        );
    } else {
        valid = false;
    }
    if let Some(note_id) = effect.optional_id("credit_note_id")? {
        if let Some(note) = customer.sources.get("credit_notes", note_id).cloned() {
            valid &= note.optional_id("refund_id")? == Some(refund)
                && note.optional_id("order_id")? == Some(effect.id("order_id")?)
                && note.id("organization_id")? == customer.organization;
            customer.compare_amount(
                table,
                refund,
                "creditNoteMinor",
                effect.amount("cash_minor")?,
                note.amount("amount_minor")?,
            );
        } else {
            valid = false;
        }
    } else if table == "plan_refund_effects" {
        valid = false;
    }
    if let Some(receipt_id) = effect.optional_id("event_receipt_id")? {
        if let Some(receipt) = customer
            .sources
            .get("merchant_event_inbox", receipt_id)
            .cloned()
        {
            let mut event: crate::VerifiedMerchantEvent =
                serde_json::from_value(receipt.json("normalized")?)
                    .map_err(crate::error::invalid)?;
            let order = effect.id("order_id")?;
            let payment = customer.sources.get("payment_orders", order).cloned();
            let binding = customer
                .sources
                .get("merchant_payment_bindings", order)
                .cloned();
            let reference = customer
                .sources
                .get("merchant_payment_refs", order)
                .cloned();
            if let (Some(original), Some(payment), Some(binding), Some(reference)) =
                (original, payment, binding, reference)
            {
                let bound = reference.id("merchant_id")? == receipt.id("merchant_id")?
                    && reference.text("provider_payment_id")?
                        == event.provider_payment_id.as_deref().unwrap_or("")
                    && binding.id("merchant_id")? == receipt.id("merchant_id")?
                    && binding.id("version_id")? == receipt.id("version_id")?
                    && binding.id("organization_id")? == customer.organization;
                if bound {
                    event.payment_id = event.payment_id.or(Some(order));
                    event.organization_id = event.organization_id.or(Some(customer.organization));
                }
                let refund_identity = match event.refund_id.as_deref() {
                    Some(provider) => {
                        original.optional_text("provider_refund_id")?.as_deref() == Some(provider)
                            || (original.optional_text("provider_refund_id")?.is_none()
                                && original.boolean("hyperswitch_body_id")?
                                && refund.to_string() == provider)
                    }
                    None => {
                        table == "nonplan_refund_effects" && {
                            let retained = effect.json("source")?;
                            retained["refund"]["refundId"] == serde_json::json!(refund)
                                && retained["event"]["receiptId"] == serde_json::json!(receipt_id)
                                && retained["event"]["normalized"] == receipt.json("normalized")?
                        }
                    }
                };
                valid &= bound
                    && refund_identity
                    && matches!(receipt.text("state")?.as_str(), "accepted" | "applied")
                    && receipt.text("event_id")? == event.event_id
                    && event.kind == crate::MerchantEventKind::RefundSucceeded
                    && event.payment_id == Some(order)
                    && event.organization_id == Some(customer.organization)
                    && Some(receipt.id("merchant_id")?) == payment.optional_id("merchant_id")?
                    && Some(receipt.id("version_id")?)
                        == payment.optional_id("merchant_version_id")?
                    && event.provider_payment_id == payment.optional_text("provider_payment_id")?
                    && event.amount_minor == Some(original.amount("display_amount_minor")?)
                    && event.currency.as_deref()
                        == Some(original.text("display_currency")?.as_str());
            } else {
                valid = false;
            }
        } else {
            valid = false;
        }
    } else if table == "nonplan_refund_effects" {
        valid = false;
    }
    if !valid {
        customer.unsupported(table,refund,"allocationSource","Cash allocation lacks its exact original order, refund, credit note or verified provider receipt");
    }
    Ok(valid)
}

fn legacy_notes(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    let refunds: Vec<_> = customer
        .sources
        .table("refunds")
        .iter()
        .filter(|(id, _)| {
            !customer
                .sources
                .table("statement_payment_effects")
                .values()
                .any(|e| {
                    e.text("kind").ok().as_deref() == Some("refund")
                        && e.id("source_ref").ok() == Some(**id)
                })
                && customer.sources.get("plan_refund_effects", **id).is_none()
                && customer
                    .sources
                    .get("nonplan_refund_effects", **id)
                    .is_none()
        })
        .map(|(id, s)| (*id, s.clone()))
        .collect();
    for (refund, source) in refunds {
        let notes: Vec<_> = customer
            .sources
            .table("credit_notes")
            .values()
            .filter(|note| {
                note.optional_id("refund_id").ok() == Some(Some(refund))
                    && note.optional_id("entry_id").ok().flatten().is_some()
            })
            .cloned()
            .collect();
        if notes.is_empty() {
            if source.text("state")? == "succeeded" {
                customer.unsupported("refunds",refund,"missingCashEffect","Succeeded refund has no immutable allocation or bound official credit-note entry");
            }
            continue;
        }
        let mut ids = BTreeSet::new();
        let mut valid = source_owner(customer, refund, source.id("order_id")?)?;
        let plan = customer
            .sources
            .get("payment_orders", source.id("order_id")?)
            .is_some_and(|p| p.text("purpose").ok().as_deref() == Some("plan_period"));
        for note in &notes {
            let entry = note.id("entry_id")?;
            valid &= customer.require_entry(
                "refunds",
                refund,
                entry,
                &format!(
                    "{}/{refund}",
                    if plan {
                        "order-refund-plan"
                    } else {
                        "order-refund"
                    }
                ),
                &format!("refund/{refund}"),
            );
            valid &= note.optional_id("order_id")? == Some(source.id("order_id")?)
                && note.id("organization_id")? == customer.organization;
            customer.compare_amount(
                "credit_notes",
                note.id("credit_note_id")?,
                "refundMinor",
                source.amount("amount_minor")?,
                note.amount("amount_minor")?,
            );
            ids.insert(entry);
        }
        if ids.len() != 1 {
            customer.unsupported(
                "refunds",
                refund,
                "legacyCashMultiplicity",
                "Historical refund must retain one exact original Cash source",
            );
            valid = false;
        }
        let expected = Amounts {
            refund_minor: source.amount("amount_minor")?,
            ..Amounts::default()
        };
        let actual = Amounts {
            refund_minor: customer.ledger.net(&ids, ledger::CASH)?,
            ..Amounts::default()
        };
        let day = ids
            .iter()
            .find_map(|id| customer.ledger.entries.get(id).and_then(|e| e.booked_on()));
        if customer.scope.contains(day) {
            customer.result.expected.add(&expected)?;
            customer.result.actual.add(&actual)?;
        }
        customer.record("refunds",refund,&expected,&actual,&ids,valid,serde_json::json!({"legacyCreditNotes":notes.iter().map(|s|&s.evidence).collect::<Vec<_>>(),"cashCohort":"original official entry booking date"}));
    }
    Ok(())
}

fn nonplan_metadata(
    customer: &mut Customer<'_>,
    refund: Uuid,
    effect: &super::super::sources::Source,
) -> Result<bool, WalletError> {
    let source = effect.json("source")?;
    let stored = effect.json("effect")?;
    let hash: String = Sha256::digest(serde_json::to_vec(&source).map_err(crate::error::invalid)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut valid =
        hash == effect.text("source_revision")? && stored["policy"] == "nonplan-refund-v1";
    for (field, column) in [
        ("cashMinor", "cash_minor"),
        ("walletMinor", "wallet_minor"),
        ("suspenseMinor", "suspense_minor"),
        ("walletRestoredMinor", "wallet_restored_minor"),
    ] {
        valid &= stored[field] == serde_json::json!(effect.amount(column)?);
    }
    for (field, column) in [
        ("refundId", "refund_id"),
        ("orderId", "order_id"),
        ("organizationId", "organization_id"),
        ("eventReceiptId", "event_receipt_id"),
        ("cashEntryId", "cash_entry_id"),
    ] {
        valid &= stored[field] == serde_json::json!(effect.id(column)?);
    }
    valid &= stored["cashEntryHash"] == effect.text("cash_entry_hash")?
        && stored["cashBookingDate"] == effect.date("cash_booking_date")?.to_string()
        && stored["correctionEntryId"]
            == serde_json::json!(effect.optional_id("correction_entry_id")?)
        && stored["correctionEntryHash"]
            == serde_json::json!(effect.optional_text("correction_entry_hash")?)
        && stored["legacyRecovery"] == serde_json::json!(effect.boolean("legacy_recovery")?)
        && source["refund"]["refundId"] == serde_json::json!(refund)
        && source["refund"]["orderId"] == serde_json::json!(effect.id("order_id")?)
        && source["refund"]["organizationId"] == serde_json::json!(customer.organization)
        && source["refund"]["businessId"] == serde_json::json!(effect.id("business_id")?)
        && source["event"]["receiptId"] == serde_json::json!(effect.id("event_receipt_id")?);
    if let Some(receipt) = customer
        .sources
        .get("merchant_event_inbox", effect.id("event_receipt_id")?)
    {
        valid &= source["event"]["normalized"] == receipt.json("normalized")?
            && source["event"]["boundPaymentId"] == serde_json::json!(effect.id("order_id")?)
            && source["event"]["boundOrganizationId"] == serde_json::json!(customer.organization);
    } else {
        valid = false;
    }
    if !valid {
        customer.unsupported("nonplan_refund_effects",refund,"retainedMetadata","Immutable source revision or retained allocation fields differ from their original metadata");
    }
    Ok(valid)
}

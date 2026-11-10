//! Compare contested amounts with retained authenticated original payment events.

use super::Customer;
use crate::{MerchantEventKind, VerifiedMerchantEvent, WalletError, error};

pub(super) fn compare(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    let disputes: Vec<_> = customer
        .sources
        .table("disputes")
        .iter()
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    for (id, dispute) in disputes {
        let order_id = dispute.id("order_id")?;
        let Some(order) = customer.sources.get("payment_orders", order_id).cloned() else {
            customer.unsupported(
                "disputes",
                id,
                "originalPayment",
                "Dispute has no captured original order",
            );
            continue;
        };
        let Some(receipt) = dispute
            .optional_id("inbox_receipt_id")?
            .and_then(|id| customer.sources.get("merchant_event_inbox", id))
            .cloned()
        else {
            customer.unsupported(
                "disputes",
                id,
                "originalEvent",
                "Dispute lacks its original authenticated event",
            );
            continue;
        };
        let mut event: VerifiedMerchantEvent =
            serde_json::from_value(receipt.json("normalized")?).map_err(error::invalid)?;
        if let (Some(binding), Some(reference)) = (
            customer.sources.get("merchant_payment_bindings", order_id),
            customer.sources.get("merchant_payment_refs", order_id),
        ) && reference.id("merchant_id")? == receipt.id("merchant_id")?
            && reference.text("provider_payment_id")?
                == event.provider_payment_id.as_deref().unwrap_or("")
            && binding.id("merchant_id")? == receipt.id("merchant_id")?
            && binding.id("version_id")? == receipt.id("version_id")?
        {
            event.payment_id = event.payment_id.or(Some(order_id));
            event.organization_id = event
                .organization_id
                .or(Some(binding.id("organization_id")?));
        }
        let mut valid = matches!(receipt.text("state")?.as_str(), "accepted" | "applied")
            && event.kind == MerchantEventKind::Dispute
            && event.payment_id == Some(order_id)
            && event.organization_id == Some(customer.organization)
            && dispute.id("organization_id")? == customer.organization
            && order.id("organization_id")? == customer.organization
            && receipt.id("merchant_id")? == dispute.id("merchant_id")?
            && order.optional_id("merchant_id")? == Some(receipt.id("merchant_id")?)
            && order.optional_id("merchant_version_id")? == Some(receipt.id("version_id")?)
            && event.refund_id.as_deref() == Some(dispute.text("provider_dispute_id")?.as_str())
            && event.currency.as_deref() == Some(dispute.text("currency")?.as_str());
        // Deliberately independent from the writer's conversion helper. A dispute
        // and its ledger can agree on the same wrong full amount; the event cannot.
        let fiat = order.amount("display_amount_minor")?;
        let credits = order.amount("amount_minor")?;
        if let Some(amount) = event.amount_minor
            && fiat > 0
            && credits > 0
            && amount > 0
            && amount <= fiat
            && event.currency.as_deref() == Some(order.text("display_currency")?.as_str())
        {
            let numerator = i128::from(amount) * i128::from(credits);
            if numerator % i128::from(fiat) == 0 {
                let expected =
                    i64::try_from(numerator / i128::from(fiat)).map_err(error::invalid)?;
                customer.compare_amount(
                    "disputes",
                    id,
                    "authenticatedAmountMinor",
                    expected,
                    dispute.amount("amount_minor")?,
                );
            } else {
                valid = false;
            }
        } else {
            valid = false;
        }
        if !valid {
            customer.unsupported("disputes", id, "authenticatedTerms", "Dispute identity or amount is not supported by its exact authenticated original payment event");
        }
    }
    Ok(())
}

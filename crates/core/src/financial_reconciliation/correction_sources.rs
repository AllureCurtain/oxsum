//! Canonical source and entry revision prerequisite for an invoice correction.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use time::Date;
use uuid::Uuid;

use super::compare::{self, Customer};
use super::types::{self, Comparison, Scope};
use super::{ledger, sources};
use crate::{TimeBasis, WalletError, error};

/// Verify the complete bounded customer history before exposing correction amounts.
/// The caller owns the repeatable snapshot or the complete writer lock set.
pub(crate) async fn revision_in(
    tx: &mut Transaction<'_, Postgres>,
    organization: Uuid,
    tenant: &str,
    journal: Uuid,
) -> Result<(String, Option<i64>), WalletError> {
    let captured = sources::capture(tx, organization).await?;
    let official = ledger::capture(tx, tenant).await?;
    if captured
        .bytes
        .checked_add(official.bytes)
        .is_none_or(|n| n > types::BYTE_LIMIT)
    {
        return Err(error::invalid(
            "invoice correction source evidence byte limit exceeded",
        ));
    }
    let captured_at = sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let mut comparison = Comparison::new(TimeBasis::Booking);
    let mut customer = Customer {
        organization,
        sources: captured,
        ledger: official,
        scope: Scope {
            start: Date::MIN,
            end: Date::MAX,
            basis: TimeBasis::Booking,
        },
        captured_at,
        result: &mut comparison,
    };
    compare::check(&mut customer)?;
    if !customer.result.coverage.complete || !customer.result.differences.is_empty() {
        return Err(WalletError::Conflict(
            "invoice correction requires exact financial source review".into(),
        ));
    }
    let header = customer
        .sources
        .get("invoice_journals", journal)
        .ok_or_else(|| error::invalid("invoice source is missing"))?;
    let payable = if header.text("state")? == "void" {
        Some(payable_at(&customer, journal, None)?)
    } else {
        None
    };
    let source_values: Vec<_> = customer.sources.tables.iter().map(|(table, rows)| {
        serde_json::json!({"table": table, "rows": rows.values().map(|row| &row.evidence).collect::<Vec<_>>()})
    }).collect();
    let hashes: Vec<_> = customer.ledger.entries.iter().map(|(id, entry)| {
        serde_json::json!({"entryId": id, "hash": entry.stored.as_ref().ok().map(|entry| entry.content_hash.to_hex())})
    }).collect();
    let bytes = serde_json::to_vec(&serde_json::json!({"organizationId": organization, "sources": source_values, "entries": hashes})).map_err(error::invalid)?;
    let mut digest = Sha256::new();
    digest.update(b"oxsum/invoice-correction-sources/v1\0");
    digest.update(bytes);
    Ok((
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        payable,
    ))
}

/// Retained source payable at an official append boundary. Later original-source
/// returns remain outside the immutable correction receipt's accepted amount.
pub(super) fn payable_at(
    customer: &Customer<'_>,
    journal: Uuid,
    boundary: Option<u64>,
) -> Result<i64, WalletError> {
    let header = customer
        .sources
        .get("invoice_journals", journal)
        .ok_or_else(|| error::invalid("invoice payable source is missing"))?;
    let mut entries = BTreeSet::new();
    let void = *crate::entry_id_for(&format!("plan-journal/{journal}/void")).as_uuid();
    if customer.ledger.entries.contains_key(&void) {
        entries.insert(void);
    } else if header.amount("settled_minor")? != 0 {
        return Err(WalletError::Conflict(
            "void payable has no official source entry".into(),
        ));
    }
    for effect in customer.sources.table("plan_refund_effects").values() {
        if effect.optional_id("journal_id")? == Some(journal) {
            entries.extend(effect.optional_id("entry_id")?);
            entries.extend(effect.optional_id("original_entry_id")?);
        }
    }
    for effect in customer.sources.table("statement_payment_effects").values() {
        if effect.optional_id("journal_id")? == Some(journal)
            && effect.text("kind")? != "payment"
            && effect
                .optional_id("original_effect_id")?
                .and_then(|id| customer.sources.get("statement_payment_effects", id))
                .is_some_and(|original| {
                    original
                        .amount("receivable_minor")
                        .is_ok_and(|amount| amount > 0)
                })
        {
            entries.insert(effect.id("entry_id")?);
        }
    }
    let mut orders = BTreeSet::new();
    if let Some(period) = header
        .optional_id("plan_period_id")?
        .and_then(|id| customer.sources.get("plan_periods", id))
        && period.text("authorization_kind")? == "payment_order"
    {
        orders.insert(period.id("authorization_ref")?);
    }
    let suffix = format!("/journal/{journal}");
    for entry in customer.ledger.entries.values() {
        if let Some(order) = entry
            .key()
            .and_then(|key| std::str::from_utf8(key).ok())
            .and_then(|key| key.strip_prefix("payment-order/"))
            .and_then(|key| key.strip_suffix(&suffix))
            .and_then(|id| Uuid::parse_str(id).ok())
        {
            orders.insert(order);
        }
    }
    for dispute in customer.sources.table("disputes").values() {
        if orders.contains(&dispute.id("order_id")?) {
            entries.extend(dispute.optional_id("recharge_entry_id")?);
        }
    }
    if let Some(boundary) = boundary {
        entries.retain(|id| {
            customer.ledger.entries.get(id).is_some_and(|entry| {
                entry
                    .stored
                    .as_ref()
                    .is_ok_and(|stored| stored.index.is_some_and(|index| index.get() <= boundary))
            })
        });
    }
    let amount = customer.ledger.net(&entries, ledger::PAYABLE)?;
    if amount < 0 || amount > header.amount("settled_minor")? {
        return Err(WalletError::Conflict(
            "void payable differs from its original retained collection".into(),
        ));
    }
    Ok(amount)
}

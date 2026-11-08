//! Independent reconstruction of daily recognition and correction amount rules.

use std::collections::{BTreeMap, BTreeSet};

use time::OffsetDateTime;

use super::Customer;
use crate::financial_reconciliation::sources::Source;
use crate::financial_reconciliation::types::{self, Amounts};
use crate::{WalletError, error};

pub(super) fn compare(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    let periods: Vec<_> = customer
        .sources
        .table("plan_accounting_periods")
        .iter()
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    for (period_id, period) in periods {
        let start = period.instant("service_start")?;
        let end = period.instant("service_end")?;
        let mut effects: Vec<_> = customer
            .sources
            .table("plan_accounting_effects")
            .iter()
            .filter(|(_, effect)| effect.id("plan_period_id").ok() == Some(period_id))
            .map(|(id, source)| (*id, source.clone()))
            .collect();
        // created_at is transaction-start time, not the posting order after a
        // lock wait. Official log positions order nonzero effects; zero effects
        // in the same transaction inherit that batch's first position.
        let mut batches = BTreeMap::new();
        for (_, effect) in &effects {
            if let Some(index) = effect
                .optional_id("entry_id")?
                .and_then(|id| customer.ledger.entries.get(&id))
                .and_then(|entry| entry.stored.as_ref().ok())
                .and_then(|stored| stored.index)
                .map(|index| index.get())
            {
                batches
                    .entry(effect.instant("created_at")?)
                    .and_modify(|current: &mut u64| *current = (*current).min(index))
                    .or_insert(index);
            }
        }
        effects.sort_by_key(|(id, source)| {
            let at = source.instant("created_at").ok();
            let position = at
                .and_then(|at| batches.get(&at).copied())
                .unwrap_or_else(|| {
                    at.and_then(|at| batches.range(at..).next().map(|(_, index)| *index))
                        .unwrap_or(u64::MAX)
                });
            (
                position,
                at,
                rank(source),
                source.date("service_day").ok(),
                *id,
            )
        });
        let mut position = Amounts::default();
        let mut recognized = BTreeSet::new();
        let mut voided = None;
        let mut last_boundary = start;
        for (id, effect) in effects {
            let kind = effect.text("kind")?;
            let day = effect.date("service_day")?;
            let at = effect.instant("created_at")?;
            let expected = match kind.as_str() {
                "activation" => Amounts {
                    billed_minor: period.amount("fee_minor")?,
                    deferred_minor: period.amount("fee_minor")?,
                    collected_minor: period.amount("fee_minor")?,
                    ..Amounts::default()
                },
                "recognition" => {
                    let boundary = day
                        .next_day()
                        .ok_or_else(|| error::invalid("recognition day overflows"))?
                        .midnight()
                        .assume_utc()
                        .min(end);
                    if !recognized.insert(day)
                        || boundary <= start
                        || boundary > customer.captured_at
                        || voided.is_some()
                    {
                        customer.unsupported("plan_accounting_effects",id,"recognitionSequence","Recognition duplicates a day, precedes completed service or follows a void");
                    }
                    last_boundary = last_boundary.max(boundary);
                    let target = earned(position.billed_minor, start, end, boundary)?;
                    let increment = types::sub(target, position.earned_minor)?;
                    if increment < 0 || increment > position.deferred_minor {
                        customer.unsupported(
                            "plan_accounting_effects",
                            id,
                            "recognitionBounds",
                            "Recognition amount is outside the original remaining deferred fee",
                        );
                    }
                    Amounts {
                        earned_minor: increment,
                        deferred_minor: types::sub(0, increment)?,
                        ..Amounts::default()
                    }
                }
                "refund" => {
                    if let Some(refund) = customer
                        .sources
                        .get("plan_refund_effects", effect.id("source_ref")?)
                        .cloned()
                    {
                        let cancelled = if voided.is_some() {
                            0
                        } else {
                            refund
                                .amount("cash_minor")?
                                .min(position.billed_minor.max(0))
                                .min(position.collected_minor.max(0))
                        };
                        let net = types::sub(position.billed_minor, cancelled)?;
                        let target = if voided.is_some() {
                            0
                        } else {
                            earned(net, start, end, last_boundary)?
                        };
                        let earned = types::sub(position.earned_minor, target)?;
                        let deferred = types::sub(cancelled, earned)?;
                        customer.compare_amount(
                            "plan_accounting_effects",
                            id,
                            "refundEarnedAllocation",
                            earned,
                            refund.amount("earned_minor")?,
                        );
                        customer.compare_amount(
                            "plan_accounting_effects",
                            id,
                            "refundDeferredAllocation",
                            deferred,
                            refund.amount("deferred_minor")?,
                        );
                        Amounts {
                            billed_minor: -cancelled,
                            earned_minor: -earned,
                            deferred_minor: -deferred,
                            collected_minor: -cancelled,
                            ..Amounts::default()
                        }
                    } else {
                        continue;
                    }
                }
                "dispute" => {
                    let restored = if voided.is_some() {
                        0
                    } else {
                        customer
                            .sources
                            .get("disputes", effect.id("source_ref")?)
                            .map(|s| s.amount("amount_minor"))
                            .transpose()?
                            .unwrap_or(0)
                            .min(position.collected_minor.max(0))
                    };
                    Amounts {
                        collected_minor: -restored,
                        receivable_minor: restored,
                        ..Amounts::default()
                    }
                }
                "void" => {
                    voided = Some(last_boundary.max(completed(start, end, at)));
                    Amounts {
                        billed_minor: -position.billed_minor,
                        earned_minor: -position.earned_minor,
                        deferred_minor: -position.deferred_minor,
                        collected_minor: -position.collected_minor,
                        receivable_minor: -position.receivable_minor,
                        refund_minor: 0,
                    }
                }
                _ => continue,
            };
            let recorded = super::daily::amounts(&effect)?;
            for ((field, expected), recorded) in types::FIELDS
                .into_iter()
                .zip(expected.fields())
                .zip(recorded.fields())
            {
                customer.compare_amount(
                    "plan_accounting_effects",
                    id,
                    &format!("schedule.{field}"),
                    expected,
                    recorded,
                );
            }
            // Advance from the independent schedule, not the effect being checked.
            position.add(&expected)?;
        }
        let cutoff = voided.unwrap_or(customer.captured_at);
        let mut day = start.date();
        while day <= end.date() {
            let boundary = day
                .next_day()
                .ok_or_else(|| error::invalid("recognition day overflows"))?
                .midnight()
                .assume_utc()
                .min(end);
            if boundary > cutoff || boundary <= start || boundary <= day.midnight().assume_utc() {
                break;
            }
            if !recognized.contains(&day) {
                customer.result.unsupported(
                    customer.organization,
                    "plan_accounting_periods",
                    &period_id.to_string(),
                    &format!("missingRecognition/{day}"),
                    "Completed service day has no retained recognition effect",
                );
            }
            if boundary == end {
                break;
            }
            day = day
                .next_day()
                .ok_or_else(|| error::invalid("recognition day overflows"))?;
        }
    }
    Ok(())
}

fn rank(source: &Source) -> u8 {
    match source.text("kind").ok().as_deref() {
        Some("activation") => 0,
        Some("recognition") => 1,
        Some("refund" | "dispute") => 2,
        _ => 3,
    }
}

pub(super) fn completed(
    start: OffsetDateTime,
    end: OffsetDateTime,
    at: OffsetDateTime,
) -> OffsetDateTime {
    if at >= end {
        end
    } else {
        at.date().midnight().assume_utc().max(start)
    }
}

fn earned(
    fee: i64,
    start: OffsetDateTime,
    end: OffsetDateTime,
    boundary: OffsetDateTime,
) -> Result<i64, WalletError> {
    let duration = (end - start).whole_microseconds();
    if fee < 0 || duration <= 0 {
        return Err(error::invalid(
            "reconciliation recognition interval or fee is invalid",
        ));
    }
    let elapsed = (boundary.max(start).min(end) - start).whole_microseconds();
    let numerator = i128::from(fee)
        .checked_mul(elapsed)
        .ok_or_else(|| error::invalid("reconciliation recognition overflows"))?;
    i64::try_from(numerator / duration).map_err(error::invalid)
}

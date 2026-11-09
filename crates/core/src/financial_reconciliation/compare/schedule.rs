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
        // Nonzero movements follow verified official positions. A zero day has
        // no position: use sequenced neighboring service days and the immutable
        // correction boundary only where these prove an order. Transaction
        // start and booking midnight are lower bounds, never commit clocks.
        let mut positions = BTreeMap::new();
        for (id, effect) in &effects {
            if let Some(index) = effect
                .optional_id("entry_id")?
                .and_then(|id| customer.ledger.entries.get(&id))
                .filter(|entry| entry.valid())
                .and_then(|entry| entry.stored.as_ref().ok())
                .and_then(|stored| stored.index)
                .map(|index| index.get())
            {
                positions.insert(*id, index);
            }
        }
        let mut anchors = BTreeMap::new();
        let mut ambiguous = false;
        for (id, effect) in &effects {
            if effect.text("kind")? != "recognition" || positions.contains_key(id) {
                continue;
            }
            let day = effect.date("service_day")?;
            let boundary = day
                .next_day()
                .ok_or_else(|| error::invalid("recognition day overflows"))?
                .midnight()
                .assume_utc()
                .min(end);
            let mut previous = None;
            let mut following = None;
            for (other, candidate) in &effects {
                let Some(index) = positions.get(other) else {
                    continue;
                };
                if candidate.text("kind")? == "recognition" {
                    let candidate_day = candidate.date("service_day")?;
                    if candidate_day < day {
                        previous = Some(previous.map_or(*index, |prior: u64| prior.max(*index)));
                    } else if candidate_day > day {
                        following = Some(following.map_or(*index, |next: u64| next.min(*index)));
                    }
                }
            }
            let mut anchor = following.unwrap_or(u64::MAX);
            for (other, candidate) in &effects {
                let kind = candidate.text("kind")?;
                if !matches!(kind.as_str(), "refund" | "void") {
                    continue;
                }
                let index = positions.get(other).copied().unwrap_or(u64::MAX);
                let before = following.is_some_and(|next| next < index)
                    || if kind == "void" {
                        // Void retains the completed cutoff's date. The final
                        // partial day may end during that same date; its retained
                        // recognition must precede the terminal void as well.
                        boundary.date() <= candidate.date("service_day")?
                    } else {
                        let booked = candidate
                            .optional_id("entry_id")?
                            .and_then(|id| customer.ledger.entries.get(&id))
                            .and_then(|entry| entry.booked_on());
                        booked.is_some_and(|day| boundary <= day.midnight().assume_utc())
                            || boundary <= candidate.instant("created_at")?
                    };
                let after = previous.is_some_and(|prior| prior > index);
                if before && !after {
                    anchor = anchor.min(index);
                } else if before || !after || kind == "void" {
                    // A refund may start before midnight or the exact final
                    // endpoint, wait for plan ownership, then recognize the day.
                    // Its old rows retain no exact recognition clock. A guessed
                    // order could make incorrect allocations appear matched.
                    customer.unsupported("plan_accounting_effects", *id, "ambiguousZeroRecognitionOrder", "Retained sources do not prove the zero service day's order relative to a refund or terminal void");
                    ambiguous = true;
                }
            }
            anchors.insert(*id, anchor);
        }
        if ambiguous {
            customer
                .result
                .coverage
                .not_compared
                .push(format!("planRecognitionSchedule/{period_id}"));
            customer.result.evidence.push(serde_json::json!({"organizationId":customer.organization,"sourceTable":"plan_accounting_periods","sourceId":period_id,"scheduleCompared":false,"reason":"Zero recognition chronology is not retained sufficiently to prove the refund allocation"}));
            continue;
        }
        effects.sort_by_key(|(id, source)| {
            let position = positions.get(id).copied().unwrap_or_else(|| {
                if source.text("kind").ok().as_deref() == Some("activation") {
                    0
                } else {
                    anchors.get(id).copied().unwrap_or(u64::MAX)
                }
            });
            (
                position,
                positions.contains_key(id),
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

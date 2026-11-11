//! Version-two signed accounting movements compared with official source entries.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use super::{Customer, refunds};
use crate::financial_reconciliation::ledger;
use crate::financial_reconciliation::sources::Source;
use crate::financial_reconciliation::types::{self, Amounts};
use crate::{TimeBasis, WalletError, error};

pub(super) fn compare(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    let effects: Vec<_> = customer
        .sources
        .table("plan_accounting_effects")
        .iter()
        .map(|(id, source)| (*id, source.clone()))
        .collect();
    let mut totals = BTreeMap::<Uuid, (Amounts, Amounts)>::new();
    for (id, effect) in effects {
        let period_id = effect.id("plan_period_id")?;
        let generation = effect.optional_id("journal_id")?;
        let Some(period) = generation
            .and_then(|id| customer.sources.get("invoice_accounting_generations", id))
            .or_else(|| customer.sources.get("plan_accounting_periods", period_id))
            .cloned()
        else {
            customer.unsupported(
                "plan_accounting_effects",
                id,
                "period",
                "Daily accounting effect has no captured original period",
            );
            continue;
        };
        let kind = effect.text("kind")?;
        let source = effect.id("source_ref")?;
        let journal = period.id("journal_id")?;
        let expected = amounts(&effect)?;
        let mut valid = if generation.is_some() {
            period.policy()? == 2 && period.id("plan_period_id")? == period_id
        } else {
            period_source(customer, &period)?
        };
        let key = match kind.as_str() {
            "activation" => format!("payment-order/{source}/prepay"),
            "recognition" => match generation {
                Some(journal) => format!(
                    "plan-recognition/{period_id}/journal/{journal}/{}",
                    effect.date("service_day")?
                ),
                None => format!(
                    "plan-recognition/{period_id}/{}",
                    effect.date("service_day")?
                ),
            },
            "correction" => format!("invoice-correction/{journal}/accrual"),
            "refund" => format!("plan-refund-cash/{source}"),
            "dispute" => format!("dispute-recharge/{source}"),
            "void" => format!("plan-journal/{journal}/void"),
            "statement_payment" | "statement_return" => {
                let e = customer
                    .sources
                    .get("statement_payment_effects", source)
                    .ok_or_else(|| error::invalid("statement accounting source missing"))?;
                crate::statement_payments::evidence::key(
                    &e.text("kind")?,
                    e.id("source_ref")?,
                    e.optional_id("journal_id")?,
                )
            }
            _ => {
                customer.unsupported(
                    "plan_accounting_effects",
                    id,
                    "kind",
                    "Unsupported daily accounting kind",
                );
                continue;
            }
        };
        let mut ids = BTreeSet::new();
        if kind == "refund" {
            let (refund_ids, refund_valid) = refunds::plan_entries(customer, source)?;
            ids = refund_ids;
            valid &= refund_valid;
            if !customer
                .sources
                .get("plan_refund_effects", source)
                .is_some_and(|refund| {
                    refund.optional_id("journal_id").ok() == Some(Some(journal))
                        && refund.optional_id("entry_id").ok()
                            == effect.optional_id("entry_id").ok()
                })
            {
                customer.unsupported(
                    "plan_accounting_effects",
                    id,
                    "refundJournal",
                    "Refund accounting effect differs from its original refund journal",
                );
                valid = false;
            }
        } else if let Some(entry) = effect.optional_id("entry_id")? {
            ids.insert(entry);
            valid &= customer.require_entry(
                "plan_accounting_effects",
                id,
                entry,
                &key,
                &if kind.starts_with("statement_") {
                    format!("statement/{source}")
                } else {
                    format!("daily/{id}")
                },
            );
        } else if expected.fields().iter().any(|amount| *amount != 0)
            || effect.amount("cash_minor")? != 0
            || effect.amount("payable_minor")? != 0
        {
            customer.unsupported(
                "plan_accounting_effects",
                id,
                "missingEntry",
                "A nonzero accounting effect has no official entry",
            );
            valid = false;
        }
        let allowed: &[&str] = match kind.as_str() {
            "activation" => &[ledger::CASH, ledger::DEFERRED],
            "correction" => &[ledger::RECEIVABLE, ledger::REVENUE, ledger::DEFERRED],
            "recognition" => &[ledger::DEFERRED, ledger::REVENUE],
            "refund" => &[
                ledger::CASH,
                ledger::DEFERRED,
                ledger::DISCOUNTS,
                ledger::PAYABLE,
                ledger::RECEIVABLE,
                ledger::SUSPENSE,
            ],
            "statement_payment" | "statement_return" | "dispute" => &[
                ledger::CASH,
                ledger::RECEIVABLE,
                ledger::PAYABLE,
                ledger::SUSPENSE,
            ],
            "void" => &[
                ledger::DISCOUNTS,
                ledger::DEFERRED,
                ledger::RECEIVABLE,
                ledger::PAYABLE,
            ],
            _ => &[],
        };
        valid &= customer.shape("plan_accounting_effects", id, &ids, allowed);
        let actual = customer.ledger.financial_amounts(&ids, false)?;
        customer.compare_amount(
            "plan_accounting_effects",
            id,
            "cashMinor",
            effect.amount("cash_minor")?,
            types::sub(0, customer.ledger.net(&ids, ledger::CASH)?)?,
        );
        customer.compare_amount(
            "plan_accounting_effects",
            id,
            "payableMinor",
            effect.amount("payable_minor")?,
            customer.ledger.net(&ids, ledger::PAYABLE)?,
        );
        valid &= effect_source(
            customer, id, &effect, &period, &kind, source, journal, &expected,
        )?;
        let observed_day = effect
            .optional_id("entry_id")?
            .and_then(|entry| customer.ledger.entries.get(&entry))
            .and_then(|entry| entry.booked_on());
        if let Some(day) = observed_day
            && day != effect.date("booked_on")?
        {
            customer.unsupported(
                "plan_accounting_effects",
                id,
                "bookingDate",
                "Effect booking date differs from its official ledger entry",
            );
            valid = false;
        }
        let expected_day = match customer.scope.basis {
            TimeBasis::Booking => effect.date("booked_on")?,
            TimeBasis::Service => effect.date("service_day")?,
            TimeBasis::Payment => effect.date("payment_on")?,
        };
        let service_day = match kind.as_str() {
            "activation" => period.instant("service_start")?.date(),
            "correction" => period.instant("recognition_from")?.date(),
            "recognition" => effect.date("service_day")?, // exact day is also bound by the verified entry key
            "refund" | "dispute" | "statement_payment" | "statement_return" => {
                observed_day.unwrap_or(effect.date("booked_on")?)
            }
            "void" => {
                let before = effect
                    .optional_id("entry_id")?
                    .and_then(|id| customer.ledger.entries.get(&id))
                    .and_then(|e| e.stored.as_ref().ok())
                    .and_then(|e| e.index)
                    .map(|v| v.get());
                let last = customer
                    .sources
                    .table("plan_accounting_effects")
                    .values()
                    .filter(|e| {
                        e.id("plan_period_id").ok() == Some(period_id)
                            && e.optional_id("journal_id").ok() == Some(generation)
                            && e.text("kind").ok().as_deref() == Some("recognition")
                    })
                    .filter(|e| {
                        e.optional_id("entry_id")
                            .ok()
                            .flatten()
                            .and_then(|id| customer.ledger.entries.get(&id))
                            .and_then(|e| e.stored.as_ref().ok())
                            .and_then(|e| e.index)
                            .map(|v| v.get())
                            .is_none_or(|index| before.is_none_or(|before| index < before))
                    })
                    .filter_map(|e| e.date("service_day").ok())
                    .max();
                match last {
                    Some(day) => day
                        .next_day()
                        .ok_or_else(|| error::invalid("recognition day overflows"))?
                        .midnight()
                        .assume_utc()
                        .min(period.instant("service_end")?)
                        .date(),
                    None => {
                        if generation.is_some() {
                            period.instant("recognition_from")?.date()
                        } else {
                            period.instant("service_start")?.date()
                        }
                    }
                }
            }
            _ => expected_day,
        };
        let payment_day = if matches!(
            kind.as_str(),
            "refund" | "dispute" | "statement_payment" | "statement_return"
        ) {
            observed_day.unwrap_or(effect.date("booked_on")?)
        } else {
            period.date("payment_on")?
        };
        if effect.date("service_day")? != service_day || effect.date("payment_on")? != payment_day {
            customer.unsupported(
                "plan_accounting_effects",
                id,
                "timeAttribution",
                "Service or payment date differs from original sources and official cash booking",
            );
            valid = false;
        }
        let actual_day = Some(match customer.scope.basis {
            TimeBasis::Booking => observed_day.unwrap_or(effect.date("booked_on")?),
            TimeBasis::Service => service_day,
            TimeBasis::Payment => payment_day,
        });
        if customer.scope.contains(Some(expected_day)) {
            customer.result.expected.add(&expected)?;
        }
        if customer.scope.contains(actual_day) {
            customer.result.actual.add(&actual)?;
        }
        let aggregate = totals.entry(journal).or_default();
        aggregate.0.add(&expected)?;
        aggregate.1.add(&actual)?;
        customer.record("plan_accounting_effects",id,&expected,&actual,&ids,valid,serde_json::json!({"period":period.evidence,"kind":kind,"sourceRef":source,"ledgerBookingDate":observed_day.map(|day|day.to_string()),"payableMinor":customer.ledger.net(&ids,ledger::PAYABLE)?,"suspenseMinor":types::sub(0,customer.ledger.net(&ids,ledger::SUSPENSE)?)?}));
    }
    let periods: Vec<_> = customer
        .sources
        .table("plan_accounting_periods")
        .values()
        .chain(
            customer
                .sources
                .table("invoice_accounting_generations")
                .values()
                .filter(|g| g.policy().ok() == Some(2)),
        )
        .cloned()
        .collect();
    for period in periods {
        let period_id = period.id("plan_period_id")?;
        let generation = period
            .optional_id("predecessor_journal_id")
            .ok()
            .flatten()
            .map(|_| period.id("journal_id"))
            .transpose()?;
        let journal = period.id("journal_id")?;
        let count = customer
            .sources
            .table("plan_accounting_effects")
            .values()
            .filter(|effect| {
                effect.id("plan_period_id").ok() == Some(period_id)
                    && effect.optional_id("journal_id").ok() == Some(generation)
                    && effect.text("kind").ok().as_deref()
                        == Some(if generation.is_some() {
                            "correction"
                        } else {
                            "activation"
                        })
            })
            .count();
        if count != 1 {
            customer.unsupported(
                "plan_accounting_periods",
                period_id,
                "activation",
                "An accounted period must have exactly one original activation effect",
            );
        }
        if let Some(header) = customer.sources.get("invoice_journals", journal).cloned() {
            let voided = header.text("state")? == "void";
            let line_total = customer
                .sources
                .table("invoice_journal_lines")
                .values()
                .filter(|line| line.id("journal_id").ok() == Some(journal))
                .try_fold(0, |sum, line| types::add(sum, line.amount("amount_minor")?))?;
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
            if header.policy()? != 2
                || header.optional_id("plan_period_id")?
                    != if generation.is_some() {
                        None
                    } else {
                        Some(period_id)
                    }
                || header.id("customer_plan_id")? != period.id("customer_plan_id")?
                || header.id("business_id")? != period.id("business_id")?
            {
                customer.unsupported("invoice_journals",journal,"dailyOwnership","Daily journal policy or original period ownership differs from the captured accounting period");
            }
            let (expected, actual) = totals.remove(&journal).unwrap_or_default();
            customer.compare_amount(
                "invoice_journals",
                journal,
                "dailyBilledMinor",
                if voided {
                    0
                } else {
                    header.amount("total_minor")?
                },
                actual.billed_minor,
            );
            customer.compare_amount(
                "invoice_journals",
                journal,
                "dailyCollectedMinor",
                if voided {
                    0
                } else {
                    header.amount("settled_minor")?
                },
                actual.collected_minor,
            );
            customer.result.evidence.push(serde_json::json!({"organizationId":customer.organization,"sourceTable":"invoice_journals","sourceId":journal,"source":header.evidence,"effectTotals":expected,"ledgerTotals":actual}));
        } else {
            customer.unsupported(
                "plan_accounting_periods",
                period_id,
                "journal",
                "Accounted period has no original journal",
            );
        }
    }
    let missing: Vec<_> = customer
        .sources
        .table("invoice_journals")
        .iter()
        .filter(|(_, journal)| {
            journal.policy().ok() == Some(2)
                && journal.text("state").ok().as_deref() != Some("draft")
                && customer
                    .sources
                    .get(
                        "invoice_accounting_generations",
                        journal.id("journal_id").unwrap_or_default(),
                    )
                    .is_none()
                && !customer
                    .sources
                    .table("plan_accounting_periods")
                    .values()
                    .any(|p| p.id("journal_id").ok() == journal.id("journal_id").ok())
        })
        .map(|(id, _)| *id)
        .collect();
    for journal in missing {
        customer.unsupported(
            "invoice_journals",
            journal,
            "missingDailyPeriod",
            "Daily journal has no immutable original accounting period",
        );
    }
    Ok(())
}

pub(super) fn amounts(source: &Source) -> Result<Amounts, WalletError> {
    Ok(Amounts {
        billed_minor: source.amount("billed_minor")?,
        earned_minor: source.amount("earned_minor")?,
        deferred_minor: source.amount("deferred_minor")?,
        collected_minor: source.amount("collected_minor")?,
        receivable_minor: source.amount("receivable_minor")?,
        refund_minor: 0,
    })
}

fn period_source(customer: &mut Customer<'_>, period: &Source) -> Result<bool, WalletError> {
    let id = period.id("plan_period_id")?;
    let original = customer.sources.get("plan_periods", id).cloned();
    let valid = if let Some(original) = original {
        original.id("customer_plan_id")? == period.id("customer_plan_id")?
            && original.instant("period_start")? == period.instant("service_start")?
            && original.instant("period_end")? == period.instant("service_end")?
            && original.text("authorization_kind")? == period.text("authorization_kind")?
            && original.id("authorization_ref")? == period.id("authorization_ref")?
            && period.id("organization_id")? == customer.organization
    } else {
        false
    };
    let version = customer
        .sources
        .get("sold_plans", period.id("plan_version_id")?)
        .cloned();
    let version_valid = if let Some(version) = version {
        version.policy()? == 2
            && version.amount("price_minor")? == period.amount("fee_minor")?
            && version.id("business_id")? == period.id("business_id")?
    } else {
        false
    };
    let valid = valid && version_valid;
    if !valid {
        customer.unsupported(
            "plan_accounting_periods",
            id,
            "originalTerms",
            "Accounting snapshot differs from its original authorized customer period",
        );
    }
    Ok(valid)
}

#[allow(clippy::too_many_arguments)]
fn effect_source(
    customer: &mut Customer<'_>,
    id: Uuid,
    effect: &Source,
    period: &Source,
    kind: &str,
    source: Uuid,
    journal: Uuid,
    expected: &Amounts,
) -> Result<bool, WalletError> {
    let mut valid = true;
    let start = period.instant("service_start")?;
    let end = period.instant("service_end")?;
    let period_id = period.id("plan_period_id")?;
    match kind {
        "activation" => {
            valid &= source == period.id("authorization_ref")?
                && effect.date("service_day")? == start.date()
                && effect.date("payment_on")? == period.date("payment_on")?;
            customer.compare_amount(
                "plan_accounting_effects",
                id,
                "originalFeeMinor",
                period.amount("fee_minor")?,
                expected.billed_minor,
            );
            if period.text("authorization_kind")? == "payment_order" {
                if let Some(order) = customer.sources.get("payment_orders", source).cloned() {
                    valid &= order.text("purpose")? == "plan_period"
                        && order.optional_id("purpose_ref")?
                            == Some(period.id("customer_plan_id")?)
                        && matches!(order.text("status")?.as_str(), "paid" | "fulfilled")
                        && order.optional_day("paid_at")? == Some(period.date("payment_on")?);
                    let terms = order.json("plan_payment")?;
                    valid &= terms
                        .get("planVersionId")
                        .and_then(serde_json::Value::as_str)
                        == Some(period.id("plan_version_id")?.to_string().as_str())
                        && terms.get("feeMinor").and_then(serde_json::Value::as_i64)
                            == Some(period.amount("fee_minor")?);
                    customer.compare_amount(
                        "plan_accounting_effects",
                        id,
                        "originalPaidMinor",
                        order.amount("amount_minor")?,
                        expected.collected_minor,
                    );
                } else {
                    valid = false;
                }
            } else {
                valid &= period.amount("fee_minor")? == 0;
            }
        }
        "correction" => {
            valid &= source == period.id("correction_id")?
                && effect.date("service_day")? == period.instant("recognition_from")?.date()
                && expected.billed_minor == period.amount("fee_minor")?
                && expected.collected_minor == 0
                && expected.receivable_minor == expected.billed_minor
                && effect.amount("cash_minor")? == 0
                && effect.amount("payable_minor")? == 0;
        }
        "recognition" => {
            let day = effect.date("service_day")?;
            let boundary = day
                .next_day()
                .ok_or_else(|| error::invalid("recognition date overflows"))?
                .midnight()
                .assume_utc()
                .min(end);
            valid &= source == period_id
                && boundary > start
                && boundary > day.midnight().assume_utc()
                && day >= start.date()
                && day <= end.date()
                && effect.date("payment_on")? == period.date("payment_on")?
                && boundary <= customer.captured_at;
        }
        "dispute" => {
            if let Some(dispute) = customer.sources.get("disputes", source).cloned() {
                valid &= dispute.id("order_id")? == period.id("authorization_ref")?
                    && dispute.optional_id("recharge_entry_id")?
                        == effect.optional_id("entry_id")?
                    && matches!(dispute.text("state")?.as_str(), "lost" | "closed");
                customer.compare_amount(
                    "plan_accounting_effects",
                    id,
                    "externalLossMinor",
                    dispute.amount("amount_minor")?,
                    types::sub(0, effect.amount("cash_minor")?)?,
                );
            } else {
                valid = false;
            }
        }
        "void" => {
            valid &= source == journal
                && customer
                    .sources
                    .get("invoice_journals", journal)
                    .is_some_and(|header| header.text("state").ok().as_deref() == Some("void"))
        }
        "statement_payment" | "statement_return" => {
            if let Some(e) = customer.sources.get("statement_payment_effects", source) {
                let sign = if e.text("kind")? == "payment" { 1 } else { -1 };
                valid &= e.optional_id("journal_id")? == Some(journal)
                    && e.optional_id("entry_id")? == effect.optional_id("entry_id")?
                    && expected.collected_minor == sign * e.amount("receivable_minor")?
                    && expected.receivable_minor == -sign * e.amount("receivable_minor")?
                    && expected.billed_minor == 0
                    && expected.earned_minor == 0
                    && expected.deferred_minor == 0;
            } else {
                valid = false;
            }
        }
        "refund" => {}
        _ => valid = false,
    }
    if !valid {
        customer.unsupported(
            "plan_accounting_effects",
            id,
            "originalSource",
            "Effect source identity, ownership or original time attribution is inconsistent",
        );
    }
    Ok(valid)
}

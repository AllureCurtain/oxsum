//! Checked correction bounds and cumulative recognition without financial writes.

use std::collections::{BTreeMap, BTreeSet};

use time::OffsetDateTime;

use super::{
    InvoiceCorrectionAmounts, InvoiceCorrectionPreviewLine, InvoiceCorrectionPreviewRequest,
};
use crate::{InvoiceJournalLine, JournalLineKind, WalletError, error, plan_recognition};

/// Preserve every original credit and permit only bounded source-charge reductions.
pub(super) fn lines(
    originals: &[InvoiceJournalLine],
    request: &InvoiceCorrectionPreviewRequest,
) -> Result<Vec<InvoiceCorrectionPreviewLine>, WalletError> {
    if request.lines.is_empty() || request.lines.len() > 100 {
        return Err(error::invalid(
            "select between one and 100 original charge lines",
        ));
    }
    let mut selected = BTreeMap::new();
    for line in &request.lines {
        if line.amount_minor < 0
            || selected
                .insert(line.source_line_id, line.amount_minor)
                .is_some()
        {
            return Err(error::invalid(
                "correction lines must be distinct and nonnegative",
            ));
        }
    }
    let mut seen = BTreeSet::new();
    let mut answer = Vec::with_capacity(originals.len());
    for original in originals {
        if !seen.insert(original.line_id) {
            return Err(error::invalid("original correction lines are duplicated"));
        }
        let corrected = selected.remove(&original.line_id);
        if corrected.is_some_and(|amount| {
            original.kind != JournalLineKind::BaseFee
                || original.amount_minor < 0
                || amount > original.amount_minor
        }) {
            return Err(error::invalid(
                "correction must reduce a proven original base fee",
            ));
        }
        answer.push(InvoiceCorrectionPreviewLine {
            source_line_id: original.line_id,
            kind: original.kind,
            description: original.description.clone(),
            original_minor: original.amount_minor,
            corrected_minor: corrected.unwrap_or(original.amount_minor),
        });
    }
    if !selected.is_empty() {
        return Err(error::invalid(
            "correction line does not belong to the original invoice",
        ));
    }
    answer.sort_by_key(|line| line.source_line_id);
    Ok(answer)
}

/// Allocate new consideration without consuming or relabeling original-source cash.
pub(super) fn calculate(
    lines: &[InvoiceCorrectionPreviewLine],
    retained_payable: i64,
    policy: i16,
    start: OffsetDateTime,
    end: OffsetDateTime,
    cutoff: OffsetDateTime,
) -> Result<(InvoiceCorrectionAmounts, OffsetDateTime), WalletError> {
    if retained_payable < 0 || !matches!(policy, 1 | 2) || end <= start {
        return Err(error::invalid(
            "invalid correction policy, service interval or payable",
        ));
    }
    let (original, corrected) =
        lines
            .iter()
            .try_fold((0i128, 0i128), |(before, after), line| {
                if line.corrected_minor > line.original_minor {
                    return Err(error::invalid(
                        "correction increases original consideration",
                    ));
                }
                Ok((
                    before + i128::from(line.original_minor),
                    after + i128::from(line.corrected_minor),
                ))
            })?;
    let original = i64::try_from(original).map_err(error::invalid)?;
    let corrected = i64::try_from(corrected).map_err(error::invalid)?;
    if original < 0 || corrected < 0 || corrected > original {
        return Err(error::invalid(
            "corrected consideration must remain within the original net fee",
        ));
    }
    let boundary = plan_recognition::schedule::completed_boundary(start, end, cutoff);
    let earned = if policy == 1 {
        corrected
    } else {
        plan_recognition::schedule::earned_at(corrected, start, end, boundary)?
    };
    Ok((
        InvoiceCorrectionAmounts {
            original_total_minor: original,
            corrected_total_minor: corrected,
            successor_collected_minor: 0,
            successor_receivable_minor: corrected,
            successor_earned_minor: earned,
            successor_deferred_minor: corrected - earned,
            original_payable_minor: retained_payable,
            cash_movement_minor: 0,
        },
        boundary,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::InvoiceCorrectionLineRequest;
    use proptest::prelude::*;
    use time::macros::datetime;
    use uuid::Uuid;

    fn source_line(id: u128, kind: JournalLineKind, amount: i64) -> InvoiceJournalLine {
        InvoiceJournalLine {
            line_id: Uuid::from_u128(id),
            journal_id: Uuid::nil(),
            kind,
            amount_minor: amount,
            description: "Original source".into(),
            source_ref: Some(Uuid::from_u128(10)),
            created_at: datetime!(2026-01-01 00:00 UTC),
        }
    }

    fn request(id: u128, amount: i64) -> InvoiceCorrectionPreviewRequest {
        InvoiceCorrectionPreviewRequest {
            lines: vec![InvoiceCorrectionLineRequest {
                source_line_id: Uuid::from_u128(id),
                amount_minor: amount,
            }],
        }
    }

    #[test]
    fn retains_prior_refund_and_promo_credits_and_rejects_negative_net_fee() {
        let original = [
            source_line(1, JournalLineKind::BaseFee, 100),
            source_line(2, JournalLineKind::Discount, -20),
            source_line(3, JournalLineKind::PromoCredit, -5),
        ];
        let proposed = lines(&original, &request(1, 80)).unwrap();
        let start = datetime!(2026-01-01 00:00 UTC);
        let end = datetime!(2026-01-31 00:00 UTC);
        let (amounts, _) = calculate(&proposed, 75, 1, start, end, end).unwrap();
        assert_eq!(amounts.original_total_minor, 75);
        assert_eq!(amounts.corrected_total_minor, 55);
        assert_eq!(amounts.original_payable_minor, 75);
        let below_credits = lines(&original, &request(1, 24)).unwrap();
        assert!(calculate(&below_credits, 75, 1, start, end, end).is_err());
        assert!(lines(&original, &request(2, 0)).is_err());
        assert!(lines(&original, &request(3, 0)).is_err());
    }

    #[test]
    fn rejects_foreign_duplicate_increased_and_unsupported_charge_selections() {
        let original = [source_line(1, JournalLineKind::BaseFee, 100)];
        for bad in [request(2, 50), request(1, 101), request(1, -1)] {
            assert!(lines(&original, &bad).is_err());
        }
        let mut duplicate = request(1, 50);
        duplicate.lines.push(duplicate.lines[0].clone());
        assert!(lines(&original, &duplicate).is_err());
        assert!(
            lines(
                &original,
                &InvoiceCorrectionPreviewRequest { lines: Vec::new() }
            )
            .is_err()
        );
        for kind in [JournalLineKind::UsageOverage, JournalLineKind::Tax] {
            let old = [source_line(1, kind, 100)];
            assert!(lines(&old, &request(1, 50)).is_err());
        }
    }

    #[test]
    fn partial_service_uses_completed_utc_boundary_and_keeps_payable_separate() {
        let original = [source_line(1, JournalLineKind::BaseFee, 100)];
        let proposed = lines(&original, &request(1, 80)).unwrap();
        let start = datetime!(2028-02-28 12:00 UTC);
        let end = datetime!(2028-03-01 06:00 UTC);
        for paid in [0, 30, 100] {
            let (a, boundary) = calculate(
                &proposed,
                paid,
                2,
                start,
                end,
                datetime!(2028-02-29 12:00 UTC),
            )
            .unwrap();
            assert_eq!(boundary, datetime!(2028-02-29 00:00 UTC));
            assert_eq!(a.successor_earned_minor, 22); // floor(80 * 12 / 42)
            assert_eq!(a.successor_deferred_minor, 58);
            assert_eq!(a.successor_receivable_minor, 80);
            assert_eq!(a.successor_collected_minor, 0);
            assert_eq!(a.original_payable_minor, paid);
            assert_eq!(a.cash_movement_minor, 0);
        }
        let (a, _) = calculate(&proposed, 100, 2, start, end, end).unwrap();
        assert_eq!(a.successor_earned_minor, 80);
        assert_eq!(a.successor_deferred_minor, 0);
    }

    #[test]
    fn maximum_integer_credits_and_final_minor_do_not_overflow_or_disappear() {
        let original = [
            source_line(1, JournalLineKind::BaseFee, i64::MAX),
            source_line(2, JournalLineKind::Discount, -(i64::MAX - 1)),
        ];
        let proposed = lines(&original, &request(1, i64::MAX)).unwrap();
        let start = datetime!(2026-01-01 00:00 UTC);
        let end = datetime!(2026-01-02 00:00 UTC);
        let (before, _) = calculate(&proposed, 1, 2, start, end, start).unwrap();
        let (after, _) = calculate(&proposed, 1, 2, start, end, end).unwrap();
        assert_eq!(before.successor_deferred_minor, 1);
        assert_eq!(after.successor_earned_minor, 1);
        let (zero, _) = calculate(
            &lines(&original, &request(1, i64::MAX - 1)).unwrap(),
            1,
            2,
            start,
            end,
            end,
        )
        .unwrap();
        assert_eq!(zero.successor_receivable_minor, 0);
        assert_eq!(zero.original_payable_minor, 1);
    }

    proptest! {
        #[test]
        fn every_partial_payment_conserves_both_generations(
            original_fee in 0i64..=i64::MAX,
            corrected_fraction in 0u16..=1000,
            paid_fraction in 0u16..=1000,
            elapsed_days in 0i64..=30,
            policy in 1i16..=2,
        ) {
            let fee = (i128::from(original_fee) * i128::from(corrected_fraction) / 1000) as i64;
            let paid = (i128::from(original_fee) * i128::from(paid_fraction) / 1000) as i64;
            let old = [source_line(1, JournalLineKind::BaseFee, original_fee)];
            let proposed = lines(&old, &request(1, fee)).unwrap();
            let start = datetime!(2026-01-01 00:00 UTC);
            let end = datetime!(2026-01-31 00:00 UTC);
            let (a, _) = calculate(&proposed, paid, policy, start, end, start + time::Duration::days(elapsed_days)).unwrap();
            prop_assert_eq!(a.corrected_total_minor, a.successor_earned_minor + a.successor_deferred_minor);
            prop_assert_eq!(a.successor_receivable_minor, a.corrected_total_minor - a.successor_collected_minor);
            prop_assert_eq!(a.original_payable_minor, paid);
            // Original cash less old payable plus new receivable equals the new fee.
            prop_assert_eq!(i128::from(paid) - i128::from(a.original_payable_minor) + i128::from(a.successor_receivable_minor), i128::from(fee));
            prop_assert_eq!(a.cash_movement_minor, 0);
        }
    }
}

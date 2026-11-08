//! Checked cumulative recognition over an exact UTC service interval.

use time::{Date, OffsetDateTime};

use crate::{WalletError, error};

/// End of a completed UTC service-day, including a partial last day.
pub(super) fn day_end(
    day: Date,
    service_end: OffsetDateTime,
) -> Result<OffsetDateTime, WalletError> {
    let next = day
        .next_day()
        .ok_or_else(|| error::invalid("service day overflows"))?;
    Ok(next.midnight().assume_utc().min(service_end))
}

/// Cumulative minor units at an exact completed boundary. Microsecond duration
/// matches PostgreSQL timestamptz precision; intermediate products use i128.
pub(super) fn earned_at(
    net_fee: i64,
    start: OffsetDateTime,
    end: OffsetDateTime,
    cutoff: OffsetDateTime,
) -> Result<i64, WalletError> {
    if net_fee < 0 || end <= start {
        return Err(error::invalid("invalid plan recognition interval or fee"));
    }
    let duration = (end - start).whole_microseconds();
    if duration <= 0 {
        return Err(error::invalid(
            "recognition interval is below stored timestamp precision",
        ));
    }
    let elapsed = (cutoff.min(end).max(start) - start).whole_microseconds();
    let amount = i128::from(net_fee)
        .checked_mul(elapsed)
        .ok_or_else(|| error::invalid("recognition amount overflows"))?
        / duration;
    i64::try_from(amount).map_err(|_| error::invalid("recognition amount overflows"))
}

/// The last service boundary completed at cutoff, rather than the current open
/// day's partial passage. An ended period includes its exact final endpoint.
pub(super) fn completed_boundary(
    start: OffsetDateTime,
    end: OffsetDateTime,
    cutoff: OffsetDateTime,
) -> OffsetDateTime {
    if cutoff >= end {
        end
    } else {
        cutoff.date().midnight().assume_utc().max(start)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use time::Duration;
    use time::macros::datetime;

    #[test]
    fn partial_days_allocate_the_final_minor_exactly() {
        let start = datetime!(2028-02-28 12:00 UTC);
        let end = datetime!(2028-03-01 06:00 UTC);
        let first = datetime!(2028-02-29 00:00 UTC);
        let second = datetime!(2028-03-01 00:00 UTC);
        assert_eq!(earned_at(7, start, end, first).unwrap(), 2);
        assert_eq!(earned_at(7, start, end, second).unwrap(), 6);
        assert_eq!(earned_at(7, start, end, end).unwrap(), 7);
        assert_eq!(
            completed_boundary(start, end, start + Duration::hours(1)),
            start
        );
        assert_eq!(day_end(end.date(), end).unwrap(), end);
    }

    #[test]
    fn tiny_and_maximum_fees_never_lose_the_final_remainder() {
        let start = datetime!(2026-01-31 23:59:59.999999 UTC);
        let end = datetime!(2026-02-28 00:00 UTC);
        for fee in [0, 1, 17, i64::MAX] {
            assert_eq!(earned_at(fee, start, end, start).unwrap(), 0);
            assert_eq!(earned_at(fee, start, end, end).unwrap(), fee);
            assert!(earned_at(fee, start, end, end - Duration::microseconds(1)).unwrap() <= fee);
        }
    }
}

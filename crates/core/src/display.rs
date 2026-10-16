//! Multi-currency display (issue #257): the presentational conversion between a
//! credit amount and the fiat unit an organization reads its bills in.
//!
//! The ledger only ever holds integer minor units of one credit — this module
//! converts for display and is never an input to a charge, a freeze or a proof.
//! The conversion itself stays in integer arithmetic: a [`DisplayRate`] is a
//! decimal stored as text and parsed to a mantissa plus a scale, so converting
//! a minor-unit figure is one `i128` multiply and one divide — no float ever
//! touches money.

use serde::{Deserialize, Serialize};

use crate::WalletError;
use crate::wallet::SCALE;

/// The fiat unit an organization displays its credit figures in.
///
/// ISO 4217-shaped: three uppercase letters, which is also all the database
/// column admits. `DisplayCurrency` carries the code and nothing else — what a
/// credit converts to it at is the [`DisplayRate`] beside it, and the two are
/// written and cleared together (`Db::set_display_currency`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayCurrency {
    /// The ISO 4217 code — `USD`, `EUR`, `CNY`. Informational: the conversion
    /// math needs only the rate, so an unrecognized-but-well-formed code still
    /// renders, and the deployer answers for what the unit means.
    pub code: String,
    /// How many of the unit's major amount one credit displays as, kept as the
    /// decimal text it was configured with (`"7.20"`).
    pub rate_per_credit: String,
    /// Decimal places the unit renders with: 2 for most currencies, 0 for
    /// JPY-shaped ones. The render rounds half-up to this precision.
    pub decimals: u8,
}

/// A display currency as a confirmed conversion: the code, and the rate parsed
/// into the integer form `convert` computes with.
#[derive(Debug, Clone)]
pub struct Display {
    currency: DisplayCurrency,
    /// `rate_per_credit` as `mantissa / 10^scale` units of currency per credit.
    mantissa: i128,
    scale: u32,
}

impl Display {
    /// The currency descriptor this display converts into.
    #[must_use]
    pub fn currency(&self) -> &DisplayCurrency {
        &self.currency
    }

    /// What `minor` credits-minor displays as, rendered with the currency's
    /// decimals — `1_500_000` minor at rate `0.0014` USD and 2 decimals is
    /// `"2.10"`. Integer arithmetic throughout: the minor figure is exact, the
    /// rate's mantissa/scale division rounds half-up at the render's own
    /// precision, and the sign rides the amount, never the rate.
    #[must_use]
    pub fn render(&self, minor: i64) -> String {
        let scale = self.currency.decimals as u32;
        // minor / 10^SCALE credits × mantissa / 10^rate_scale units/credit,
        // rendered to `scale` decimals of the unit:
        //   display = minor × mantissa × 10^scale / (10^SCALE × 10^rate_scale)
        // Split into a multiply and a divide with i128 headroom: minor is
        // i64-bounded and the rate is input-bounded, so the numerator cannot
        // overflow what the check below permits.
        let numerator = (minor as i128)
            .saturating_mul(self.mantissa)
            .saturating_mul(pow10(scale));
        let denominator = pow10(SCALE as u32) * pow10(self.scale);
        // Round half-up on the magnitude; the sign is applied afterwards so a
        // negative charge renders `-2.10`, never `2.10-`.
        let negative = numerator < 0;
        let magnitude = numerator.unsigned_abs();
        let rounded = (magnitude + denominator.unsigned_abs() / 2) / denominator.unsigned_abs();
        let unit_scale = pow10(scale).unsigned_abs();
        let whole = rounded / unit_scale;
        let frac = rounded % unit_scale;
        let sign = if negative && rounded > 0 { "-" } else { "" };
        if scale == 0 {
            format!("{sign}{whole}")
        } else {
            format!("{sign}{whole}.{:0width$}", frac, width = scale as usize)
        }
    }
}

/// `10^n` as `i128` — the scales involved are small enough that a lookup beats
/// a checked pow on every render.
fn pow10(n: u32) -> i128 {
    debug_assert!(n <= 18, "display scales stay under 10^18");
    10i128.pow(n)
}

/// Parses a `display_currency`/`display_rate_per_credit` pair as stored (or as
/// the request carried them) into a [`Display`]. `None` means the organization
/// renders credits only — the absence is not an error.
///
/// # Errors
///
/// [`WalletError::InvalidInput`] on a half-pair, a malformed code or a rate that
/// is not a positive decimal — the setter validates through here, so what the
/// column's CHECK constrains and what the API refuses are the same reading.
pub fn display_currency(
    code: Option<&str>,
    rate: Option<&str>,
    decimals: u8,
) -> Result<Option<Display>, WalletError> {
    let (Some(code), Some(rate)) = (code, rate) else {
        if code.is_some() || rate.is_some() {
            return Err(WalletError::InvalidInput(
                "display currency needs a code and a rate together".into(),
            ));
        }
        return Ok(None);
    };
    let code = code.trim();
    if !(code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase())) {
        return Err(WalletError::InvalidInput(
            "displayCurrency must be a three-letter ISO 4217 code".into(),
        ));
    }
    let (mantissa, scale) = parse_decimal(rate)?;
    if mantissa <= 0 {
        return Err(WalletError::InvalidInput(
            "displayRatePerCredit must be positive".into(),
        ));
    }
    if decimals > 6 {
        return Err(WalletError::InvalidInput(
            "displayDecimals must be between 0 and 6".into(),
        ));
    }
    Ok(Some(Display {
        currency: DisplayCurrency {
            code: code.to_owned(),
            rate_per_credit: rate.trim().to_owned(),
            decimals,
        },
        mantissa,
        scale,
    }))
}

/// A positive decimal literal into `(mantissa, scale)`: `"7.20"` is `(720, 2)`
/// and `"0.0014"` is `(14, 4)`. Trailing zeros on the fraction are kept in the
/// stored string but the mantissa they produce converts identically — the
/// check the column carries (`^\d+(\.\d+)?$`) is the same shape this accepts.
fn parse_decimal(rate: &str) -> Result<(i128, u32), WalletError> {
    let invalid = || {
        WalletError::InvalidInput(
            "displayRatePerCredit must be a positive decimal like \"7.20\"".into(),
        )
    };
    let rate = rate.trim();
    let (whole, frac) = match rate.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (rate, ""),
    };
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !(frac.bytes().all(|b| b.is_ascii_digit()))
        || frac.len() > 8
    {
        return Err(invalid());
    }
    let digits = format!("{whole}{frac}");
    let mantissa: i128 = digits.parse().map_err(|_| invalid())?;
    Ok((mantissa, frac.len() as u32))
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn display(rate: &str, decimals: u8) -> Display {
        display_currency(Some("USD"), Some(rate), decimals)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_rate_renders_credits_as_fiat() {
        let usd = display("0.0014", 2);
        // 1.5 credits at $0.0014/credit = $0.0021 → $0.00 at 2dp rounding up.
        assert_eq!(usd.render(1_500_000), "0.00");
        // 1500 credits = $2.10.
        assert_eq!(usd.render(1_500_000_000), "2.10");
    }

    #[test]
    fn the_conversion_is_exact_at_the_render_precision() {
        let usd = display("7.20", 2);
        // 1 credit = 1_000_000 minor → 7.20 USD.
        assert_eq!(usd.render(1_000_000), "7.20");
        // Half-up rounding: 0.5 credit → 3.60, 0.005 credit → 0.04.
        assert_eq!(usd.render(500_000), "3.60");
        assert_eq!(usd.render(5_000), "0.04");
        // Negative amounts render the sign, never a dropped magnitude — but a
        // magnitude that rounds to zero shows no sign, so a -1-minor refund
        // reads 0.00 rather than the lie "-0.00".
        assert_eq!(usd.render(-1_000_000), "-7.20");
        assert_eq!(usd.render(-1), "0.00");
    }

    #[test]
    fn a_zero_decimal_currency_drops_the_fraction() {
        let jpy = display("0.22", 0);
        // 1 credit → 0.22 JPY → 0 at 0 decimals; 10 credits → 2 JPY.
        assert_eq!(jpy.render(10_000_000), "2");
        assert_eq!(jpy.render(1_000_000), "0");
    }

    #[test]
    fn the_pair_moves_together() {
        assert!(display_currency(None, None, 2).unwrap().is_none());
        assert!(display_currency(Some("USD"), None, 2).is_err());
        assert!(display_currency(None, Some("1.0"), 2).is_err());
    }

    #[test]
    fn malformed_rates_and_codes_are_refused() {
        for rate in ["", "-1", "1.2.3", "abc", "1e3", ".5"] {
            assert!(
                display_currency(Some("USD"), Some(rate), 2).is_err(),
                "{rate:?} must refuse"
            );
        }
        for code in ["usd", "US", "USDT", "U1D"] {
            assert!(
                display_currency(Some(code), Some("1"), 2).is_err(),
                "{code:?} must refuse"
            );
        }
        assert!(display_currency(Some("USD"), Some("0"), 2).is_err());
        assert!(display_currency(Some("USD"), Some("0.0"), 2).is_err());
    }
}

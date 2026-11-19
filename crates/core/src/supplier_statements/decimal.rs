//! Bounded exact decimal arithmetic; never pass supplier money through binary floats.
use crate::WalletError;

pub(super) const SCALE: i128 = 1_000_000_000_000_000_000;
pub(super) fn invalid() -> WalletError {
    WalletError::InvalidInput(
        "unsupported or overflowing exact decimal (maximum 18 fractional places)".into(),
    )
}
/// Parse ordinary or exponent notation into exact 10^-18 USD units.
pub(super) fn parse(raw: &str) -> Result<i128, WalletError> {
    if raw.is_empty() || raw.len() > 128 || !raw.is_ascii() || raw.trim() != raw {
        return Err(invalid());
    }
    let (negative, value) = raw.strip_prefix('-').map_or((false, raw), |v| (true, v));
    let mut parts = value.split(['e', 'E']);
    let mantissa = parts.next().ok_or_else(invalid)?;
    let exp = parts
        .next()
        .map(|e| e.parse::<i32>().map_err(|_| invalid()))
        .transpose()?
        .unwrap_or(0);
    if parts.next().is_some() || !(-38..=38).contains(&exp) {
        return Err(invalid());
    }
    let (whole, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || (whole.len() > 1 && whole.starts_with('0'))
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || (mantissa.contains('.') && frac.is_empty())
    {
        return Err(invalid());
    }
    let mut coefficient = 0_i128;
    for digit in whole.bytes().chain(frac.bytes()) {
        coefficient = coefficient
            .checked_mul(10)
            .and_then(|v| v.checked_add(i128::from(digit - b'0')))
            .ok_or_else(invalid)?;
    }
    let power = 18_i32
        .checked_add(exp)
        .and_then(|v| v.checked_sub(frac.len() as i32))
        .ok_or_else(invalid)?;
    if power >= 0 {
        coefficient = coefficient
            .checked_mul(10_i128.checked_pow(power as u32).ok_or_else(invalid)?)
            .ok_or_else(invalid)?;
    } else {
        let divisor = 10_i128.checked_pow((-power) as u32).ok_or_else(invalid)?;
        if coefficient % divisor != 0 {
            return Err(invalid());
        }
        coefficient /= divisor;
    }
    if negative {
        coefficient = coefficient.checked_neg().ok_or_else(invalid)?;
    }
    Ok(coefficient)
}
pub(super) fn text(value: i128) -> String {
    let abs = value.unsigned_abs();
    let whole = abs / SCALE as u128;
    let frac = abs % SCALE as u128;
    let sign = if value < 0 { "-" } else { "" };
    if frac == 0 {
        format!("{sign}{whole}")
    } else {
        format!(
            "{sign}{whole}.{}",
            format!("{frac:018}").trim_end_matches('0')
        )
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[test]
    fn precise_signed_exponents_and_hostile_literals() {
        for (s, want) in [
            ("0.06", "0.06"),
            ("1e-7", "0.0000001"),
            ("-0.5", "-0.5"),
            ("-1.5", "-1.5"),
            ("12.34000000001", "12.34000000001"),
            ("1E+2", "100"),
        ] {
            assert_eq!(text(parse(s).unwrap()), want);
        }
        for s in [
            "",
            " 1",
            "01",
            ".5",
            "1.",
            "NaN",
            "1e100",
            "1e-19",
            "1.123456x",
            "1.12345😀",
            "170141183460469231731687303715884105728",
            "1e2e3",
        ] {
            assert!(parse(s).is_err(), "{s}");
        }
        assert_eq!(
            text(parse("0.0000006").unwrap() + parse("0.0000006").unwrap()),
            "0.0000012"
        );
    }
}

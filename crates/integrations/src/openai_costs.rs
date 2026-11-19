//! Legacy per-channel OpenAI cost synchronization is intentionally disabled.
//!
//! The real Costs API is organization/project scoped. Assigning its whole total
//! to every compatible channel invents coverage and duplicates costs. Use the
//! explicitly scoped immutable raw-page import in Business Reports instead.
use crate::provider_http::ProviderHttp;
use oxsum_core::{ChannelBilling, DailyBill, UpstreamBillProvider};
use time::Date;

/// Compatibility provider that never attributes an organization total to a channel.
#[derive(Debug)]
pub struct OpenAiBillProvider;
impl OpenAiBillProvider {
    /// Retains constructor compatibility; no network request is issued.
    #[must_use]
    pub fn new(_http: ProviderHttp) -> Self {
        Self
    }
}
impl UpstreamBillProvider for OpenAiBillProvider {
    fn daily_cost<'a>(
        &'a self,
        _channel: &'a ChannelBilling,
        _date: Date,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<DailyBill>> + Send + 'a>> {
        Box::pin(async { None })
    }
}
/// Strict signed decimal-to-micro conversion for the existing Anthropic adapter.
/// Truncation is toward zero after validating every fractional byte.
pub(crate) fn parse_minor(value: &str) -> Option<i64> {
    let value = value.trim();
    let (negative, text) = value
        .strip_prefix('-')
        .map_or((false, value), |s| (true, s));
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || (text.contains('.') && frac.is_empty())
    {
        return None;
    }
    let whole = whole.parse::<i128>().ok()?;
    let mut digits = frac
        .bytes()
        .take(6)
        .fold(0_i128, |a, b| a * 10 + i128::from(b - b'0'));
    for _ in frac.len().min(6)..6 {
        digits *= 10;
    }
    let amount = whole.checked_mul(1_000_000)?.checked_add(digits)?;
    i64::try_from(if negative {
        amount.checked_neg()?
    } else {
        amount
    })
    .ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_decimal_is_strict_and_unicode_cannot_panic() {
        for (raw, want) in [
            ("12.34", 12340000),
            ("-1.5", -1500000),
            ("-0.5", -500000),
            ("0.0000009", 0),
            ("1.9999999", 1999999),
        ] {
            assert_eq!(parse_minor(raw), Some(want));
        }
        for raw in [
            "",
            "12.",
            "1.123456junk",
            "1.12345😀",
            "1e3",
            "9223372036854775808",
        ] {
            assert_eq!(parse_minor(raw), None);
        }
    }
    #[tokio::test]
    async fn unscoped_openai_total_is_never_attributed_to_a_channel() {
        let provider = OpenAiBillProvider::new(ProviderHttp::default());
        let channel = ChannelBilling {
            channel_id: uuid::Uuid::new_v4(),
            name: "fixture".into(),
            protocol: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            bill_key: Some("never-sent".into()),
            destination_policy: Default::default(),
        };
        assert!(
            provider
                .daily_cost(&channel, time::macros::date!(2026 - 11 - 01))
                .await
                .is_none()
        );
    }
}

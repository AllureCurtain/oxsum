//! The OpenAI Costs provider (issue #283): the narrow client the
//! `upstream-bill-sync` job pulls a channel's billed cost through — `GET
//! /v1/organization/costs`, the admin-side usage API, answering what the
//! organization actually paid for a day rather than what oxsum estimated.
//!
//! The endpoint is day-grained — `bucket_width=1d` returns one bucket per UTC
//! day with a `results` roll-up per group — and org-scoped: the billed total an
//! admin key sees is the whole organization that key governs, which is exactly
//! the granularity a channel's `bill_key_sealed` was provisioned for. The
//! amount is a decimal string in the org's billing currency (`usd`); the
//! client parses it into minor units rather than trust a float — money is
//! integer, always (the root hard rule).
//!
//! Only `protocol == "openai"` resolves to a bill — an Anthropic channel's cost
//! API is a second implementation (the issue's PR 2), and a channel speaking
//! anything else has no provider to ask.

use serde::Deserialize;
use time::Date;

use oxsum_core::{ChannelBilling, DailyBill, UpstreamBillProvider};

/// The admin costs endpoint's path under a channel's `base_url`.
const COSTS_PATH: &str = "/organization/costs";

/// The client over `OXSUM_*`-independent coordinates — a base URL and the HTTP
/// stack the caller already holds, so a test can stand a stub at the URL. The
/// billing credential arrives per-call on the [`ChannelBilling`], not held here.
#[derive(Debug)]
pub struct OpenAiBillProvider {
    http: reqwest::Client,
}

/// One day's bucket as the Costs API answers it. `results` aggregates whatever
/// `group_by` was asked for — with none, one result per day carrying the total.
#[derive(Deserialize)]
struct CostBucket {
    /// Bucket start, unix seconds — read for the struct's shape; the day's
    /// window is already fixed by the query's `start_time`/`end_time`.
    #[allow(dead_code)]
    start_time: i64,
    /// The bucket's cost entries — one roll-up when ungrouped.
    #[serde(default)]
    results: Vec<CostResult>,
}

/// One billed-cost entry: a decimal-string amount in `currency`.
#[derive(Deserialize)]
struct CostResult {
    amount: CostAmount,
}

#[derive(Deserialize)]
struct CostAmount {
    /// Decimal text, e.g. `"12.345678"` — parsed to integer minor, never floated.
    value: String,
    /// The billing currency (`usd`); read for completeness, the wallet's minor
    /// units are currency-agnostic credits.
    #[allow(dead_code)]
    currency: String,
}

#[derive(Deserialize)]
struct CostsPage {
    /// The day's buckets — `[start_time, end_time)` windows.
    #[serde(default)]
    buckets: Vec<CostBucket>,
    /// Whether another page follows; the fetch is one day's one page, so a
    /// paged answer is unexpected but read honestly (the first page is the day).
    #[allow(dead_code)]
    has_more: bool,
}

impl OpenAiBillProvider {
    /// Builds the provider around the shared HTTP client the job carries.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// `GET {base_url}/organization/costs?start_time=…&bucket_width=1d` — the
    /// day's billed total, in minor units. Decimal-string amounts are summed
    /// across the day's results and parsed to integers; a malformed decimal or
    /// a missing amount is a `None`, the coverage-gap answer.
    async fn fetch(&self, channel: &ChannelBilling, date: Date) -> Option<DailyBill> {
        let key = channel.bill_key.as_deref()?;
        let start = date.midnight().assume_utc().unix_timestamp();
        // end_time = next midnight — the API's window is [start, end).
        let end = start + 86_400;
        let url = format!(
            "{}{}?start_time={}&end_time={}&bucket_width=1d",
            channel.base_url.trim_end_matches('/'),
            COSTS_PATH,
            start,
            end
        );
        let res = self
            .http
            .get(&url)
            .bearer_auth(key)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .ok()?;
        if !res.status().is_success() {
            return None;
        }
        let page: CostsPage = res.json().await.ok()?;
        let mut total: i64 = 0;
        for bucket in &page.buckets {
            for result in &bucket.results {
                total += parse_minor(&result.amount.value)?;
            }
        }
        Some(DailyBill {
            provider_billed_minor: total,
        })
    }
}

impl UpstreamBillProvider for OpenAiBillProvider {
    fn daily_cost<'a>(
        &'a self,
        channel: &'a ChannelBilling,
        date: Date,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<DailyBill>> + Send + 'a>> {
        Box::pin(async move {
            // Only an OpenAI channel bills through this API — any other
            // protocol has no cost endpoint here to answer.
            if channel.protocol != oxsum_core::OPENAI {
                return None;
            }
            self.fetch(channel, date).await
        })
    }
}

/// Parses a decimal-string money amount into integer minor units (1 unit =
/// 1_000_000 minor): `"12.34"` → `12_340_000`. Up to six decimal places are
/// honored; more are truncated, never rounded — a provider's fractional cent
/// under-reports rather than over-reports drift. `None` on a non-decimal or an
/// amount too large for i64.
///
/// `pub(crate)` — the Anthropic provider's `cost_report` answer carries the
/// same decimal-string shape, so the parser is shared rather than duplicated.
pub(crate) fn parse_minor(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    let (whole, frac) = match trimmed.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (trimmed, "0"),
    };
    // A bare trailing or leading dot, an empty whole or a non-numeric side is
    // not a decimal the provider would send — refuse rather than guess.
    if whole.is_empty() || frac.is_empty() {
        return None;
    }
    let whole: i64 = whole.parse().ok()?;
    // Pad or truncate the fraction to exactly six digits — micro-units.
    let mut frac_digits = frac.to_owned();
    frac_digits.truncate(6);
    while frac_digits.len() < 6 {
        frac_digits.push('0');
    }
    let frac_minor: i64 = frac_digits.parse().ok()?;
    whole.checked_mul(1_000_000)?.checked_add(frac_minor)
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used)]

    use super::parse_minor;

    #[test]
    fn parse_minor_reads_decimal_strings_as_minor() {
        assert_eq!(parse_minor("12.34"), Some(12_340_000));
        assert_eq!(parse_minor("12"), Some(12_000_000));
        assert_eq!(parse_minor("0.000001"), Some(1));
        // Seven decimals truncate, never round up.
        assert_eq!(parse_minor("0.0000009"), Some(0));
        assert_eq!(parse_minor("1.9999999"), Some(1_999_999));
    }

    #[test]
    fn parse_minor_refuses_a_non_decimal() {
        assert_eq!(parse_minor("not-a-number"), None);
        assert_eq!(parse_minor(""), None);
        assert_eq!(parse_minor("12."), None);
    }
}

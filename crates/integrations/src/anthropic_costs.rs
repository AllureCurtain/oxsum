//! The Anthropic Usage & Cost provider (issue #367): the second impl the
//! `UpstreamBillProvider` seam was cut for, beside OpenAI's Costs
//! ([`crate::openai_costs`]). `GET {base_url}/organizations/cost_report`
//! answers what the organization actually paid for a day — the billed figure
//! the margin view reconciles `upstream_cost_minor` against.
//!
//! The wire differs from OpenAI's in three ways and shares the rest:
//!
//! - **Auth** is `x-api-key: <key>` plus the pinned `anthropic-version` header —
//!   the admin API does not take `Authorization: Bearer`.
//! - **The path** sits under the same `base_url` the relay uses
//!   (`https://api.anthropic.com/v1`), so no scheme switch is needed.
//! - **The response** is `{data: [{starting_at, ending_at, results: [{amount,
//!   currency, cost_type, …}]}], has_more, next_page}` — `amount` is a decimal
//!   string in `currency` (`"USD"`), summed across the day's `results` exactly
//!   as OpenAI's `CostResult` is.
//!
//! The grain stays `(channel, bill_date)`: `bucket_width=1d` is the only width
//! the cost report supports, and `starting_at`/`ending_at` bracket the UTC day
//! the same way the OpenAI impl's `start_time`/`end_time` do. A day the API
//! has not closed yet, a non-Anthropic channel, a missing `bill_key`, a refused
//! call or an unreadable body all answer `None` — the coverage-gap answer every
//! provider gives, recorded rather than read as a zero.

use serde::Deserialize;
use time::Date;

use oxsum_core::{ChannelBilling, DailyBill, UpstreamBillProvider};

use crate::openai_costs::parse_minor;

/// The admin cost endpoint's path under a channel's `base_url`.
const COST_REPORT_PATH: &str = "/organizations/cost_report";

/// The admin API version the request pins — Anthropic's header-versioned
/// contract, independent of the `v1` path prefix the base URL carries.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The client over the shared HTTP stack; the billing credential arrives
/// per-call on the [`ChannelBilling`], never held here — the same shape the
/// OpenAI provider keeps.
#[derive(Debug)]
pub struct AnthropicBillProvider {
    http: reqwest::Client,
}

/// One day's bucket as the cost report answers it. `starting_at`/`ending_at`
/// bound the window the query already fixed; `results` carries the day's
/// line items — one per (model, token_type, …) when `group_by` was asked for,
/// one roll-up otherwise.
#[derive(Deserialize)]
struct CostBucket {
    /// Bucket start, RFC 3339 — read for the struct's shape; the day's window
    /// is already fixed by `starting_at`/`ending_at`.
    #[allow(dead_code)]
    starting_at: String,
    /// The bucket's billed-cost entries.
    #[serde(default)]
    results: Vec<CostResult>,
}

/// One billed-cost entry: a decimal-string `amount` in `currency`.
#[derive(Deserialize)]
struct CostResult {
    /// Decimal text, e.g. `"123.78912"` USD — parsed to integer minor, never
    /// floated (the root rule).
    amount: String,
    /// The billing currency (`"USD"`); read for completeness, the wallet's
    /// minor units are currency-agnostic credits.
    #[allow(dead_code)]
    currency: String,
    /// What kind of spend this line is (`"tokens"`, `"code_execution"`, …);
    /// summed indiscriminately — the day's bill is every cost type together.
    #[allow(dead_code)]
    cost_type: Option<String>,
}

#[derive(Deserialize)]
struct CostReportPage {
    /// The day's buckets — `[starting_at, ending_at)` windows. Not
    /// `#[serde(default)]`: a 200 answering an error body
    /// (`{"error": …}`) must fail to parse rather than read as a zero-bill
    /// day — a missing `data` is a malformed answer, a coverage gap.
    data: Vec<CostBucket>,
    /// Whether another page follows; a single day's fetch is one page, but a
    /// `group_by`-wide answer could page — read honestly and take the first.
    #[allow(dead_code)]
    has_more: Option<bool>,
    /// The cursor for the next page — unused; the day-grain fetch does not
    /// paginate.
    #[allow(dead_code)]
    next_page: Option<String>,
}

impl AnthropicBillProvider {
    /// Builds the provider around the shared HTTP client the job carries.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// `GET {base_url}/organizations/cost_report?starting_at=…&ending_at=…&bucket_width=1d`
    /// — the day's billed total, in minor units. Decimal-string amounts are
    /// summed across the day's results and parsed to integers; a malformed
    /// decimal or a missing amount is a `None`, the coverage-gap answer.
    async fn fetch(&self, channel: &ChannelBilling, date: Date) -> Option<DailyBill> {
        let key = channel.bill_key.as_deref()?;
        let start = date.midnight().assume_utc();
        let end = start + time::Duration::days(1);
        // RFC 3339 with a `Z` — Anthropic's `starting_at`/`ending_at` are
        // timestamps, not unix seconds like OpenAI's.
        let start_s = start
            .format(&time::format_description::well_known::Rfc3339)
            .ok()?;
        let end_s = end
            .format(&time::format_description::well_known::Rfc3339)
            .ok()?;
        let url = format!(
            "{}{}?starting_at={}&ending_at={}&bucket_width=1d",
            channel.base_url.trim_end_matches('/'),
            COST_REPORT_PATH,
            start_s,
            end_s,
        );
        let res = self
            .http
            .get(&url)
            // Anthropic's admin API authenticates on `x-api-key`, not Bearer —
            // and pins the contract version on a second header the relay's own
            // `/v1/messages` surface uses the same way.
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .ok()?;
        if !res.status().is_success() {
            return None;
        }
        let page: CostReportPage = res.json().await.ok()?;
        let mut total: i64 = 0;
        for bucket in &page.data {
            for result in &bucket.results {
                total += parse_minor(&result.amount)?;
            }
        }
        Some(DailyBill {
            provider_billed_minor: total,
        })
    }
}

impl UpstreamBillProvider for AnthropicBillProvider {
    fn daily_cost<'a>(
        &'a self,
        channel: &'a ChannelBilling,
        date: Date,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<DailyBill>> + Send + 'a>> {
        Box::pin(async move {
            // Only an Anthropic channel bills through this API — any other
            // protocol has no cost endpoint here to answer.
            if channel.protocol != oxsum_core::ANTHROPIC {
                return None;
            }
            self.fetch(channel, date).await
        })
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used)]

    use super::*;
    use axum::Json;
    use axum::Router;
    use axum::http::HeaderMap;
    use axum::routing::get;
    use oxsum_core::UpstreamBillProvider;
    use uuid::Uuid;

    fn channel(protocol: &str, base_url: &str, bill_key: Option<&str>) -> ChannelBilling {
        ChannelBilling {
            channel_id: Uuid::new_v4(),
            name: "claude-main".into(),
            protocol: protocol.into(),
            base_url: base_url.into(),
            bill_key: bill_key.map(str::to_owned),
        }
    }

    fn date() -> Date {
        Date::from_calendar_date(2026, time::Month::October, 20).unwrap()
    }

    /// A stub cost_report endpoint answering the wire Anthropic serves:
    /// `{data: [{starting_at, ending_at, results: [{amount, currency, cost_type}]}]}`.
    /// Returns the base URL the stub listens on, plus a handle to inspect the
    /// request's headers — `x-api-key` and `anthropic-version` are the contract
    /// this impl must honor.
    async fn stub_api(
        body: serde_json::Value,
    ) -> (String, std::sync::Arc<std::sync::Mutex<HeaderMap>>) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(HeaderMap::new()));
        let seen2 = seen.clone();
        let app = Router::new().route(
            "/organizations/cost_report",
            get(move |headers: HeaderMap| {
                *seen2.lock().unwrap() = headers.clone();
                std::future::ready(Json(body.clone()))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://127.0.0.1:{port}"), seen)
    }

    #[tokio::test]
    async fn a_non_anthropic_channel_is_a_coverage_gap() {
        let provider = AnthropicBillProvider::new(reqwest::Client::new());
        // No HTTP server stands behind the URL — a protocol mismatch must
        // answer `None` without a request being attempted.
        let c = channel(
            oxsum_core::OPENAI,
            "https://api.anthropic.com/v1",
            Some("sk-ant-admin-x"),
        );
        assert!(provider.daily_cost(&c, date()).await.is_none());
    }

    #[tokio::test]
    async fn a_missing_bill_key_is_a_coverage_gap() {
        let provider = AnthropicBillProvider::new(reqwest::Client::new());
        let c = channel(oxsum_core::ANTHROPIC, "https://api.anthropic.com/v1", None);
        assert!(provider.daily_cost(&c, date()).await.is_none());
    }

    #[tokio::test]
    async fn a_billed_day_sums_the_results_into_minor() {
        // The wire Anthropic documents: `amount` is a decimal string in USD,
        // several results per bucket (one per group_by cell).
        let (base_url, seen) = stub_api(serde_json::json!({
            "data": [{
                "starting_at": "2026-10-20T00:00:00Z",
                "ending_at": "2026-10-21T00:00:00Z",
                "results": [
                    {"amount": "12.5", "currency": "USD", "cost_type": "tokens",
                     "model": "claude-sonnet-4-5", "token_type": "output_tokens"},
                    {"amount": "0.75", "currency": "USD", "cost_type": "tokens",
                     "model": "claude-haiku-4-5", "token_type": "cache_read_input_tokens"},
                    {"amount": "1.2", "currency": "USD", "cost_type": "web_search",
                     "description": "Web search"},
                ]
            }],
            "has_more": false,
            "next_page": null
        }))
        .await;

        let provider = AnthropicBillProvider::new(reqwest::Client::new());
        let c = channel(oxsum_core::ANTHROPIC, &base_url, Some("sk-ant-admin-real"));
        let bill = provider.daily_cost(&c, date()).await.unwrap();
        // 12.5 + 0.75 + 1.2 = 14.45 USD → 14_450_000 minor.
        assert_eq!(bill.provider_billed_minor, 14_450_000);

        // The request carried the two headers the admin API expects.
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.get("x-api-key").and_then(|v| v.to_str().ok()),
            Some("sk-ant-admin-real")
        );
        assert_eq!(
            seen.get("anthropic-version").and_then(|v| v.to_str().ok()),
            Some("2023-06-01")
        );
    }

    #[tokio::test]
    async fn an_empty_day_records_a_zero_bill_not_a_gap() {
        // `data: []` is the API answering "the day closed with nothing billed" —
        // a zero, not a coverage gap. The margin view reads a missing row as
        // "no answer", so the distinction matters.
        let (base_url, _seen) = stub_api(serde_json::json!({
            "data": [],
            "has_more": false,
            "next_page": null
        }))
        .await;

        let provider = AnthropicBillProvider::new(reqwest::Client::new());
        let c = channel(oxsum_core::ANTHROPIC, &base_url, Some("sk-ant-admin-x"));
        let bill = provider.daily_cost(&c, date()).await.unwrap();
        assert_eq!(bill.provider_billed_minor, 0);
    }

    #[tokio::test]
    async fn a_refusal_is_a_coverage_gap_not_a_panic() {
        let (base_url, _seen) = stub_api(serde_json::json!({
            "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        }))
        .await;

        let provider = AnthropicBillProvider::new(reqwest::Client::new());
        let c = channel(oxsum_core::ANTHROPIC, &base_url, Some("sk-ant-admin-bad"));
        // A 401 body parses as JSON but has no `data` — the deserialize fails
        // and the answer is None, never a panic or a zero.
        assert!(provider.daily_cost(&c, date()).await.is_none());
    }
}

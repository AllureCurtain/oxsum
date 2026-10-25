//! The bill-provider fan-out (issue #367): the `upstream-bill-sync` job asks
//! one [`UpstreamBillProvider`] for each channel's day; the deployment
//! actually carries *several* impls — one per provider API that exists — and
//! each impl already refuses a channel whose `protocol` is not its own. The
//! composite asks each in turn and returns the first `Some`, so the job's
//! trait-object signature does not have to change when a third provider lands.
//!
//! The order is arbitrary by construction: a channel names one protocol, so at
//! most one impl can answer it — there is no priority to get wrong, only a
//! protocol to match. A channel whose protocol no impl knows returns `None`,
//! the coverage-gap answer the margin view reads as "no bill to reconcile".

use std::sync::Arc;

use oxsum_core::{ChannelBilling, DailyBill, UpstreamBillProvider};

/// A provider that delegates to each impl in turn until one answers. Built by
/// `Config::bill_provider_of` from the impls the deployment has credentials
/// for; tests stand a stub behind it the same way they do a concrete provider.
#[derive(Debug)]
pub struct CompositeBillProvider {
    providers: Vec<Arc<dyn UpstreamBillProvider>>,
}

impl CompositeBillProvider {
    /// Builds the composite over the impls the deployment carries. The order
    /// is irrelevant — a channel's `protocol` admits exactly one impl, so the
    /// first `Some` is the only `Some` possible.
    #[must_use]
    pub fn new(providers: Vec<Arc<dyn UpstreamBillProvider>>) -> Self {
        Self { providers }
    }
}

impl UpstreamBillProvider for CompositeBillProvider {
    fn daily_cost<'a>(
        &'a self,
        channel: &'a ChannelBilling,
        date: time::Date,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<DailyBill>> + Send + 'a>> {
        Box::pin(async move {
            for provider in &self.providers {
                if let Some(bill) = provider.daily_cost(channel, date).await {
                    return Some(bill);
                }
            }
            None
        })
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used)]

    use super::*;
    use time::Date;
    use uuid::Uuid;

    /// A scripted provider that answers a fixed bill for a named protocol.
    #[derive(Debug)]
    struct Stub {
        protocol: &'static str,
        bill: Option<i64>,
    }

    impl UpstreamBillProvider for Stub {
        fn daily_cost<'a>(
            &'a self,
            channel: &'a ChannelBilling,
            _date: Date,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<DailyBill>> + Send + 'a>>
        {
            Box::pin(async move {
                if channel.protocol != self.protocol {
                    return None;
                }
                self.bill.map(|provider_billed_minor| DailyBill {
                    provider_billed_minor,
                })
            })
        }
    }

    fn channel(protocol: &str) -> ChannelBilling {
        ChannelBilling {
            channel_id: Uuid::new_v4(),
            name: "ch".into(),
            protocol: protocol.into(),
            base_url: "https://example.invalid".into(),
            bill_key: Some("k".into()),
        }
    }

    fn date() -> Date {
        Date::from_calendar_date(2026, time::Month::October, 20).unwrap()
    }

    #[tokio::test]
    async fn the_composite_routes_each_protocol_to_its_impl() {
        let provider = CompositeBillProvider::new(vec![
            Arc::new(Stub {
                protocol: "openai",
                bill: Some(1_000_000),
            }),
            Arc::new(Stub {
                protocol: "anthropic",
                bill: Some(2_000_000),
            }),
        ]);
        assert_eq!(
            provider
                .daily_cost(&channel("openai"), date())
                .await
                .unwrap()
                .provider_billed_minor,
            1_000_000
        );
        assert_eq!(
            provider
                .daily_cost(&channel("anthropic"), date())
                .await
                .unwrap()
                .provider_billed_minor,
            2_000_000
        );
    }

    #[tokio::test]
    async fn an_unknown_protocol_is_a_coverage_gap_not_an_error() {
        let provider = CompositeBillProvider::new(vec![Arc::new(Stub {
            protocol: "openai",
            bill: Some(1),
        })]);
        assert!(
            provider
                .daily_cost(&channel("gemini"), date())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_first_some_wins_and_a_none_falls_through() {
        // A None from the first impl does not stop the second from answering —
        // the coverage-gap semantics are per-impl, not per-composite.
        let provider = CompositeBillProvider::new(vec![
            Arc::new(Stub {
                protocol: "openai",
                bill: None,
            }),
            Arc::new(Stub {
                protocol: "openai",
                bill: Some(7),
            }),
        ]);
        assert_eq!(
            provider
                .daily_cost(&channel("openai"), date())
                .await
                .unwrap()
                .provider_billed_minor,
            7
        );
    }
}

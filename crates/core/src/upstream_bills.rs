//! Upstream provider bill reconciliation (issue #283): what a provider *said*
//! it billed, recorded beside the estimate the ledger priced.
//!
//! `usage_records.upstream_cost_minor` is oxsum's own `Price::upstream_cost`
//! recomputation over the same usage the customer was billed — an estimate.
//! decisions.md deferred comparing it to the provider's invoice because no
//! upstream rail exposed one; both major providers now ship admin-level cost
//! APIs, so the estimate can be reconciled against the billed fact.
//!
//! The grain is `(channel, bill_date)`: the provider cost APIs (OpenAI Costs,
//! Anthropic Usage & Cost Admin) expose day-grained billed totals, never
//! per-request, so a bill row is one channel's one UTC date. `our_cost_minor`
//! is that day's Σ `upstream_cost_minor` frozen at write time; `drift_*` is the
//! answer the margin view reads — provider minus ours. A row whose drift
//! crosses the alert threshold broadcasts `anomaly.detected` once (the
//! `reconciliation_entry_id` latch) so a re-run cannot double-flag.
//!
//! The seam is [`UpstreamBillProvider`]: the daily `upstream-bill-sync` job asks
//! each channel's provider for a day's billed total through a trait rather than
//! a concrete client, so a stubbed provider serves the whole test suite and a
//! second implementation (Anthropic) drops in without touching the job.

use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use sqlx::Row as _;
use time::Date;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::webhooks::ANOMALY_DETECTED;

/// Drift past this percentage of the estimate is flagged `anomaly.detected`.
/// Providers bill their own rounding and minimums, so the threshold is a real
/// divergence, not a sub-credit rounding difference: 5% of the estimate, with a
/// floor of 1 credit (1_000_000 minor) so a near-zero day's noise cannot flag.
pub const DRIFT_ALERT_PCT: f64 = 5.0;
/// The absolute drift floor — minor units — under `DRIFT_ALERT_PCT`'s share.
pub const DRIFT_ALERT_FLOOR_MINOR: i64 = 1_000_000;

/// One day's billed cost, as a provider answers it: minor units in the same
/// 1-credit scale every money column keeps. `None` from the provider means
/// "no bill for this channel on this date" — the row records it as absent
/// rather than as a zero, because an unanswered day is a coverage gap the
/// margin view should not read as the provider billing nothing.
#[derive(Debug, Clone, Copy)]
pub struct DailyBill {
    /// The provider-billed total, minor units.
    pub provider_billed_minor: i64,
}

/// The seam a provider's admin cost API drops into: answer what one channel was
/// billed on one UTC date. Implementations live in `oxsum-integrations` — the
/// job holds an `Arc<dyn …>` and the test suite a scripted stub, so the trait
/// stays object-safe (`Pin<Box<dyn Future>>`, the [`crate::RailVerifier`]
/// pattern) and `Debug + Send + Sync` for the shared context.
pub trait UpstreamBillProvider: Debug + Send + Sync {
    /// Fetch the billed cost for `channel` on `date`, in minor units. `None`
    /// means the provider has no bill to report for that day — unreachable,
    /// a channel with no billing credential, or a day the provider has not
    /// closed yet are all "nothing to reconcile", never a zero.
    fn daily_cost<'a>(
        &'a self,
        channel: &'a ChannelBilling,
        date: Date,
    ) -> Pin<Box<dyn Future<Output = Option<DailyBill>> + Send + 'a>>;
}

/// A channel's identity for the bill fetch — what the provider call needs and
/// nothing more. `bill_key` is the opened admin credential (the metering key
/// `api_key_sealed` deliberately stays a relay secret); `None` on a channel
/// that never had one configured, which the provider impl can treat as an
/// immediate `None` — no bill is fetchable without it.
#[derive(Debug, Clone)]
pub struct ChannelBilling {
    /// The channel's row id — the bill's `channel_id`.
    pub channel_id: Uuid,
    /// The channel's display name, for the provider's `group_by` and for logs.
    pub name: String,
    /// The upstream protocol (`openai`, `anthropic`, …) — the provider impl
    /// picks its cost endpoint on it; an unsupported protocol answers `None`.
    pub protocol: String,
    /// The provider's base URL — the channel's `base_url` the relay already
    /// serves; the cost API is a path under it.
    pub base_url: String,
    /// The opened admin billing credential (`bill_key_sealed` unsealed), or
    /// `None` when the channel carries none.
    pub bill_key: Option<String>,
}

/// One `upstream_bills` row, as the margin join reads it back.
#[derive(Debug, Clone)]
pub struct UpstreamBill {
    /// The channel the bill covers.
    pub channel_id: Uuid,
    /// The UTC date it covers.
    pub bill_date: Date,
    /// What the provider billed — `None` on a coverage-gap day.
    pub provider_billed_minor: Option<i64>,
    /// The day's Σ `upstream_cost_minor`, frozen at write time.
    pub our_cost_minor: i64,
    /// `provider_billed_minor - our_cost_minor`, `None` when the provider side is.
    pub drift_minor: Option<i64>,
    /// `drift_minor / our_cost_minor * 100`, `None` when either side is missing
    /// or the estimate is zero.
    pub drift_pct: Option<f64>,
    /// Whether the drift crossed the alert threshold and was broadcast.
    pub flagged: bool,
}

/// The drift event's `data` — what `anomaly.detected` subscribers read off the
/// broadcast without a lookup.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BillDriftEvent<'a> {
    kind: &'static str,
    channel: &'a str,
    bill_date: &'a str,
    provider_billed_minor: i64,
    our_cost_minor: i64,
    drift_minor: i64,
    drift_pct: f64,
}

impl Db {
    /// The channels a bill pass may resolve a provider for: name, protocol,
    /// base URL and both sealed credentials (the job opens `bill_key_sealed`
    /// for the billing call; `api_key_sealed` stays sealed — the relay's key
    /// is never the billing credential). `key` is the deployment's
    /// `OXSUM_SECRET_KEY` — the same unsealer every stored credential opens
    /// under. A channel with no `bill_key_sealed` still lists — its `bill_key`
    /// is `None` and the provider impl answers `None` for it, so the day's
    /// coverage gap records as absent rather than silently skipped.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]; a `bill_key_sealed` that
    /// does not open under `key` fails the whole read — a mis-sealed billing
    /// credential is a misconfiguration to surface, not a day to silently skip.
    pub async fn billable_channels(
        &self,
        key: &crate::channels::SecretKey,
    ) -> Result<Vec<ChannelBilling>, WalletError> {
        let rows = sqlx::query(
            "SELECT channel_id, name, protocol, base_url, bill_key_sealed \
             FROM oxsum.channels ORDER BY name",
        )
        .fetch_all(self.pool())
        .await?;
        let mut channels = Vec::with_capacity(rows.len());
        for row in &rows {
            let sealed: Option<String> = row.try_get("bill_key_sealed")?;
            channels.push(ChannelBilling {
                channel_id: row.try_get("channel_id")?,
                name: row.try_get("name")?,
                protocol: row.try_get("protocol")?,
                base_url: row.try_get("base_url")?,
                bill_key: sealed.map(|s| key.open(&s)).transpose()?,
            });
        }
        Ok(channels)
    }

    /// The day's Σ `upstream_cost_minor` for one channel — the estimate the
    /// provider bill is reconciled against. `upstream_cost_minor` is NULL on
    /// untracked rows (a price without an `upstream` block, a swept turn); the
    /// sum ignores them the same way `margin()` does — NULL is untracked, not
    /// zero — so a day with only untracked usage sums 0, not NULL.
    ///
    /// `date` is the UTC day the usage row `settled_at` lands in — the
    /// settlement's own day, the same grain the provider cost APIs report.
    /// `channel` is the name `usage_records.channel` stores (the column keys on
    /// the channel's name, not its id — the bill's `channel_id` is the row id).
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn our_cost_for(&self, channel: &str, date: Date) -> Result<i64, WalletError> {
        let sum: i64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(upstream_cost_minor), 0)::bigint \
             FROM oxsum.usage_records \
             WHERE channel = $1 AND settled_at::date = $2",
        )
        .bind(channel)
        .bind(date)
        .fetch_one(self.pool())
        .await?;
        Ok(sum)
    }

    /// Writes (or refreshes) one channel-day bill row and flags drift over the
    /// alert threshold. The `(channel_id, bill_date)` primary key makes a
    /// re-run's upsert idempotent; `flagged` latches once the
    /// `reconciliation_entry_id` is set, so a re-detected drift cannot
    /// double-broadcast `anomaly.detected`.
    ///
    /// `provider` is the answer [`UpstreamBillProvider::daily_cost`] returned —
    /// `Some` a billed total, `None` a coverage-gap day (recorded so the margin
    /// view can show "the provider answered nothing" rather than a missing row).
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_bill(
        &self,
        channel: &ChannelBilling,
        date: Date,
        provider: Option<DailyBill>,
    ) -> Result<UpstreamBill, WalletError> {
        let our_cost = self.our_cost_for(&channel.name, date).await?;
        let (drift_minor, drift_pct) = match provider {
            Some(DailyBill {
                provider_billed_minor,
            }) => {
                let drift = provider_billed_minor - our_cost;
                let pct = (our_cost != 0).then(|| drift as f64 / our_cost as f64 * 100.0);
                (Some(drift), pct)
            }
            None => (None, None),
        };
        sqlx::query(
            "INSERT INTO oxsum.upstream_bills \
                 (channel_id, bill_date, provider_billed_minor, our_cost_minor, \
                  drift_minor, drift_pct) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (channel_id, bill_date) DO UPDATE SET \
                 provider_billed_minor = EXCLUDED.provider_billed_minor, \
                 our_cost_minor = EXCLUDED.our_cost_minor, \
                 drift_minor = EXCLUDED.drift_minor, \
                 drift_pct = EXCLUDED.drift_pct, \
                 updated_at = now()",
        )
        .bind(channel.channel_id)
        .bind(date)
        .bind(provider.map(|b| b.provider_billed_minor))
        .bind(our_cost)
        .bind(drift_minor)
        .bind(drift_pct)
        .execute(self.pool())
        .await?;

        let flagged = self
            .maybe_flag_bill_drift(channel, date, our_cost, drift_minor, drift_pct)
            .await?;
        Ok(UpstreamBill {
            channel_id: channel.channel_id,
            bill_date: date,
            provider_billed_minor: provider.map(|b| b.provider_billed_minor),
            our_cost_minor: our_cost,
            drift_minor,
            drift_pct,
            flagged,
        })
    }

    /// Flags a bill whose drift crosses the alert threshold — `anomaly.detected`
    /// once, latched by `reconciliation_entry_id` — and returns whether this row
    /// is now flagged. Under the threshold, or with no provider total to drift
    /// from, the row stays unflagged.
    async fn maybe_flag_bill_drift(
        &self,
        channel: &ChannelBilling,
        date: Date,
        our_cost_minor: i64,
        drift_minor: Option<i64>,
        drift_pct: Option<f64>,
    ) -> Result<bool, WalletError> {
        let (Some(drift), Some(pct)) = (drift_minor, drift_pct) else {
            return Ok(false);
        };
        if drift.abs() < DRIFT_ALERT_FLOOR_MINOR || pct.abs() < DRIFT_ALERT_PCT {
            return Ok(false);
        }
        // The latch: claim the row by marking it flagged; if it was already
        // claimed (`reconciliation_entry_id` set by an earlier run), the UPDATE
        // touches zero rows and we broadcast nothing — a re-run is quiet.
        let claimed = sqlx::query(
            "UPDATE oxsum.upstream_bills \
             SET reconciliation_entry_id = gen_random_uuid(), updated_at = now() \
             WHERE channel_id = $1 AND bill_date = $2 \
               AND reconciliation_entry_id IS NULL",
        )
        .bind(channel.channel_id)
        .bind(date)
        .execute(self.pool())
        .await?
        .rows_affected()
            > 0;
        if claimed {
            let date_s = date.to_string();
            self.broadcast_event(
                ANOMALY_DETECTED,
                serde_json::to_value(BillDriftEvent {
                    kind: "upstream_bill_drift",
                    channel: &channel.name,
                    bill_date: &date_s,
                    provider_billed_minor: drift + our_cost_minor,
                    our_cost_minor,
                    drift_minor: drift,
                    drift_pct: pct,
                })
                .map_err(|error| {
                    WalletError::Misconfigured(format!("bill drift event: {error}"))
                })?,
            )
            .await?;
        }
        Ok(true)
    }

    /// The latest bill per channel, for the margin join: one row per channel
    /// with a bill, `provider_billed_minor`/`our_cost_minor` summed across all
    /// recorded days (a margin is a cumulative figure, not a day's). Channels
    /// the pass never reached simply do not appear — `LEFT JOIN`ing them would
    /// fake a zero where there is no data.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn provider_billed(&self) -> Result<Vec<(Uuid, Option<i64>, i64)>, WalletError> {
        let rows = sqlx::query(
            "SELECT channel_id, \
                    sum(provider_billed_minor)::bigint AS provider_billed_minor, \
                    sum(our_cost_minor)::bigint AS our_cost_minor \
             FROM oxsum.upstream_bills \
             GROUP BY channel_id",
        )
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push((
                row.try_get("channel_id")?,
                row.try_get("provider_billed_minor")?,
                row.try_get("our_cost_minor")?,
            ));
        }
        Ok(out)
    }
}

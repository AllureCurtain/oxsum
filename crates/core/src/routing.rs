//! Latency-aware routing (issue #499): observed upstream behavior folded into
//! the route pick.
//!
//! `route_order` (channels.rs) picks by `weight` first, then cheapest-in-tier —
//! a channel that is currently slow or erroring still leads when its weight is
//! highest, because the pick has no signal about what upstream is doing *now*.
//! This module is that signal: a process-local per-channel `RouteStats` — an
//! exponential moving average of time-to-first-byte and error rate over a
//! trailing window — that scales a serving's effective weight and drops a
//! badly-erroring channel for a cool-down.
//!
//! Like [`crate::pools::Affinity`], the stats live on [`Db`] and stay
//! in-memory: they are a routing hint, never a billing fact — the bill only
//! names the channel that served, so which candidate a turn *almost* took is
//! not part of its proof. A process restart simply starts the window cold, and
//! the gateway falls back to weight+price until the first samples land — the
//! healthy default.
//!
//! The relay feeds samples at the two points it already observes: the
//! per-attempt `call.send()` resolution (success / 5xx+429 / unreachable) and
//! the first upstream byte, so a channel that is slow-but-alive sheds weight
//! the way a dead one sheds attempts.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::db::Db;

/// EMA smoothing for one new sample: recent behavior weighs a fifth against the
/// running estimate — enough that a single slow turn does not tank a channel,
/// fast enough that a channel that turned bad sheds picks within a few calls.
const EMA_ALPHA: f64 = 0.2;

/// The error-rate EMA above which a channel is dropped entirely for the
/// cool-down window: half of recent attempts failing is past the point a
/// retry-on-another-route is worth more than keeping it in rotation.
const ERROR_DROP_THRESHOLD: f64 = 0.5;

/// How long an erroring channel sits out once `error_rate` crosses
/// [`ERROR_DROP_THRESHOLD`]. After it lapses the channel is re-admitted at its
/// scaled weight — the decaying EMA means a channel that stopped being asked
/// reads healthier over time, so a recovered upstream regains picks without a
/// probe.
const ERROR_COOLDOWN: Duration = Duration::from_secs(60);

/// The slowest a channel can get before its weight floor binds: observed TTFT
/// is compared against the fleet's healthy TTFT, and `base * healthy/observed`
/// shrinks the weight — capped at `base / SLOW_WEIGHT_FLOOR_DIVISOR` so a
/// slow-but-alive channel never fully vanishes on latency alone (only a real
/// error rate removes it).
const SLOW_WEIGHT_FLOOR_DIVISOR: i64 = 8;

/// A healthy baseline TTFT in milliseconds: when the fleet has no other sample
/// to compare against, a channel is judged against this — a generous figure so
/// the first slow turn does not collapse a healthy deployment's weights.
const HEALTHY_TTFT_MS: f64 = 2_000.0;

impl RouteStats {
    /// The baseline a pick with no stats store falls back to — `route_order`'s
    /// `None`-stats path, kept public so `route_order` need not re-declare it.
    pub const DEFAULT_TTFT_MS: f64 = HEALTHY_TTFT_MS;
}

/// The upper bound on the per-channel map before the oldest entry is evicted:
/// a hint, not a record, so a deployment with many channels stays bounded.
const STATS_CAP: usize = 256;

/// One channel's observed upstream health — the running EMA the pick reads.
#[derive(Debug, Default)]
pub struct ChannelStats {
    /// EMA of time-to-first-byte in milliseconds; `None` until the first
    /// streamed sample (a non-streamed turn contributes no TTFT).
    pub ttft_ms: Option<f64>,
    /// EMA of the terminal error rate in `[0,1]`: a failed attempt feeds 1.0,
    /// a served one 0.0. `None` until the first outcome lands.
    pub error_rate: Option<f64>,
    /// When the last sample landed — the cool-down and the decay read it.
    pub last_sample: Option<Instant>,
    /// When an error-rate-driven drop lapses; `Some` while the channel sits
    /// out, `None` once it is readmitted.
    dropped_until: Option<Instant>,
}

/// The process-local, per-channel health store the gateway reads before a pick
/// and the relay writes after an attempt — kept on [`Db`] the way
/// [`crate::pools::Affinity`] is, so the request path and the settle path share
/// one window.
#[derive(Debug, Default)]
pub struct RouteStats {
    inner: Mutex<HashMap<String, ChannelStats>>,
}

impl RouteStats {
    /// Records an attempt's terminal outcome for `channel`. `ttft_ms` is the
    /// milliseconds to the first upstream byte where the surface measured one —
    /// `None` on a non-streamed or never-answered attempt. `error` is whether
    /// the attempt counted as a failure for routing (5xx, 429, unreachable);
    /// the channel's TTFT EMA only learns from an answered attempt.
    pub fn note(&self, channel: &str, ttft_ms: Option<f64>, error: bool) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        if !map.contains_key(channel) && map.len() >= STATS_CAP {
            // Evict the stalest entry rather than grow unboundedly — a hint.
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, s)| s.last_sample)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        let stats = map.entry(channel.to_owned()).or_default();
        stats.last_sample = Some(Instant::now());
        if let Some(ttft) = ttft_ms {
            stats.ttft_ms = Some(match stats.ttft_ms {
                Some(running) => running + EMA_ALPHA * (ttft - running),
                None => ttft,
            });
        }
        let outcome = if error { 1.0 } else { 0.0 };
        let rate = match stats.error_rate {
            Some(running) => running + EMA_ALPHA * (outcome - running),
            None => outcome,
        };
        stats.error_rate = Some(rate);
        // A crossed threshold drops the channel for the window — the pick reads
        // `dropped_until`, the writer just arms it.
        if error && rate > ERROR_DROP_THRESHOLD {
            stats.dropped_until = Some(Instant::now() + ERROR_COOLDOWN);
        }
    }

    /// The channel's effective weight for the pick: `base` scaled by observed
    /// health. `healthy_ttft_ms` is the fleet's healthy baseline the channel is
    /// judged against (see [`Self::healthy_ttft_ms`]). Returns `0` for a
    /// channel dropped on error rate inside its cool-down — that serving is
    /// skipped, not merely de-picked.
    pub fn effective_weight(&self, channel: &str, base: i64, healthy_ttft_ms: f64) -> i64 {
        let Ok(map) = self.inner.lock() else {
            return base;
        };
        let Some(stats) = map.get(channel) else {
            return base; // no observation — healthy default, today's behavior.
        };
        if stats
            .dropped_until
            .is_some_and(|until| until > Instant::now())
        {
            return 0;
        }
        // Slowness scales the weight down proportionally: a channel 2× slower
        // than the fleet's healthy TTFT counts as half its weight, floored so a
        // slow-but-alive channel never vanishes on latency alone.
        match stats.ttft_ms {
            Some(ttft) if ttft > healthy_ttft_ms => {
                let scaled = (base as f64 * healthy_ttft_ms / ttft).round() as i64;
                scaled.max(base / SLOW_WEIGHT_FLOOR_DIVISOR).max(1)
            }
            _ => base,
        }
    }

    /// The TTFT the fleet's healthy channels are judged against: the median-ish
    /// of observed channels — implemented as the smallest running TTFT, which
    /// for a "what does good look like" comparison is the honest figure (the
    /// fastest observed upstream), with [`HEALTHY_TTFT_MS`] as the cold-start
    /// floor so a healthy deployment's weights never collapse on a single slow
    /// turn.
    pub fn healthy_ttft_ms(&self) -> f64 {
        let Ok(map) = self.inner.lock() else {
            return HEALTHY_TTFT_MS;
        };
        map.values()
            .filter_map(|s| s.ttft_ms)
            .reduce(f64::min)
            .map(|best| best.max(1.0))
            .unwrap_or(HEALTHY_TTFT_MS)
    }

    /// Test seam: directly place a channel's stats so a routing test can pose a
    /// slow or erroring channel without driving real upstream calls. Hidden
    /// from the public docs — a test's fixture, not a production write path.
    #[doc(hidden)]
    pub fn seed(&self, channel: &str, ttft_ms: Option<f64>, error_rate: f64, dropped: bool) {
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        map.insert(
            channel.to_owned(),
            ChannelStats {
                ttft_ms,
                error_rate: Some(error_rate),
                last_sample: Some(Instant::now()),
                dropped_until: dropped.then(|| Instant::now() + ERROR_COOLDOWN),
            },
        );
    }
}

/// `Arc`-shared so [`Db`]'s cheap clones all reach the one window — the same
/// sharing [`crate::pools::SharedAffinity`] uses for the affinity map.
pub type SharedRouteStats = std::sync::Arc<RouteStats>;

impl Db {
    /// The routing-health store every pick reads and every attempt writes.
    pub fn route_stats(&self) -> &RouteStats {
        &self.route_stats
    }
}

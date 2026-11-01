//! Latency-aware routing (issue #499): the pick folds observed upstream health
//! into each serving's effective weight. These tests pose channels through the
//! `RouteStats::seed` test seam — no database, no real upstream — and walk the
//! `roll` range the way `route_order`'s own tests do.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Price, RouteStats, Serving};

/// A serving built by hand — `route_order_stats` only reads `channel`,
/// `weight` and `price`, so the rest is filler that keeps the struct honest.
fn serving(channel: &str, weight: i64, input_price: i64) -> Serving {
    Serving {
        channel: channel.to_owned(),
        channel_id: uuid::Uuid::new_v4(),
        base_url: format!("https://{channel}.example/v1"),
        protocol: "openai".to_owned(),
        credentials: Vec::new(),
        settings: serde_json::json!({}),
        version: 1,
        weight,
        price: Price {
            input_price_per_million: input_price,
            output_price_per_million: 2 * input_price,
            max_output_tokens: 1_000,
            cache_read_price_per_million: None,
            cache_write_5m_price_per_million: None,
            cache_write_1h_price_per_million: None,
            reasoning_price_per_million: None,
            cost_per_request: None,
            upstream: None,
            mode: Default::default(),
            rules: Vec::new(),
        },
    }
}

/// The leading channel for a roll — the only position the health signal moves.
fn leader(stats: &RouteStats, servings: &[Serving], roll: f64) -> String {
    oxsum_core::route_order_stats(servings.to_vec(), roll, Some(stats))[0]
        .channel
        .clone()
}

/// How often `channel` leads across the whole roll range — the weighted pick's
/// empirical share, which the effective weight should track.
fn lead_share(stats: &RouteStats, servings: &[Serving], channel: &str) -> f64 {
    const STEPS: usize = 1_000;
    let leads = (0..STEPS)
        .filter(|i| leader(stats, servings, *i as f64 / STEPS as f64) == channel)
        .count();
    leads as f64 / STEPS as f64
}

/// No stats store is the historical pick: an empty `RouteStats` must reproduce
/// `route_order`'s weight+price order exactly.
#[test]
fn no_observations_is_the_historical_pick() {
    let stats = RouteStats::default();
    // cheap is heavier AND cheaper — leads every roll with or without health.
    let servings = vec![
        serving("alpha", 100, 10),
        serving("bravo", 10, 40),
        serving("charlie", 50, 10),
    ];
    for roll in [0.0, 0.2, 0.5, 0.8, 0.99] {
        let healthy = leader(&stats, &servings, roll);
        let plain = oxsum_core::route_order(servings.clone(), roll)[0]
            .channel
            .clone();
        assert_eq!(healthy, plain, "roll {roll}: no stats is the plain pick");
    }
}

/// A slow channel sheds picks proportionally: observed TTFT twice the fleet's
/// healthy baseline halves its effective weight, so it leads roughly half as
/// often as it would healthy.
#[test]
fn a_slow_channel_loses_picks_proportionally() {
    let stats = RouteStats::default();
    // alpha leads at weight 100 healthy. Make it 4× slower than the fleet's
    // observed-best (bravo at 500ms -> alpha at 2000ms): effective weight
    // 100 * 500/2000 = 25, so alpha leads ~25/(25+10+50) vs 100/160 healthy.
    stats.seed("alpha", Some(2_000.0), 0.0, false);
    stats.seed("bravo", Some(500.0), 0.0, false);
    stats.seed("charlie", Some(500.0), 0.0, false);
    let servings = vec![
        serving("alpha", 100, 10),
        serving("bravo", 10, 40),
        serving("charlie", 50, 10),
    ];
    let slow_share = lead_share(&stats, &servings, "alpha");
    // Effective weight 25 of (25+10+50)=85 -> ~0.29. Assert it is meaningfully
    // below the healthy 100/160≈0.63 and near the scaled share.
    assert!(
        slow_share < 0.40,
        "a 4x-slower alpha should shed most leads, got {slow_share}"
    );
    assert!(
        slow_share > 0.15,
        "slowness floors the weight, never removes it, got {slow_share}"
    );
}

/// An erroring channel loses every lead while its cool-down holds: its
/// effective weight is 0, so it never leads and ranks last in the failover
/// order.
#[test]
fn an_erroring_channel_loses_all_leads_until_it_cools() {
    let stats = RouteStats::default();
    // alpha is heaviest and healthiest-priced but erroring past the threshold
    // and armed for the cool-down window.
    stats.seed("alpha", Some(500.0), 0.9, true);
    stats.seed("bravo", Some(500.0), 0.0, false);
    let servings = vec![serving("alpha", 100, 10), serving("bravo", 10, 40)];
    for roll in [0.0, 0.3, 0.6, 0.9, 0.99] {
        let ordered = oxsum_core::route_order_stats(servings.clone(), roll, Some(&stats));
        assert_ne!(
            ordered[0].channel, "alpha",
            "roll {roll}: a dropped alpha must never lead"
        );
        assert_eq!(ordered[0].channel, "bravo");
        assert_eq!(ordered[1].channel, "alpha", "a dropped channel ranks last");
    }
}

/// A channel whose error EMA has decayed below the threshold — and whose
/// cool-down has lapsed — is readmitted at its scaled weight: it leads again.
#[test]
fn a_recovered_channel_regains_weight() {
    let stats = RouteStats::default();
    // alpha was erroring but its cool-down lapsed (dropped=false) and the EMA
    // decayed under the threshold: it leads at full weight again.
    stats.seed("alpha", Some(500.0), 0.2, false);
    stats.seed("bravo", Some(500.0), 0.0, false);
    let servings = vec![serving("alpha", 100, 10), serving("bravo", 10, 40)];
    let share = lead_share(&stats, &servings, "alpha");
    // Effective weight back to 100 of 110 -> ~0.91 lead share.
    assert!(
        share > 0.80,
        "a cooled-and-decayed alpha should lead near full weight, got {share}"
    );
}

/// The stats feed itself: `note` folds a latency sample and an outcome into the
/// EMA, and an erroring streak arms the drop.
#[test]
fn the_stats_feed_builds_the_window() {
    let stats = RouteStats::default();
    stats.note("alpha", Some(400.0), false);
    stats.note("alpha", Some(600.0), false);
    // EMA pulls 400 toward 600; the observed figure is between them.
    let base = 100_i64;
    let healthy = stats.healthy_ttft_ms();
    let w = stats.effective_weight("alpha", base, healthy);
    // alpha is the only channel -> it IS the healthy baseline -> full weight.
    assert_eq!(w, base);
    // A run of errors arms the drop -> effective weight 0.
    for _ in 0..8 {
        stats.note("alpha", None, true);
    }
    assert_eq!(stats.effective_weight("alpha", base, healthy), 0);
}

//! Anomaly scoring (issue #263): a 0–100 severity for each turn that did not
//! price cleanly.
//!
//! The score is a view-layer enrichment: computed at request time from facts the
//! ledger already stores, never written back, and never an input to what a turn
//! charges. The formula is a pure function so the bands are pinned by unit tests
//! without a database.
//!
//! ## The formula
//!
//! Each settlement kind carries a base severity — what the classification alone
//! says about the turn — and the stored facts weight it up:
//!
//! | kind               | base | the reasoning                                            |
//! |--------------------|------|----------------------------------------------------------|
//! | `capped`           |  40  | the charge hit the freeze's ceiling; the ratio weights it |
//! | `unpriced`         |  40  | a dimension the book cannot bill — money the platform ate |
//! | `estimated`        |  30  | usage the provider never reported, priced on a guess      |
//! | `swept`            |  20  | a hold nobody settled, released at zero                   |
//! | `client_cancelled` |  10  | the caller left mid-stream; often benign                  |
//!
//! On top of the base:
//!
//! - **`capped` charge ratio** — `charged / freeze` fills up to +20: a turn that
//!   hit the ceiling exactly is the worst; one that barely brushed it less so.
//! - **Upstream-cost divergence** — when `upstream_cost_minor` is tracked and the
//!   platform charged less than it cost, the shortfall as a share of the cost
//!   adds up to +25.
//! - **Failover** — `upstream_attempts` beyond the first add +10 each (capped
//!   contribution +20): a turn that had to retry across channels is a stronger
//!   signal than one that priced oddly on a single call.
//! - **Repeat offender** — every earlier anomalous row the same `requestId`
//!   already carries adds +5 (capped contribution +15): a request id that keeps
//!   showing up anomalously is worth triaging before a one-off.
//!
//! The sum clamps to 100 and bands at `low` (< 40), `medium` (< 70), `high`
//! (≥ 70).

use crate::billing::SettlementKind;

/// The severity band a score bands into — the filter the anomalies endpoint and
/// page name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Below 40 — worth a look, rarely urgent.
    Low,
    /// 40–69 — the review queue's middle.
    Medium,
    /// 70 and up — where the money leaks.
    High,
}

impl Severity {
    /// The band's stored spelling — what the API answers and filters on.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// The inputs a score reads, each already stored on the turn's settlement or
/// usage row. `Option` fields are `None` when the fact was never tracked — a
/// score degrades gracefully rather than treating "untracked" as zero.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnomalyFacts {
    /// The settlement kind — the base severity.
    pub kind: Option<SettlementKind>,
    /// What the turn charged, in minor units.
    pub charged_minor: i64,
    /// What the turn had frozen, in minor units.
    pub freeze_minor: i64,
    /// The platform's upstream cost for the same turn; `None` when untracked.
    pub upstream_cost_minor: Option<i64>,
    /// How many upstream calls the turn made; above 1 means it failed over.
    pub upstream_attempts: i64,
    /// How many anomalous rows the same request id already carries — the
    /// repeat-offender count `Db::anomaly_inputs` answers (1 on the first).
    pub prior_anomalies: i64,
}

/// Scores one anomalous turn 0–100 from facts the ledger already stores.
///
/// Read-only and total: a turn whose facts are absent scores its kind's base,
/// never refuses. The band names the severity the API filters on.
#[must_use]
pub fn anomaly_score(facts: &AnomalyFacts) -> (u8, Severity) {
    let kind = facts.kind.unwrap_or(SettlementKind::Estimated);
    let mut score: i64 = match kind {
        SettlementKind::Capped => 40,
        SettlementKind::Unpriced => 40,
        SettlementKind::Estimated => 30,
        SettlementKind::Swept => 20,
        SettlementKind::ClientCancelled => 10,
        // A cleanly-priced turn is not an anomaly; scoring one anyway yields 0.
        SettlementKind::Usage
        | SettlementKind::UpstreamError
        | SettlementKind::UpstreamUnreachable
        | SettlementKind::Released
        | SettlementKind::Refused => 0,
    };

    // The capped ratio: how hard the charge pressed against the freeze's ceiling.
    if kind == SettlementKind::Capped && facts.freeze_minor > 0 {
        let ratio = facts.charged_minor.max(0) * 100 / facts.freeze_minor;
        score += (ratio * 20 / 100).min(20);
    }

    // Upstream-cost divergence: charged less than the turn cost the platform.
    if let Some(cost) = facts.upstream_cost_minor
        && cost > 0
        && facts.charged_minor < cost
    {
        let shortfall_share = (cost - facts.charged_minor) * 100 / cost;
        score += (shortfall_share * 25 / 100).min(25);
    }

    // Failover: every upstream attempt past the first.
    if facts.upstream_attempts > 1 {
        score += ((facts.upstream_attempts - 1) * 10).min(20);
    }

    // Repeat offender: earlier anomalous rows on the same request id.
    if facts.prior_anomalies > 0 {
        score += (facts.prior_anomalies * 5).min(15);
    }

    let clamped = score.clamp(0, 100) as u8;
    let band = if clamped >= 70 {
        Severity::High
    } else if clamped >= 40 {
        Severity::Medium
    } else {
        Severity::Low
    };
    (clamped, band)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(kind: SettlementKind) -> AnomalyFacts {
        AnomalyFacts {
            kind: Some(kind),
            charged_minor: 0,
            freeze_minor: 0,
            upstream_cost_minor: None,
            upstream_attempts: 1,
            prior_anomalies: 0,
        }
    }

    #[test]
    fn each_kind_carries_its_base() {
        assert_eq!(anomaly_score(&facts(SettlementKind::Capped)).0, 40);
        assert_eq!(anomaly_score(&facts(SettlementKind::Unpriced)).0, 40);
        assert_eq!(anomaly_score(&facts(SettlementKind::Estimated)).0, 30);
        assert_eq!(anomaly_score(&facts(SettlementKind::Swept)).0, 20);
        assert_eq!(anomaly_score(&facts(SettlementKind::ClientCancelled)).0, 10);
        assert_eq!(anomaly_score(&facts(SettlementKind::Usage)).0, 0);
    }

    #[test]
    fn a_capped_turn_weights_by_the_charge_ratio() {
        // 900k charged of a 1M freeze: ratio 90 → +18.
        let mut f = facts(SettlementKind::Capped);
        f.freeze_minor = 1_000_000;
        f.charged_minor = 900_000;
        assert_eq!(anomaly_score(&f).0, 58);
        // A full press to the ceiling adds the whole 20.
        f.charged_minor = 1_000_000;
        assert_eq!(anomaly_score(&f).0, 60);
    }

    #[test]
    fn an_untracked_cost_adds_nothing() {
        let mut f = facts(SettlementKind::Unpriced);
        f.upstream_cost_minor = None;
        assert_eq!(anomaly_score(&f).0, 40);
    }

    #[test]
    fn charging_less_than_cost_adds_divergence() {
        // Charged 100 of a 400 cost: shortfall 300/400 → +18.
        let mut f = facts(SettlementKind::Unpriced);
        f.charged_minor = 100;
        f.upstream_cost_minor = Some(400);
        assert_eq!(anomaly_score(&f).0, 58);
        // Charged nothing of a cost: the whole 25.
        f.charged_minor = 0;
        assert_eq!(anomaly_score(&f).0, 65);
    }

    #[test]
    fn failover_and_repeats_add_up() {
        let mut f = facts(SettlementKind::Estimated);
        f.upstream_attempts = 3; // +20
        f.prior_anomalies = 2; // +10
        assert_eq!(anomaly_score(&f).0, 60);
    }

    #[test]
    fn the_score_clamps_and_bands() {
        let mut f = facts(SettlementKind::Capped);
        f.freeze_minor = 1_000_000;
        f.charged_minor = 1_000_000; // +20
        f.upstream_cost_minor = Some(2_000_000); // charged < cost → +25
        f.upstream_attempts = 4; // +30 → capped at +20
        f.prior_anomalies = 9; // +45 → capped at +15
        let (score, band) = anomaly_score(&f);
        assert_eq!(score, 100);
        assert_eq!(band, Severity::High);

        // Band edges.
        let mut low = facts(SettlementKind::ClientCancelled);
        low.prior_anomalies = 1; // 10 + 5 = 15 → low
        assert_eq!(anomaly_score(&low).1, Severity::Low);
        let mut mid = facts(SettlementKind::Capped);
        mid.freeze_minor = 100;
        mid.charged_minor = 100; // 40 + 20 = 60 → medium
        assert_eq!(anomaly_score(&mid).1, Severity::Medium);
    }
}

//! Upstream price drift: what the scheduled `SYNC_UPSTREAM_PRICES` job writes
//! when a channel's live catalog no longer agrees with the rates oxsum stores
//! (issue #222).
//!
//! Pricing is fail-closed — a dimension the price book cannot bill settles
//! `unpriced` — which makes a *stale* upstream rate worse than an absent one:
//! it silently under- or over-charges the deployment's margin on every turn
//! until someone notices. The job probes each channel's oxsum-compatible
//! `GET {baseUrl}/api/v1/pricing`, diffs every model's `upstream` rates against
//! the stored `upstream_prices`, and this module persists the result.
//!
//! Drift is *open state*, not an event log: one row per divergent
//! `(channel, model, field)`, upserted on every detection and resolved the
//! moment the stored and live rates agree again. Reconciliation's
//! `upstream_price_drift` class reports the outstanding count, and a rate that
//! re-converges clears itself with no operator touching a queue. Every drift
//! also broadcasts `anomaly.detected` — platform scope, fanned out per enabled
//! subscriber because `upstream` rates are already public on `/api/v1/pricing`.
//!
//! A channel that cannot be probed records a `upstream_price_probes` outcome
//! for that run and is left alone: drift is detected, never inferred from
//! silence.

use serde::Serialize;

use crate::billing::UpstreamPrices;
use crate::db::Db;
use crate::error::WalletError;
use crate::webhooks::ANOMALY_DETECTED;

/// The camelCase wire name a drift row names on its `field` — the
/// [`UpstreamPrices`] key, so the drift and the catalog spell the rate alike.
#[derive(Debug, Clone, Copy)]
pub struct DriftField {
    /// The wire/`field` spelling, e.g. `inputPricePerMillion`.
    pub name: &'static str,
    /// Reads the rate out of an [`UpstreamPrices`].
    pub get: fn(&UpstreamPrices) -> Option<i64>,
}

/// Every [`UpstreamPrices`] dimension, in declaration order. `diff_upstream`
/// iterates it, so a field added to the struct needs one line here.
const UPSTREAM_FIELDS: &[DriftField] = &[
    DriftField {
        name: "inputPricePerMillion",
        get: |p| p.input_price_per_million,
    },
    DriftField {
        name: "outputPricePerMillion",
        get: |p| p.output_price_per_million,
    },
    DriftField {
        name: "cacheReadPricePerMillion",
        get: |p| p.cache_read_price_per_million,
    },
    DriftField {
        name: "cacheWrite5mPricePerMillion",
        get: |p| p.cache_write_5m_price_per_million,
    },
    DriftField {
        name: "cacheWrite1hPricePerMillion",
        get: |p| p.cache_write_1h_price_per_million,
    },
    DriftField {
        name: "reasoningPricePerMillion",
        get: |p| p.reasoning_price_per_million,
    },
    DriftField {
        name: "costPerRequest",
        get: |p| p.cost_per_request,
    },
];

/// One dimension where the stored and live upstream rates disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDrift {
    /// The wire name of the divergent field.
    pub field: &'static str,
    /// What oxsum stores, or `None` when the stored price has no rate here.
    pub stored_minor: Option<i64>,
    /// What the live catalog answers, or `None` when it prices the model but
    /// carries no rate here.
    pub live_minor: Option<i64>,
}

/// How one channel's probe ended, recorded on `upstream_price_probes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The catalog was fetched and diffed.
    Ok,
    /// The request never reached a usable HTTP answer.
    Unreachable,
    /// The response was not an oxsum-compatible catalog.
    Invalid,
    /// A credential problem — the sealed key would not open, or 401/403.
    Auth,
}

impl ProbeOutcome {
    /// The `outcome` column's spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unreachable => "unreachable",
            Self::Invalid => "invalid",
            Self::Auth => "auth",
        }
    }
}

/// Diffs a stored against a live [`UpstreamPrices`], field by field. A `None`
/// either side is *not* equal to a `Some` the other: an absent stored rate with
/// a live rate, and a stored rate the live catalog dropped, both drift.
#[must_use]
pub fn diff_upstream(stored: &UpstreamPrices, live: &UpstreamPrices) -> Vec<FieldDrift> {
    UPSTREAM_FIELDS
        .iter()
        .filter_map(|field| {
            let stored_minor = (field.get)(stored);
            let live_minor = (field.get)(live);
            (stored_minor != live_minor).then_some(FieldDrift {
                field: field.name,
                stored_minor,
                live_minor,
            })
        })
        .collect()
}

/// One `(channel, model)`'s stored and live `upstream` rates, paired for the
/// diff. `stored` is the `price.upstream` of the model's current version (the
/// same object `/api/v1/pricing` serves); `live` is the `upstream` block the
/// probed catalog answers for the model, `None` when the model is absent from
/// the catalog — an absent model is left to [`reconcile_model`], not diffed
/// field-by-field.
pub struct ModelPair<'a> {
    /// The model both sides name.
    pub model: &'a str,
    /// What oxsum stores, or `None` when the model's stored price has no
    /// upstream block.
    pub stored: Option<&'a UpstreamPrices>,
    /// What the live catalog answers, or `None` when the catalog does not
    /// carry the model.
    pub live: Option<&'a UpstreamPrices>,
}

/// The drift a probe's broadcast payload carries — the event's `data`, so a
/// receiver reads the channel, model, field and both rates without a lookup.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DriftEvent<'a> {
    kind: &'static str,
    channel: &'a str,
    model: &'a str,
    field: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stored_minor: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_minor: Option<i64>,
}

impl Db {
    /// Records how one channel's probe ended. Called for every probe — reached
    /// and diffed, or one of the failure outcomes — so the run leaves evidence
    /// even where the catalog never answered.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_probe(
        &self,
        channel: &str,
        outcome: ProbeOutcome,
        detail: &str,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.upstream_price_probes (channel, outcome, detail) \
             VALUES ($1, $2, $3)",
        )
        .bind(channel)
        .bind(outcome.as_str())
        .bind(detail)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Reconciles one channel's stored `upstream` rates against the live rates
    /// a probe fetched, and persists the divergence. For every model the diff
    /// covers: a differing field opens (or refreshes) an open drift row and
    /// broadcasts `anomaly.detected`; an agreeing field resolves any open row.
    /// A model the live catalog dropped is *not* drift — its fields stay under
    /// the stored price, and dropping the catalog entry is a catalog change,
    /// not a rate change this class tracks; the probe row records the reach.
    ///
    /// `pairs` is one entry per model to check, stored and live side by side.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn sync_upstream_prices(
        &self,
        channel: &str,
        pairs: &[ModelPair<'_>],
    ) -> Result<(), WalletError> {
        for pair in pairs {
            self.reconcile_model(channel, pair).await?;
        }
        Ok(())
    }

    /// One model's stored-vs-live diff, persisted and broadcast.
    async fn reconcile_model(
        &self,
        channel: &str,
        pair: &ModelPair<'_>,
    ) -> Result<(), WalletError> {
        // The diff needs both sides. A model absent from the live catalog, or
        // a stored price with no upstream block, has no two rates to disagree:
        // resolve nothing open against it and move on. Only a field where both
        // sides exist but differ, or where one side has a rate the other
        // dropped, is drift.
        match (pair.stored, pair.live) {
            (Some(stored), Some(live)) => {
                for drift in diff_upstream(stored, live) {
                    self.open_drift(channel, pair.model, &drift).await?;
                }
                // Fields that agree resolve any open drift on them.
                for field in UPSTREAM_FIELDS {
                    if (field.get)(stored) == (field.get)(live) {
                        self.resolve_drift(channel, pair.model, field.name).await?;
                    }
                }
                Ok(())
            }
            // No live `upstream` block for a model the catalog carries, or no
            // model at all: nothing on the stored side is contradicted, so any
            // previously-open drift resolves — the catalog no longer disagrees.
            _ => self.resolve_model(channel, pair.model).await,
        }
    }

    /// Opens or refreshes the drift row for `(channel, model, field)` and
    /// broadcasts the event. The partial unique index on open rows makes the
    /// upsert a no-op when the same drift is re-detected — `last_seen_at`
    /// advances but the count stays one.
    async fn open_drift(
        &self,
        channel: &str,
        model: &str,
        drift: &FieldDrift,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.upstream_price_drift \
                 (channel, model, field, stored_minor, live_minor) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (channel, model, field) WHERE resolved_at IS NULL \
             DO UPDATE SET stored_minor = EXCLUDED.stored_minor, \
                           live_minor = EXCLUDED.live_minor, \
                           last_seen_at = now()",
        )
        .bind(channel)
        .bind(model)
        .bind(drift.field)
        .bind(drift.stored_minor)
        .bind(drift.live_minor)
        .execute(self.pool())
        .await?;
        self.broadcast_event(
            ANOMALY_DETECTED,
            serde_json::to_value(DriftEvent {
                kind: "upstream_price_drift",
                channel,
                model,
                field: drift.field,
                stored_minor: drift.stored_minor,
                live_minor: drift.live_minor,
            })
            .map_err(|error| WalletError::Misconfigured(format!("drift event: {error}")))?,
        )
        .await
    }

    /// Resolves the open drift on `(channel, model, field)`, if any.
    async fn resolve_drift(
        &self,
        channel: &str,
        model: &str,
        field: &str,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.upstream_price_drift SET resolved_at = now() \
             WHERE channel = $1 AND model = $2 AND field = $3 AND resolved_at IS NULL",
        )
        .bind(channel)
        .bind(model)
        .bind(field)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Resolves every open drift on `(channel, model)` — used when the live
    /// catalog stops carrying a usable `upstream` block for the model.
    async fn resolve_model(&self, channel: &str, model: &str) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.upstream_price_drift SET resolved_at = now() \
             WHERE channel = $1 AND model = $2 AND resolved_at IS NULL",
        )
        .bind(channel)
        .bind(model)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

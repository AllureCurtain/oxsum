//! Experiments (issue #509): A/B variant sets a request opts into — the relay
//! picks a variant by weight and serves its model, the settlement records
//! which ran, and a results read groups spend/usage by variant for offline
//! scoring.
//!
//! Scoring stays offline: oxsum reports which variant served, it does not
//! judge outputs or promote a winner. The honest boundary a billing wallet
//! keeps — the bill proves which model answered; whether that answer was
//! *better* is the customer's own eval, run on the responses they hold.
//!
//! A variant is just a model name plus a weight: it resolves through the same
//! `servings_named` a plain request uses, so a variant can only name a model
//! the org could already call — the write validates against the served catalog,
//! and the pick reuses `route_order`'s weighted roll rather than a second
//! router. The table is mutable config (a binding an admin edits), not ledger,
//! for the same reason `model_fallbacks` is its own table: config changes, the
//! ledger only accrues.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// The longest an experiment name or a variant's model name may be — the same
/// bound the model allowlist keeps (`keys.rs`), since a variant names a model.
const MAX_NAME: usize = 200;
/// The most variants one experiment may carry — a pick among more than a
/// handful is a config error, not a real experiment.
const MAX_VARIANTS: usize = 8;

/// One variant of an experiment: the model it serves and its relative share of
/// the pick, as `variants` stores it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    /// The model this variant serves — resolved through `servings_named`, so
    /// it must name a model with a current price (or an alias that resolves).
    pub model: String,
    /// The variant's relative share of the pick — ≥ 1.
    pub weight: i64,
}

/// One experiment as the member surface answers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Experiment {
    /// The experiment's own id — what `X-Oxsum-Experiment` and `experimentId`
    /// name.
    pub id: Uuid,
    pub name: String,
    /// The variant set `[{model, weight}]`.
    pub variants: Vec<Variant>,
    /// `open` serves variants; `ended` resolves the plain model instead.
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
}

/// The experiment one turn serves under, carried from the pick to the settle:
/// the v13 settlement description's `experiment` block and the usage row's
/// `usage_details.experiment` both read it (issue #509).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExperimentBinding {
    /// The experiment's own id (`oxsum.experiments.id`).
    pub experiment_id: Uuid,
    /// The variant the weighted pick landed on — a `variants[].model` name.
    pub variant: String,
}

/// One variant's slice of the results read: the usage the experiment served
/// under it, grouped for the offline scorer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantResult {
    /// The variant's model name — the group key.
    pub variant: String,
    /// How many settled turns served this variant.
    pub turns: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// What the variant's turns charged, in minor units.
    pub charged_minor: i64,
    /// How many turns errored (served nothing after their hold).
    pub errored: i64,
}

impl Db {
    /// The experiments of an organization, oldest first.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn list_experiments(&self, org_id: Uuid) -> Result<Vec<Experiment>, WalletError> {
        let rows = sqlx::query(
            "SELECT id, name, variants, status, created_at, ended_at \
             FROM oxsum.experiments WHERE org_id = $1 ORDER BY created_at",
        )
        .bind(org_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(experiment_from_row).collect()
    }

    /// One open experiment of the organization by name — what a request's
    /// `X-Oxsum-Experiment` resolves. `None` for an unknown or ended name: an
    /// ended experiment serves no variants.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn experiment_named(
        &self,
        org_id: Uuid,
        name: &str,
    ) -> Result<Option<Experiment>, WalletError> {
        let row = sqlx::query(
            "SELECT id, name, variants, status, created_at, ended_at \
             FROM oxsum.experiments WHERE org_id = $1 AND name = $2 AND status = 'open'",
        )
        .bind(org_id)
        .bind(name)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(experiment_from_row).transpose()
    }

    /// One experiment of the organization by id — for the results read, which
    /// answers an ended experiment too (its recorded spend still groups).
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn experiment_by_id(
        &self,
        org_id: Uuid,
        id: Uuid,
    ) -> Result<Option<Experiment>, WalletError> {
        let row = sqlx::query(
            "SELECT id, name, variants, status, created_at, ended_at \
             FROM oxsum.experiments WHERE org_id = $1 AND id = $2",
        )
        .bind(org_id)
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(experiment_from_row).transpose()
    }

    /// Creates or replaces one experiment's variant set atomically — config,
    /// not ledger, so `ON CONFLICT DO UPDATE`, never an append. Every variant's
    /// `model` must resolve to a served name (or an alias): a variant that
    /// names an unserved model would dead-end every request it lands on, so the
    /// write refuses before it lands — the same name-must-resolve rule
    /// `set_fallbacks` keeps. An empty set clears to zero variants (a stored
    /// experiment serving nothing is a config error the pick will never reach —
    /// the read refuses it).
    ///
    /// # Errors
    ///
    /// `WalletError::InvalidInput` when a variant's model is unserved, a weight
    /// is below 1, a name is empty or over-long, or the set exceeds
    /// [`MAX_VARIANTS`]; storage failures surface as `WalletError`.
    pub async fn put_experiment(
        &self,
        org_id: Uuid,
        name: &str,
        variants: &[Variant],
    ) -> Result<Experiment, WalletError> {
        validate_experiment(name, variants)?;
        let access = self.serving_access(org_id, None).await?;
        let served: HashSet<_> = self
            .catalog_for_access(&access)
            .await?
            .into_iter()
            .map(|route| route.model)
            .collect();
        for variant in variants {
            if !served.contains(&variant.model) {
                return Err(WalletError::InvalidInput(
                    "variant is not an available customer model".into(),
                ));
            }
        }
        let row = sqlx::query(
            "INSERT INTO oxsum.experiments (org_id, name, variants, status) \
             VALUES ($1, $2, $3, 'open') \
             ON CONFLICT (org_id, name) DO UPDATE \
             SET variants = $3, status = 'open', ended_at = NULL \
             RETURNING id, name, variants, status, created_at, ended_at",
        )
        .bind(org_id)
        .bind(name)
        .bind(serde_json::to_value(variants).unwrap_or_default())
        .fetch_one(self.pool())
        .await?;
        experiment_from_row(&row)
    }

    /// Ends one experiment: flips `status` to `ended`, so a later request under
    /// it resolves the plain model. `false` when no such experiment — or it is
    /// already ended.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn end_experiment(&self, org_id: Uuid, name: &str) -> Result<bool, WalletError> {
        let done = sqlx::query(
            "UPDATE oxsum.experiments SET status = 'ended', ended_at = now() \
             WHERE org_id = $1 AND name = $2 AND status = 'open'",
        )
        .bind(org_id)
        .bind(name)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(done > 0)
    }

    /// The usage one experiment served, grouped by variant: turns, token and
    /// charge sums, and the errored count — the columns an offline scorer
    /// groups its judgment over. Reads `usage_details -> 'experiment'`, the
    /// same escape hatch the cache hit uses, so no schema column is spent on
    /// what is already recorded.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn experiment_results(
        &self,
        org_id: Uuid,
        experiment_id: Uuid,
    ) -> Result<Vec<VariantResult>, WalletError> {
        let rows = sqlx::query(
            "SELECT usage_details -> 'experiment' ->> 'variant' AS variant, \
                    count(*) AS turns, \
                    coalesce(sum(input_tokens), 0)::bigint AS input_tokens, \
                    coalesce(sum(output_tokens), 0)::bigint AS output_tokens, \
                    coalesce(sum(charged_minor), 0)::bigint AS charged_minor, \
                    coalesce(count(*) FILTER (WHERE status = 'errored'), 0)::bigint AS errored \
             FROM oxsum.usage_records \
             WHERE tenant_id = $1 \
               AND usage_details -> 'experiment' ->> 'id' = $2::text \
             GROUP BY 1 ORDER BY 1",
        )
        // usage_records.tenant_id is the ledger tenant id, which is the org's
        // uuid without dashes — the org's own tenant, not a member's.
        .bind(org_id.simple().to_string())
        .bind(experiment_id.to_string())
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(VariantResult {
                    variant: row
                        .try_get::<Option<String>, _>("variant")?
                        .unwrap_or_default(),
                    turns: row.try_get("turns")?,
                    input_tokens: row.try_get("input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    charged_minor: row.try_get("charged_minor")?,
                    errored: row.try_get("errored")?,
                })
            })
            .collect()
    }
}

/// The variant a request's weighted pick lands on: `roll` scaled into the
/// summed weights walks the variants in order — the same convention
/// `route_order` uses, kept deterministic for a passed `roll` so a test can
/// walk the range. `None` on an empty set (a stored experiment the write should
/// never have admitted — the pick treats it as no experiment).
#[must_use]
pub fn pick_variant(variants: &[Variant], roll: f64) -> Option<&Variant> {
    if variants.is_empty() {
        return None;
    }
    let total: i64 = variants.iter().map(|v| v.weight.max(0)).sum();
    if total <= 0 {
        return variants.first();
    }
    let mut mark = (roll.clamp(0.0, 1.0 - f64::EPSILON) * total as f64) as i64;
    for variant in variants {
        mark -= variant.weight.max(0);
        if mark < 0 {
            return Some(variant);
        }
    }
    variants.last()
}

/// The write-time checks a variant set must pass before the served-model
/// lookup: name bounds, a weight floor of 1, and the set-size cap.
fn validate_experiment(name: &str, variants: &[Variant]) -> Result<(), WalletError> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(WalletError::InvalidInput(format!(
            "experiment name is 1..={MAX_NAME} bytes"
        )));
    }
    if variants.is_empty() {
        return Err(WalletError::InvalidInput(
            "an experiment needs at least one variant".into(),
        ));
    }
    if variants.len() > MAX_VARIANTS {
        return Err(WalletError::InvalidInput(format!(
            "an experiment carries at most {MAX_VARIANTS} variants"
        )));
    }
    for variant in variants {
        if variant.model.is_empty() || variant.model.len() > MAX_NAME {
            return Err(WalletError::InvalidInput(format!(
                "a variant's model is 1..={MAX_NAME} bytes"
            )));
        }
        if variant.weight < 1 {
            return Err(WalletError::InvalidInput(
                "a variant's weight is at least 1".into(),
            ));
        }
    }
    Ok(())
}

/// One row of `oxsum.experiments` as an [`Experiment`].
fn experiment_from_row(row: &sqlx::postgres::PgRow) -> Result<Experiment, WalletError> {
    let variants: Vec<Variant> =
        serde_json::from_value(row.try_get("variants")?).unwrap_or_default();
    Ok(Experiment {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        variants,
        status: row.try_get("status")?,
        created_at: row.try_get("created_at")?,
        ended_at: row.try_get("ended_at")?,
    })
}

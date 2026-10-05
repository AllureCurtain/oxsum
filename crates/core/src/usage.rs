//! What one settled turn used, in every dimension the price book can look at.
//!
//! [`UsageRecord`] is the normalized shape every upstream protocol is read into (the
//! adapter layer owns that normalization). Two invariants are what make one record
//! comparable across protocols: `input_tokens` *includes* the cached part and
//! `output_tokens` *includes* the reasoning part, with `cached_tokens` and
//! `reasoning_tokens` naming the subsets — a provider that does not follow that
//! convention is normalized by its adapter, not by the pricing layer.
//!
//! The settled row ([`UsageRow`], `oxsum.usage_records`) is the record's mutable
//! home. It is written beside the settlement write — the ledger entry stays the
//! source of truth — because it holds what a proof's content hash deliberately
//! does not: `end_user` and `tags` are pseudonymized when an organization is
//! deleted, and `usage_details.provider_raw` is nulled after the retention
//! window, both impossible inside an immutable entry description.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::billing::SettlementKind;
use crate::db::Db;
use crate::error::WalletError;

/// The longest a caller-declared end-user id may be (docs/decisions.md: `endUser`
/// writes freely but bounded).
pub const MAX_END_USER: usize = 128;

/// The most tag pairs a record may carry.
pub const MAX_TAGS: usize = 10;

/// The longest a tag key or value may be.
pub const MAX_TAG: usize = 64;

/// The longest a service-tier or event-type name may be: a slot the price book
/// matches on, not free text.
pub const MAX_CONTEXT_NAME: usize = 64;

/// One turn's usage, normalized: the token-metered dimensions as columns, plus the
/// context the caller supplied, plus the escape hatch for everything else.
///
/// The structured fields hold token-metered quantities only. Units that are not
/// tokens — per image, per second, per request — ride in [`usage_details`]
/// instead, so the columns never mix units (docs/decisions.md).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UsageRecord {
    /// Prompt-side tokens, *including* the cached subset. What upstream reported,
    /// or a local estimate standing in for it.
    pub input_tokens: i64,
    /// Completion-side tokens, *including* the reasoning subset.
    pub output_tokens: i64,
    /// The part of `input_tokens` that hit a provider cache, billed at its own
    /// (lower) price. Never more than `input_tokens`.
    pub cached_tokens: i64,
    /// Tokens written into a short-lived provider cache (Anthropic's 5-minute
    /// tier), a billing dimension of its own.
    pub cache_write_5m_tokens: i64,
    /// Tokens written into a long-lived provider cache (the 1-hour tier), priced
    /// separately from the short tier.
    pub cache_write_1h_tokens: i64,
    /// The part of `output_tokens` that is model reasoning, billed at its own
    /// price when the price book prices it. Never more than `output_tokens`.
    pub reasoning_tokens: i64,
    /// Billable tool invocations the turn made (web search and friends), priced
    /// per call.
    pub tool_calls: i64,
    /// Media tokens, metered the same way as text but priced on their own rows.
    pub image_input_tokens: i64,
    pub audio_input_tokens: i64,
    pub video_input_tokens: i64,
    pub image_output_tokens: i64,
    pub audio_output_tokens: i64,
    /// The service tier the caller asked for (OpenAI's `service_tier` and its
    /// equivalents): a schema slot — it is recorded and forwarded, and prices
    /// may match on it, but nothing prices it yet.
    pub service_tier: Option<String>,
    /// What kind of billable event this record is (`None` for a native gateway
    /// turn; external metering events name theirs).
    pub event_type: Option<String>,
    /// The caller's own end-user id, for attribution — which of the customer's
    /// users spent. Never an input to pricing.
    pub end_user: Option<String>,
    /// The caller's own tags on the turn: cost-centre attribution and, later,
    /// tag-scoped limits. Writes are free within the bounds; no allowlist.
    pub tags: BTreeMap<String, String>,
    /// Provider-specific extras that fit no column, including `provider_raw`:
    /// upstream's usage object verbatim, retained for the reconciliation window
    /// and then nulled — it may carry prompt fragments, so it never reaches a
    /// ledger description (docs/decisions.md, data retention).
    pub usage_details: Option<Value>,
}

/// The caller-supplied part of a turn's usage record: attribution and the tier
/// slot. What a request declares; the token counts are upstream's, not the
/// caller's.
#[derive(Debug, Clone, Default)]
pub struct Attribution {
    /// [`UsageRecord::end_user`], bounded to [`MAX_END_USER`].
    pub end_user: Option<String>,
    /// [`UsageRecord::tags`], bounded to [`MAX_TAGS`] pairs of [`MAX_TAG`].
    pub tags: BTreeMap<String, String>,
    /// [`UsageRecord::service_tier`], bounded to [`MAX_CONTEXT_NAME`].
    pub service_tier: Option<String>,
}

impl UsageRecord {
    /// A record of the two counts alone: what a local estimate produces.
    ///
    /// # Errors
    ///
    /// Refuses a negative count, as [`validate`](Self::validate).
    pub fn tokens(input_tokens: i64, output_tokens: i64) -> Result<Self, WalletError> {
        let record = Self {
            input_tokens,
            output_tokens,
            ..Self::default()
        };
        record.validate()?;
        Ok(record)
    }

    /// What this record would have been before normalization: subset counts clamped
    /// into the totals they belong to.
    ///
    /// An adapter calls this rather than rejecting upstream's report outright: a
    /// provider that reports `cached > input` is wrong, and clamping bills the
    /// bounded claim instead of discarding the counts for an estimate.
    pub fn clamped(mut self) -> Self {
        self.cached_tokens = self.cached_tokens.clamp(0, self.input_tokens.max(0));
        // The cache-write tiers belong to the input total too, behind the
        // cached read: clamp what is left.
        let rest = self.input_tokens - self.cached_tokens;
        self.cache_write_5m_tokens = self.cache_write_5m_tokens.clamp(0, rest.max(0));
        self.cache_write_1h_tokens = self
            .cache_write_1h_tokens
            .clamp(0, (rest - self.cache_write_5m_tokens).max(0));
        self.reasoning_tokens = self.reasoning_tokens.clamp(0, self.output_tokens.max(0));
        self
    }

    /// Checks the invariants no downstream arithmetic can repair.
    ///
    /// # Errors
    ///
    /// Names the field when a count is negative, a subset exceeds its total, or an
    /// attribution field exceeds its bound.
    pub fn validate(&self) -> Result<(), WalletError> {
        for (name, count) in [
            ("inputTokens", self.input_tokens),
            ("outputTokens", self.output_tokens),
            ("cachedTokens", self.cached_tokens),
            ("cacheWrite5mTokens", self.cache_write_5m_tokens),
            ("cacheWrite1hTokens", self.cache_write_1h_tokens),
            ("reasoningTokens", self.reasoning_tokens),
            ("toolCalls", self.tool_calls),
            ("imageInputTokens", self.image_input_tokens),
            ("audioInputTokens", self.audio_input_tokens),
            ("videoInputTokens", self.video_input_tokens),
            ("imageOutputTokens", self.image_output_tokens),
            ("audioOutputTokens", self.audio_output_tokens),
        ] {
            if count < 0 {
                return Err(WalletError::InvalidInput(format!(
                    "{name} cannot be negative"
                )));
            }
        }
        // Cached reads and both cache-write tiers are all part of `inputTokens` —
        // a provider whose writes sit outside the prompt count must fold them in
        // at the adapter, or the charge's input-side lines cannot account for it.
        if self.cached_tokens + self.cache_write_5m_tokens + self.cache_write_1h_tokens
            > self.input_tokens
        {
            return Err(WalletError::InvalidInput(
                "the cached and cache-write tokens together cannot exceed inputTokens".into(),
            ));
        }
        if self.reasoning_tokens > self.output_tokens {
            return Err(WalletError::InvalidInput(
                "reasoningTokens cannot exceed outputTokens".into(),
            ));
        }
        validate_attribution(&self.end_user, &self.tags, &self.service_tier)?;
        if let Some(event_type) = &self.event_type
            && event_type.len() > MAX_CONTEXT_NAME
        {
            return Err(WalletError::InvalidInput(format!(
                "eventType is at most {MAX_CONTEXT_NAME} characters"
            )));
        }
        Ok(())
    }
}

/// Checks the bounds on caller-declared attribution: what a request parser and
/// the record's own [`validate`](UsageRecord::validate) both enforce.
///
/// # Errors
///
/// Names the offending field with its bound.
pub fn validate_attribution(
    end_user: &Option<String>,
    tags: &BTreeMap<String, String>,
    service_tier: &Option<String>,
) -> Result<(), WalletError> {
    if let Some(end_user) = end_user
        && end_user.len() > MAX_END_USER
    {
        return Err(WalletError::InvalidInput(format!(
            "endUser is at most {MAX_END_USER} characters"
        )));
    }
    if tags.len() > MAX_TAGS {
        return Err(WalletError::InvalidInput(format!(
            "tags carry at most {MAX_TAGS} entries"
        )));
    }
    for (key, value) in tags {
        if key.is_empty() || key.len() > MAX_TAG {
            return Err(WalletError::InvalidInput(format!(
                "a tag key is 1-{MAX_TAG} characters"
            )));
        }
        if value.len() > MAX_TAG {
            return Err(WalletError::InvalidInput(format!(
                "a tag value is at most {MAX_TAG} characters"
            )));
        }
    }
    if let Some(service_tier) = service_tier
        && service_tier.len() > MAX_CONTEXT_NAME
    {
        return Err(WalletError::InvalidInput(format!(
            "serviceTier is at most {MAX_CONTEXT_NAME} characters"
        )));
    }
    Ok(())
}

/// One settled turn's row in `oxsum.usage_records`: the normalized record plus
/// the context of what charged it.
///
/// A row is written only by the path whose settlement landed — the turn itself,
/// or the sweeper for a timed-out hold — never for a settlement the ledger
/// refused, so `request_id` can be the primary key: a second writer can only be
/// the same turn replaying and is ignored.
#[derive(Debug, Clone)]
pub struct UsageRow {
    /// From `x-oxsum-request-id`.
    pub request_id: String,
    /// The organization whose ledger settled the turn, as its ledger tenant id.
    pub tenant_id: String,
    /// The key that paid, where the hold attributed one.
    pub key_id: Option<Uuid>,
    /// What the turn was priced by, copied from the settlement record.
    pub model: String,
    pub channel: String,
    pub price_version: i64,
    /// How the turn was priced, the settlement record's kind.
    pub kind: SettlementKind,
    /// The settlement entry's id: derived from the hold's key, so it is known
    /// before the write answers.
    pub entry_id: Uuid,
    /// The normalized usage.
    pub usage: UsageRecord,
    /// What the settlement charged and what it had frozen, in minor units.
    pub charged_minor: i64,
    pub freeze_minor: i64,
}

impl Db {
    /// Records one settled turn's usage. Ignores a second writer of the same
    /// `request_id` — a replay rewrites nothing, and the first row stands.
    ///
    /// Called beside the settlement write, on the caller's pool: the ledger entry
    /// is the source of truth, and a missing row is drift for the reconciler to
    /// report, not a second opinion about money.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_usage(&self, row: &UsageRow) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.usage_records \
             (request_id, tenant_id, key_id, model, channel, price_version, kind, entry_id, \
              input_tokens, output_tokens, cached_tokens, cache_write_5m_tokens, \
              cache_write_1h_tokens, reasoning_tokens, tool_calls, image_input_tokens, \
              audio_input_tokens, video_input_tokens, image_output_tokens, \
              audio_output_tokens, service_tier, event_type, end_user, tags, usage_details, \
              charged_minor, freeze_minor) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                     $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27) \
             ON CONFLICT (request_id) DO NOTHING",
        )
        .bind(&row.request_id)
        .bind(&row.tenant_id)
        .bind(row.key_id)
        .bind(&row.model)
        .bind(&row.channel)
        .bind(row.price_version)
        .bind(row.kind.as_str())
        .bind(row.entry_id)
        .bind(row.usage.input_tokens)
        .bind(row.usage.output_tokens)
        .bind(row.usage.cached_tokens)
        .bind(row.usage.cache_write_5m_tokens)
        .bind(row.usage.cache_write_1h_tokens)
        .bind(row.usage.reasoning_tokens)
        .bind(row.usage.tool_calls)
        .bind(row.usage.image_input_tokens)
        .bind(row.usage.audio_input_tokens)
        .bind(row.usage.video_input_tokens)
        .bind(row.usage.image_output_tokens)
        .bind(row.usage.audio_output_tokens)
        .bind(&row.usage.service_tier)
        .bind(&row.usage.event_type)
        .bind(&row.usage.end_user)
        .bind(serde_json::to_value(&row.usage.tags).unwrap_or_default())
        .bind(&row.usage.usage_details)
        .bind(row.charged_minor)
        .bind(row.freeze_minor)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_token_only_record_defaults_the_rest() {
        let record = UsageRecord::tokens(11, 7).unwrap();
        assert_eq!(record.input_tokens, 11);
        assert_eq!(record.output_tokens, 7);
        assert_eq!(record.cached_tokens, 0);
        assert_eq!(record.tags, BTreeMap::new());
        assert_eq!(record.usage_details, None);
    }

    #[test]
    fn counts_cannot_be_negative() {
        assert!(UsageRecord::tokens(-1, 0).is_err());
        let record = UsageRecord {
            cached_tokens: -1,
            ..UsageRecord::default()
        };
        assert!(record.validate().is_err());
    }

    #[test]
    fn subsets_cannot_exceed_their_totals() {
        let mut record = UsageRecord::tokens(10, 5).unwrap();
        record.cached_tokens = 10;
        record.reasoning_tokens = 5;
        record.validate().unwrap();
        record.cached_tokens = 11;
        assert!(record.validate().is_err());
        record.cached_tokens = 10;
        record.reasoning_tokens = 6;
        assert!(record.validate().is_err());
    }

    #[test]
    fn clamping_bounds_subsets_into_their_totals() {
        let mut record = UsageRecord::tokens(10, 5).unwrap();
        record.cached_tokens = 40;
        record.reasoning_tokens = 9;
        let record = record.clamped();
        assert_eq!(record.cached_tokens, 10);
        assert_eq!(record.reasoning_tokens, 5);
        record.validate().unwrap();
    }

    #[test]
    fn attribution_is_bounded() {
        let mut record = UsageRecord::tokens(1, 1).unwrap();
        record.end_user = Some("u".repeat(MAX_END_USER + 1));
        assert!(record.validate().is_err());
        record.end_user = Some("u_123".into());
        record.service_tier = Some("s".repeat(MAX_CONTEXT_NAME + 1));
        assert!(record.validate().is_err());
        record.service_tier = Some("flex".into());
        record.event_type = Some("e".repeat(MAX_CONTEXT_NAME + 1));
        assert!(record.validate().is_err());
        record.event_type = Some("inference".into());
        for i in 0..MAX_TAGS + 1 {
            record.tags.insert(format!("k{i}"), "v".into());
        }
        assert!(record.validate().is_err());
        record.tags.clear();
        record.tags.insert(String::new(), "v".into());
        assert!(record.validate().is_err());
        record.tags.insert("k".into(), "v".repeat(MAX_TAG + 1));
        assert!(record.validate().is_err());
        record.tags.clear();
        record.tags.insert("cost-centre".into(), "eng".into());
        record.validate().unwrap();
    }

    #[test]
    fn the_record_round_trips_through_json() {
        let mut record = UsageRecord::tokens(11_000, 1_500).unwrap();
        record.cached_tokens = 8_000;
        record.cache_write_5m_tokens = 1_000;
        record.reasoning_tokens = 300;
        record.end_user = Some("u_42".into());
        record.service_tier = Some("priority".into());
        record.tags.insert("team".into(), "search".into());
        record.usage_details = Some(serde_json::json!({"provider_raw": {"x": 1}}));
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"cacheWrite5mTokens\":1000"), "{json}");
        assert!(json.contains("\"endUser\":\"u_42\""), "{json}");
        let back: UsageRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
        // An absent field deserializes as its default, so a partial payload parses.
        let sparse: UsageRecord =
            serde_json::from_str(r#"{"inputTokens": 3, "outputTokens": 2}"#).unwrap();
        assert_eq!(sparse, UsageRecord::tokens(3, 2).unwrap());
    }
}

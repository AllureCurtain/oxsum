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
use sqlx::Row;
use time::OffsetDateTime;
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

    /// A stream that reports usage across several frames folds them into one record.
    ///
    /// Anthropic reports the input side in `message_start` and the output side,
    /// cumulatively, in `message_delta`; OpenAI reports once. The merge is per
    /// field: a count keeps the larger of the two reports, because the counts a
    /// protocol reports mid-stream are cumulative, and the provider's raw object
    /// merges shallowly so both frames' fields survive. A record produced this
    /// way is validated exactly like a single-frame one before it is settled.
    pub fn merge_report(&mut self, later: &Self) {
        self.input_tokens = self.input_tokens.max(later.input_tokens);
        self.output_tokens = self.output_tokens.max(later.output_tokens);
        self.cached_tokens = self.cached_tokens.max(later.cached_tokens);
        self.cache_write_5m_tokens = self.cache_write_5m_tokens.max(later.cache_write_5m_tokens);
        self.cache_write_1h_tokens = self.cache_write_1h_tokens.max(later.cache_write_1h_tokens);
        self.reasoning_tokens = self.reasoning_tokens.max(later.reasoning_tokens);
        self.tool_calls = self.tool_calls.max(later.tool_calls);
        self.image_input_tokens = self.image_input_tokens.max(later.image_input_tokens);
        self.audio_input_tokens = self.audio_input_tokens.max(later.audio_input_tokens);
        self.video_input_tokens = self.video_input_tokens.max(later.video_input_tokens);
        self.image_output_tokens = self.image_output_tokens.max(later.image_output_tokens);
        self.audio_output_tokens = self.audio_output_tokens.max(later.audio_output_tokens);
        if later.service_tier.is_some() {
            self.service_tier.clone_from(&later.service_tier);
        }
        if later.event_type.is_some() {
            self.event_type.clone_from(&later.event_type);
        }
        if later.end_user.is_some() {
            self.end_user.clone_from(&later.end_user);
        }
        self.tags.extend(
            later
                .tags
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        if let Some(Value::Object(theirs)) = later.usage_details.as_ref() {
            match self.usage_details.as_mut() {
                Some(Value::Object(mine)) => {
                    for (key, value) in theirs {
                        // `provider_raw` is one object per frame: the two frames'
                        // fields merge, so `message_start`'s input side survives
                        // `message_delta`'s output side.
                        if let (Some(Value::Object(existing)), Value::Object(incoming)) =
                            (mine.get_mut(key.as_str()), value)
                        {
                            for (inner, incoming) in incoming {
                                existing.insert(inner.clone(), incoming.clone());
                            }
                        } else {
                            mine.insert(key.clone(), value.clone());
                        }
                    }
                }
                _ => self.usage_details.clone_from(&later.usage_details),
            }
        }
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

/// Who counted a settled turn's billed counts (issue #230).
///
/// The four `usage_records` columns this maps onto are nullable singly: a
/// degraded settle counts only the input side, so a `Local` provenance may
/// carry `output: None` — NULL means "not counted", which is a different
/// statement from a counted zero.
#[derive(Debug, Clone, Copy, Default)]
pub enum CountsProvenance {
    /// Upstream's usage report produced the counts (`provider_*` columns).
    Provider { input: i64, output: i64 },
    /// A local estimate produced them (`local_*` columns): tiktoken over the
    /// request text and, where the policy counts it, the forwarded prefix.
    /// `output: None` is the degraded settle — only the input side is counted.
    Local { input: i64, output: Option<i64> },
    /// Nothing was counted — a refusal, or a swept turn the watch row timed out.
    #[default]
    None,
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
    /// The mandate the turn's hold ran under (issue #262): the delegation the
    /// settled charge attributes to on the bills page. `None` on an undelegated
    /// turn. The mandate's committed spend is summed from the ledger, not here.
    pub mandate_id: Option<Uuid>,
    /// What the turn was priced by, copied from the settlement record.
    pub model: String,
    pub channel: String,
    pub price_version: i64,
    /// How the turn was priced, the settlement record's kind.
    pub kind: SettlementKind,
    /// The settlement entry's id: derived from the hold's key, so it is known
    /// before the write answers.
    pub entry_id: Uuid,
    /// How the turn ended — [`SettlementKind::status`]'s spelling (issue #233):
    /// `settled` for every settled outcome including the anomalous kinds,
    /// `errored` for the turns that served nothing after their hold, `refused`
    /// for the requests admission turned away before a hold.
    pub status: String,
    /// Why the turn refused or errored, when it did — the words the answer
    /// already carried, so the billing page can show a terminal cause instead
    /// of leaving it in server logs (issue #233).
    pub failure_reason: Option<String>,
    /// The normalized usage — what the settlement billed.
    pub usage: UsageRecord,
    /// Who counted the billed counts (issue #230): the provider's own report
    /// when it made one, the local estimate when it did not, `None` on a
    /// refusal or a sweep that counts nothing. The row's `provider_*`/`local_*`
    /// columns keep the two apart — `kind` says *that* an estimate happened,
    /// the columns say *which* figures it produced. A degraded settle counts
    /// only the input side, so `local` may carry a `None` output: NULL means
    /// "not counted", never a silently-zero bill.
    pub counts: CountsProvenance,
    /// What the settlement charged and what it had frozen, in minor units.
    pub charged_minor: i64,
    pub freeze_minor: i64,
    /// What the channel's `upstream` prices made of the same usage — the
    /// platform's cost for the turn (roadmap P1-6, issue #112). `None` means
    /// untracked, not zero: a price with no `upstream` block, a swept turn
    /// whose watch row carries no price, or history from before the column.
    pub upstream_cost_minor: Option<i64>,
    /// How many upstream calls the turn made (issue #168): above 1 means it
    /// failed over between channels under the one hold, or waited out a
    /// bounded `Retry-After` on a lone channel's 429.
    pub upstream_attempts: i64,
}

impl Db {
    /// Records one settled turn's usage. Ignores a second writer of the same
    /// `request_id` — a replay rewrites nothing, and the first row stands.
    ///
    /// Called beside the settlement write, on the caller's pool: the ledger entry
    /// is the source of truth, and a missing row is drift for the reconciler to
    /// report, not a second opinion about money.
    ///
    /// A row that lands also rolls into `oxsum.usage_daily` in the same
    /// transaction, keyed by the settlement entry's `booking_date` — the same
    /// day a statement period would count the turn under, not the instant the
    /// write happened. A replay inserts no usage row and rolls nothing, so the
    /// daily table can never double-count one turn.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_usage(&self, row: &UsageRow) -> Result<(), WalletError> {
        self.record_usage_balanced(row, None).await
    }

    /// `record_usage` carrying the post-settle balance: `available` is the
    /// organization's balance after this settlement, in minor units, when the
    /// caller holds the wallet to read it — the figure `balance.low` judges its
    /// threshold against (issue #200). `None` (a caller without the wallet in
    /// hand) skips the check rather than guess at a balance.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_usage_balanced(
        &self,
        row: &UsageRow,
        available: Option<i64>,
    ) -> Result<(), WalletError> {
        let mut tx = self.pool().begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO oxsum.usage_records \
             (request_id, tenant_id, key_id, mandate_id, model, channel, price_version, kind, entry_id, \
              input_tokens, output_tokens, cached_tokens, cache_write_5m_tokens, \
              cache_write_1h_tokens, reasoning_tokens, tool_calls, image_input_tokens, \
              audio_input_tokens, video_input_tokens, image_output_tokens, \
              audio_output_tokens, service_tier, event_type, end_user, tags, usage_details, \
              charged_minor, freeze_minor, upstream_cost_minor, upstream_attempts, \
              provider_input_tokens, provider_output_tokens, \
              local_input_tokens, local_output_tokens, \
              status, failure_reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, \
                     $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, \
                     $30, $31, $32, $33, $34, $35, $36) \
             ON CONFLICT (request_id) DO NOTHING",
        )
        .bind(&row.request_id)
        .bind(&row.tenant_id)
        .bind(row.key_id)
        .bind(row.mandate_id)
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
        .bind(row.upstream_cost_minor)
        .bind(row.upstream_attempts)
        .bind(match row.counts {
            CountsProvenance::Provider { input, .. } => Some(input),
            _ => None,
        })
        .bind(match row.counts {
            CountsProvenance::Provider { output, .. } => Some(output),
            _ => None,
        })
        .bind(match row.counts {
            CountsProvenance::Local { input, .. } => Some(input),
            _ => None,
        })
        .bind(match row.counts {
            CountsProvenance::Local {
                output: Some(output),
                ..
            } => Some(output),
            _ => None,
        })
        .bind(&row.status)
        .bind(&row.failure_reason)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            tx.rollback().await?;
            return Ok(());
        }
        // The day comes from the settlement entry itself. A usage row can
        // name a tenant whose ledger does not exist — the margin suite seeds
        // rows for a synthetic tenant — so the rollup guards on the schema
        // the way the migration's backfill does. A row whose entry cannot be
        // found rolls nothing; the missing day is drift for the reconciler.
        // The schema name is quoted and `"`-escaped rather than trusting the
        // tenant-id shape, so a synthetic id cannot break the statement.
        let ledger = format!("ledger_{}", row.tenant_id.replace('"', "\"\""));
        let has_ledger = sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text")
            .bind(format!("\"{ledger}\".entries"))
            .fetch_one(&mut *tx)
            .await?
            .is_some();
        if has_ledger {
            sqlx::query(&format!(
                "INSERT INTO oxsum.usage_daily \
                     (tenant_id, day, key_id, channel, model, turns, \
                      input_tokens, output_tokens, cached_tokens, reasoning_tokens, \
                      charged_minor) \
                 SELECT $1, e.booking_date, $2, $3, $4, 1, $5, $6, $7, $8, $9 \
                 FROM \"{ledger}\".entries e WHERE e.entry_id = $10 \
                 ON CONFLICT (tenant_id, day, channel, model, key_id) DO UPDATE SET \
                     turns = usage_daily.turns + 1, \
                     input_tokens = usage_daily.input_tokens + EXCLUDED.input_tokens, \
                     output_tokens = usage_daily.output_tokens + EXCLUDED.output_tokens, \
                     cached_tokens = usage_daily.cached_tokens + EXCLUDED.cached_tokens, \
                     reasoning_tokens = usage_daily.reasoning_tokens + EXCLUDED.reasoning_tokens, \
                     charged_minor = usage_daily.charged_minor + EXCLUDED.charged_minor",
            ))
            .bind(&row.tenant_id)
            .bind(row.key_id)
            .bind(&row.channel)
            .bind(&row.model)
            .bind(row.usage.input_tokens)
            .bind(row.usage.output_tokens)
            .bind(row.usage.cached_tokens)
            .bind(row.usage.reasoning_tokens)
            .bind(row.charged_minor)
            .bind(row.entry_id)
            .execute(&mut *tx)
            .await?;
        }
        // A settled turn feeds its key's token window in the same transaction —
        // the durable rolling-minute allowance `tokensPerMinute` checks (issue
        // #203). The accrual keys on the settle's own instant (`now()`), because
        // the window is when the spend landed, not the day the ledger books it
        // under; a replayed write lands nothing, so the early return above keeps
        // the window from double-counting. Holds never reach here — only a
        // settle moves the window. Rows past the window are pruned as each
        // settle lands, so the table stays bounded by what admission can still
        // read.
        if let Some(key_id) = row.key_id {
            let tokens = row.usage.input_tokens + row.usage.output_tokens;
            if tokens > 0 {
                sqlx::query(
                    "INSERT INTO oxsum.tpm_windows (key_id, scope, window_start, tokens) \
                     VALUES ($1, '', date_trunc('second', now()), $2) \
                     ON CONFLICT (key_id, scope, window_start) DO UPDATE SET \
                         tokens = tpm_windows.tokens + EXCLUDED.tokens",
                )
                .bind(key_id)
                .bind(tokens)
                .execute(&mut *tx)
                .await?;
                // Tag-scoped buckets (issue #209): the turn accrues into each
                // bucket its tags select, in the same transaction, so a scoped
                // cap reads the same settle that fills it. The turn's tag values
                // name the buckets (`tag:<name>=<value>` for Each/Concrete,
                // `tag:<name>` for Total); a turn carrying no matching tag writes
                // only the plain row above. Scopes are read off the key row — a
                // `key_id` that names no key (the `usage_daily` rollup keys on an
                // unattributed label, not a credential) reads no scopes, so a
                // re-scoped PATCH shapes the next settle, not this one.
                let scopes: Option<serde_json::Value> = sqlx::query_scalar(
                    "SELECT tokens_per_minute_scopes FROM oxsum.api_keys WHERE key_id = $1",
                )
                .bind(key_id)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
                let scopes: Option<crate::keys::TpmScopes> =
                    scopes.and_then(|v| serde_json::from_value(v).ok());
                if let Some(scopes) = scopes {
                    for (tag, scope) in scopes {
                        let tag_scope = crate::keys::TagScope { tag, scope };
                        if let Some(name) = tag_scope.scope_name(&row.usage.tags) {
                            sqlx::query(
                                "INSERT INTO oxsum.tpm_windows \
                                     (key_id, scope, window_start, tokens) \
                                 VALUES ($1, $2, date_trunc('second', now()), $3) \
                                 ON CONFLICT (key_id, scope, window_start) DO UPDATE SET \
                                     tokens = tpm_windows.tokens + EXCLUDED.tokens",
                            )
                            .bind(key_id)
                            .bind(name)
                            .bind(tokens)
                            .execute(&mut *tx)
                            .await?;
                        }
                    }
                }
                sqlx::query(
                    "DELETE FROM oxsum.tpm_windows \
                     WHERE key_id = $1 \
                       AND window_start <= now() - interval '60 seconds'",
                )
                .bind(key_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        // A settled turn resolves the request's idempotency claim, whenever one was
        // made: a retry of a streamed turn cannot be handed the stream back, so it
        // is answered with this receipt (issue #132). Running here covers the
        // sweeper's settlements too, not only the turn's own write.
        let receipt = serde_json::json!({
            "object": "oxsum.receipt",
            "requestId": row.request_id,
            "kind": row.kind.as_str(),
            "chargedMinor": row.charged_minor,
            "freezeMinor": row.freeze_minor,
            "settlementEntryId": row.entry_id,
            "usage": serde_json::to_value(&row.usage).unwrap_or_default(),
        });
        sqlx::query(
            "UPDATE oxsum.idempotency_records \
             SET status = 'completed', response_status = 200, response_body = $2 \
             WHERE request_id = $1 AND status = 'in_flight'",
        )
        .bind(&row.request_id)
        .bind(&receipt)
        .execute(&mut *tx)
        .await?;
        // Webhook subscribers hear about the turn in the same transaction, so a
        // landed settlement and its notification can never diverge — and the
        // early return on a replayed row above keeps the queue free of
        // duplicates (issue #144).
        Self::enqueue_request_settled(
            &mut tx,
            &row.tenant_id,
            serde_json::json!({
                "requestId": row.request_id,
                "model": row.model,
                "channel": row.channel,
                "kind": row.kind.as_str(),
                "chargedMinor": row.charged_minor,
                "freezeMinor": row.freeze_minor,
                "settlementEntryId": row.entry_id,
                "inputTokens": row.usage.input_tokens,
                "outputTokens": row.usage.output_tokens,
            }),
        )
        .await?;
        // An anomalous close is its own event (issue #200): a `usage` row and a
        // zero-charge refusal carry no anomaly to report.
        Self::enqueue_anomaly(
            &mut tx,
            &row.tenant_id,
            row.kind,
            serde_json::json!({
                "requestId": row.request_id,
                "model": row.model,
                "channel": row.channel,
                "kind": row.kind.as_str(),
                "chargedMinor": row.charged_minor,
                "freezeMinor": row.freeze_minor,
                "settlementEntryId": row.entry_id,
                "upstreamAttempts": row.upstream_attempts,
            }),
        )
        .await?;
        // balance.low judges the post-settle balance the caller measured against
        // the endpoint's threshold, once per crossing (issue #200).
        if let (Some(available), Ok(organization)) = (available, Uuid::parse_str(&row.tenant_id)) {
            Self::enqueue_balance_low(&mut tx, organization, available).await?;
        }
        // The settling key's budget window is forecast off the same settled
        // figures this transaction just wrote (issue #220): a key carrying both
        // `spendLimitMinor` and `budgetDuration` is projected forward, and the
        // warning enqueues once per window when the projection crosses inside it.
        Self::forecast_key_budget(&mut tx, &row.tenant_id, row.key_id).await?;
        // Armed budget thresholds re-evaluate off those same settled figures
        // (issue #232) — an organization the settle belongs to and the ledger
        // it posts to are both required for the read.
        if let Ok(organization) = Uuid::parse_str(&row.tenant_id) {
            Self::budget_thresholds(&mut tx, organization, &row.tenant_id, row.key_id).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// `budget.exhaustion_forecast` (issue #220): project the settling key's
    /// current `budgetDuration` window forward at the rate its committed spend
    /// has run so far, and enqueue the event when the projection crosses the
    /// `spendLimitMinor` inside the window.
    ///
    /// Runs inside `record_usage`'s transaction, so the projection reads the
    /// settled figures this settle just wrote — the same `key_committed_in`
    /// scope the hold-side limit check reads (`pending` holds in full plus
    /// settled entries booked on or after the period start) — and the event, the
    /// latch and the usage row commit or roll back together. Once per key per
    /// window: `budget_forecast_sent_for` records which window the forecast was
    /// sent for, locked `FOR UPDATE` so two settles of the key cannot both pass
    /// the not-yet-sent check. A re-PATCH of the constraint clears the latch.
    ///
    /// A key with no periodic limit, an unattributed settle (`key_id` `None`), or
    /// a synthetic tenant with no ledger all skip quietly — there is no window
    /// to project, or no books to read it from.
    async fn forecast_key_budget(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        tenant_id: &str,
        key_id: Option<Uuid>,
    ) -> Result<(), WalletError> {
        let Some(key_id) = key_id else { return Ok(()) };
        // The key's constraint and latch. `FOR UPDATE` serializes the check the
        // same way `balance.low`'s latch does: two settles of this key cannot
        // both read `sent_for != window_start` and enqueue the event twice.
        let key = sqlx::query(
            "SELECT spend_limit_minor, budget_duration, budget_forecast_sent_for \
             FROM oxsum.api_keys WHERE key_id = $1 FOR UPDATE",
        )
        .bind(key_id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(key) = key else { return Ok(()) };
        let limit: Option<i64> = key.try_get("spend_limit_minor")?;
        let duration: Option<String> = key.try_get("budget_duration")?;
        let sent_for: Option<time::Date> = key.try_get("budget_forecast_sent_for")?;
        let (Some(limit), Some(duration)) = (limit, duration) else {
            return Ok(());
        };
        let duration = crate::keys::BudgetDuration::parse(&duration)?;
        let today = OffsetDateTime::now_utc().date();
        let window_start = duration.period_start(today);
        // Already forecast for this window — one event per key per period.
        if sent_for == Some(window_start) {
            return Ok(());
        }
        // The pool account indexes the key's committed spend is summed over —
        // resolved from the tenant ledger's account registry by path, the same
        // three pools the hold check sums. A tenant with no ledger yet (the
        // synthetic ids the suites seed) has no accounts table: nothing to read.
        let ledger = format!("ledger_{}", tenant_id.replace('"', "\"\""));
        let has_ledger = sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text")
            .bind(format!("\"{ledger}\".accounts"))
            .fetch_one(&mut **tx)
            .await?
            .is_some();
        if !has_ledger {
            return Ok(());
        }
        let pools: Vec<i32> = sqlx::query_scalar(&format!(
            "SELECT account_index FROM \"{ledger}\".accounts \
             WHERE path = ANY($1)",
        ))
        .bind(vec![
            crate::wallet::WALLET.to_owned(),
            crate::wallet::BONUS.to_owned(),
            crate::wallet::CREDIT_LINE.to_owned(),
        ])
        .fetch_all(&mut **tx)
        .await?;
        if pools.is_empty() {
            return Ok(());
        }
        // Committed spend this window: settled charges booked on or after the
        // period start, plus every outstanding hold — the figure the limit check
        // itself reads, so the forecast and the enforcement agree.
        let committed: i64 = sqlx::query_scalar(&format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                                  ELSE -p.amount_minor END), 0)::bigint \
             FROM \"{ledger}\".postings p \
             JOIN \"{ledger}\".entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_actor = $2 \
               AND (p.layer = 'pending' OR e.booking_date >= $3)",
        ))
        .bind(&pools)
        .bind(key_id.as_simple().to_string())
        .bind(window_start)
        .fetch_one(&mut **tx)
        .await?;
        let Some(mut forecast) =
            crate::webhooks::forecast_exhaustion(duration, window_start, today, committed, limit)
        else {
            return Ok(());
        };
        forecast.key_id = key_id;
        Self::enqueue_budget_forecast(tx, tenant_id, &forecast).await
    }

    /// `budget.threshold_crossed` (issue #232): every budget scope this settle
    /// burned against re-evaluates its consumption percentage, and each armed
    /// line — `budgetThresholdsPercent` on an enabled subscribed endpoint —
    /// fires at most once per window, latched by `budget_threshold_fires`
    /// inside this transaction.
    ///
    /// Three scopes where a numeric limit exists: the settling key's
    /// `spendLimitMinor` (windowed by its `budgetDuration`, cumulative without
    /// one), the minting member's `budgetLimitMinor` across every key they
    /// minted, and the organization's credit line. `committed` is the figure
    /// the hold-side limit checks read — settled spend plus every outstanding
    /// hold — and `consumedPercent` floors `committed * 100 / limit`, so the
    /// crossing line is the one the caller sees.
    ///
    /// The latch write and the delivery enqueue happen in `record_usage`'s
    /// transaction: commit lands both, rollback lands neither, a replayed
    /// usage row refires nothing, and two settles racing the same threshold
    /// cannot both pass — `INSERT … ON CONFLICT DO NOTHING` returns the newly
    /// latched rows only. A constraint PATCH deletes the scope's rows, so a
    /// re-set limit announces again.
    async fn budget_thresholds(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization: Uuid,
        tenant_id: &str,
        key_id: Option<Uuid>,
    ) -> Result<(), WalletError> {
        // Nobody armed means no read at all — the usual case costs one EXISTS.
        let armed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.webhook_endpoints \
             WHERE organization_id = $1 AND enabled \
               AND 'budget.threshold_crossed' = ANY(events) \
               AND cardinality(budget_thresholds) > 0)",
        )
        .bind(organization)
        .fetch_one(&mut **tx)
        .await?;
        if !armed {
            return Ok(());
        }
        let ledger = format!("ledger_{}", tenant_id.replace('"', "\"\""));
        let has_ledger = sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text")
            .bind(format!("\"{ledger}\".accounts"))
            .fetch_one(&mut **tx)
            .await?
            .is_some();
        if !has_ledger {
            return Ok(());
        }
        let accounts: Vec<(i32, String)> = sqlx::query_as(&format!(
            "SELECT account_index, path FROM \"{ledger}\".accounts \
             WHERE path = ANY($1)"
        ))
        .bind(vec![
            crate::wallet::WALLET.to_owned(),
            crate::wallet::BONUS.to_owned(),
            crate::wallet::CREDIT_LINE.to_owned(),
            crate::wallet::FACILITY.to_owned(),
        ])
        .fetch_all(&mut **tx)
        .await?;
        let pools: Vec<i32> = accounts
            .iter()
            .filter(|(_, path)| path != crate::wallet::FACILITY)
            .map(|(index, _)| *index)
            .collect();
        if pools.is_empty() {
            return Ok(());
        }
        let facility = accounts
            .iter()
            .find(|(_, path)| path == crate::wallet::FACILITY)
            .map(|(index, _)| *index);
        let credit_pool = accounts
            .iter()
            .find(|(_, path)| path == crate::wallet::CREDIT_LINE)
            .map(|(index, _)| *index);

        // `(scope, scope id, window start, limit, committed)` for every scope a
        // settle can burn against. `window_start` latches on the epoch for the
        // cumulative scopes — one row per endpoint+threshold until the limit
        // itself is re-set.
        let epoch = time::macros::date!(1970 - 01 - 01);
        let today = OffsetDateTime::now_utc().date();
        let mut scopes: Vec<(&'static str, Uuid, time::Date, i64, i64)> = Vec::new();
        if let Some(key_id) = key_id {
            let key = sqlx::query(
                "SELECT spend_limit_minor, budget_duration, created_by \
                 FROM oxsum.api_keys WHERE key_id = $1",
            )
            .bind(key_id)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some(key) = key {
                let limit: Option<i64> = key.try_get("spend_limit_minor")?;
                let duration: Option<String> = key.try_get("budget_duration")?;
                if let Some(limit) = limit {
                    let window_start = duration
                        .as_deref()
                        .map(crate::keys::BudgetDuration::parse)
                        .transpose()?
                        .map(|duration| duration.period_start(today));
                    let committed = Self::committed_for(
                        tx,
                        &ledger,
                        &pools,
                        &[key_id.as_simple().to_string()],
                        window_start,
                    )
                    .await?;
                    scopes.push((
                        "key",
                        key_id,
                        window_start.unwrap_or(epoch),
                        limit,
                        committed,
                    ));
                }
                // The member's own line: the budget a membership carries across
                // every key its owner minted — cumulative, no window.
                if let Some(member) = key.try_get::<Option<Uuid>, _>("created_by")? {
                    let limit: Option<i64> = sqlx::query_scalar(
                        "SELECT budget_limit_minor FROM oxsum.memberships \
                         WHERE organization_id = $1 AND user_id = $2",
                    )
                    .bind(organization)
                    .bind(member)
                    .fetch_one(&mut **tx)
                    .await?;
                    if let Some(limit) = limit {
                        let actors: Vec<String> = sqlx::query_scalar(
                            "SELECT replace(key_id::text, '-', '') FROM oxsum.api_keys \
                             WHERE organization_id = $1 AND created_by = $2",
                        )
                        .bind(organization)
                        .bind(member)
                        .fetch_all(&mut **tx)
                        .await?;
                        let committed =
                            Self::committed_for(tx, &ledger, &pools, &actors, None).await?;
                        scopes.push(("member", member, epoch, limit, committed));
                    }
                }
            }
        }
        // The organization's line is the credit facility against what is drawn —
        // settled draws plus the reservations in flight, like `credit_used`.
        if let (Some(facility), Some(credit_pool)) = (facility, credit_pool) {
            let limit: i64 = sqlx::query_scalar(&format!(
                "SELECT -COALESCE(SUM(CASE WHEN p.direction = 'C' \
                         THEN p.amount_minor ELSE -p.amount_minor END), 0)::bigint \
                 FROM \"{ledger}\".postings p \
                 JOIN \"{ledger}\".entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = $1 AND p.layer = 'settled' AND e.log_index IS NOT NULL"
            ))
            .bind(facility)
            .fetch_one(&mut **tx)
            .await?;
            let headroom: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(SUM(CASE \
                         WHEN p.layer = 'settled' AND p.direction = 'C' THEN p.amount_minor \
                         WHEN p.layer = 'settled' AND p.direction = 'D' THEN -p.amount_minor \
                         WHEN p.layer = 'pending' AND p.direction = 'D' THEN -p.amount_minor \
                         WHEN p.layer = 'pending' AND p.direction = 'C' THEN p.amount_minor \
                         ELSE 0 END), 0)::bigint \
                 FROM \"{ledger}\".postings p \
                 JOIN \"{ledger}\".entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = $1 AND e.log_index IS NOT NULL"
            ))
            .bind(credit_pool)
            .fetch_one(&mut **tx)
            .await?;
            if limit > 0 {
                scopes.push((
                    "organization",
                    organization,
                    epoch,
                    limit,
                    (limit - headroom).max(0),
                ));
            }
        }

        for (scope, scope_id, window_start, limit, committed) in scopes {
            if limit <= 0 {
                continue;
            }
            let consumed_percent = (i128::from(committed.max(0)) * 100 / i128::from(limit)) as i32;
            // The crossing writes the latch row; `RETURNING` gives back only the
            // thresholds latched by this settle — a refire is impossible even
            // under two settles racing the same line.
            let fired = sqlx::query(
                "INSERT INTO oxsum.budget_threshold_fires \
                 (endpoint_id, scope, scope_id, window_start, threshold_percent) \
                 SELECT e.endpoint_id, $2, $3, $4, t \
                 FROM oxsum.webhook_endpoints e \
                 CROSS JOIN LATERAL unnest(e.budget_thresholds) AS t \
                 WHERE e.organization_id = $1 AND e.enabled \
                   AND 'budget.threshold_crossed' = ANY(e.events) AND t <= $5 \
                 ON CONFLICT DO NOTHING \
                 RETURNING endpoint_id, threshold_percent",
            )
            .bind(organization)
            .bind(scope)
            .bind(scope_id)
            .bind(window_start)
            .bind(consumed_percent)
            .fetch_all(&mut **tx)
            .await?;
            let scope_key = match scope {
                "key" => "keyId",
                "member" => "memberId",
                _ => "organizationId",
            };
            for fire in fired {
                let endpoint_id: Uuid = fire.try_get("endpoint_id")?;
                let threshold: i16 = fire.try_get("threshold_percent")?;
                let delivery_id = Uuid::new_v4();
                let payload = crate::webhooks::event_envelope(
                    delivery_id,
                    crate::webhooks::BUDGET_THRESHOLD_CROSSED,
                    organization,
                    serde_json::json!({
                        "scope": scope,
                        scope_key: scope_id,
                        "limitMinor": limit,
                        "committedMinor": committed,
                        "consumedPercent": consumed_percent,
                        "thresholdPercent": threshold,
                        "windowStart": (window_start != epoch).then(|| window_start.to_string()),
                    }),
                );
                sqlx::query(
                    "INSERT INTO oxsum.webhook_deliveries \
                     (delivery_id, endpoint_id, organization_id, event_type, payload) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(delivery_id)
                .bind(endpoint_id)
                .bind(organization)
                .bind(crate::webhooks::BUDGET_THRESHOLD_CROSSED)
                .bind(payload)
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// Settled spend plus outstanding holds attributed to `actors` — the
    /// committed figure the budget scopes share, windowed to entries booked on
    /// or after `since` when the scope's `budgetDuration` gives one (pending
    /// always counts in full, like `key_committed_in`).
    async fn committed_for(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        ledger: &str,
        pools: &[i32],
        actors: &[String],
        since: Option<time::Date>,
    ) -> Result<i64, WalletError> {
        Ok(sqlx::query_scalar(&format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                                  ELSE -p.amount_minor END), 0)::bigint \
             FROM \"{ledger}\".postings p \
             JOIN \"{ledger}\".entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_actor = ANY($2) \
               AND (p.layer = 'pending' OR $3::date IS NULL OR e.booking_date >= $3)"
        ))
        .bind(pools)
        .bind(actors)
        .bind(since)
        .fetch_one(&mut **tx)
        .await?)
    }

    /// How many tokens a key has settled inside the trailing minute, and when
    /// the window next frees — the two figures a `tokensPerMinute` admission
    /// check needs (issue #203).
    ///
    /// Returns `(settled_tokens, retry_after_secs)`: the sum over
    /// `oxsum.tpm_windows` rows inside the window, and the seconds until the
    /// oldest in-window row ages out — `0` on an empty window, so an admitted
    /// request reads a harmless retry_after. Durable by design: RPM counts
    /// requests spent at admission and lives in the in-process limiter, while
    /// TPM counts tokens actually settled, so the read is a Postgres sum.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn tpm_window(&self, key_id: Uuid) -> Result<(i64, u64), WalletError> {
        let row = sqlx::query(
            "SELECT COALESCE(sum(tokens), 0)::bigint AS settled, \
                    min(window_start) AS oldest \
             FROM oxsum.tpm_windows \
             WHERE key_id = $1 AND scope = '' \
               AND window_start > now() - interval '60 seconds'",
        )
        .bind(key_id)
        .fetch_one(self.pool())
        .await?;
        let settled: i64 = row.try_get("settled")?;
        // The row frees when it is 60s old. `whole_seconds` truncates, so the
        // difference is already the ceiling; `.max(0)` guards clock skew, and
        // the header site applies its own `.max(1)` floor.
        let retry_after = match row.try_get::<Option<OffsetDateTime>, _>("oldest")? {
            Some(oldest) => {
                (60 - (OffsetDateTime::now_utc() - oldest).whole_seconds()).max(0) as u64
            }
            None => 0,
        };
        Ok((settled, retry_after))
    }

    /// [`tpm_window`](Self::tpm_window) for one named bucket: the trailing-minute
    /// sum and retry-after anchor over `oxsum.tpm_windows` rows whose `scope`
    /// matches. A `TagScope::scope_name` result is the caller's `scope` — the
    /// tag-scoped cap a turn checks is read from the same bucket the turn just
    /// accrued into (issue #209). The plain window is `tpm_window` (`scope = ''`).
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn tpm_window_scoped(
        &self,
        key_id: Uuid,
        scope: &str,
    ) -> Result<(i64, u64), WalletError> {
        let row = sqlx::query(
            "SELECT COALESCE(sum(tokens), 0)::bigint AS settled, \
                    min(window_start) AS oldest \
             FROM oxsum.tpm_windows \
             WHERE key_id = $1 AND scope = $2 \
               AND window_start > now() - interval '60 seconds'",
        )
        .bind(key_id)
        .bind(scope)
        .fetch_one(self.pool())
        .await?;
        let settled: i64 = row.try_get("settled")?;
        let retry_after = match row.try_get::<Option<OffsetDateTime>, _>("oldest")? {
            Some(oldest) => {
                (60 - (OffsetDateTime::now_utc() - oldest).whole_seconds()).max(0) as u64
            }
            None => 0,
        };
        Ok((settled, retry_after))
    }

    /// One organization's daily usage rollup, `[from, to]` inclusive, as the
    /// usage dashboard reads it (roadmap P3-3, issue #126).
    ///
    /// Rows come back one per `(day, channel, model, key)` — `key_id` stays on
    /// the row so the caller applies the members' scope rule losslessly: a
    /// member reads their own keys' days plus the unattributed (`None`) shared
    /// usage, the same rule the bills page applies to the ledger.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn usage_daily(
        &self,
        tenant_id: &str,
        from: time::Date,
        to: time::Date,
    ) -> Result<Vec<UsageDay>, WalletError> {
        let rows = sqlx::query(
            "SELECT day, key_id, channel, model, turns, \
                    input_tokens, output_tokens, cached_tokens, reasoning_tokens, \
                    charged_minor \
             FROM oxsum.usage_daily \
             WHERE tenant_id = $1 AND day >= $2 AND day <= $3 \
             ORDER BY day, channel, model, key_id",
        )
        .bind(tenant_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await?;
        let mut days = Vec::with_capacity(rows.len());
        for row in &rows {
            days.push(UsageDay {
                day: row.try_get("day")?,
                key_id: row.try_get("key_id")?,
                channel: row.try_get("channel")?,
                model: row.try_get("model")?,
                turns: row.try_get("turns")?,
                input_tokens: row.try_get("input_tokens")?,
                output_tokens: row.try_get("output_tokens")?,
                cached_tokens: row.try_get("cached_tokens")?,
                reasoning_tokens: row.try_get("reasoning_tokens")?,
                charged_minor: row.try_get("charged_minor")?,
            });
        }
        Ok(days)
    }

    /// The charged-versus-upstream sums per `(channel, model)` the admin margin
    /// view answers (roadmap P1-6, issue #112).
    ///
    /// `untrackedTurns` is the first number a coverage gap shows up in — a price
    /// without an `upstream` block, swept turns, history — and where upstream
    /// misbilling would hide if a channel were priced but not tracked.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn margin(&self) -> Result<Vec<Margin>, WalletError> {
        let rows = sqlx::query(
            "SELECT channel, model, count(*) AS turns, \
                    COALESCE(sum(charged_minor), 0)::bigint AS charged_minor, \
                    COALESCE(sum(upstream_cost_minor), 0)::bigint AS upstream_cost_minor, \
                    count(*) FILTER (WHERE upstream_cost_minor IS NULL) AS untracked_turns \
             FROM oxsum.usage_records \
             GROUP BY channel, model \
             ORDER BY channel, model",
        )
        .fetch_all(self.pool())
        .await?;
        let mut margin = Vec::with_capacity(rows.len());
        for row in &rows {
            margin.push(Margin {
                channel: row.try_get("channel")?,
                model: row.try_get("model")?,
                turns: row.try_get("turns")?,
                charged_minor: row.try_get("charged_minor")?,
                upstream_cost_minor: row.try_get("upstream_cost_minor")?,
                untracked_turns: row.try_get("untracked_turns")?,
            });
        }
        Ok(margin)
    }
}

/// One daily rollup row of `oxsum.usage_daily` — what the usage dashboard sums
/// into its chart and table.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDay {
    /// The settlement entries' `booking_date` these turns were booked under.
    pub day: time::Date,
    /// The key that paid for these turns; `None` is the shared unattributed
    /// usage (session settles) every member may see.
    pub key_id: Option<Uuid>,
    pub channel: String,
    pub model: String,
    /// The settled turns the row sums.
    pub turns: i64,
    /// Token sums over the turns, the normalized record's same four counts.
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub reasoning_tokens: i64,
    /// What the turns charged, in minor units.
    pub charged_minor: i64,
}

/// What one `(channel, model)` pair charged against what upstream cost it, as the
/// margin view answers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Margin {
    pub channel: String,
    pub model: String,
    /// The settled turns the row sums.
    pub turns: i64,
    /// What the organizations were charged, in minor units.
    pub charged_minor: i64,
    /// What the channel's `upstream` prices made of the same usage, over the
    /// turns that track it. `chargedMinor` minus this is the margin, computed
    /// at the edge rather than stored.
    pub upstream_cost_minor: i64,
    /// Settled turns with no upstream cost recorded — an untracked price, a
    /// swept hold, or history.
    pub untracked_turns: i64,
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

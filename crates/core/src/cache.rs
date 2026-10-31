//! The semantic response cache (issue #497): an upstream answer an
//! organization may be re-served for a duplicate request, charged at the
//! cache-hit price rather than a fresh turn's.
//!
//! A cache row is keyed by `(organization, channel, model, prompt_digest)` —
//! the digest is `oxsum/prompt/v1` over the surface's normalized prompt and the
//! model, so "the same question" is a stable, exact thing rather than a
//! similarity score. A lookup is org-scoped first and always: one
//! organization's cached answer is never served to another, which is the
//! privacy rule the whole feature stands on. Within an org the cache is shared
//! across its keys — the opposite of bifrost's `disable_identity_scoping`
//! default, because an organization's own turns paid for the answer.
//!
//! A hit settles like a turn that reported upstream usage: the cached answer's
//! `usage_json` prices through the same `itemize` path, so a customer pays the
//! cache-read rate — or the input rate where the price book names no
//! `cacheReadPricePerMillion` — never a zero charge for a served answer and
//! never a fresh-rate charge for a reused one. `source_entry_id` names the
//! original ledger entry, which is what the settlement's `cacheHit` block
//! proves reuse against: "this answer was the one the upstream produced for
//! entry X at time T" is a statement the verifier can check.

use doubleentry::Hash;
use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::usage::UsageRecord;

/// The cache-policy key a channel's `settings` column carries — a
/// `semanticCache` object naming whether the channel caches at all and how a
/// hit is scored. Absent or `{enabled: false}` is the same answer: never
/// cached.
pub const SETTINGS_KEY: &str = "semanticCache";

/// The digest domain: `oxsum/prompt/v1` over the normalized prompt joined with
/// the model, so an identical ask under a different model is a different row
/// — a cached GPT-4 answer must never serve a Claude request.
const PROMPT_DOMAIN: &[u8] = b"oxsum/prompt/v1";

/// The policy a channel's `settings.semanticCache` resolves into.
///
/// `enabled` is the whole gate: a channel that does not opt in is never read
/// from and never written to, so the feature costs nothing on channels that
/// did not ask for it.
#[derive(Debug, Clone, Default)]
pub struct CachePolicy {
    /// Whether the channel reads and writes the cache at all.
    pub enabled: bool,
    /// The rows' lifetime in seconds; `None` is no expiry — a forever-reusable
    /// answer is a deliberate deployment choice, not a default.
    pub ttl_seconds: Option<i64>,
    /// Drop system/instructions text from the digest: two requests that differ
    /// only in their system preamble answer the same. Defaults to false —
    /// the system text is part of the question, and only a channel that
    /// deliberately ignores it should be allowed to.
    pub exclude_system: bool,
}

impl CachePolicy {
    /// Reads `settings.semanticCache` off a channel row. An absent or
    /// non-object setting is the disabled policy — a channel that never
    /// opted in caches nothing. A malformed one is refused loudly rather than
    /// silently treated as either on or off, because "did we serve a cached
    /// answer" is a billing question.
    ///
    /// # Errors
    ///
    /// [`WalletError::Misconfigured`] when `semanticCache` is present but is
    /// not an object with the understood keys.
    pub fn of(settings: Option<&Value>) -> Result<Self, WalletError> {
        let Some(raw) = settings.and_then(|s| s.get(SETTINGS_KEY)) else {
            return Ok(Self::default());
        };
        let map = raw.as_object().ok_or_else(|| {
            WalletError::Misconfigured(format!("{SETTINGS_KEY} must be an object"))
        })?;
        let enabled = map.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        let ttl_seconds = match map.get("ttlSeconds") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let seconds = value.as_i64().ok_or_else(|| {
                    WalletError::Misconfigured(format!(
                        "{SETTINGS_KEY}.ttlSeconds must be an integer"
                    ))
                })?;
                if seconds <= 0 {
                    return Err(WalletError::Misconfigured(format!(
                        "{SETTINGS_KEY}.ttlSeconds must be positive"
                    )));
                }
                Some(seconds)
            }
        };
        let exclude_system = map
            .get("excludeSystem")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(Self {
            enabled,
            ttl_seconds,
            exclude_system,
        })
    }
}

/// One normalized prompt segment: the surface's role label and the text it
/// carried, lowered and whitespace-folded. The role rides into the digest
/// with the text so a `user` turn and an `assistant` turn carrying the same
/// words do not collide.
#[derive(Debug, Clone)]
pub struct PromptPart {
    pub role: &'static str,
    pub text: String,
}

impl PromptPart {
    /// The normalized text: lowercase plus folded whitespace — the two
    /// variations a paraphrase never hides behind, and the most a digest can
    /// tolerate before "same question" stops meaning anything a bill can
    /// defend.
    fn normalized(text: &str) -> String {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }
}

/// The digest a `(channel, model, prompt)` key is built on: which surface the
/// ask came in on (the shape decides which fields count as prompt), the
/// public model name, and the normalized parts — `exclude_system` drops the
/// `system`/`instructions` share before hashing.
///
/// `None` is the answer a cache must not store: a request with no text (an
/// embeddings call on token ids, a media-only ask) has no stable "same
/// question" to key on, and hashing an empty shape would collide unrelated
/// asks into one row.
#[must_use]
pub fn prompt_digest(
    surface: &str,
    model: &str,
    parts: &[PromptPart],
    exclude_system: bool,
) -> Option<String> {
    let mut canonical =
        String::with_capacity(parts.iter().map(|p| p.text.len() + 8).sum::<usize>());
    canonical.push_str(surface);
    canonical.push('\n');
    canonical.push_str(model);
    canonical.push('\n');
    let mut any = false;
    for part in parts {
        if exclude_system && part.role == "system" {
            continue;
        }
        let text = PromptPart::normalized(&part.text);
        if text.is_empty() {
            continue;
        }
        canonical.push_str(part.role);
        canonical.push(':');
        canonical.push_str(&text);
        canonical.push('\n');
        any = true;
    }
    if !any {
        return None;
    }
    Some(Hash::digest(PROMPT_DOMAIN, canonical.as_bytes()).to_hex())
}

/// A cached answer ready to serve.
#[derive(Debug, Clone)]
pub struct CachedAnswer {
    /// The stored response — what the hit answers, verbatim.
    pub body: Value,
    /// The original turn's usage, so the settle prices what upstream counted.
    pub usage: UsageRecord,
    /// The ledger entry the answer came from — the proof's link on the
    /// `cacheHit` bill.
    pub source_entry_id: Uuid,
    /// When the row was written, for the expiry check a caller does not
    /// re-implement.
    pub created_at: OffsetDateTime,
    /// The row's TTL at write time; `None` means it does not expire.
    pub ttl_seconds: Option<i64>,
    /// The row's own id: the hit counter increments on it.
    pub cache_id: Uuid,
}

/// The answer to store: the response body plus the original turn's usage and
/// the ledger entry it settled under — everything a hit needs to bill and to
/// prove reuse. Owned throughout: the store write typically rides a
/// `tokio::spawn` after the turn has settled, so borrowed fields would pin
/// the caller's stack to a task that outlives it.
#[derive(Debug)]
pub struct CacheStore {
    /// The channel that answered; the hit settles under its current version.
    pub channel_id: Uuid,
    /// The public model name the request asked for.
    pub model: String,
    /// `oxsum/prompt/v1` of the normalized prompt — must already be computed,
    /// so a miss that decided not to store never re-derives it.
    pub prompt_digest: String,
    /// The upstream response verbatim.
    pub body: Value,
    /// The turn's usage as reported, so the hit prices the real count.
    pub usage: UsageRecord,
    /// The settlement entry the original turn wrote — the link a `cacheHit`
    /// bill names.
    pub source_entry_id: Uuid,
    /// The row's lifetime in seconds; `None` is no expiry.
    pub ttl_seconds: Option<i64>,
}

impl Db {
    /// The channel's `settings` column, for `CachePolicy::of`. A missing
    /// channel answers `None`.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn channel_settings(&self, channel_id: Uuid) -> Result<Option<Value>, WalletError> {
        sqlx::query_scalar("SELECT settings FROM oxsum.channels WHERE channel_id = $1")
            .bind(channel_id)
            .fetch_optional(self.pool())
            .await
            .map_err(Into::into)
    }

    /// Looks the digest up under `(organization, channel, model)`. An expired
    /// row is deleted on the way out — a stale row serving a hit is worse
    /// than a miss — and the answer reads the live row only.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn cache_lookup(
        &self,
        organization_id: Uuid,
        channel_id: Uuid,
        model: &str,
        prompt_digest: &str,
    ) -> Result<Option<CachedAnswer>, WalletError> {
        let row = sqlx::query(
            "SELECT cache_id, response_body, usage_json, source_entry_id, created_at, ttl_seconds \
             FROM oxsum.cached_responses \
             WHERE organization_id = $1 AND channel_id = $2 AND model = $3 AND prompt_digest = $4",
        )
        .bind(organization_id)
        .bind(channel_id)
        .bind(model)
        .bind(prompt_digest)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let created_at: OffsetDateTime = row.try_get("created_at")?;
        let ttl_seconds: Option<i64> = row.try_get("ttl_seconds")?;
        // The TTL is judged once, here, so every caller sees the same answer:
        // an expired row is deleted before it can serve — a race where a
        // miss in one process and a hit in another is exactly what the delete
        // guards against.
        if let Some(ttl) = ttl_seconds
            && created_at + time::Duration::seconds(ttl) < OffsetDateTime::now_utc()
        {
            sqlx::query("DELETE FROM oxsum.cached_responses WHERE cache_id = $1")
                .bind(row.try_get::<Uuid, _>("cache_id")?)
                .execute(self.pool())
                .await?;
            return Ok(None);
        }
        let usage_json: Value = row.try_get("usage_json")?;
        let usage: UsageRecord = serde_json::from_value(usage_json).map_err(|error| {
            WalletError::Misconfigured(format!("stored cache usage does not parse: {error}"))
        })?;
        Ok(Some(CachedAnswer {
            body: row.try_get("response_body")?,
            usage,
            source_entry_id: row.try_get("source_entry_id")?,
            created_at,
            ttl_seconds,
            cache_id: row.try_get("cache_id")?,
        }))
    }

    /// Stores an answer for later hits. `ON CONFLICT DO NOTHING` keeps the
    /// first write: two concurrent misses both finishing upstream produces
    /// one row, and the loser's answer is just as good — a hit later cannot
    /// tell them apart.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn cache_store(
        &self,
        organization_id: Uuid,
        store: &CacheStore,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.cached_responses \
             (cache_id, organization_id, channel_id, model, prompt_digest, \
              response_body, usage_json, source_entry_id, ttl_seconds) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (organization_id, channel_id, model, prompt_digest) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(organization_id)
        .bind(store.channel_id)
        .bind(&store.model)
        .bind(&store.prompt_digest)
        .bind(&store.body)
        .bind(
            serde_json::to_value(&store.usage)
                .map_err(|error| WalletError::InvalidInput(format!("cache usage: {error}")))?,
        )
        .bind(store.source_entry_id)
        .bind(store.ttl_seconds)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Counts a served hit: best-effort, a missed increment is a wrong admin
    /// counter, not a wrong bill.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn cache_hit(&self, cache_id: Uuid) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.cached_responses SET hit_count = hit_count + 1 WHERE cache_id = $1",
        )
        .bind(cache_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;

    fn parts(pairs: &[(&'static str, &str)]) -> Vec<PromptPart> {
        pairs
            .iter()
            .map(|(role, text)| PromptPart {
                role,
                text: (*text).to_owned(),
            })
            .collect()
    }

    #[test]
    fn the_digest_is_stable_under_case_and_whitespace() {
        let a = prompt_digest(
            "openai",
            "gpt-4",
            &parts(&[("user", "  What  is\n the   ANSWER? ")]),
            false,
        );
        let b = prompt_digest(
            "openai",
            "gpt-4",
            &parts(&[("user", "what is the answer?")]),
            false,
        );
        assert_eq!(a, b, "case and whitespace fold into one digest");
    }

    #[test]
    fn a_different_model_is_a_different_row() {
        let parts = parts(&[("user", "same words")]);
        let gpt = prompt_digest("openai", "gpt-4", &parts, false);
        let claude = prompt_digest("openai", "claude", &parts, false);
        assert_ne!(gpt, claude, "the model is part of the key");
    }

    #[test]
    fn the_role_is_part_of_the_key() {
        let user = prompt_digest("openai", "m", &parts(&[("user", "answer")]), false);
        let assistant = prompt_digest("openai", "m", &parts(&[("assistant", "answer")]), false);
        assert_ne!(user, assistant, "the same words under another role differ");
    }

    #[test]
    fn exclude_system_drops_only_the_preamble() {
        let with = parts(&[("system", "be terse"), ("user", "answer")]);
        let without = parts(&[("user", "answer")]);
        let a = prompt_digest("openai", "m", &with, true);
        let b = prompt_digest("openai", "m", &without, true);
        assert_eq!(a, b, "the system part folds out when asked");
        let c = prompt_digest("openai", "m", &with, false);
        assert_ne!(a, c, "kept by default");
    }

    #[test]
    fn an_empty_prompt_is_never_cached() {
        assert!(prompt_digest("openai", "m", &[], false).is_none());
        assert!(prompt_digest("openai", "m", &parts(&[("user", "   ")]), false).is_none());
        // A media-only ask has no text part either.
        assert!(prompt_digest("gemini", "m", &parts(&[("user", "")]), false,).is_none());
    }

    #[test]
    fn the_policy_reads_the_settings_object() {
        let policy = CachePolicy::of(Some(&json!({
            "semanticCache": {"enabled": true, "ttlSeconds": 300, "excludeSystem": true}
        })))
        .unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.ttl_seconds, Some(300));
        assert!(policy.exclude_system);

        // Absent or disabled is the same answer.
        assert!(!CachePolicy::of(None).unwrap().enabled);
        assert!(
            !CachePolicy::of(Some(&json!({"semanticCache": {"enabled": false}})))
                .unwrap()
                .enabled
        );
        // A bad shape refuses rather than guessing.
        assert!(CachePolicy::of(Some(&json!({"semanticCache": "yes"}))).is_err());
        assert!(CachePolicy::of(Some(&json!({"semanticCache": {"ttlSeconds": -1}}))).is_err());
    }
}

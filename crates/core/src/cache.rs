//! The semantic response cache (issue #497): an upstream answer an
//! organization may be re-served for a duplicate request, charged at the
//! cache-hit price rather than a fresh turn's.
//!
//! A cache row is keyed by `(organization, channel, model, prompt_digest)`.
//! The historical column name now stores a v2 HMAC of the complete canonical
//! request and admitted serving context. Exact text, tools, media, headers,
//! permissions and route configuration participate. No prompt is persisted by
//! this digest; old prompt-only and credential-wrapped keys cannot hit.
//!
//! A hit settles like a turn that reported upstream usage: the cached answer's
//! `usage_json` prices through the same `itemize` path, so a customer pays the
//! cache-read rate — or the input rate where the price book names no
//! `cacheReadPricePerMillion` — never a zero charge for a served answer and
//! never a fresh-rate charge for a reused one. `source_entry_id` names the
//! original ledger entry, which is what the settlement's `cacheHit` block
//! proves reuse against: "this answer was the one the upstream produced for
//! entry X at time T" is a statement the verifier can check.

use serde::Serialize;
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
        Ok(Self {
            enabled,
            ttl_seconds,
        })
    }
}

/// Complete admitted context for one response-cache lookup. The body retains
/// exact strings, nulls, arrays and unknown provider parameters. Object order
/// alone is canonicalized. Do not add Debug: this value holds private input.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseCacheContext<'a> {
    pub access: &'a crate::ServingAccess,
    pub surface: &'a str,
    pub requested_model: &'a str,
    pub requested_upstream: Option<&'a str>,
    pub served_model: &'a str,
    pub upstream_model: Option<&'a str>,
    pub request: &'a Value,
    pub forwarded: &'a Value,
    pub semantic_headers: &'a Value,
    pub key_constraints: &'a Value,
    pub experiment: &'a Value,
    pub routes: &'a Value,
    pub discount_percent: Option<i64>,
}

/// Canonical object keys; preserve every array element and string byte.
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: std::collections::BTreeMap<_, _> = map.into_iter().collect();
            Value::Object(sorted.into_iter().map(|(k, v)| (k, canonical(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        value => value,
    }
}

/// Fingerprint the complete response context, without storing request content.
/// A deployment sealing-key rotation intentionally invalidates old cache rows.
pub fn response_cache_digest(
    secret: &crate::SecretKey,
    context: &ResponseCacheContext<'_>,
) -> Result<String, WalletError> {
    let value = serde_json::to_value(context)
        .map_err(|_| WalletError::Misconfigured("invalid response cache context".into()))?;
    let bytes = serde_json::to_vec(&canonical(value))
        .map_err(|_| WalletError::Misconfigured("invalid response cache context".into()))?;
    secret.cache_fingerprint(&bytes)
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
    /// Versioned digest of complete admitted context, computed before the call.
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

    #[test]
    fn canonical_objects_preserve_exact_strings_arrays_and_nulls() {
        let first: Value = serde_json::from_str(r#"{"z":["A  B",null],"a":{"b":1,"a":2}}"#).unwrap();
        let reordered: Value = serde_json::from_str(r#"{"a":{"a":2,"b":1},"z":["A  B",null]}"#).unwrap();
        let secret = crate::SecretKey::from_bytes([7; 32]);
        let digest = |value| secret.cache_fingerprint(&serde_json::to_vec(&canonical(value)).unwrap()).unwrap();
        assert_eq!(digest(first.clone()), digest(reordered));
        for different in [
            json!({"z":["a  b",null],"a":{"b":1,"a":2}}),
            json!({"z":["A B",null],"a":{"b":1,"a":2}}),
            json!({"z":[null,"A  B"],"a":{"b":1,"a":2}}),
            json!({"z":["A  B"],"a":{"b":1,"a":2}}),
        ] {
            assert_ne!(digest(first.clone()), digest(different));
        }
        assert_ne!(
            digest(first.clone()),
            crate::SecretKey::from_bytes([8;32]).cache_fingerprint(&serde_json::to_vec(&canonical(first)).unwrap()).unwrap()
        );
    }

    #[test]
    fn the_policy_reads_the_settings_object() {
        let policy = CachePolicy::of(Some(&json!({
            "semanticCache": {"enabled": true, "ttlSeconds": 300, "excludeSystem": true}
        })))
        .unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.ttl_seconds, Some(300));
        // Legacy excludeSystem is accepted, but has no effect on the digest.

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

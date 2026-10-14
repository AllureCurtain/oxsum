//! API keys: the credential that names the organization.
//!
//! A key belongs to an organization and holds no money of its own. The plaintext exists
//! once, in the response that created it; the database keeps its SHA-256 hash and a display
//! prefix, so a dump of `oxsum.api_keys` authenticates nothing. See docs/product.md,
//! "API keys".

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::orgs::{self, Organization};
use crate::sessions::KeyScope;

/// Every secret starts with this, so secret scanners and users can recognize one.
const SECRET_MARK: &str = "oxs-";
/// 32 random bytes, per docs/product.md.
const SECRET_BYTES: usize = 32;
/// How much of the secret is kept for display: the mark plus 8 hex characters.
const PREFIX_CHARS: usize = 4 + 8;
/// The longest a key name may be.
pub(crate) const MAX_NAME: usize = 80;
/// The longest one entry of a model allowlist may be.
pub(crate) const MAX_MODEL: usize = 128;
/// The columns every `ApiKey` read projects, in one place for select and returning.
const KEY_COLS: &str = "key_id, name, prefix, created_by, created_at, expires_at, \
                        revoked_at, spend_limit_minor, budget_duration, model_allowlist, \
                        requests_per_minute, max_concurrent_holds, tokens_per_minute, \
                        tokens_per_minute_scopes";

/// The rule every model allowlist follows — a key's own, or a tier's: null or a
/// non-empty list of non-empty model names.
pub(crate) fn validate_model_allowlist(models: &[String]) -> Result<(), WalletError> {
    if models.is_empty() {
        return Err(WalletError::InvalidInput(
            "modelAllowlist must be null or name at least one model".into(),
        ));
    }
    if models
        .iter()
        .any(|m| m.trim().is_empty() || m.len() > MAX_MODEL)
    {
        return Err(WalletError::InvalidInput(format!(
            "a modelAllowlist entry is a model name, 1..={MAX_MODEL} bytes"
        )));
    }
    Ok(())
}

/// The window a periodic spend limit applies to. Without one the limit is cumulative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetDuration {
    Daily,
    Weekly,
    Monthly,
}

impl BudgetDuration {
    /// Parses the contract's lowercase name for the duration.
    pub fn parse(name: &str) -> Result<Self, WalletError> {
        match name {
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            other => Err(WalletError::InvalidInput(format!(
                "budgetDuration must be daily, weekly or monthly, not {other:?}"
            ))),
        }
    }

    /// The contract's lowercase name, for storage.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }

    /// The first day of the UTC calendar period `on` falls in: `daily` is `on` itself,
    /// `weekly` is the Monday of its ISO week, `monthly` is the first of its month.
    pub fn period_start(self, on: time::Date) -> time::Date {
        match self {
            Self::Daily => on,
            Self::Weekly => {
                on - time::Duration::days(i64::from(on.weekday().number_from_monday() - 1))
            }
            // The first of a real month always exists; the fallback is unreachable.
            Self::Monthly => time::Date::from_calendar_date(on.year(), on.month(), 1).unwrap_or(on),
        }
    }
}

/// One tag-scoped form of a TPM allowance (issue #209): how a key's settled
/// tokens are sliced into a bucket beside the plain per-key window. The cap is
/// the scope's `tokensPerMinute`; a turn accrues into the bucket only when its
/// `metadata` carries the scope's tag — and, for [`Concrete`](TpmScope::Concrete),
/// the pinned value.
///
/// The wire shape is one object naming its form — `{"each": 5000}`,
/// `{"concrete": "free", "tokensPerMinute": 1000}`, `{"total": 20000}` — which
/// no serde derive spells, so `Serialize`/`Deserialize` are written by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TpmScope {
    /// A bucket per tag value, capped at the given settled tokens per minute.
    /// `{ "each": 5000 }` on tag `team` makes `team=red` and `team=blue` separate
    /// buckets of 5000 each.
    Each(i64),
    /// One named value, capped at `tokens_per_minute`: only turns whose tag
    /// carries exactly `concrete` accrue.
    Concrete {
        /// The tag value the scope pins.
        concrete: String,
        /// The pinned bucket's settled-token allowance per minute.
        tokens_per_minute: i64,
    },
    /// The whole tag aggregated into one bucket, capped at the given settled
    /// tokens per minute, whatever value the turn carries. `{ "total": 20000 }`.
    Total(i64),
}

impl Serialize for TpmScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(match self {
            Self::Concrete { .. } => 2,
            _ => 1,
        }))?;
        match self {
            Self::Each(cap) => map.serialize_entry("each", cap)?,
            Self::Concrete {
                concrete,
                tokens_per_minute,
            } => {
                map.serialize_entry("concrete", concrete)?;
                map.serialize_entry("tokensPerMinute", tokens_per_minute)?;
            }
            Self::Total(cap) => map.serialize_entry("total", cap)?,
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for TpmScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // An intermediate map keeps the three shapes flat: whichever of `each`,
        // `concrete`, `total` is present names the form; `concrete` carries
        // `tokensPerMinute` beside it.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Flat {
            each: Option<i64>,
            concrete: Option<String>,
            tokens_per_minute: Option<i64>,
            total: Option<i64>,
        }
        let flat = Flat::deserialize(deserializer)?;
        match (flat.each, flat.concrete, flat.tokens_per_minute, flat.total) {
            (Some(cap), None, None, None) => Ok(TpmScope::Each(cap)),
            (None, Some(concrete), Some(tpm), None) => Ok(TpmScope::Concrete {
                concrete,
                tokens_per_minute: tpm,
            }),
            (None, None, None, Some(cap)) => Ok(TpmScope::Total(cap)),
            _ => Err(serde::de::Error::custom(
                "a token scope is exactly one of each, concrete+tokensPerMinute, or total",
            )),
        }
    }
}

impl TpmScope {
    /// The settled-token-per-minute cap the scope carries, whichever form it is.
    pub fn cap(&self) -> i64 {
        match self {
            Self::Each(cap)
            | Self::Concrete {
                tokens_per_minute: cap,
                ..
            }
            | Self::Total(cap) => *cap,
        }
    }
}

/// A tag name plus the scope the key places on it: one entry of
/// `tokensPerMinuteScopes`. The tag names the `metadata` key the turn's
/// attribution carries; the scope says how its settled tokens bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagScope {
    /// The `metadata` key the scope reads, bounded like a tag key.
    pub tag: String,
    /// The scope form and its cap.
    pub scope: TpmScope,
}

impl TagScope {
    /// The `oxsum.tpm_windows.scope` name a turn accrues into, or `None` when the
    /// turn's tags do not select this scope — an untagged turn never accrues a
    /// scoped row, and a `concrete` scope accrues only on the pinned value.
    ///
    /// `Each` and `Concrete` name their bucket `tag:<tag>=<value>`; `Total` names
    /// it `tag:<tag>`. The empty scope stays the plain per-key window, which a
    /// scoped name can never collide with because it always carries a tag.
    pub fn scope_name(&self, tags: &BTreeMap<String, String>) -> Option<String> {
        match &self.scope {
            TpmScope::Each(_) => tags
                .get(&self.tag)
                .map(|value| format!("tag:{}={value}", self.tag)),
            TpmScope::Concrete { concrete, .. } => match tags.get(&self.tag) {
                Some(value) if value == concrete => Some(format!("tag:{}={concrete}", self.tag)),
                _ => None,
            },
            TpmScope::Total(_) => tags.get(&self.tag).map(|_| format!("tag:{}", self.tag)),
        }
    }
}

/// The scopes a `tokensPerMinuteScopes` JSON object decodes into: the stored
/// `jsonb` maps a tag name to its one scope object. Sorted by tag so a stored
/// set reads back deterministically.
pub type TpmScopes = BTreeMap<String, TpmScope>;

/// The constraints one key carries, written as a set — at mint or replaced wholesale
/// by a patch. Every field is optional: `None` lifts that constraint.
#[derive(Debug, Clone, Default)]
pub struct KeyConstraints {
    /// Settled charges plus outstanding holds attributed to the key may not exceed it.
    pub spend_limit_minor: Option<i64>,
    /// Makes the limit periodic over the current UTC period. Requires a limit.
    pub budget_duration: Option<BudgetDuration>,
    /// The gateway models the key may call. `None` allows every served model.
    pub model_allowlist: Option<Vec<String>>,
    /// Requests the key may start inside a rolling minute — on the gateway and on
    /// `/api/v1/holds`. `None` is uncapped.
    pub requests_per_minute: Option<i32>,
    /// Holds the key may have outstanding at once, counted from the ledger's
    /// pending entries under the per-key lock. `None` is uncapped.
    pub max_concurrent_holds: Option<i32>,
    /// Tokens the key's settled turns may burn inside a rolling minute, counted
    /// from the durable window a settlement feeds — not from holds, which never
    /// occupy it. `None` is uncapped (issue #203).
    pub tokens_per_minute: Option<i32>,
    /// Tag-scoped slices of that allowance (issue #209): a map from a `metadata`
    /// tag key to one scope — `each` fans a bucket out per value, `concrete`
    /// pins one value, `total` aggregates the whole tag. Each scoped bucket is
    /// an additional cap alongside `tokens_per_minute`, not a replacement.
    /// `None` (or an empty map) scopes nothing.
    pub tokens_per_minute_scopes: Option<TpmScopes>,
}

impl KeyConstraints {
    /// The domain rules the column checks cannot express: a window needs a limit, and
    /// an allowlist is null or a non-empty list of non-empty names.
    fn validate(&self) -> Result<(), WalletError> {
        validate_limit(self.spend_limit_minor)?;
        if self.budget_duration.is_some() && self.spend_limit_minor.is_none() {
            return Err(WalletError::InvalidInput(
                "budgetDuration requires spendLimitMinor".into(),
            ));
        }
        if let Some(models) = &self.model_allowlist {
            validate_model_allowlist(models)?;
        }
        for (name, value) in [
            ("requestsPerMinute", self.requests_per_minute),
            ("maxConcurrentHolds", self.max_concurrent_holds),
            ("tokensPerMinute", self.tokens_per_minute),
        ] {
            if let Some(value) = value
                && value <= 0
            {
                return Err(WalletError::InvalidInput(format!(
                    "{name} must be at least 1"
                )));
            }
        }
        if let Some(scopes) = &self.tokens_per_minute_scopes {
            validate_tpm_scopes(scopes)?;
        }
        Ok(())
    }
}

/// The rules a `tokensPerMinuteScopes` map follows (issue #209): a tag name is a
/// non-empty tag-key-sized name, each scope's cap is at least 1, and a
/// `concrete` scope names a non-empty tag-value-sized value. The map may name
/// several tags but each tag carries one scope — the object shape enforces it.
fn validate_tpm_scopes(scopes: &TpmScopes) -> Result<(), WalletError> {
    for (tag, scope) in scopes {
        if tag.trim().is_empty() || tag.len() > crate::usage::MAX_TAG {
            return Err(WalletError::InvalidInput(format!(
                "a tokensPerMinuteScopes tag is a tag name, 1..={} bytes",
                crate::usage::MAX_TAG
            )));
        }
        if scope.cap() <= 0 {
            return Err(WalletError::InvalidInput(format!(
                "tokensPerMinuteScopes.{tag} must cap at least 1"
            )));
        }
        if let TpmScope::Concrete { concrete, .. } = scope
            && (concrete.is_empty() || concrete.len() > crate::usage::MAX_TAG)
        {
            return Err(WalletError::InvalidInput(format!(
                "tokensPerMinuteScopes.{tag}.concrete is a tag value, 1..={} bytes",
                crate::usage::MAX_TAG
            )));
        }
    }
    Ok(())
}

/// An API key as the API presents it. Never the secret, never the hash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    pub id: Uuid,
    pub name: Option<String>,
    /// The leading characters of the secret, for recognizing a key in a list.
    pub prefix: String,
    /// Who minted the key, for the per-member role rules. `None` for keys minted with an
    /// API key, because no person acts there.
    pub created_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    /// The key's spend limit in minor units: settled charges plus outstanding holds
    /// attributed to the key may not exceed it. `None` is unlimited.
    pub spend_limit_minor: Option<i64>,
    /// The window the limit applies to; `None` makes it cumulative.
    pub budget_duration: Option<BudgetDuration>,
    /// The gateway models the key may call; `None` allows every served model.
    pub model_allowlist: Option<Vec<String>>,
    /// The key's rolling-minute request allowance; `None` is uncapped.
    pub requests_per_minute: Option<i32>,
    /// The key's outstanding-holds cap; `None` is uncapped.
    pub max_concurrent_holds: Option<i32>,
    /// The key's rolling-minute token allowance; `None` is uncapped (issue #203).
    pub tokens_per_minute: Option<i32>,
    /// The key's tag-scoped token allowances (issue #209); `None` scopes nothing.
    pub tokens_per_minute_scopes: Option<TpmScopes>,
}

/// The credential behind a key-authenticated request: which key acted, and the
/// constraints the admission path needs without a second read — the spend limit
/// and the per-minute request allowance. Carried on the principal so the hold
/// path can enforce it.
#[derive(Debug, Clone)]
pub struct ActingKey {
    pub key_id: Uuid,
    pub spend_limit_minor: Option<i64>,
    /// The rolling-minute request allowance in force when the key authenticated.
    /// Checked against the limiter's own window at admission; a PATCH landing
    /// between authentication and a hold tightens nothing retroactively for a
    /// request already read, and loosens on the next authentication.
    pub requests_per_minute: Option<i32>,
    /// The rolling-minute token allowance in force when the key authenticated,
    /// checked against the durable settled-usage window at admission (issue #203).
    pub tokens_per_minute: Option<i32>,
    /// The tag-scoped token allowances in force when the key authenticated
    /// (issue #209), checked bucket-by-bucket against the same durable window.
    /// `None` scopes nothing.
    pub tokens_per_minute_scopes: Option<TpmScopes>,
}

/// A key plus its secret, returned exactly once: at creation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedApiKey {
    #[serde(flatten)]
    pub key: ApiKey,
    pub secret: String,
}

impl Db {
    /// Mints a key for an organization and returns it with its secret, once.
    pub async fn create_key(
        &self,
        organization_id: Uuid,
        name: Option<String>,
        expires_at: Option<OffsetDateTime>,
        created_by: Option<Uuid>,
        constraints: KeyConstraints,
    ) -> Result<CreatedApiKey, WalletError> {
        let name = validate_name(name)?;
        if let Some(expires) = expires_at
            && expires <= OffsetDateTime::now_utc()
        {
            return Err(WalletError::InvalidInput(
                "expiresAt must be in the future".into(),
            ));
        }
        constraints.validate()?;
        let mut tx = self.pool().begin().await?;
        let created = insert(
            &mut tx,
            organization_id,
            name,
            expires_at,
            created_by,
            constraints,
        )
        .await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Resolves a presented secret to the organization it spends for, and the key that acted.
    ///
    /// `None` covers every way a key can fail — unknown, revoked, expired — so a caller
    /// cannot tell them apart, and neither can anyone probing for valid keys.
    pub async fn authenticate(
        &self,
        secret: &str,
    ) -> Result<Option<(Organization, ActingKey)>, WalletError> {
        let hash = hash_secret(secret);
        let row = sqlx::query(
            "SELECT o.organization_id, o.name, o.tenant_id, o.kind, o.created_at, \
                    k.key_id, k.spend_limit_minor, k.requests_per_minute, \
                    k.tokens_per_minute, k.tokens_per_minute_scopes \
             FROM oxsum.api_keys k JOIN oxsum.organizations o USING (organization_id) \
             WHERE k.secret_hash = $1 \
               AND k.revoked_at IS NULL \
               AND (k.expires_at IS NULL OR k.expires_at > now())",
        )
        .bind(hash.as_slice())
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            use sqlx::Row;
            let organization = orgs::organization_from_row(&row)?;
            let key = ActingKey {
                key_id: row.try_get("key_id")?,
                spend_limit_minor: row.try_get("spend_limit_minor")?,
                requests_per_minute: row.try_get("requests_per_minute")?,
                tokens_per_minute: row.try_get("tokens_per_minute")?,
                tokens_per_minute_scopes: scopes_from_json(
                    row.try_get::<Option<serde_json::Value>, _>("tokens_per_minute_scopes")?,
                ),
            };
            Ok((organization, key))
        })
        .transpose()
    }

    /// Replaces a key's whole constraint set: the request names the limit, the budget
    /// window and the allowlist, each cleared by null. A constraint write also clears
    /// `budget_forecast_sent_for`, so a new limit or window forecasts again from
    /// scratch instead of inheriting the old terms' latch. `None` returned means this
    /// organization has no such key — which is also the answer for another
    /// organization's key id, and for a member naming a key they did not create, so
    /// ids cannot be probed. The scope rules are the revoke's.
    ///
    /// # Errors
    ///
    /// A negative limit, a window without a limit or a malformed allowlist is
    /// [`WalletError::InvalidInput`]; storage failures surface as [`WalletError`].
    pub async fn update_key_constraints(
        &self,
        organization_id: Uuid,
        key_id: Uuid,
        scope: KeyScope,
        constraints: KeyConstraints,
    ) -> Result<Option<ApiKey>, WalletError> {
        constraints.validate()?;
        // `model_allowlist` moves into its bind below, so the scopes JSON — a
        // borrow of the whole struct — is built before any field leaves.
        let scopes = scopes_json(&constraints);
        // Like revoke: the scope is part of the lookup, not a check after it, so a member
        // naming a key they did not create gets the same answer as a key that does not exist.
        let row = match scope {
            KeyScope::Own(user_id) => {
                sqlx::query(&format!(
                    "UPDATE oxsum.api_keys \
                     SET spend_limit_minor = $4, budget_duration = $5, model_allowlist = $6, \
                         requests_per_minute = $7, max_concurrent_holds = $8, \
                         tokens_per_minute = $9, tokens_per_minute_scopes = $10, \
                         budget_forecast_sent_for = NULL \
                     WHERE organization_id = $1 AND key_id = $2 AND created_by = $3 \
                     RETURNING {KEY_COLS}"
                ))
                .bind(organization_id)
                .bind(key_id)
                .bind(user_id)
                .bind(constraints.spend_limit_minor)
                .bind(constraints.budget_duration.map(|d| d.as_str()))
                .bind(constraints.model_allowlist)
                .bind(constraints.requests_per_minute)
                .bind(constraints.max_concurrent_holds)
                .bind(constraints.tokens_per_minute)
                .bind(scopes.clone())
                .fetch_optional(self.pool())
                .await?
            }
            KeyScope::Organization | KeyScope::All => {
                sqlx::query(&format!(
                    "UPDATE oxsum.api_keys \
                     SET spend_limit_minor = $3, budget_duration = $4, model_allowlist = $5, \
                         requests_per_minute = $6, max_concurrent_holds = $7, \
                         tokens_per_minute = $8, tokens_per_minute_scopes = $9, \
                         budget_forecast_sent_for = NULL \
                     WHERE organization_id = $1 AND key_id = $2 \
                     RETURNING {KEY_COLS}"
                ))
                .bind(organization_id)
                .bind(key_id)
                .bind(constraints.spend_limit_minor)
                .bind(constraints.budget_duration.map(|d| d.as_str()))
                .bind(constraints.model_allowlist)
                .bind(constraints.requests_per_minute)
                .bind(constraints.max_concurrent_holds)
                .bind(constraints.tokens_per_minute)
                .bind(scopes)
                .fetch_optional(self.pool())
                .await?
            }
        };
        if row.is_some() {
            // The re-set limit re-arms the key's threshold latches: a budget
            // that already fired announces again under the new line (issue #232).
            sqlx::query(
                "DELETE FROM oxsum.budget_threshold_fires \
                 WHERE scope = 'key' AND scope_id = $1",
            )
            .bind(key_id)
            .execute(self.pool())
            .await?;
        }
        row.as_ref().map(key_from_row).transpose()
    }

    /// Every key of an organization the principal may see, newest first. Metadata only.
    ///
    /// A member sees only the keys they created; an owner, an admin, or an API key acting
    /// as the organization sees every key of the organization.
    pub async fn list_keys(
        &self,
        organization_id: Uuid,
        scope: KeyScope,
    ) -> Result<Vec<ApiKey>, WalletError> {
        let rows = match scope {
            KeyScope::Own(user_id) => {
                sqlx::query(&format!(
                    "SELECT {KEY_COLS} FROM oxsum.api_keys \
                 WHERE organization_id = $1 AND created_by = $2 \
                 ORDER BY created_at DESC, key_id"
                ))
                .bind(organization_id)
                .bind(user_id)
                .fetch_all(self.pool())
                .await?
            }
            KeyScope::Organization | KeyScope::All => {
                sqlx::query(&format!(
                    "SELECT {KEY_COLS} FROM oxsum.api_keys \
                 WHERE organization_id = $1 \
                 ORDER BY created_at DESC, key_id"
                ))
                .bind(organization_id)
                .fetch_all(self.pool())
                .await?
            }
        };
        rows.iter().map(key_from_row).collect()
    }

    /// The exhaustion projection [`forecast_exhaustion`](crate::webhooks::forecast_exhaustion)
    /// computes at settle time, recomputed on read for every key of the organization that
    /// carries both a `spend_limit_minor` and a `budget_duration` — the keys page renders
    /// one line per returned forecast (issue #220).
    ///
    /// The committed spend is the same scope the hold-side limit check and the settle-time
    /// forecast read: settled charges booked on or after the current window's start plus
    /// every outstanding hold, summed over the wallet, bonus and credit-line pools. A key
    /// whose projection lands outside its window does not appear — like the event, the
    /// dashboard line exists only when the burn rate crosses inside the period.
    ///
    /// An organization with no ledger yet (a fresh tenant before its first hold) reads
    /// back empty: there is nothing to project.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn key_budget_forecasts(
        &self,
        organization_id: Uuid,
        scope: KeyScope,
    ) -> Result<Vec<crate::webhooks::BudgetForecast>, WalletError> {
        // The keys this caller may see, constrained like `list_keys`: the scope is part
        // of the lookup, so a member reads only their own keys' forecasts.
        let keys: Vec<(Uuid, i64, String)> = match scope {
            KeyScope::Own(user_id) => {
                sqlx::query_as(
                    "SELECT key_id, spend_limit_minor, budget_duration FROM oxsum.api_keys \
                     WHERE organization_id = $1 AND created_by = $2 \
                       AND spend_limit_minor IS NOT NULL AND budget_duration IS NOT NULL \
                       AND revoked_at IS NULL",
                )
                .bind(organization_id)
                .bind(user_id)
                .fetch_all(self.pool())
                .await?
            }
            KeyScope::Organization | KeyScope::All => {
                sqlx::query_as(
                    "SELECT key_id, spend_limit_minor, budget_duration FROM oxsum.api_keys \
                     WHERE organization_id = $1 \
                       AND spend_limit_minor IS NOT NULL AND budget_duration IS NOT NULL \
                       AND revoked_at IS NULL",
                )
                .bind(organization_id)
                .fetch_all(self.pool())
                .await?
            }
        };
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        // The tenant's ledger name and its three drawable pools, resolved by path like the
        // settle-time forecast does. A tenant with no ledger yet reads back empty.
        let ledger = {
            let tenant: String = sqlx::query_scalar(
                "SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1",
            )
            .bind(organization_id)
            .fetch_one(self.pool())
            .await?;
            format!("ledger_{}", tenant.replace('"', "\"\""))
        };
        let has_ledger = sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text")
            .bind(format!("\"{ledger}\".accounts"))
            .fetch_one(self.pool())
            .await?
            .is_some();
        if !has_ledger {
            return Ok(Vec::new());
        }
        let pools: Vec<i32> = sqlx::query_scalar(&format!(
            "SELECT account_index FROM \"{ledger}\".accounts WHERE path = ANY($1)",
        ))
        .bind(vec![
            crate::wallet::WALLET.to_owned(),
            crate::wallet::BONUS.to_owned(),
            crate::wallet::CREDIT_LINE.to_owned(),
        ])
        .fetch_all(self.pool())
        .await?;
        if pools.is_empty() {
            return Ok(Vec::new());
        }
        let today = OffsetDateTime::now_utc().date();
        let mut forecasts = Vec::with_capacity(keys.len());
        for (key_id, limit, duration) in keys {
            let duration = BudgetDuration::parse(&duration)?;
            let window_start = duration.period_start(today);
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
            .fetch_one(self.pool())
            .await?;
            if let Some(mut forecast) = crate::webhooks::forecast_exhaustion(
                duration,
                window_start,
                today,
                committed,
                limit,
            ) {
                forecast.key_id = key_id;
                forecasts.push(forecast);
            }
        }
        Ok(forecasts)
    }

    /// Revokes a key. `None` means this organization has no such key — which is also the
    /// answer for another organization's key id, and for a member naming a key they did not
    /// create, so ids cannot be probed.
    ///
    /// Revoking twice is not an error: the first revocation's timestamp stands.
    pub async fn revoke_key(
        &self,
        organization_id: Uuid,
        key_id: Uuid,
        scope: KeyScope,
    ) -> Result<Option<ApiKey>, WalletError> {
        // A member revoking a key they did not create gets the same answer as a key that
        // does not exist — the key stays live — because the scope is part of the lookup,
        // not a check after it.
        let row = match scope {
            KeyScope::Own(user_id) => {
                sqlx::query(&format!(
                    "UPDATE oxsum.api_keys SET revoked_at = COALESCE(revoked_at, now()) \
                     WHERE organization_id = $1 AND key_id = $2 AND created_by = $3 \
                     RETURNING {KEY_COLS}"
                ))
                .bind(organization_id)
                .bind(key_id)
                .bind(user_id)
                .fetch_optional(self.pool())
                .await?
            }
            KeyScope::Organization | KeyScope::All => {
                sqlx::query(&format!(
                    "UPDATE oxsum.api_keys SET revoked_at = COALESCE(revoked_at, now()) \
                     WHERE organization_id = $1 AND key_id = $2 \
                     RETURNING {KEY_COLS}"
                ))
                .bind(organization_id)
                .bind(key_id)
                .fetch_optional(self.pool())
                .await?
            }
        };
        row.as_ref().map(key_from_row).transpose()
    }
}

/// Inserts a key on the caller's transaction, so signup can mint the first one inside the
/// same transaction that creates its organization.
pub(crate) async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
    name: Option<String>,
    expires_at: Option<OffsetDateTime>,
    created_by: Option<Uuid>,
    constraints: KeyConstraints,
) -> Result<CreatedApiKey, WalletError> {
    let secret = generate_secret();
    let prefix = secret.chars().take(PREFIX_CHARS).collect::<String>();
    let hash = hash_secret(&secret);
    // `model_allowlist` moves into its bind, so the scopes JSON — a borrow of the
    // whole struct — is built before any field leaves.
    let scopes = scopes_json(&constraints);
    let row = sqlx::query(&format!(
        "INSERT INTO oxsum.api_keys \
             (key_id, organization_id, name, prefix, secret_hash, created_by, expires_at, \
              spend_limit_minor, budget_duration, model_allowlist, \
              requests_per_minute, max_concurrent_holds, tokens_per_minute, \
              tokens_per_minute_scopes) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         RETURNING {KEY_COLS}"
    ))
    .bind(Uuid::new_v4())
    .bind(organization_id)
    .bind(name.as_deref())
    .bind(&prefix)
    .bind(hash.as_slice())
    .bind(created_by)
    .bind(expires_at)
    .bind(constraints.spend_limit_minor)
    .bind(constraints.budget_duration.map(|d| d.as_str()))
    .bind(constraints.model_allowlist)
    .bind(constraints.requests_per_minute)
    .bind(constraints.max_concurrent_holds)
    .bind(constraints.tokens_per_minute)
    .bind(scopes)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| conflict_or_storage(e, "api_keys_secret_hash_key", "key collision"))?;
    Ok(CreatedApiKey {
        key: key_from_row(&row)?,
        secret,
    })
}

/// A new secret: the mark plus 32 random bytes from the operating system's CSPRNG.
fn generate_secret() -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut secret = String::with_capacity(SECRET_MARK.len() + SECRET_BYTES * 2);
    secret.push_str(SECRET_MARK);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(secret, "{byte:02x}");
    }
    secret
}

/// SHA-256 of the secret, the only form the database ever holds.
///
/// A plain hash rather than a password hash: the secret is 256 bits of machine randomness,
/// so there is no guessing to slow down, while every authenticated request pays for it.
fn hash_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

fn validate_name(name: Option<String>) -> Result<Option<String>, WalletError> {
    match name {
        None => Ok(None),
        Some(name) => {
            let name = name.trim().to_owned();
            if name.is_empty() {
                Ok(None)
            } else if name.chars().count() > MAX_NAME {
                Err(WalletError::InvalidInput(format!(
                    "name must be at most {MAX_NAME} characters"
                )))
            } else {
                Ok(Some(name))
            }
        }
    }
}

fn key_from_row(row: &sqlx::postgres::PgRow) -> Result<ApiKey, WalletError> {
    use sqlx::Row;
    Ok(ApiKey {
        id: row.try_get("key_id")?,
        name: row.try_get("name")?,
        prefix: row.try_get("prefix")?,
        created_by: row.try_get("created_by")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        revoked_at: row.try_get("revoked_at")?,
        spend_limit_minor: row.try_get("spend_limit_minor")?,
        budget_duration: row
            .try_get::<Option<String>, _>("budget_duration")?
            .as_deref()
            .map(BudgetDuration::parse)
            .transpose()?,
        model_allowlist: row.try_get("model_allowlist")?,
        requests_per_minute: row.try_get("requests_per_minute")?,
        max_concurrent_holds: row.try_get("max_concurrent_holds")?,
        tokens_per_minute: row.try_get("tokens_per_minute")?,
        tokens_per_minute_scopes: scopes_from_json(
            row.try_get::<Option<serde_json::Value>, _>("tokens_per_minute_scopes")?,
        ),
    })
}

/// The `tokens_per_minute_scopes` value the column stores: the scopes map as
/// `jsonb`, or `NULL` when the key scopes nothing — an empty map reads as "no
/// scopes" too, so it is stored as NULL rather than an empty object.
fn scopes_json(constraints: &KeyConstraints) -> Option<serde_json::Value> {
    constraints
        .tokens_per_minute_scopes
        .as_ref()
        .filter(|scopes| !scopes.is_empty())
        .map(serde_json::to_value)
        .and_then(Result::ok)
}

/// Reads the stored `jsonb` scopes back into a map; `NULL` and an unreadable
/// shape both yield `None`, matching the "no scopes" reading a NULL takes.
fn scopes_from_json(value: Option<serde_json::Value>) -> Option<TpmScopes> {
    value.and_then(|v| serde_json::from_value(v).ok())
}

/// A spend limit must be a non-negative amount in minor units; `None` clears it to unlimited.
fn validate_limit(spend_limit_minor: Option<i64>) -> Result<(), WalletError> {
    if spend_limit_minor.is_some_and(|limit| limit < 0) {
        return Err(WalletError::InvalidInput(
            "spendLimitMinor must be non-negative".into(),
        ));
    }
    Ok(())
}

/// A unique-constraint violation on `constraint` becomes the named conflict; every other
/// database failure stays a storage failure.
///
/// Shared with signup, where the same constraint on `users.email_normalized` is the race
/// two simultaneous registrations would otherwise win together.
pub(crate) fn conflict_or_storage(
    error: sqlx::Error,
    constraint: &str,
    message: &str,
) -> WalletError {
    let violated =
        matches!(&error, sqlx::Error::Database(db) if db.constraint() == Some(constraint));
    if violated {
        WalletError::Conflict(message.to_owned())
    } else {
        WalletError::from(error)
    }
}

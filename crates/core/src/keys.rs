//! API keys: the credential that names the organization.
//!
//! A key belongs to an organization and holds no money of its own. The plaintext exists
//! once, in the response that created it; the database keeps its SHA-256 hash and a display
//! prefix, so a dump of `oxsum.api_keys` authenticates nothing. See docs/product.md,
//! "API keys".

use std::fmt::Write as _;

use rand::Rng;
use serde::Serialize;
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
                        requests_per_minute, max_concurrent_holds";

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
    pub(crate) fn period_start(self, on: time::Date) -> time::Date {
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
        ] {
            if let Some(value) = value
                && value <= 0
            {
                return Err(WalletError::InvalidInput(format!(
                    "{name} must be at least 1"
                )));
            }
        }
        Ok(())
    }
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
                    k.key_id, k.spend_limit_minor, k.requests_per_minute \
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
            };
            Ok((organization, key))
        })
        .transpose()
    }

    /// Replaces a key's whole constraint set: the request names the limit, the budget
    /// window and the allowlist, each cleared by null. `None` returned means this
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
        // Like revoke: the scope is part of the lookup, not a check after it, so a member
        // naming a key they did not create gets the same answer as a key that does not exist.
        let row = match scope {
            KeyScope::Own(user_id) => {
                sqlx::query(&format!(
                    "UPDATE oxsum.api_keys \
                     SET spend_limit_minor = $4, budget_duration = $5, model_allowlist = $6, \
                         requests_per_minute = $7, max_concurrent_holds = $8 \
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
                .fetch_optional(self.pool())
                .await?
            }
            KeyScope::Organization | KeyScope::All => {
                sqlx::query(&format!(
                    "UPDATE oxsum.api_keys \
                     SET spend_limit_minor = $3, budget_duration = $4, model_allowlist = $5, \
                         requests_per_minute = $6, max_concurrent_holds = $7 \
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
                .fetch_optional(self.pool())
                .await?
            }
        };
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
    let row = sqlx::query(&format!(
        "INSERT INTO oxsum.api_keys \
             (key_id, organization_id, name, prefix, secret_hash, created_by, expires_at, \
              spend_limit_minor, budget_duration, model_allowlist, \
              requests_per_minute, max_concurrent_holds) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
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
    })
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

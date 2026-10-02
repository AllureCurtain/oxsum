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

/// Every secret starts with this, so secret scanners and users can recognize one.
const SECRET_MARK: &str = "oxs-";
/// 32 random bytes, per docs/product.md.
const SECRET_BYTES: usize = 32;
/// How much of the secret is kept for display: the mark plus 8 hex characters.
const PREFIX_CHARS: usize = 4 + 8;
/// The longest a key name may be.
pub(crate) const MAX_NAME: usize = 80;

/// An API key as the API presents it. Never the secret, never the hash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    pub id: Uuid,
    pub name: Option<String>,
    /// The leading characters of the secret, for recognizing a key in a list.
    pub prefix: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
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
    ) -> Result<CreatedApiKey, WalletError> {
        let name = validate_name(name)?;
        if let Some(expires) = expires_at
            && expires <= OffsetDateTime::now_utc()
        {
            return Err(WalletError::InvalidInput(
                "expiresAt must be in the future".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let created = insert(&mut tx, organization_id, name, expires_at, created_by).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Resolves a presented secret to the organization it spends for.
    ///
    /// `None` covers every way a key can fail — unknown, revoked, expired — so a caller
    /// cannot tell them apart, and neither can anyone probing for valid keys.
    pub async fn authenticate(&self, secret: &str) -> Result<Option<Organization>, WalletError> {
        let hash = hash_secret(secret);
        let row = sqlx::query(
            "SELECT o.organization_id, o.name, o.tenant_id, o.kind, o.created_at \
             FROM oxsum.api_keys k JOIN oxsum.organizations o USING (organization_id) \
             WHERE k.secret_hash = $1 \
               AND k.revoked_at IS NULL \
               AND (k.expires_at IS NULL OR k.expires_at > now())",
        )
        .bind(hash.as_slice())
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(orgs::organization_from_row).transpose()
    }

    /// Every key of an organization, newest first. Metadata only.
    pub async fn list_keys(&self, organization_id: Uuid) -> Result<Vec<ApiKey>, WalletError> {
        let rows = sqlx::query(
            "SELECT key_id, name, prefix, created_at, expires_at, revoked_at \
             FROM oxsum.api_keys WHERE organization_id = $1 ORDER BY created_at DESC, key_id",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(key_from_row).collect()
    }

    /// Revokes a key. `None` means this organization has no such key — which is also the
    /// answer for another organization's key id, so ids cannot be probed.
    ///
    /// Revoking twice is not an error: the first revocation's timestamp stands.
    pub async fn revoke_key(
        &self,
        organization_id: Uuid,
        key_id: Uuid,
    ) -> Result<Option<ApiKey>, WalletError> {
        let row = sqlx::query(
            "UPDATE oxsum.api_keys SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE organization_id = $1 AND key_id = $2 \
             RETURNING key_id, name, prefix, created_at, expires_at, revoked_at",
        )
        .bind(organization_id)
        .bind(key_id)
        .fetch_optional(self.pool())
        .await?;
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
) -> Result<CreatedApiKey, WalletError> {
    let secret = generate_secret();
    let prefix = secret.chars().take(PREFIX_CHARS).collect::<String>();
    let hash = hash_secret(&secret);
    let row = sqlx::query(
        "INSERT INTO oxsum.api_keys \
             (key_id, organization_id, name, prefix, secret_hash, created_by, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING key_id, name, prefix, created_at, expires_at, revoked_at",
    )
    .bind(Uuid::new_v4())
    .bind(organization_id)
    .bind(name.as_deref())
    .bind(&prefix)
    .bind(hash.as_slice())
    .bind(created_by)
    .bind(expires_at)
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
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
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

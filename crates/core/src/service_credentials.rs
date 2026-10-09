//! Service credentials: the metering API's bearer (issue #172, roadmap P8-4).
//!
//! A service credential is the deployer's own services' key to `/api/v1/metering/*`,
//! where a request names the organization it meters for. It belongs to no
//! organization — that is the point: an organization's API key or a member's session
//! can never report usage, only the deployer's services can. The plaintext exists
//! once, in the mint answer; the database keeps its SHA-256 hash and a display
//! prefix, so a dump of `oxsum.service_credentials` authenticates nothing.

use std::fmt::Write as _;

use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// Every secret starts with this, so secret scanners and operators can recognize one.
const SECRET_MARK: &str = "oxs-svc-";
/// 32 random bytes, like an API key's secret.
const SECRET_BYTES: usize = 32;
/// How much of the secret is kept for display: the mark plus 8 hex characters.
const PREFIX_CHARS: usize = 8 + 8;
/// The columns every `ServiceCredential` read projects, in one place.
const CREDENTIAL_COLS: &str = "credential_id, name, prefix, created_at, revoked_at";

/// A service credential as the API presents it. Never the secret, never the hash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCredential {
    #[serde(rename = "credentialId")]
    pub id: Uuid,
    /// Which service holds it — the name settlement descriptions record as the
    /// reporter. `None` when minted unnamed.
    pub name: Option<String>,
    /// The leading characters of the secret, for recognizing it in a list.
    pub prefix: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// The credential behind a metering request: which credential authenticated,
/// carried so the settlement description can name the reporting service.
#[derive(Debug, Clone)]
pub struct ActingService {
    pub credential_id: Uuid,
    pub name: Option<String>,
}

/// A credential plus its secret, returned exactly once: at mint.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedServiceCredential {
    #[serde(flatten)]
    pub credential: ServiceCredential,
    pub secret: String,
}

impl Db {
    /// Mints a service credential and returns it with its secret, once.
    ///
    /// # Errors
    ///
    /// An over-long name is [`WalletError::InvalidInput`]; storage failures surface
    /// as [`WalletError`].
    pub async fn create_service_credential(
        &self,
        name: Option<String>,
    ) -> Result<CreatedServiceCredential, WalletError> {
        let name = validate_name(name)?;
        let secret = generate_secret();
        let prefix = secret.chars().take(PREFIX_CHARS).collect::<String>();
        let hash = hash_secret(&secret);
        let row = sqlx::query(&format!(
            "INSERT INTO oxsum.service_credentials \
                 (credential_id, name, prefix, secret_hash) \
             VALUES ($1, $2, $3, $4) RETURNING {CREDENTIAL_COLS}"
        ))
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(&prefix)
        .bind(hash.as_slice())
        .fetch_one(self.pool())
        .await?;
        Ok(CreatedServiceCredential {
            credential: credential_from_row(&row)?,
            secret,
        })
    }

    /// Resolves a presented secret to the credential that acts.
    ///
    /// `None` covers every way a credential can fail — unknown, revoked — so a
    /// caller cannot tell them apart, and neither can anyone probing for a live
    /// one.
    pub async fn authenticate_service(
        &self,
        secret: &str,
    ) -> Result<Option<ActingService>, WalletError> {
        let hash = hash_secret(secret);
        let row = sqlx::query(
            "SELECT credential_id, name FROM oxsum.service_credentials \
             WHERE secret_hash = $1 AND revoked_at IS NULL",
        )
        .bind(hash.as_slice())
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            use sqlx::Row;
            Ok(ActingService {
                credential_id: row.try_get("credential_id")?,
                name: row.try_get("name")?,
            })
        })
        .transpose()
    }

    /// Every credential, newest first. Metadata only.
    pub async fn list_service_credentials(&self) -> Result<Vec<ServiceCredential>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {CREDENTIAL_COLS} FROM oxsum.service_credentials \
             ORDER BY created_at DESC, credential_id"
        ))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(credential_from_row).collect()
    }

    /// Revokes a credential. `None` means no live credential has this id — a
    /// revoked one answers the same, so a second revoke is not an answer to
    /// anything new.
    pub async fn revoke_service_credential(
        &self,
        credential_id: Uuid,
    ) -> Result<Option<ServiceCredential>, WalletError> {
        let row = sqlx::query(&format!(
            "UPDATE oxsum.service_credentials SET revoked_at = now() \
             WHERE credential_id = $1 AND revoked_at IS NULL \
             RETURNING {CREDENTIAL_COLS}"
        ))
        .bind(credential_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(credential_from_row).transpose()
    }
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
fn hash_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

fn validate_name(name: Option<String>) -> Result<Option<String>, WalletError> {
    use crate::keys::MAX_NAME;
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

fn credential_from_row(row: &sqlx::postgres::PgRow) -> Result<ServiceCredential, WalletError> {
    use sqlx::Row;
    Ok(ServiceCredential {
        id: row.try_get("credential_id")?,
        name: row.try_get("name")?,
        prefix: row.try_get("prefix")?,
        created_at: row.try_get("created_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

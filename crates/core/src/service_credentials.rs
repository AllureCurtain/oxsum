//! Retired global service credentials: metadata/revocation for deployment recovery.
//!
//! Legacy secrets never authenticate after migration 0077. New issuance requires
//! explicit business owner consent and scoped integration grants.

use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// The columns every `ServiceCredential` read projects, in one place.
const CREDENTIAL_COLS: &str = "credential_id, name, prefix, created_at, revoked_at";

/// A service credential as the API presents it. Never the secret, never the hash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceCredential {
    /// The credential's identifier.
    #[serde(rename = "credentialId")]
    pub id: Uuid,
    /// Which service holds it — the name settlement descriptions record as the
    /// reporter. `None` when minted unnamed.
    pub name: Option<String>,
    /// The leading characters of the secret, for recognizing it in a list.
    pub prefix: String,
    /// When the credential was minted.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the credential was revoked; `None` while it is live.
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

impl Db {
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

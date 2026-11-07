//! Persistent public seller identity with owner authority and revisioned replay.

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{BusinessCapability, Db, WalletError, business_staff as authority, users};

/// Public seller identity. This never changes ledger or historical document data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicBusinessProfile {
    /// Display name, between one and eighty characters.
    pub name: String,
    /// Optional absolute HTTP(S) image URL.
    #[serde(deserialize_with = "required_nullable")]
    pub logo_url: Option<String>,
    /// Optional hexadecimal RGB accent.
    #[serde(deserialize_with = "required_nullable")]
    pub accent_color: Option<String>,
    /// Optional public contact email; this does not configure outbound mail.
    #[serde(deserialize_with = "required_nullable")]
    pub contact_email: Option<String>,
    /// Optional absolute HTTP(S) help URL.
    #[serde(deserialize_with = "required_nullable")]
    pub help_url: Option<String>,
}

impl Default for PublicBusinessProfile {
    fn default() -> Self {
        Self {
            name: "oxsum".into(),
            logo_url: None,
            accent_color: None,
            contact_email: None,
            help_url: None,
        }
    }
}

impl PublicBusinessProfile {
    /// Validate untrusted profile text and return normalized display values.
    pub fn validated(&self) -> Result<Self, WalletError> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
            return Err(WalletError::InvalidInput(
                "business name must contain 1 to 80 characters without controls".into(),
            ));
        }
        if let Some(color) = &self.accent_color
            && (color.len() != 7
                || !color.starts_with('#')
                || !color[1..].bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(WalletError::InvalidInput(
                "accentColor must be #rrggbb or null".into(),
            ));
        }
        let contact_email = self
            .contact_email
            .as_deref()
            .map(|email| {
                if email.chars().any(char::is_control) || email.contains(['?', '#', '%', '&']) {
                    return Err(WalletError::InvalidInput(
                        "contactEmail must be a plain email address".into(),
                    ));
                }
                users::validate_email(email)
            })
            .transpose()?;
        Ok(Self {
            name: name.into(),
            logo_url: self.logo_url.as_deref().map(profile_url).transpose()?,
            accent_color: self.accent_color.clone(),
            contact_email,
            help_url: self.help_url.as_deref().map(profile_url).transpose()?,
        })
    }
}

fn required_nullable<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

fn profile_url(raw: &str) -> Result<String, WalletError> {
    let value = raw.trim();
    let parsed = url::Url::parse(value)
        .map_err(|_| WalletError::InvalidInput("profile URL must be absolute HTTP(S)".into()))?;
    if value.len() > 2048
        || raw.chars().any(char::is_control)
        || !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(WalletError::InvalidInput(
            "profile URL must be HTTP(S), at most 2048 bytes and contain no user info or controls"
                .into(),
        ));
    }
    Ok(parsed.to_string())
}

/// Effective identity together with its concurrency marker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessProfile {
    /// Singleton business identity.
    pub business_id: Uuid,
    /// Saved revision; zero denotes deployment/setup defaults.
    pub revision: i64,
    /// `defaults` before an explicit save, `stored` afterwards.
    pub source: String,
    /// Time of the last explicit save.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
    /// Public fields shared by portal, login and the owner editor.
    pub branding: PublicBusinessProfile,
}

/// A complete profile replacement under an expected revision.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessProfileRequest {
    /// Exact retry identity, scoped to the current actor/business.
    pub idempotency_key: String,
    /// Required human-readable audit reason.
    pub reason: String,
    /// Revision displayed when the draft was opened.
    pub expected_revision: i64,
    /// Every optional field must be present; null clears it.
    pub branding: PublicBusinessProfile,
}

/// Immutable save receipt; subsequent changes do not change replay output.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessProfileMutation {
    /// Audit mutation identity.
    pub id: Uuid,
    /// Snapshot committed by this mutation.
    pub profile: BusinessProfile,
}

const PROFILE_SELECT: &str = "SELECT business_id,name,initialized_at,profile_revision,contact_email,help_url,logo_url,accent_color,profile_updated_at FROM oxsum.businesses WHERE singleton";

fn from_row(row: &PgRow, defaults: &PublicBusinessProfile) -> Result<BusinessProfile, WalletError> {
    let revision: i64 = row.try_get("profile_revision")?;
    let initialized: Option<OffsetDateTime> = row.try_get("initialized_at")?;
    let branding = if revision > 0 {
        PublicBusinessProfile {
            name: row.try_get("name")?,
            contact_email: row.try_get("contact_email")?,
            help_url: row.try_get("help_url")?,
            logo_url: row.try_get("logo_url")?,
            accent_color: row.try_get("accent_color")?,
        }
    } else {
        let mut branding = defaults.clone();
        if initialized.is_some() {
            branding.name = row.try_get("name")?;
        }
        branding
    };
    Ok(BusinessProfile {
        business_id: row.try_get("business_id")?,
        revision,
        source: if revision == 0 { "defaults" } else { "stored" }.into(),
        updated_at: row.try_get("profile_updated_at")?,
        branding,
    })
}

async fn profile_in(
    tx: &mut Transaction<'_, Postgres>,
    defaults: &PublicBusinessProfile,
) -> Result<BusinessProfile, WalletError> {
    let row = sqlx::query(PROFILE_SELECT).fetch_one(&mut **tx).await?;
    from_row(&row, defaults)
}

impl Db {
    /// Resolve public identity from the stored profile or unsaved defaults.
    pub async fn public_business_profile(
        &self,
        defaults: &PublicBusinessProfile,
    ) -> Result<PublicBusinessProfile, WalletError> {
        let row = sqlx::query(PROFILE_SELECT).fetch_one(self.pool()).await?;
        Ok(from_row(&row, defaults)?.branding)
    }

    /// Read the effective profile under current business overview authority.
    pub async fn business_profile(
        &self,
        token: &str,
        defaults: &PublicBusinessProfile,
    ) -> Result<BusinessProfile, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(&mut tx, business, token, BusinessCapability::OverviewRead)
            .await?;
        profile_in(&mut tx, defaults).await
    }

    /// Replace public identity atomically with current owner authority and audit.
    pub async fn set_business_profile(
        &self,
        token: &str,
        request: &BusinessProfileRequest,
    ) -> Result<BusinessProfileMutation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let branding = request.branding.validated()?;
        if !(0..9_007_199_254_740_991).contains(&request.expected_revision) {
            return Err(WalletError::InvalidInput(
                "expectedRevision is outside the supported range".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::ProfileWrite,
        )
        .await?;
        let action = "business.profile.update";
        let detail = serde_json::json!(request);
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            let result: serde_json::Value = sqlx::query_scalar(
                "SELECT result FROM oxsum.business_profile_revisions WHERE mutation_id=$1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            return serde_json::from_value(result).map_err(|_| {
                WalletError::Misconfigured("stored business profile receipt is invalid".into())
            });
        }
        let changed = sqlx::query("UPDATE oxsum.businesses SET name=$2,contact_email=$3,help_url=$4,logo_url=$5,accent_color=$6,profile_revision=profile_revision+1,profile_updated_at=now() WHERE business_id=$1 AND profile_revision=$7")
            .bind(business).bind(&branding.name).bind(&branding.contact_email).bind(&branding.help_url)
            .bind(&branding.logo_url).bind(&branding.accent_color).bind(request.expected_revision)
            .execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(WalletError::Conflict(
                "business profile changed; reload it before saving a revised draft".into(),
            ));
        }
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let profile = profile_in(&mut tx, &branding).await?;
        let result = BusinessProfileMutation {
            id: receipt.id,
            profile,
        };
        sqlx::query("INSERT INTO oxsum.business_profile_revisions(mutation_id,business_id,revision,result) VALUES($1,$2,$3,$4)")
            .bind(result.id).bind(business).bind(result.profile.revision).bind(serde_json::json!(result)).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }
}

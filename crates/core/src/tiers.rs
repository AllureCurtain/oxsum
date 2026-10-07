//! Organization tiers (roadmap P7-1, issue #158).
//!
//! A tier profile is a capability package — an organization-wide
//! requests-per-minute allowance and a model allowlist — that
//! `organizations.tier` assigns to an organization. Per docs/decisions.md a
//! tier is never a pricing input: the gateway enforces the package at
//! admission, and pricing reads nothing from it. An organization without a
//! tier is unconstrained, which is the behavior every organization had before.
//!
//! The profiles live in the shared `oxsum` schema, mutable rows — they carry
//! no money, so they need no history: what a turn's tier allowed is not part
//! of its bill, only whether it was admitted.

use serde::Serialize;
use sqlx::Row as _;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::keys::validate_model_allowlist;

/// The longest a tier name may be: a slug like `enterprise` or `partner-2026`.
const MAX_TIER: usize = 40;

/// A tier profile, as the admin surface reads it — with how many organizations
/// are assigned to it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TierProfile {
    /// The package's name; `organizations.tier` references it.
    pub name: String,
    /// One rolling-minute allowance shared by every key of the assigned
    /// organizations; `None` uncaps.
    pub requests_per_minute: Option<i32>,
    /// The models the tier may call; `None` is everything the deployment serves.
    pub model_allowlist: Option<Vec<String>>,
    /// How many organizations are assigned the tier.
    pub organizations: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The name a tier answers to: lowercase ASCII letters and digits separated by
/// single dashes — a slug, not a sentence.
fn validate_tier_name(name: &str) -> Result<(), WalletError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_TIER
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");
    if valid {
        Ok(())
    } else {
        Err(WalletError::InvalidInput(format!(
            "a tier name is a lowercase slug, 1..={MAX_TIER} bytes: {name:?}"
        )))
    }
}

/// One profile row out of the `SELECT`s below. `organizations` is the left-join
/// count the list computes; [`Db::tier_of`]'s read counts it too, so the row
/// builder is shared.
fn tier_from_row(row: &sqlx::postgres::PgRow) -> Result<TierProfile, WalletError> {
    Ok(TierProfile {
        name: row.try_get("name")?,
        requests_per_minute: row.try_get("requests_per_minute")?,
        model_allowlist: row
            .try_get::<Option<serde_json::Value>, _>("model_allowlist")?
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| WalletError::InvalidInput(format!("tier model_allowlist: {error}")))?,
        organizations: row.try_get("organizations")?,
        created_at: row.try_get("created_at")?,
    })
}

impl Db {
    /// Every tier profile, name first, with the count of organizations assigned
    /// to it — the admin list.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn tiers(&self) -> Result<Vec<TierProfile>, WalletError> {
        let rows = sqlx::query(
            "SELECT t.name, t.requests_per_minute, t.model_allowlist, t.created_at, \
                    count(o.organization_id) AS organizations \
             FROM oxsum.tier_profiles t \
             LEFT JOIN oxsum.organizations o ON o.tier = t.name \
             GROUP BY t.name ORDER BY t.name",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(tier_from_row).collect()
    }

    /// The tier an organization is assigned to, with its limits — what the
    /// gateway enforces at admission. `None` when it carries none.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn tier_of(&self, organization_id: Uuid) -> Result<Option<TierProfile>, WalletError> {
        let row = sqlx::query(
            "SELECT t.name, t.requests_per_minute, t.model_allowlist, t.created_at, \
                    (SELECT count(*) FROM oxsum.organizations m WHERE m.tier = t.name) AS organizations \
             FROM oxsum.tier_profiles t \
             JOIN oxsum.organizations o ON o.tier = t.name \
             WHERE o.organization_id = $1",
        )
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(tier_from_row).transpose()
    }

    /// Writes a tier profile — creates it, or replaces the whole package under
    /// the name. A `PUT` by name is naturally idempotent: replaying the same
    /// body writes the same row.
    ///
    /// # Errors
    ///
    /// A malformed name, a non-positive allowance or a malformed allowlist is
    /// [`WalletError::InvalidInput`]; storage failures surface as
    /// [`WalletError`].
    pub async fn set_tier(
        &self,
        name: &str,
        requests_per_minute: Option<i32>,
        model_allowlist: Option<Vec<String>>,
    ) -> Result<TierProfile, WalletError> {
        validate_tier_name(name)?;
        if let Some(rpm) = requests_per_minute
            && rpm <= 0
        {
            return Err(WalletError::InvalidInput(
                "requestsPerMinute must be at least 1".into(),
            ));
        }
        if let Some(models) = &model_allowlist {
            validate_model_allowlist(models)?;
        }
        let row = sqlx::query(
            "INSERT INTO oxsum.tier_profiles (name, requests_per_minute, model_allowlist) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (name) DO UPDATE \
             SET requests_per_minute = EXCLUDED.requests_per_minute, \
                 model_allowlist = EXCLUDED.model_allowlist \
             RETURNING name, requests_per_minute, model_allowlist, created_at, \
                 (SELECT count(*) FROM oxsum.organizations o WHERE o.tier = $1) AS organizations",
        )
        .bind(name)
        .bind(requests_per_minute)
        .bind(
            model_allowlist
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| WalletError::InvalidInput(format!("modelAllowlist: {error}")))?,
        )
        .fetch_one(self.pool())
        .await?;
        tier_from_row(&row)
    }

    /// Removes a tier profile. `None` means the name was never a tier; a tier
    /// still assigned to organizations refuses — reassign or clear them first,
    /// so a deleted package never silently uncaps its members.
    ///
    /// # Errors
    ///
    /// [`WalletError::Conflict`] when organizations still carry the tier;
    /// storage failures surface as [`WalletError`].
    pub async fn delete_tier(&self, name: &str) -> Result<Option<()>, WalletError> {
        let deleted = sqlx::query("DELETE FROM oxsum.tier_profiles WHERE name = $1")
            .bind(name)
            .execute(self.pool())
            .await
            .map_err(|error| {
                crate::keys::conflict_or_storage(
                    error,
                    "organizations_tier_fkey",
                    "organizations are still assigned to the tier",
                )
            })?;
        Ok((deleted.rows_affected() > 0).then_some(()))
    }

    /// Assigns or clears an organization's tier — `None` clears. `None` returned
    /// means no organization carries the id.
    ///
    /// # Errors
    ///
    /// A tier name no profile carries is [`WalletError::InvalidInput`] — the
    /// foreign key names it — and storage failures surface as [`WalletError`].
    pub async fn set_organization_tier(
        &self,
        organization_id: Uuid,
        tier: Option<&str>,
    ) -> Result<Option<()>, WalletError> {
        let updated =
            sqlx::query("UPDATE oxsum.organizations SET tier = $2 WHERE organization_id = $1")
                .bind(organization_id)
                .bind(tier)
                .execute(self.pool())
                .await
                .map_err(|error| {
                    let unknown = matches!(&error, sqlx::Error::Database(db)
                if db.constraint() == Some("organizations_tier_fkey"));
                    if unknown {
                        WalletError::InvalidInput("the tier does not exist".into())
                    } else {
                        WalletError::from(error)
                    }
                })?;
        Ok((updated.rows_affected() > 0).then_some(()))
    }
}

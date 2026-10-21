//! Share links for end-user usage reports (issue #300).
//!
//! A downstream customer holds no account, so the link is the capability: the
//! organization mints one ([`Db::create_report_link`] or
//! [`Db::create_end_user_link`]), hands the plaintext to the customer once, and
//! can revoke it ([`Db::revoke_report_link`]). [`Db::resolve_report_link`] turns a
//! presented token into the organization, tenant, scope and target the bill page
//! then serves — expired, revoked and unknown tokens all answer `None`, so the
//! page can return one indistinguishable 404.
//!
//! The table keeps only the token's SHA-256, for the same reason `api_keys` keeps
//! key hashes: a dump of `oxsum.report_links` opens nothing.

use std::fmt::Write;

use rand::Rng;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// Every link token starts with this, so the value is recognizable in logs.
const TOKEN_MARK: &str = "oxb-";
const TOKEN_BYTES: usize = 32;
/// A link's default life when the caller names no expiry.
const DEFAULT_LIFETIME: Duration = Duration::days(30);
/// The longest expiry a caller may ask for: a bill link is not a standing grant.
const MAX_LIFETIME: Duration = Duration::days(90);
/// The longest label, matching the report endpoints' `endUser` bound.
const MAX_LABEL: usize = 128;
/// The longest `endUser` a link may name, matching the mint endpoint.
const MAX_END_USER: usize = 128;

/// What a link resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkScope {
    /// One minted usage report — a customer's bill for one span.
    Report,
    /// One downstream customer: the reports the organization has issued them.
    EndUser,
}

impl LinkScope {
    /// The spelling the table stores.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::EndUser => "end_user",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "report" => Some(Self::Report),
            "end_user" => Some(Self::EndUser),
            _ => None,
        }
    }
}

/// A link as the organization's API lists it — never the token.
#[derive(Debug, Clone)]
pub struct ReportLink {
    /// The link's own id, used to revoke it.
    pub link_id: Uuid,
    /// The organization that minted it.
    pub organization_id: Uuid,
    /// What it points at.
    pub scope: LinkScope,
    /// The report id for `report` scope; `None` for `end_user`.
    pub report_id: Option<Uuid>,
    /// The downstream customer the link is for.
    pub end_user: String,
    /// The organization's own label for the recipient or purpose.
    pub label: Option<String>,
    /// When it was minted.
    pub created_at: OffsetDateTime,
    /// When it stops resolving.
    pub expires_at: OffsetDateTime,
    /// When the organization revoked it, if it did.
    pub revoked_at: Option<OffsetDateTime>,
    /// When it last resolved — the operator's "the customer opened the bill" signal.
    pub last_used_at: Option<OffsetDateTime>,
}

/// A freshly minted link: the only time the token is visible.
#[derive(Debug, Clone)]
pub struct MintedReportLink {
    /// The link's descriptor.
    pub link: ReportLink,
    /// The plaintext token, answered once and never stored.
    pub token: String,
}

/// What a presented token resolves to — enough for the bill page to load the
/// bundle without a session.
#[derive(Debug, Clone)]
pub struct ResolvedReportLink {
    /// The organization the report belongs to.
    pub organization_id: Uuid,
    /// Its ledger tenant, so the page can open the wallet.
    pub tenant_id: String,
    /// What the link points at.
    pub scope: LinkScope,
    /// The report for `report` scope.
    pub report_id: Option<Uuid>,
    /// The downstream customer.
    pub end_user: String,
}

/// The shared shape of both mints — one struct so the insert keeps a readable
/// signature instead of eight positional arguments.
struct NewLink<'a> {
    organization_id: Uuid,
    scope: LinkScope,
    report_id: Option<Uuid>,
    end_user: &'a str,
    label: Option<&'a str>,
    expires_at: Option<OffsetDateTime>,
    created_by: Option<Uuid>,
}

impl Db {
    /// Mints a link to one minted usage report. The report must belong to the
    /// organization; its `endUser` is copied onto the link so the customer page
    /// can name the bill without reading the org's other rows.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for a report the organization does not own;
    /// [`WalletError::InvalidInput`] for a label or expiry outside the bounds.
    pub async fn create_report_link(
        &self,
        organization_id: Uuid,
        report_id: Uuid,
        label: Option<&str>,
        expires_at: Option<OffsetDateTime>,
        created_by: Option<Uuid>,
    ) -> Result<MintedReportLink, WalletError> {
        let end_user: Option<String> = sqlx::query_scalar(
            "SELECT end_user FROM oxsum.usage_reports \
             WHERE report_id = $1 AND organization_id = $2",
        )
        .bind(report_id)
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(end_user) = end_user else {
            return Err(WalletError::NotFound(
                "the report or organization does not exist".into(),
            ));
        };
        self.insert_report_link(NewLink {
            organization_id,
            scope: LinkScope::Report,
            report_id: Some(report_id),
            end_user: &end_user,
            label,
            expires_at,
            created_by,
        })
        .await
    }

    /// Mints a link to one downstream customer: the page lists every report the
    /// organization has issued them. The customer must have usage in the
    /// organization's ledger — a link to a name nothing was billed to would
    /// open an empty page and read as a broken bill.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] for an organization that does not exist;
    /// [`WalletError::InvalidInput`] for an empty/oversized `endUser`, an
    /// unknown customer, or a label/expiry outside the bounds.
    pub async fn create_end_user_link(
        &self,
        organization_id: Uuid,
        end_user: &str,
        label: Option<&str>,
        expires_at: Option<OffsetDateTime>,
        created_by: Option<Uuid>,
    ) -> Result<MintedReportLink, WalletError> {
        let end_user = end_user.trim();
        if end_user.is_empty() || end_user.chars().count() > MAX_END_USER {
            return Err(WalletError::InvalidInput(
                "endUser must be between 1 and 128 characters".into(),
            ));
        }
        let tenant_id: Option<String> = sqlx::query_scalar(
            "SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1",
        )
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(tenant_id) = tenant_id else {
            return Err(WalletError::NotFound(
                "the report or organization does not exist".into(),
            ));
        };
        let has_usage: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM oxsum.usage_records \
             WHERE tenant_id = $1 AND end_user = $2)",
        )
        .bind(&tenant_id)
        .bind(end_user)
        .fetch_one(self.pool())
        .await?;
        if !has_usage {
            return Err(WalletError::InvalidInput(
                "the organization has no usage attributed to this endUser".into(),
            ));
        }
        self.insert_report_link(NewLink {
            organization_id,
            scope: LinkScope::EndUser,
            report_id: None,
            end_user,
            label,
            expires_at,
            created_by,
        })
        .await
    }

    /// The organization's links, newest first — descriptors only, never tokens.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn report_links_of(
        &self,
        organization_id: Uuid,
    ) -> Result<Vec<ReportLink>, WalletError> {
        let rows = sqlx::query(
            "SELECT link_id, organization_id, scope, report_id, end_user, label, \
                    created_at, expires_at, revoked_at, last_used_at \
             FROM oxsum.report_links WHERE organization_id = $1 ORDER BY created_at DESC",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(link_from_row).collect()
    }

    /// Revokes one of the organization's links and answers its descriptor. An
    /// already-revoked link is idempotent (it answers the same row); an id of
    /// another organization — or an unknown one — is `None`, indistinguishable.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn revoke_report_link(
        &self,
        organization_id: Uuid,
        link_id: Uuid,
    ) -> Result<Option<ReportLink>, WalletError> {
        let row = sqlx::query(
            "UPDATE oxsum.report_links SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE link_id = $1 AND organization_id = $2 \
             RETURNING link_id, organization_id, scope, report_id, end_user, label, \
                       created_at, expires_at, revoked_at, last_used_at",
        )
        .bind(link_id)
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(link_from_row).transpose()
    }

    /// Resolves a presented token, touching `last_used_at`. Expired, revoked and
    /// unknown tokens all answer `None` — the caller cannot tell them apart.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn resolve_report_link(
        &self,
        token: &str,
    ) -> Result<Option<ResolvedReportLink>, WalletError> {
        let row = sqlx::query(
            "UPDATE oxsum.report_links AS l SET last_used_at = now() \
             FROM oxsum.organizations AS o \
             WHERE l.token_hash = $1 AND l.organization_id = o.organization_id \
               AND l.revoked_at IS NULL AND l.expires_at > now() \
             RETURNING o.tenant_id, l.organization_id, l.scope, l.report_id, l.end_user",
        )
        .bind(hash_token(token).as_slice())
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let scope: String = row.try_get("scope")?;
        let Some(scope) = LinkScope::parse(&scope) else {
            return Ok(None);
        };
        Ok(Some(ResolvedReportLink {
            organization_id: row.try_get("organization_id")?,
            tenant_id: row.try_get("tenant_id")?,
            scope,
            report_id: row.try_get("report_id")?,
            end_user: row.try_get("end_user")?,
        }))
    }

    /// The shared insert behind the two public mints: bounds, token and row.
    async fn insert_report_link(&self, new: NewLink<'_>) -> Result<MintedReportLink, WalletError> {
        let NewLink {
            organization_id,
            scope,
            report_id,
            end_user,
            label,
            expires_at,
            created_by,
        } = new;
        let label = label.map(str::trim).filter(|text| !text.is_empty());
        if let Some(label) = label
            && label.chars().count() > MAX_LABEL
        {
            return Err(WalletError::InvalidInput(
                "label must be at most 128 characters".into(),
            ));
        }
        let now = OffsetDateTime::now_utc();
        let expires_at = expires_at.unwrap_or(now + DEFAULT_LIFETIME);
        if expires_at <= now {
            return Err(WalletError::InvalidInput(
                "expiresAt must be in the future".into(),
            ));
        }
        if expires_at > now + MAX_LIFETIME {
            return Err(WalletError::InvalidInput(
                "expiresAt may not be more than 90 days out".into(),
            ));
        }

        let link_id = Uuid::new_v4();
        let token = generate_token();
        let created_at: OffsetDateTime = sqlx::query_scalar(
            "INSERT INTO oxsum.report_links \
                 (link_id, organization_id, scope, report_id, end_user, label, \
                  token_hash, created_by, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             RETURNING created_at",
        )
        .bind(link_id)
        .bind(organization_id)
        .bind(scope.as_str())
        .bind(report_id)
        .bind(end_user)
        .bind(label)
        .bind(hash_token(&token).as_slice())
        .bind(created_by)
        .bind(expires_at)
        .fetch_one(self.pool())
        .await?;

        Ok(MintedReportLink {
            link: ReportLink {
                link_id,
                organization_id,
                scope,
                report_id,
                end_user: end_user.to_owned(),
                label: label.map(str::to_owned),
                created_at,
                expires_at,
                revoked_at: None,
                last_used_at: None,
            },
            token,
        })
    }
}

/// One `report_links` row as the descriptor the API answers — shared by the list
/// and the revoke, which return the same shape.
fn link_from_row(row: &sqlx::postgres::PgRow) -> Result<ReportLink, WalletError> {
    let scope: String = row.try_get("scope")?;
    let Some(scope) = LinkScope::parse(&scope) else {
        return Err(WalletError::Misconfigured(
            "report_links.scope holds an unknown value".into(),
        ));
    };
    Ok(ReportLink {
        link_id: row.try_get("link_id")?,
        organization_id: row.try_get("organization_id")?,
        scope,
        report_id: row.try_get("report_id")?,
        end_user: row.try_get("end_user")?,
        label: row.try_get("label")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        revoked_at: row.try_get("revoked_at")?,
        last_used_at: row.try_get("last_used_at")?,
    })
}

/// A new link token: the mark plus 32 random bytes from the operating system's
/// CSPRNG, hex-encoded like an API key secret.
fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(TOKEN_MARK.len() + TOKEN_BYTES * 2);
    token.push_str(TOKEN_MARK);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(token, "{byte:02x}");
    }
    token
}

/// SHA-256 of the token, the only form the database ever holds.
fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

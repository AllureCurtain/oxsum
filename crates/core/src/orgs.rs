//! Organizations and memberships.
//!
//! A tenant in oxsum is an organization: its balance, its ledger, its API keys. Every user
//! gets a personal organization at signup, so the common case is "an organization with one
//! member" and the pages never have to surface the organization layer. See docs/decisions.md,
//! "a tenant is an organization".

use serde::Serialize;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::WalletError;

/// Whether an organization is one person's or a team's. product.md: a personal
/// organization flips to `team` when its first member joins; the ledger does not move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Personal,
    Team,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Team => "team",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, WalletError> {
        match raw {
            "personal" => Ok(Self::Personal),
            "team" => Ok(Self::Team),
            other => Err(malformed("organizations.kind", other)),
        }
    }
}

/// A member's role in an organization. product.md's platform admin is a deployment-level
/// identity on the user, not a role here.
///
/// Stored from the first membership so product.md's rules ("members manage their own keys,
/// admins manage all") need no migration when sessions make a user the acting principal.
/// Nothing enforces a role yet: an API key authenticates an organization, not a person, and
/// enforcement arrives with web login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

/// An organization as the API presents it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    /// The ledger's tenant id: `ledger_<tenant_id>` is this organization's schema.
    pub tenant_id: String,
    pub kind: Kind,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The tenant id of a fresh organization.
///
/// The organization's UUID without dashes: 32 lowercase hex characters, which the ledger's
/// tenant-id rule already accepts (`[a-z0-9_]{1,40}`, see `Wallet::open`), with no slug
/// collisions to resolve and nothing for a caller to choose or probe.
pub(crate) fn tenant_id_for(id: Uuid) -> String {
    id.simple().to_string()
}

/// Inserts the organization row. Runs on the caller's transaction: signup writes the user,
/// the organization, the owner membership and the first key together or not at all.
pub(crate) async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    name: &str,
    kind: Kind,
) -> Result<Organization, WalletError> {
    let tenant_id = tenant_id_for(id);
    let created_at: OffsetDateTime = sqlx::query_scalar(
        "INSERT INTO oxsum.organizations (organization_id, name, tenant_id, kind) \
         VALUES ($1, $2, $3, $4) RETURNING created_at",
    )
    .bind(id)
    .bind(name)
    .bind(&tenant_id)
    .bind(kind.as_str())
    .fetch_one(&mut **tx)
    .await?;
    Ok(Organization {
        id,
        name: name.to_owned(),
        tenant_id,
        kind,
        created_at,
    })
}

/// Inserts a membership.
pub(crate) async fn add_member(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
    user_id: Uuid,
    role: Role,
) -> Result<(), WalletError> {
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role) VALUES ($1, $2, $3)",
    )
    .bind(organization_id)
    .bind(user_id)
    .bind(role.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Maps a row of the `organizations` projection to [`Organization`].
pub(crate) fn organization_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<Organization, WalletError> {
    use sqlx::Row;
    Ok(Organization {
        id: row.try_get("organization_id")?,
        name: row.try_get("name")?,
        tenant_id: row.try_get("tenant_id")?,
        kind: Kind::parse(&row.try_get::<String, _>("kind")?)?,
        created_at: row.try_get("created_at")?,
    })
}

/// A value in the database that this build does not understand: a schema/version mismatch,
/// not a caller error, so it surfaces as a storage failure and stays out of responses.
pub(crate) fn malformed(column: &str, value: &str) -> WalletError {
    WalletError::Storage(doubleentry::storage::postgres::PostgresError::Malformed(
        format!("{column} holds an unknown value: {value:?}"),
    ))
}

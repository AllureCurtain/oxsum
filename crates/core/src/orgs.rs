//! Organizations and memberships.
//!
//! A tenant in oxsum is an organization: its balance, its ledger, its API keys. Every user
//! gets a personal organization at signup, so the common case is "an organization with one
//! member" and the pages never have to surface the organization layer. See docs/decisions.md,
//! "a tenant is an organization".
//!
//! Membership itself is managed here, and it is a *person's* action: [`MembershipActor`] is
//! built from a session's role, and an API key never has one. The rules — only an owner or an
//! admin, an admin may not touch an owner, the last owner is protected, ownership moves only
//! through [`Db::transfer_ownership`] — live in this module, so the REST endpoints and the
//! dashboard's server functions refuse exactly the same calls. See docs/decisions.md,
//! "membership management is a person's action".

use serde::Serialize;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::keys::conflict_or_storage;

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
/// Roles constrain sessions, not API keys: a key acts as the organization, while a user
/// acts with the authority their role grants them (see [`crate::Principal::key_scope`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
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

    pub(crate) fn parse(raw: &str) -> Result<Self, WalletError> {
        match raw {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            other => Err(malformed("memberships.role", other)),
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

/// An organization as the platform admin lists it: identity, kind, headcount, and the
/// tenant id its ledger answers to. The tenant id is internal — it is how the wallet
/// is opened, not something an answer reports.
#[derive(Debug, Clone)]
pub struct AdminOrganization {
    pub id: Uuid,
    pub name: String,
    pub tenant_id: String,
    pub kind: Kind,
    /// How many people belong: a personal organization is normally one, a team's more.
    pub members: i64,
    pub created_at: OffsetDateTime,
}

/// One membership as the dashboard lists it: who holds it, in which role, since when.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub user_id: Uuid,
    pub email: String,
    pub role: Role,
    #[serde(with = "time::serde::rfc3339")]
    pub joined_at: OffsetDateTime,
}

/// An organization the user belongs to, with their role in it: the organization
/// switcher's list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserOrganization {
    pub organization: Organization,
    pub role: Role,
}

/// The ownership after a transfer: the new owner, and the owner who transferred it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ownership {
    pub owner: Member,
    /// The transferring owner, in their new role (an admin).
    pub previous_owner: Member,
}

/// Who a membership operation acts for: the person, and the role the session carries.
///
/// Built by the credential layer — [`crate::Principal::membership_actor`] on the REST surface,
/// the session principal on the pages — so both surfaces hand the same thing to [`Db`], and
/// the rules below are the only place they are written down. There is deliberately no variant
/// for an API key: a key is not a person and names no role.
#[derive(Debug, Clone, Copy)]
pub struct MembershipActor {
    pub user_id: Uuid,
    pub role: Role,
}

impl MembershipActor {
    /// Refuses anyone but an owner or an admin. The first check of every membership write,
    /// so a member cannot reach one however the endpoint is written.
    pub(crate) fn authorized(self) -> Result<Self, WalletError> {
        match self.role {
            Role::Owner | Role::Admin => Ok(self),
            Role::Member => Err(WalletError::Forbidden(
                "only an owner or an admin may manage members".into(),
            )),
        }
    }
}

/// The role a member may be changed to. Ownership is not one of them: it is a single seat,
/// moved by [`Db::transfer_ownership`], so no role change can promote anyone.
fn assignable(role: Role) -> Result<Role, WalletError> {
    match role {
        Role::Owner => Err(WalletError::InvalidInput(
            "ownership is transferred, not assigned: an organization has exactly one owner".into(),
        )),
        Role::Admin | Role::Member => Ok(role),
    }
}

impl Db {
    /// Every organization, oldest first, with its headcount: what the platform admin's
    /// organizations page lists. Balances are read per organization through the wallet
    /// by the caller — this is the identity list, not the money.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn organizations(&self) -> Result<Vec<AdminOrganization>, WalletError> {
        let rows = sqlx::query(
            "SELECT o.organization_id, o.name, o.tenant_id, o.kind, o.created_at, \
             count(m.user_id) AS members \
             FROM oxsum.organizations o LEFT JOIN oxsum.memberships m USING (organization_id) \
             GROUP BY o.organization_id ORDER BY o.created_at",
        )
        .fetch_all(self.pool())
        .await?;
        let mut organizations = Vec::with_capacity(rows.len());
        for row in &rows {
            organizations.push(AdminOrganization {
                id: row.try_get("organization_id")?,
                name: row.try_get("name")?,
                tenant_id: row.try_get("tenant_id")?,
                kind: Kind::parse(&row.try_get::<String, _>("kind")?)?,
                members: row.try_get("members")?,
                created_at: row.try_get("created_at")?,
            });
        }
        Ok(organizations)
    }

    /// One organization by id, for the platform admin — `NOT_FOUND` whether the id
    /// never existed or was never an organization.
    ///
    /// # Errors
    ///
    /// [`WalletError::NotFound`] when no organization carries the id; storage
    /// failures surface as [`WalletError`].
    pub async fn organization_by_id(&self, id: Uuid) -> Result<AdminOrganization, WalletError> {
        let row = sqlx::query(
            "SELECT o.organization_id, o.name, o.tenant_id, o.kind, o.created_at,              count(m.user_id) AS members              FROM oxsum.organizations o LEFT JOIN oxsum.memberships m USING (organization_id)              WHERE o.organization_id = $1 GROUP BY o.organization_id",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| WalletError::NotFound("the organization is not known".into()))?;
        Ok(AdminOrganization {
            id: row.try_get("organization_id")?,
            name: row.try_get("name")?,
            tenant_id: row.try_get("tenant_id")?,
            kind: Kind::parse(&row.try_get::<String, _>("kind")?)?,
            members: row.try_get("members")?,
            created_at: row.try_get("created_at")?,
        })
    }

    /// Every organization `user_id` belongs to, oldest membership first, with the
    /// role they hold — the list the organization switcher offers.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn organizations_of(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<UserOrganization>, WalletError> {
        let rows = sqlx::query(
            "SELECT o.organization_id, o.name, o.tenant_id, o.kind, o.created_at, m.role              FROM oxsum.memberships m JOIN oxsum.organizations o USING (organization_id)              WHERE m.user_id = $1 ORDER BY m.created_at, m.organization_id",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        let mut organizations = Vec::with_capacity(rows.len());
        for row in &rows {
            organizations.push(UserOrganization {
                organization: organization_from_row(row)?,
                role: Role::parse(&row.try_get::<String, _>("role")?)?,
            });
        }
        Ok(organizations)
    }

    /// Creates a `team` organization — its own tenant id, its own ledger — and makes
    /// `user_id` its owner, in one transaction.
    ///
    /// The kind is fixed: a `personal` organization is exactly what signup makes, so
    /// no caller chooses it. The same name rule signup applies holds here.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for an empty or overlong name; storage failures
    /// surface as [`WalletError`].
    pub async fn create_team_organization(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> Result<Organization, WalletError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(WalletError::InvalidInput("name must not be empty".into()));
        }
        if name.chars().count() > 80 {
            return Err(WalletError::InvalidInput(
                "name must be at most 80 characters".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let organization = insert(&mut tx, Uuid::new_v4(), name, Kind::Team).await?;
        add_member(&mut tx, organization.id, user_id, Role::Owner).await?;
        tx.commit().await?;
        Ok(organization)
    }

    /// The members of an organization, oldest first: what the dashboard's members page
    /// lists. Invitations do not exist yet, so today this is everyone the organization
    /// has.
    ///
    /// Readable by any member of the organization: reading who is in it is not a management
    /// action (docs/decisions.md, issue #61). The caller arrives here through a session
    /// resolved against `memberships`, so an organization's list is only ever read by
    /// someone in it.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn members(&self, organization_id: Uuid) -> Result<Vec<Member>, WalletError> {
        let rows = sqlx::query(
            "SELECT m.user_id, u.email, m.role, m.created_at \
             FROM oxsum.memberships m JOIN oxsum.users u USING (user_id) \
             WHERE m.organization_id = $1 ORDER BY m.created_at",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        let mut members = Vec::with_capacity(rows.len());
        for row in &rows {
            members.push(member_from_row(row)?);
        }
        Ok(members)
    }

    /// Adds an existing account, named by its email, to the organization as a member.
    ///
    /// "Invite" means exactly this: the account must already exist. Pending invitations for
    /// people who have none — invite links, tokens, email — are issue #59, so an email no
    /// account holds is [`WalletError::NotFound`] and no invitation is stored. The new role is
    /// always [`Role::Member`]: this call cannot grant a role, so it cannot grant ownership.
    ///
    /// Adding the second person to a `personal` organization flips its kind to `team`
    /// (`oxsum.organizations.kind`), which is what the migration comment promises. The flip is
    /// one-way: the ledger does not move, and removing a member does not move it back.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for a member; [`WalletError::InvalidInput`] for an email that
    /// is not one; [`WalletError::NotFound`] for an unknown account; [`WalletError::Conflict`]
    /// when the account is already a member.
    pub async fn add_member(
        &self,
        organization_id: Uuid,
        acting: MembershipActor,
        email: &str,
    ) -> Result<Member, WalletError> {
        acting.authorized()?;
        let email = crate::users::validate_email(email)?;
        let normalized = email.to_lowercase();
        let mut tx = self.pool().begin().await?;
        lock_organization(&mut tx, organization_id).await?;
        let row = sqlx::query("SELECT user_id, email FROM oxsum.users WHERE email_normalized = $1")
            .bind(&normalized)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            return Err(WalletError::NotFound(format!(
                "no account with the email {email}"
            )));
        };
        let user_id: Uuid = row.try_get("user_id")?;
        let stored_email: String = row.try_get("email")?;
        let joined_at = add_member(&mut tx, organization_id, user_id, Role::Member).await?;
        sqlx::query(
            "UPDATE oxsum.organizations SET kind = 'team' \
             WHERE organization_id = $1 AND kind = 'personal' \
               AND (SELECT count(*) FROM oxsum.memberships WHERE organization_id = $1) > 1",
        )
        .bind(organization_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Member {
            user_id,
            email: stored_email,
            role: Role::Member,
            joined_at,
        })
    }

    /// Removes a membership. The user and their own organization are untouched.
    ///
    /// An admin may not remove an owner, and no one may remove the last owner: an
    /// organization always has an owner, so ownership has to be transferred first. Removing
    /// a user who is not a member of this organization is [`WalletError::NotFound`], never a
    /// silent success.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for a member, and for an admin naming an owner;
    /// [`WalletError::NotFound`] for a user who is not a member; [`WalletError::Conflict`] for
    /// the organization's last owner.
    pub async fn remove_member(
        &self,
        organization_id: Uuid,
        acting: MembershipActor,
        user_id: Uuid,
    ) -> Result<Member, WalletError> {
        let acting = acting.authorized()?;
        let mut tx = self.pool().begin().await?;
        lock_organization(&mut tx, organization_id).await?;
        let target = locked_member(&mut tx, organization_id, user_id)
            .await?
            .ok_or_else(not_a_member)?;
        if target.role == Role::Owner {
            if acting.role == Role::Admin {
                return Err(admin_on_owner("remove"));
            }
            if owner_count(&mut tx, organization_id).await? <= 1 {
                return Err(last_owner("removed"));
            }
        }
        sqlx::query("DELETE FROM oxsum.memberships WHERE organization_id = $1 AND user_id = $2")
            .bind(organization_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(target)
    }

    /// Changes a member's role. `Owner` is refused: ownership is transferred, not assigned.
    ///
    /// An admin may not change an owner's role, and the last owner cannot be demoted. Setting
    /// the role a member already has answers the membership unchanged, so a retry is not an
    /// error.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for a member, and for an admin naming an owner;
    /// [`WalletError::InvalidInput`] when the role is `owner`; [`WalletError::NotFound`] for a
    /// user who is not a member; [`WalletError::Conflict`] for the organization's last owner.
    pub async fn change_member_role(
        &self,
        organization_id: Uuid,
        acting: MembershipActor,
        user_id: Uuid,
        role: Role,
    ) -> Result<Member, WalletError> {
        let acting = acting.authorized()?;
        let role = assignable(role)?;
        let mut tx = self.pool().begin().await?;
        lock_organization(&mut tx, organization_id).await?;
        let mut target = locked_member(&mut tx, organization_id, user_id)
            .await?
            .ok_or_else(not_a_member)?;
        if target.role == Role::Owner {
            if acting.role == Role::Admin {
                return Err(admin_on_owner("change the role of"));
            }
            if owner_count(&mut tx, organization_id).await? <= 1 {
                return Err(last_owner("demoted"));
            }
        }
        if target.role != role {
            sqlx::query(
                "UPDATE oxsum.memberships SET role = $3 \
                 WHERE organization_id = $1 AND user_id = $2",
            )
            .bind(organization_id)
            .bind(user_id)
            .bind(role.as_str())
            .execute(&mut *tx)
            .await?;
            target.role = role;
        }
        tx.commit().await?;
        Ok(target)
    }

    /// Transfers ownership: the named member becomes the owner and the acting owner becomes
    /// an admin, in one transaction, so the organization has exactly one owner before and
    /// after.
    ///
    /// Only an owner may call this, so the owner seat is never moved by an admin or a member.
    /// The target must be a member of the same organization, and cannot already be the owner:
    /// this moves the seat rather than creating a second one.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] for an admin or a member; [`WalletError::NotFound`] for a
    /// user who is not a member of this organization; [`WalletError::Conflict`] when the
    /// target already owns the organization.
    pub async fn transfer_ownership(
        &self,
        organization_id: Uuid,
        acting: MembershipActor,
        user_id: Uuid,
    ) -> Result<Ownership, WalletError> {
        let acting = acting.authorized()?;
        if acting.role != Role::Owner {
            return Err(WalletError::Forbidden(
                "only an owner may transfer ownership".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        lock_organization(&mut tx, organization_id).await?;
        let owner = locked_member(&mut tx, organization_id, user_id)
            .await?
            .ok_or_else(not_a_member)?;
        if owner.role == Role::Owner {
            return Err(WalletError::Conflict(
                "that member already owns this organization".into(),
            ));
        }
        // The acting owner's own row, read under the lock: the membership this session names
        // is the authority for the transfer, and the response states what it became.
        let mut previous_owner = locked_member(&mut tx, organization_id, acting.user_id)
            .await?
            .ok_or_else(not_a_member)?;
        if previous_owner.role != Role::Owner {
            return Err(WalletError::Forbidden(
                "only an owner may transfer ownership".into(),
            ));
        }
        sqlx::query(
            "UPDATE oxsum.memberships SET role = $3 WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
        .bind(user_id)
        .bind(Role::Owner.as_str())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.memberships SET role = $3 WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
        .bind(acting.user_id)
        .bind(Role::Admin.as_str())
        .execute(&mut *tx)
        .await?;
        previous_owner.role = Role::Admin;
        tx.commit().await?;
        Ok(Ownership {
            owner: Member {
                role: Role::Owner,
                ..owner
            },
            previous_owner,
        })
    }
}

/// Serializes every membership write of one organization.
///
/// The rules count owners and change two rows together, so they need one writer at a time:
/// the organization row is the lock, taken for the whole transaction. A concurrent transfer
/// and removal therefore cannot both see the same owner count and both act on it.
async fn lock_organization(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
) -> Result<(), WalletError> {
    sqlx::query(
        "SELECT organization_id FROM oxsum.organizations WHERE organization_id = $1 FOR UPDATE",
    )
    .bind(organization_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| WalletError::NotFound("no such organization".into()))?;
    Ok(())
}

/// The owners of an organization: what makes "the last owner" decidable.
async fn owner_count(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
) -> Result<i64, WalletError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.memberships \
         WHERE organization_id = $1 AND role = 'owner'",
    )
    .bind(organization_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(count)
}

/// One membership, read on the caller's transaction with its row locked for update: the
/// decision a role change or a removal makes is made on this row.
async fn locked_member(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
    user_id: Uuid,
) -> Result<Option<Member>, WalletError> {
    let row = sqlx::query(
        "SELECT m.user_id, u.email, m.role, m.created_at \
         FROM oxsum.memberships m JOIN oxsum.users u USING (user_id) \
         WHERE m.organization_id = $1 AND m.user_id = $2 FOR UPDATE OF m",
    )
    .bind(organization_id)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(member_from_row).transpose()
}

/// Maps a row of the members projection to [`Member`].
fn member_from_row(row: &sqlx::postgres::PgRow) -> Result<Member, WalletError> {
    Ok(Member {
        user_id: row.try_get("user_id")?,
        email: row.try_get("email")?,
        role: Role::parse(&row.try_get::<String, _>("role")?)?,
        joined_at: row.try_get("created_at")?,
    })
}

fn not_a_member() -> WalletError {
    WalletError::NotFound("that user is not a member of this organization".into())
}

fn admin_on_owner(verb: &str) -> WalletError {
    WalletError::Forbidden(format!("an admin may not {verb} an owner"))
}

fn last_owner(verb: &str) -> WalletError {
    WalletError::Conflict(format!(
        "the last owner of an organization cannot be {verb}: transfer ownership first"
    ))
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

/// Inserts a membership and returns when it was created.
///
/// The one insert path, shared by signup (which ignores the timestamp) and by
/// [`Db::add_member`] (which answers with it). A duplicate membership is a conflict rather
/// than a storage failure, because the only way to reach it is naming an account that is
/// already in the organization.
pub(crate) async fn add_member(
    tx: &mut Transaction<'_, Postgres>,
    organization_id: Uuid,
    user_id: Uuid,
    role: Role,
) -> Result<OffsetDateTime, WalletError> {
    let created_at = sqlx::query_scalar(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role) VALUES ($1, $2, $3) \
         RETURNING created_at",
    )
    .bind(organization_id)
    .bind(user_id)
    .bind(role.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| {
        conflict_or_storage(
            e,
            "memberships_pkey",
            "that account is already a member of this organization",
        )
    })?;
    Ok(created_at)
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

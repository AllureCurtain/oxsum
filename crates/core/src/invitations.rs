//! Customer and team invitations require explicit recipient consent. Tokens are
//! hashed, single-use and expiring; creation/revocation/acceptance commit with audit.
use std::fmt::Write;

use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::orgs::{self, Kind, Role};
use crate::users::{self, NewUser, Registration, User};
use crate::{
    BusinessCapability, BusinessMutationResult, BusinessRole, Db, WalletError,
    business_staff as authority, keys,
};

/// A newly created invitation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedInvitation {
    /// Invitation identifier.
    pub id: Uuid,
    /// Only the first creation returns the secret. A retry cannot recover it.
    pub token: Option<String>,
    /// Organization the invitation admits the recipient to.
    pub organization_id: Uuid,
    /// When the invitation stops being accepted.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}
/// Request to invite someone into the caller's customer organization.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamInvitationRequest {
    /// Caller-supplied key that makes repeated requests return the same invitation.
    pub idempotency_key: String,
    /// Email the invitation is bound to; required for a single recipient.
    pub intended_email: Option<String>,
    /// Also requests email delivery at the HTTP boundary.
    pub email: Option<String>,
}
/// Request to invite a staff member into the business by email.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StaffInvitationRequest {
    /// Caller-supplied key that makes repeated requests return the same invitation.
    pub idempotency_key: String,
    /// Email the invitation is bound to.
    pub email: String,
    /// Staff role to grant on acceptance; `owner` is never offered.
    pub role: BusinessRole,
    /// Requests one SMTP submission to the bound email at the HTTP boundary.
    #[serde(default)]
    pub send_email: bool,
    /// Operator-visible justification recorded in the audit log.
    pub reason: String,
}
/// Request to invite a new customer identity into the business.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomerInvitationRequest {
    /// Caller-supplied key that makes repeated requests return the same invitation.
    pub idempotency_key: String,
    /// Email the invitation is bound to.
    pub email: String,
    /// Display name for the new customer, 1 to 80 characters.
    pub name: String,
    /// Caller-chosen customer identifier, 1 to 200 characters and unique in the business.
    pub external_id: String,
    /// Request one SMTP submission to the bound recipient at the HTTP boundary.
    #[serde(default, skip_serializing_if = "is_false")]
    pub send_email: bool,
    /// Operator-visible justification recorded in the audit log.
    pub reason: String,
}
/// Which invitation surface a revoke or list acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvitationScope {
    /// A customer invitation, owned by `CustomersWrite`.
    Customer,
    /// A staff invitation, owned by `StaffManage`.
    Staff,
    /// The caller's own customer-organization team link.
    Team,
}
/// Request to revoke a pending invitation.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvitationRevokeRequest {
    /// Caller-supplied key that makes repeated revocations idempotent.
    pub idempotency_key: String,
    /// Operator-visible justification recorded in the audit log.
    pub reason: String,
}
/// Request to accept an invitation with the current session.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvitationAcceptRequest {
    /// One-time invitation secret.
    pub token: String,
}
/// Receipt for an accepted invitation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationAcceptance {
    /// Invitation identifier.
    pub id: Uuid,
    /// Organization the recipient joined; the nil UUID for a staff invitation,
    /// which admits to the business, not to a customer organization.
    pub organization_id: Uuid,
    /// Historical accepted role; this receipt never grants current authority.
    pub role: Role,
    /// Business staff role a `staff` invitation granted; absent otherwise.
    pub staff_role: Option<BusinessRole>,
}

/// Public details shown before an invitation is accepted.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationPreview {
    /// Organization the invitation admits the recipient to.
    pub organization_id: Uuid,
    /// Name of the organization or pending customer.
    pub name: String,
    /// Role the recipient will hold after accepting.
    pub role: Role,
    /// Business staff role a `staff` invitation grants; absent for team/customer.
    pub staff_role: Option<BusinessRole>,
    /// When the invitation stops being accepted.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

fn is_false(value: &bool) -> bool {
    !*value
}
fn invalid() -> WalletError {
    WalletError::NotFound("the invitation is not valid".into())
}
fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
/// The stored form of an invitation secret, shared with the staff mail claim.
pub(crate) fn hash_token_pub(token: &str) -> Vec<u8> {
    hash_token(token)
}
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::from("oxi-");
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    token
}
fn normalize_email(email: &str) -> Result<String, WalletError> {
    Ok(users::validate_email(email)?.to_lowercase())
}
fn team_email(request: &TeamInvitationRequest) -> Result<Option<String>, WalletError> {
    let intended = request
        .intended_email
        .as_deref()
        .map(normalize_email)
        .transpose()?;
    let delivery = request.email.as_deref().map(normalize_email).transpose()?;
    if let (Some(a), Some(b)) = (&intended, &delivery)
        && a != b
    {
        return Err(WalletError::InvalidInput(
            "email and intendedEmail must match".into(),
        ));
    }
    Ok(intended.or(delivery))
}
/// Re-read identity and role while holding the same organization lock as member
/// removal/role transfer. Cached request roles cannot authorize a pending write.
async fn team_actor(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
) -> Result<(Uuid, Uuid), WalletError> {
    let identity = authority::personal_identity(tx, token).await?;
    if identity.context != "customer" {
        return Err(WalletError::Forbidden(
            "select customer context first".into(),
        ));
    }
    let org = identity
        .organization
        .ok_or(WalletError::InvalidCredentials)?;
    orgs::lock_organization(tx, org).await?;
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM oxsum.memberships WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(org)
    .bind(identity.user.id)
    .fetch_optional(&mut **tx)
    .await?;
    if !matches!(role.as_deref(), Some("owner" | "admin")) {
        return Err(WalletError::Forbidden(
            "current customer owner or admin membership is required".into(),
        ));
    }
    Ok((org, identity.user.id))
}
fn created(row: &PgRow, token: Option<String>) -> Result<CreatedInvitation, WalletError> {
    // A staff invitation names no customer organization; the nil UUID marks
    // the admission as business-scoped rather than customer-scoped.
    Ok(CreatedInvitation {
        id: row.try_get("invitation_id")?,
        token,
        organization_id: row
            .try_get::<Option<Uuid>, _>("pending_customer_id")?
            .or(row.try_get::<Option<Uuid>, _>("organization_id")?)
            .unwrap_or_else(Uuid::nil),
        expires_at: row.try_get("expires_at")?,
    })
}
async fn creation_replay(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<CreatedInvitation, WalletError> {
    let row = sqlx::query("SELECT invitation_id,organization_id,pending_customer_id,expires_at FROM oxsum.invitations WHERE creation_mutation_id=$1")
        .bind(id).fetch_one(&mut **tx).await?;
    created(&row, None)
}
impl Db {
    /// Reads the public details of a live invitation by its secret. Invalid,
    /// expired or already-used tokens are reported as not found.
    pub async fn invitation_preview(&self, token: &str) -> Result<InvitationPreview, WalletError> {
        let row=sqlx::query("SELECT COALESCE(i.organization_id,i.pending_customer_id) AS organization_id,COALESCE(o.name,i.customer_name) AS name,i.kind,i.staff_role,i.expires_at FROM oxsum.invitations i LEFT JOIN oxsum.organizations o USING (organization_id) WHERE i.token_hash=$1 AND i.accepted_at IS NULL AND i.revoked_at IS NULL AND i.expires_at>clock_timestamp()")
            .bind(hash_token(token)).fetch_optional(self.pool()).await?.ok_or_else(invalid)?;
        let kind: String = row.try_get("kind")?;
        let staff_role = if kind == "staff" {
            Some(staff_member_role(
                row.try_get::<Option<String>, _>("staff_role")?,
            )?)
        } else {
            None
        };
        Ok(InvitationPreview {
            organization_id: row.try_get("organization_id")?,
            name: row.try_get("name")?,
            role: if kind == "customer" {
                Role::Owner
            } else {
                Role::Member
            },
            staff_role,
            expires_at: row.try_get("expires_at")?,
        })
    }

    /// Creates a team invitation for the caller's customer organization. The
    /// secret is returned only on first creation.
    pub async fn create_invitation(
        &self,
        token: &str,
        request: &TeamInvitationRequest,
    ) -> Result<CreatedInvitation, WalletError> {
        authority::validate(&request.idempotency_key, None)?;
        let email = team_email(request)?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let (org, actor) = team_actor(&mut tx, token).await?;
        let detail = json!(request);
        let action = "invitation.team.create";
        if let Some(id) = authority::replay(
            &mut tx,
            org,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return creation_replay(&mut tx, id).await;
        }
        let mutation = authority::record(
            &mut tx,
            org,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let secret = generate_token();
        let row = sqlx::query("INSERT INTO oxsum.invitations (invitation_id,business_id,organization_id,invited_by,token_hash,expires_at,intended_email,creation_mutation_id) VALUES ($1,$2,$3,$4,$5,clock_timestamp()+interval '7 days',$6,$7) RETURNING invitation_id,organization_id,pending_customer_id,expires_at")
            .bind(Uuid::new_v4()).bind(business).bind(org).bind(actor).bind(hash_token(&secret)).bind(email).bind(mutation.id).fetch_one(&mut *tx).await?;
        let result = created(&row, Some(secret))?;
        tx.commit().await?;
        Ok(result)
    }
    /// Creates an invitation that establishes a new customer identity with the
    /// given `externalId`. An assigned or reserved identifier is a conflict.
    pub async fn create_customer_invitation(
        &self,
        token: &str,
        request: &CustomerInvitationRequest,
    ) -> Result<CreatedInvitation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let email = normalize_email(&request.email)?;
        let name = request.name.trim();
        let external = request.external_id.trim();
        if name.is_empty()
            || name.chars().count() > 80
            || external.is_empty()
            || external.chars().count() > 200
        {
            return Err(WalletError::InvalidInput(
                "name must contain 1 to 80 characters and externalId 1 to 200".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::CustomersWrite,
        )
        .await?;
        let detail = json!(request);
        let action = "business.customer.invitation.create";
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
            return creation_replay(&mut tx, id).await;
        }
        let taken: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.customer_profiles WHERE business_id=$1 AND external_id=$2) OR EXISTS(SELECT 1 FROM oxsum.invitations WHERE business_id=$1 AND external_id=$2 AND kind='customer' AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>clock_timestamp())")
            .bind(business).bind(external).fetch_one(&mut *tx).await?;
        if taken {
            return Err(WalletError::Conflict(
                "externalId is already assigned or reserved".into(),
            ));
        }
        let mutation = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let secret = generate_token();
        let row = sqlx::query("INSERT INTO oxsum.invitations (invitation_id,business_id,kind,invited_by,token_hash,expires_at,intended_email,pending_customer_id,customer_name,external_id,creation_mutation_id,mail_state) VALUES ($1,$2,'customer',$3,$4,clock_timestamp()+interval '7 days',$5,$6,$7,$8,$9,$10) RETURNING invitation_id,organization_id,pending_customer_id,expires_at")
            .bind(Uuid::new_v4()).bind(business).bind(actor.user_id).bind(hash_token(&secret)).bind(email).bind(Uuid::new_v4()).bind(name).bind(external).bind(mutation.id).bind(if request.send_email { "not_attempted" } else { "not_requested" }).fetch_one(&mut *tx).await?;
        let result = created(&row, Some(secret))?;
        tx.commit().await?;
        Ok(result)
    }
    /// Creates a staff invitation that admits the bound recipient to the
    /// business staff at `request.role`. `owner` is refused — ownership moves
    /// only through ownership transfer — and the inviter must hold
    /// `StaffManage`, which today means only the owner can send one.
    pub async fn create_staff_invitation(
        &self,
        token: &str,
        request: &StaffInvitationRequest,
    ) -> Result<CreatedInvitation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let email = normalize_email(&request.email)?;
        if request.role == BusinessRole::Owner {
            return Err(WalletError::InvalidInput(
                "an owner role is never granted through an invitation".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::StaffManage,
        )
        .await?;
        let detail = json!(request);
        let action = "business.staff.invitation.create";
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
            return creation_replay(&mut tx, id).await;
        }
        // An email already holding a staff seat cannot be re-invited; a pending
        // invitation for the same address is a conflict, not a second grant.
        let member: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.business_staff s JOIN oxsum.users u USING(user_id) \
             WHERE s.business_id=$1 AND u.email_normalized=$2)",
        )
        .bind(business)
        .bind(&email)
        .fetch_one(&mut *tx)
        .await?;
        if member {
            return Err(WalletError::Conflict(
                "that address already holds a staff seat".into(),
            ));
        }
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.invitations WHERE business_id=$1 AND intended_email=$2 \
             AND kind='staff' AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>clock_timestamp())",
        )
        .bind(business)
        .bind(&email)
        .fetch_one(&mut *tx)
        .await?;
        if pending {
            return Err(WalletError::Conflict(
                "a staff invitation for that address is already pending".into(),
            ));
        }
        let mutation = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let secret = generate_token();
        let row = sqlx::query(
            "INSERT INTO oxsum.invitations (invitation_id,business_id,kind,invited_by,token_hash,expires_at,intended_email,staff_role,creation_mutation_id) \
             VALUES ($1,$2,'staff',$3,$4,clock_timestamp()+interval '7 days',$5,$6,$7) \
             RETURNING invitation_id,organization_id,pending_customer_id,expires_at",
        )
        .bind(Uuid::new_v4())
        .bind(business)
        .bind(actor.user_id)
        .bind(hash_token(&secret))
        .bind(&email)
        .bind(request.role.as_str())
        .bind(mutation.id)
        .fetch_one(&mut *tx)
        .await?;
        let result = created(&row, Some(secret))?;
        tx.commit().await?;
        Ok(result)
    }
    /// Revokes a pending invitation. `scope` selects which surface owns the
    /// grant — a customer invitation (CustomersWrite), a staff invitation
    /// (StaffManage) or the caller's own team invitation (owner/admin member);
    /// accepted invitations cannot be revoked.
    pub async fn revoke_invitation(
        &self,
        token: &str,
        id: Uuid,
        scope: InvitationScope,
        request: &InvitationRevokeRequest,
    ) -> Result<BusinessMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let (scope_id, actor, kind) = match scope {
            InvitationScope::Customer => {
                let actor = authority::require_capability(
                    &mut tx,
                    business,
                    token,
                    BusinessCapability::CustomersWrite,
                )
                .await?;
                (business, actor.user_id, "customer")
            }
            InvitationScope::Staff => {
                let actor = authority::require_capability(
                    &mut tx,
                    business,
                    token,
                    BusinessCapability::StaffManage,
                )
                .await?;
                (business, actor.user_id, "staff")
            }
            InvitationScope::Team => {
                let (org, actor) = team_actor(&mut tx, token).await?;
                (org, actor, "team")
            }
        };
        // Team revokes additionally pin the row to the caller's organization;
        // customer and staff scopes are business-wide.
        let row = sqlx::query(
            "SELECT accepted_at FROM oxsum.invitations WHERE invitation_id=$1 AND business_id=$2 \
             AND kind=$3 AND ($3<>'team' OR organization_id=$4) FOR UPDATE",
        )
        .bind(id)
        .bind(business)
        .bind(kind)
        .bind(scope_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(invalid)?;
        let action = "invitation.revoke";
        let detail = json!({"invitationId":id,"request":request});
        if let Some(id) = authority::replay(
            &mut tx,
            scope_id,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return Ok(BusinessMutationResult { id });
        }
        if row
            .try_get::<Option<OffsetDateTime>, _>("accepted_at")?
            .is_some()
        {
            return Err(WalletError::Conflict(
                "accepted invitations cannot be revoked; manage the membership instead".into(),
            ));
        }
        sqlx::query("UPDATE oxsum.invitations SET revoked_at=COALESCE(revoked_at,clock_timestamp()) WHERE invitation_id=$1").bind(id).execute(&mut *tx).await?;
        let result = authority::record(
            &mut tx,
            scope_id,
            actor,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Consent is valid in either session context. The selected context and any
    /// existing keys/organizations stay unchanged; no additional key is minted.
    pub async fn accept_invitation(
        &self,
        token: &str,
        invitation_token: &str,
    ) -> Result<InvitationAcceptance, WalletError> {
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        let actor = authority::personal_identity(&mut tx, token).await?;
        let invitation = locked_invitation(
            &mut tx,
            invitation_token,
            &actor.user.email,
            Some(actor.user.id),
        )
        .await?;
        let result = accept(&mut tx, &invitation, actor.user.id).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// New-user redemption for a customer/team invitation retains the existing
    /// response and one-time key. An existing user must authenticate and
    /// accept; this path cannot attach them. A staff invitation is refused
    /// here — a new staff member registers through
    /// [`Db::redeem_staff_invitation`], which grants no customer org or key.
    pub async fn redeem_invitation(
        &self,
        token: &str,
        new: NewUser,
    ) -> Result<Registration, WalletError> {
        let email = users::validate_email(&new.email)?;
        users::validate_password(&new.password)?;
        let password_hash = password_auth::generate_hash(&new.password);
        let user_id = Uuid::new_v4();
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        let invitation = locked_invitation(&mut tx, token, &email, None).await?;
        if invitation.try_get::<String, _>("kind")? == "staff" {
            return Err(invalid());
        }
        sqlx::query("INSERT INTO oxsum.users (user_id,email,email_normalized,password_hash) VALUES ($1,$2,$3,$4)")
            .bind(user_id).bind(&email).bind(email.to_lowercase()).bind(password_hash).execute(&mut *tx).await
            .map_err(|e| keys::conflict_or_storage(e, "users_email_normalized_key", "email already registered; sign in to accept"))?;
        let receipt = accept(&mut tx, &invitation, user_id).await?;
        let api_key = keys::insert(
            &mut tx,
            receipt.organization_id,
            Some("default".into()),
            None,
            Some(user_id),
            keys::KeyConstraints::default(),
        )
        .await?;
        let row = sqlx::query("SELECT organization_id,name,tenant_id,kind,created_at,display_currency,display_rate_per_credit FROM oxsum.organizations WHERE organization_id=$1")
            .bind(receipt.organization_id).fetch_one(&mut *tx).await?;
        let organization = orgs::organization_from_row(&row)?;
        tx.commit().await?;
        Ok(Registration {
            user: User {
                id: user_id,
                email,
                email_verified: false,
            },
            organization,
            api_key,
            verification_sent: false,
        })
    }
    /// Registers a brand-new staff recipient. The account holds only the staff
    /// seat the invitation granted — no personal customer organization, no API
    /// key and no customer funds are created, so the response carries the user
    /// alone. A non-staff token is refused here and belongs in
    /// [`Db::redeem_invitation`].
    pub async fn redeem_staff_invitation(
        &self,
        token: &str,
        new: NewUser,
    ) -> Result<User, WalletError> {
        let email = users::validate_email(&new.email)?;
        users::validate_password(&new.password)?;
        let password_hash = password_auth::generate_hash(&new.password);
        let user_id = Uuid::new_v4();
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        let invitation = locked_invitation(&mut tx, token, &email, None).await?;
        if invitation.try_get::<String, _>("kind")? != "staff" {
            return Err(invalid());
        }
        sqlx::query("INSERT INTO oxsum.users (user_id,email,email_normalized,password_hash) VALUES ($1,$2,$3,$4)")
            .bind(user_id).bind(&email).bind(email.to_lowercase()).bind(password_hash).execute(&mut *tx).await
            .map_err(|e| keys::conflict_or_storage(e, "users_email_normalized_key", "email already registered; sign in to accept"))?;
        accept(&mut tx, &invitation, user_id).await?;
        tx.commit().await?;
        Ok(User {
            id: user_id,
            email,
            email_verified: false,
        })
    }
}
async fn locked_invitation(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
    email: &str,
    replay_user: Option<Uuid>,
) -> Result<PgRow, WalletError> {
    let row = sqlx::query("SELECT *,expires_at>clock_timestamp() AS live FROM oxsum.invitations WHERE token_hash=$1 FOR UPDATE")
        .bind(hash_token(token)).fetch_optional(&mut **tx).await?.ok_or_else(invalid)?;
    if let Some(intended) = row.try_get::<Option<String>, _>("intended_email")?
        && intended != email.to_lowercase()
    {
        return Err(invalid());
    }
    if row
        .try_get::<Option<OffsetDateTime>, _>("accepted_at")?
        .is_some()
    {
        if replay_user.is_none() || replay_user != row.try_get::<Option<Uuid>, _>("accepted_by")? {
            return Err(invalid());
        }
    } else if row
        .try_get::<Option<OffsetDateTime>, _>("revoked_at")?
        .is_some()
        || !row.try_get::<bool, _>("live")?
    {
        return Err(invalid());
    }
    Ok(row)
}
/// Resolves the staff role stored on an invitation into its typed value; a
/// missing or out-of-domain value means the row is not a valid staff grant.
fn staff_member_role(raw: Option<String>) -> Result<BusinessRole, WalletError> {
    match raw.as_deref() {
        Some("admin") => Ok(BusinessRole::Admin),
        Some("finance") => Ok(BusinessRole::Finance),
        Some("support") => Ok(BusinessRole::Support),
        Some("viewer") => Ok(BusinessRole::Viewer),
        // `owner` is never a valid invitation grant — ownership moves only
        // through ownership transfer — so a stored owner role fails closed.
        _ => Err(invalid()),
    }
}
async fn accept(
    tx: &mut Transaction<'_, Postgres>,
    row: &PgRow,
    user: Uuid,
) -> Result<InvitationAcceptance, WalletError> {
    let kind = row.try_get::<String, _>("kind")?;
    let customer = kind == "customer";
    let staff_invite = kind == "staff";
    let staff_role = if staff_invite {
        Some(staff_member_role(
            row.try_get::<Option<String>, _>("staff_role")?,
        )?)
    } else {
        None
    };
    let org = if customer {
        row.try_get("pending_customer_id")?
    } else if staff_invite {
        Uuid::nil()
    } else {
        row.try_get("organization_id")?
    };
    let id: Uuid = row.try_get("invitation_id")?;
    let role = if customer { Role::Owner } else { Role::Member };
    let result = InvitationAcceptance {
        id,
        organization_id: org,
        role,
        staff_role,
    };
    if row
        .try_get::<Option<OffsetDateTime>, _>("accepted_at")?
        .is_some()
    {
        return Ok(result);
    }
    if staff_invite {
        // A staff invitation joins the recipient to the business staff at the
        // invited role — never to a customer membership, and never as owner.
        let business = row.try_get::<Uuid, _>("business_id")?;
        // A staff invitation always carries a role: the migration's CHECK
        // constraint and `staff_member_role` above both reject a missing or
        // out-of-domain value, so reaching `None` here means the row is
        // malformed and fails closed rather than panicking.
        let Some(seat_role) = staff_role else {
            return Err(invalid());
        };
        // Refuse to overwrite a live owner seat; an existing non-owner seat is
        // updated to the invited role so the invite cannot silently demote an
        // owner while still letting a re-invited admin/finance/support/viewer
        // land the new grant.
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT role FROM oxsum.business_staff WHERE business_id=$1 AND user_id=$2 FOR UPDATE",
        )
        .bind(business)
        .bind(user)
        .fetch_optional(&mut **tx)
        .await?;
        if existing.as_deref() == Some("owner") {
            return Err(WalletError::Conflict(
                "an owner seat cannot be changed by an invitation".into(),
            ));
        }
        sqlx::query(
            "INSERT INTO oxsum.business_staff (business_id,user_id,role) VALUES ($1,$2,$3) \
             ON CONFLICT (business_id,user_id) DO UPDATE SET role=EXCLUDED.role",
        )
        .bind(business)
        .bind(user)
        .bind(seat_role.as_str())
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.invitations SET accepted_by=$2,accepted_at=clock_timestamp() WHERE invitation_id=$1",
        )
        .bind(id)
        .bind(user)
        .execute(&mut **tx)
        .await?;
        authority::record(
            tx,
            business,
            user,
            "accept",
            "invitation.staff.accept",
            &json!({"invitationId":id,"businessId":business,"staffRole":seat_role.as_str()}),
        )
        .await?;
        return Ok(result);
    }
    if customer {
        orgs::insert(
            tx,
            org,
            &row.try_get::<String, _>("customer_name")?,
            Kind::Personal,
        )
        .await
        .map_err(|e| match e {
            WalletError::Storage(doubleentry::storage::postgres::PostgresError::Database(
                error,
            )) => keys::conflict_or_storage(
                error,
                "customer_profiles_business_id_external_id_key",
                "externalId is already assigned",
            ),
            other => other,
        })?;
    } else {
        orgs::lock_organization(tx, org).await?;
    }
    orgs::add_member(tx, org, user, role).await?;
    if !customer {
        sqlx::query("UPDATE oxsum.organizations SET kind='team' WHERE organization_id=$1 AND kind='personal' AND (SELECT count(*) FROM oxsum.memberships WHERE organization_id=$1)>1")
            .bind(org).execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE oxsum.invitations SET organization_id=$2,accepted_by=$3,accepted_at=clock_timestamp() WHERE invitation_id=$1")
        .bind(id).bind(org).bind(user).execute(&mut **tx).await?;
    authority::record(
        tx,
        id,
        user,
        "accept",
        "invitation.accept",
        &json!({"invitationId":id,"organizationId":org,"role":role}),
    )
    .await?;
    Ok(result)
}

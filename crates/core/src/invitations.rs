//! Customer and team invitations require explicit recipient consent. Tokens are
//! hashed, single-use and expiring; creation/revocation/acceptance commit with audit.
use std::fmt::Write;

use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::business_staff as authority;
use crate::keys;
use crate::orgs::{self, Kind, Role};
use crate::users::{self, NewUser, Registration, User};
use crate::{BusinessCapability, BusinessMutationResult, Db, WalletError};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedInvitation {
    pub id: Uuid,
    /// Only the first creation returns the secret. A retry cannot recover it.
    pub token: Option<String>,
    pub organization_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamInvitationRequest {
    pub idempotency_key: String,
    pub intended_email: Option<String>,
    /// Also requests email delivery at the HTTP boundary.
    pub email: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomerInvitationRequest {
    pub idempotency_key: String,
    pub email: String,
    pub name: String,
    pub external_id: String,
    pub reason: String,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvitationRevokeRequest {
    pub idempotency_key: String,
    pub reason: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvitationAcceptRequest {
    pub token: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationAcceptance {
    pub id: Uuid,
    pub organization_id: Uuid,
    /// Historical accepted role; this receipt never grants current authority.
    pub role: Role,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationPreview {
    pub organization_id: Uuid,
    pub name: String,
    pub role: Role,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

fn invalid() -> WalletError {
    WalletError::NotFound("the invitation is not valid".into())
}
fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
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
    Ok(CreatedInvitation {
        id: row.try_get("invitation_id")?,
        token,
        organization_id: row
            .try_get::<Option<Uuid>, _>("pending_customer_id")?
            .or(row.try_get::<Option<Uuid>, _>("organization_id")?)
            .ok_or_else(|| {
                WalletError::Misconfigured("invitation has no customer identity".into())
            })?,
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
    pub async fn invitation_preview(&self, token: &str) -> Result<InvitationPreview, WalletError> {
        let row=sqlx::query("SELECT COALESCE(i.organization_id,i.pending_customer_id) AS organization_id,COALESCE(o.name,i.customer_name) AS name,i.kind,i.expires_at FROM oxsum.invitations i LEFT JOIN oxsum.organizations o USING (organization_id) WHERE i.token_hash=$1 AND i.accepted_at IS NULL AND i.revoked_at IS NULL AND i.expires_at>clock_timestamp()")
            .bind(hash_token(token)).fetch_optional(self.pool()).await?.ok_or_else(invalid)?;
        Ok(InvitationPreview {
            organization_id: row.try_get("organization_id")?,
            name: row.try_get("name")?,
            role: if row.try_get::<String, _>("kind")? == "customer" {
                Role::Owner
            } else {
                Role::Member
            },
            expires_at: row.try_get("expires_at")?,
        })
    }

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
        let row = sqlx::query("INSERT INTO oxsum.invitations (invitation_id,business_id,kind,invited_by,token_hash,expires_at,intended_email,pending_customer_id,customer_name,external_id,creation_mutation_id) VALUES ($1,$2,'customer',$3,$4,clock_timestamp()+interval '7 days',$5,$6,$7,$8,$9) RETURNING invitation_id,organization_id,pending_customer_id,expires_at")
            .bind(Uuid::new_v4()).bind(business).bind(actor.user_id).bind(hash_token(&secret)).bind(email).bind(Uuid::new_v4()).bind(name).bind(external).bind(mutation.id).fetch_one(&mut *tx).await?;
        let result = created(&row, Some(secret))?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn revoke_invitation(
        &self,
        token: &str,
        id: Uuid,
        business_invite: bool,
        request: &InvitationRevokeRequest,
    ) -> Result<BusinessMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let (scope, actor) = if business_invite {
            let actor = authority::require_capability(
                &mut tx,
                business,
                token,
                BusinessCapability::CustomersWrite,
            )
            .await?;
            (business, actor.user_id)
        } else {
            team_actor(&mut tx, token).await?
        };
        let row = sqlx::query("SELECT accepted_at FROM oxsum.invitations WHERE invitation_id=$1 AND business_id=$2 AND (($3 AND kind='customer') OR (NOT $3 AND kind='team' AND organization_id=$4)) FOR UPDATE")
            .bind(id).bind(business).bind(business_invite).bind(scope).fetch_optional(&mut *tx).await?.ok_or_else(invalid)?;
        let action = "invitation.revoke";
        let detail = json!({"invitationId":id,"request":request});
        if let Some(id) = authority::replay(
            &mut tx,
            scope,
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
            scope,
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
    /// New-user redemption retains the existing response and one-time key. An
    /// existing user must authenticate and accept; this path cannot attach them.
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
async fn accept(
    tx: &mut Transaction<'_, Postgres>,
    row: &PgRow,
    user: Uuid,
) -> Result<InvitationAcceptance, WalletError> {
    let customer = row.try_get::<String, _>("kind")? == "customer";
    let org = if customer {
        row.try_get("pending_customer_id")?
    } else {
        row.try_get("organization_id")?
    };
    let id: Uuid = row.try_get("invitation_id")?;
    let role = if customer { Role::Owner } else { Role::Member };
    let result = InvitationAcceptance {
        id,
        organization_id: org,
        role,
    };
    if row
        .try_get::<Option<OffsetDateTime>, _>("accepted_at")?
        .is_some()
    {
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

//! Explicit initial ownership: deployment invitation plus intended owner consent.
use crate::{BusinessOwnership, Db, Registration, WalletError, users};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Constructed by the deployment-authenticated transport, never a customer principal.
#[derive(Clone, Copy)]
pub enum SetupAuthority {
    Deployment,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessSetupTokenRequest {
    pub idempotency_key: String,
    pub owner_email: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessSetupRequest {
    pub token: String,
    pub business_name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessSetupRegistrationRequest {
    pub token: String,
    pub business_name: String,
    pub email: String,
    pub password: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessSetupToken {
    pub id: Uuid,
    pub business_id: Uuid,
    pub owner_email: String,
    pub token: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}
#[derive(Serialize)]
pub struct BusinessSetupResult {
    pub ownership: BusinessOwnership,
    pub registration: Option<Registration>,
}

fn name(raw: &str) -> Result<&str, WalletError> {
    let value = raw.trim();
    if value.is_empty() || value.chars().count() > 120 {
        return Err(WalletError::InvalidInput(
            "businessName must contain 1 to 120 characters".into(),
        ));
    }
    Ok(value)
}
fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
async fn lock_business(tx: &mut Transaction<'_, Postgres>) -> Result<Uuid, WalletError> {
    let row = sqlx::query(
        "SELECT business_id,initialized_at FROM oxsum.businesses WHERE singleton FOR UPDATE",
    )
    .fetch_one(&mut **tx)
    .await?;
    if row
        .try_get::<Option<OffsetDateTime>, _>("initialized_at")?
        .is_some()
    {
        return Err(WalletError::Conflict(
            "business is already initialized".into(),
        ));
    }
    Ok(row.try_get("business_id")?)
}
async fn check_token(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    token: &str,
    email: &str,
) -> Result<Uuid, WalletError> {
    sqlx::query_scalar("SELECT setup_id FROM oxsum.business_setup_tokens WHERE business_id=$1 AND token_hash=$2 AND owner_email=$3 AND accepted_at IS NULL AND expires_at>now() FOR UPDATE")
        .bind(business).bind(token_hash(token)).bind(email.to_lowercase()).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::NotFound("setup invitation is not valid".into()))
}
async fn initialize(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    setup: Uuid,
    user: Uuid,
    name: &str,
) -> Result<(), WalletError> {
    sqlx::query(
        "INSERT INTO oxsum.business_staff (business_id,user_id,role) VALUES ($1,$2,'owner')",
    )
    .bind(business)
    .bind(user)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE oxsum.businesses SET name=$2,initialized_at=now(),registration_mode='invite' WHERE business_id=$1").bind(business).bind(name).execute(&mut **tx).await?;
    sqlx::query(
        "UPDATE oxsum.business_setup_tokens SET accepted_at=now(),accepted_by=$2 WHERE setup_id=$1",
    )
    .bind(setup)
    .bind(user)
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO oxsum.admin_audit (action,actor,target,detail,idempotency_key) VALUES ('business.initialized',$1,$2,$3,$4)").bind(format!("user:{user}")).bind(business.to_string()).bind(serde_json::json!({"ownerUserId":user,"setupId":setup,"registrationMode":"invite"})).bind(format!("business-init:{business}")).execute(&mut **tx).await?;
    Ok(())
}
impl Db {
    pub async fn mint_business_setup(
        &self,
        _authority: SetupAuthority,
        request: &BusinessSetupTokenRequest,
    ) -> Result<BusinessSetupToken, WalletError> {
        if request.idempotency_key.trim().is_empty()
            || request.idempotency_key.chars().count() > 200
        {
            return Err(WalletError::InvalidInput(
                "idempotencyKey must contain 1 to 200 characters".into(),
            ));
        }
        let email = users::validate_email(&request.owner_email)?.to_lowercase();
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        if let Some(row)=sqlx::query("SELECT setup_id,owner_email,expires_at FROM oxsum.business_setup_tokens WHERE business_id=$1 AND idempotency_key=$2").bind(business).bind(&request.idempotency_key).fetch_optional(&mut *tx).await? {
            let prior: String=row.try_get("owner_email")?;
            if prior!=email { return Err(WalletError::Conflict("setup key was used for a different owner".into())); }
            return Ok(BusinessSetupToken{id:row.try_get("setup_id")?,business_id:business,owner_email:prior,token:None,expires_at:row.try_get("expires_at")?});
        }
        let bytes: [u8; 32] = rand::rng().random();
        let token = format!(
            "oxsetup-{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let row=sqlx::query("INSERT INTO oxsum.business_setup_tokens (business_id,idempotency_key,owner_email,token_hash) VALUES ($1,$2,$3,$4) RETURNING setup_id,expires_at").bind(business).bind(&request.idempotency_key).bind(&email).bind(token_hash(&token)).fetch_one(&mut *tx).await?;
        let id: Uuid = row.try_get("setup_id")?;
        sqlx::query("INSERT INTO oxsum.admin_audit (action,actor,target,detail,idempotency_key) VALUES ('business.setup.authorized','deployment',$1,$2,$3)").bind(business.to_string()).bind(serde_json::json!({"setupId":id})).bind(format!("business-setup:{id}")).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(BusinessSetupToken {
            id,
            business_id: business,
            owner_email: email,
            token: Some(token),
            expires_at: row.try_get("expires_at")?,
        })
    }
    /// Reauthenticate the session under the singleton lock; a key/user ID is not owner consent.
    pub async fn accept_business_setup(
        &self,
        session: &str,
        request: &BusinessSetupRequest,
    ) -> Result<BusinessSetupResult, WalletError> {
        let business_name = name(&request.business_name)?;
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let row=sqlx::query("SELECT s.user_id,u.email_normalized FROM oxsum.sessions s JOIN oxsum.users u USING (user_id) JOIN oxsum.memberships m ON m.user_id=s.user_id AND m.organization_id=s.organization_id WHERE s.token_hash=$1 AND s.revoked_at IS NULL AND s.expires_at>now() FOR SHARE OF s,u,m").bind(token_hash(session)).fetch_optional(&mut *tx).await?.ok_or(WalletError::InvalidCredentials)?;
        let user: Uuid = row.try_get("user_id")?;
        let setup = check_token(
            &mut tx,
            business,
            &request.token,
            &row.try_get::<String, _>("email_normalized")?,
        )
        .await?;
        initialize(&mut tx, business, setup, user, business_name).await?;
        tx.commit().await?;
        Ok(BusinessSetupResult {
            ownership: self.business_ownership().await?,
            registration: None,
        })
    }
    pub async fn register_business_owner(
        &self,
        request: &BusinessSetupRegistrationRequest,
    ) -> Result<BusinessSetupResult, WalletError> {
        let business_name = name(&request.business_name)?;
        let email = users::validate_email(&request.email)?;
        users::validate_password(&request.password)?;
        let org_name = users::organization_name(None, &email)?;
        let hash = password_auth::generate_hash(&request.password);
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let setup = check_token(&mut tx, business, &request.token, &email).await?;
        let registration = users::insert_registration(&mut tx, email, &org_name, &hash).await?;
        initialize(
            &mut tx,
            business,
            setup,
            registration.user.id,
            business_name,
        )
        .await?;
        tx.commit().await?;
        Ok(BusinessSetupResult {
            ownership: self.business_ownership().await?,
            registration: Some(registration),
        })
    }
}

//! Selected business authority, independent of customer memberships.
//!
//! Mutations serialize on the singleton business before resolving the live
//! session and current role. No cached role or caller-supplied user ID grants
//! authority. Security changes, immutable receipts and audit commit together.
use crate::{Db, User, WalletError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BusinessRole {
    Owner,
    Admin,
    Finance,
    Support,
    Viewer,
}
impl BusinessRole {
    pub(crate) fn parse(raw: &str) -> Result<Self, WalletError> {
        match raw {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "finance" => Ok(Self::Finance),
            "support" => Ok(Self::Support),
            "viewer" => Ok(Self::Viewer),
            _ => Err(WalletError::Forbidden("unknown business role".into())),
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Finance => "finance",
            Self::Support => "support",
            Self::Viewer => "viewer",
        }
    }
    /// Explicit allowlists: a new capability is denied until deliberately granted.
    #[must_use]
    pub fn allows(self, capability: BusinessCapability) -> bool {
        use BusinessCapability as C;
        match capability {
            C::OverviewRead => true,
            C::CustomersRead | C::RequestsRead => matches!(
                self,
                Self::Owner | Self::Admin | Self::Finance | Self::Support
            ),
            C::CustomersWrite | C::CatalogWrite | C::RoutingWrite => {
                matches!(self, Self::Owner | Self::Admin)
            }
            C::CustomersSuspend => matches!(self, Self::Owner | Self::Admin | Self::Support),
            C::CommercialWrite | C::FundsWrite | C::RefundsWrite => {
                matches!(self, Self::Owner | Self::Finance)
            }
            C::ReportsRead => matches!(
                self,
                Self::Owner | Self::Admin | Self::Finance | Self::Viewer
            ),
            C::ProviderSecretsWrite
            | C::MerchantSecretsWrite
            | C::StaffManage
            | C::OwnershipTransfer
            | C::RegistrationWrite
            | C::IntegrationsManage => self == Self::Owner,
        }
    }
    #[must_use]
    pub fn capabilities(self) -> Vec<BusinessCapability> {
        BusinessCapability::ALL
            .iter()
            .copied()
            .filter(|c| self.allows(*c))
            .collect()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BusinessCapability {
    OverviewRead,
    CustomersRead,
    CustomersWrite,
    CustomersSuspend,
    RequestsRead,
    CatalogWrite,
    RoutingWrite,
    ProviderSecretsWrite,
    CommercialWrite,
    FundsWrite,
    RefundsWrite,
    ReportsRead,
    MerchantSecretsWrite,
    StaffManage,
    OwnershipTransfer,
    RegistrationWrite,
    IntegrationsManage,
}
impl BusinessCapability {
    pub const ALL: &[Self] = &[
        Self::OverviewRead,
        Self::CustomersRead,
        Self::CustomersWrite,
        Self::CustomersSuspend,
        Self::RequestsRead,
        Self::CatalogWrite,
        Self::RoutingWrite,
        Self::ProviderSecretsWrite,
        Self::CommercialWrite,
        Self::FundsWrite,
        Self::RefundsWrite,
        Self::ReportsRead,
        Self::MerchantSecretsWrite,
        Self::StaffManage,
        Self::OwnershipTransfer,
        Self::RegistrationWrite,
        Self::IntegrationsManage,
    ];
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessPrincipal {
    pub business_id: Uuid,
    pub user_id: Uuid,
    pub role: BusinessRole,
    pub capabilities: Vec<BusinessCapability>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionContext {
    Customer,
    Business,
}
impl SessionContext {
    fn as_str(self) -> &'static str {
        match self {
            Self::Customer => "customer",
            Self::Business => "business",
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextInfo {
    pub context: SessionContext,
    pub user: User,
    pub organization_id: Option<Uuid>,
    pub business: Option<BusinessPrincipal>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionContextRequest {
    pub idempotency_key: String,
    pub context: SessionContext,
    pub organization_id: Option<Uuid>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessStaff {
    pub user_id: Uuid,
    pub email: String,
    pub role: BusinessRole,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessStaffRequest {
    pub idempotency_key: String,
    pub user_id: Uuid,
    pub role: BusinessRole,
    pub reason: String,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessStaffRevokeRequest {
    pub idempotency_key: String,
    pub user_id: Uuid,
    pub reason: String,
}
pub type BusinessOwnershipRequest = BusinessStaffRevokeRequest;
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RegistrationMode {
    Invite,
    Open,
}
impl RegistrationMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Invite => "invite",
            Self::Open => "open",
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessRegistrationRequest {
    pub idempotency_key: String,
    pub mode: RegistrationMode,
    pub reason: String,
}
#[derive(Debug, Serialize)]
pub struct BusinessMutationResult {
    pub id: Uuid,
}

pub(crate) async fn lock_business(tx: &mut Transaction<'_, Postgres>) -> Result<Uuid, WalletError> {
    Ok(
        sqlx::query_scalar("SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE")
            .fetch_one(&mut **tx)
            .await?,
    )
}
struct Identity {
    session: Uuid,
    user: User,
    organization: Option<Uuid>,
    context: String,
    second_factor_satisfied: bool,
}
async fn identity(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
) -> Result<Identity, WalletError> {
    let row=sqlx::query("SELECT s.session_id,s.user_id,s.organization_id,s.context,(s.totp_verified_at IS NOT NULL OR NOT EXISTS(SELECT 1 FROM oxsum.user_totp t WHERE t.user_id=s.user_id AND t.confirmed_at IS NOT NULL)) AS second_factor_satisfied,u.email,(u.email_verified_at IS NOT NULL) AS verified FROM oxsum.sessions s JOIN oxsum.users u USING (user_id) WHERE s.token_hash=$1 AND s.revoked_at IS NULL AND s.expires_at>now() AND u.password_hash<>'!deleted' FOR SHARE OF s")
        .bind(Sha256::digest(token.as_bytes()).to_vec()).fetch_optional(&mut **tx).await?.ok_or(WalletError::InvalidCredentials)?;
    Ok(Identity {
        session: row.try_get("session_id")?,
        user: User {
            id: row.try_get("user_id")?,
            email: row.try_get("email")?,
            email_verified: row.try_get("verified")?,
        },
        organization: row.try_get("organization_id")?,
        context: row.try_get("context")?,
        second_factor_satisfied: row.try_get("second_factor_satisfied")?,
    })
}
async fn staff(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    user: Uuid,
) -> Result<BusinessPrincipal, WalletError> {
    let role:String=sqlx::query_scalar("SELECT s.role FROM oxsum.business_staff s JOIN oxsum.businesses b USING (business_id) WHERE s.business_id=$1 AND s.user_id=$2 AND b.initialized_at IS NOT NULL")
        .bind(business).bind(user).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::Forbidden("current business staff membership is required".into()))?;
    let role = BusinessRole::parse(&role)?;
    Ok(BusinessPrincipal {
        business_id: business,
        user_id: user,
        role,
        capabilities: role.capabilities(),
    })
}
/// Authorization inside the caller's business-locked transaction. Future business
/// services must use this guard in the same transaction as their mutation.
pub(crate) async fn require_capability(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    token: &str,
    capability: BusinessCapability,
) -> Result<BusinessPrincipal, WalletError> {
    let actor = identity(tx, token).await?;
    if !actor.second_factor_satisfied {
        return Err(WalletError::Forbidden(
            "business access requires a login verified with your second factor".into(),
        ));
    }
    if actor.context != "business" {
        return Err(WalletError::Forbidden(
            "select business context first".into(),
        ));
    }
    let principal = staff(tx, business, actor.user.id).await?;
    if !principal.role.allows(capability) {
        return Err(WalletError::Forbidden(
            "business capability is not granted".into(),
        ));
    }
    Ok(principal)
}
async fn context_info(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    actor: Identity,
) -> Result<SessionContextInfo, WalletError> {
    if actor.context == "business" {
        if !actor.second_factor_satisfied {
            return Err(WalletError::Forbidden(
                "business access requires a login verified with your second factor".into(),
            ));
        }
        let principal = staff(tx, business, actor.user.id).await?;
        return Ok(SessionContextInfo {
            context: SessionContext::Business,
            user: actor.user,
            organization_id: None,
            business: Some(principal),
        });
    }
    let member: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM oxsum.memberships WHERE user_id=$1 AND organization_id=$2)",
    )
    .bind(actor.user.id)
    .bind(actor.organization)
    .fetch_one(&mut **tx)
    .await?;
    Ok(SessionContextInfo {
        context: SessionContext::Customer,
        user: actor.user,
        organization_id: actor.organization.filter(|_| member),
        business: None,
    })
}
pub(crate) fn validate(key: &str, reason: Option<&str>) -> Result<(), WalletError> {
    if key.trim().is_empty() || key.chars().count() > 200 {
        return Err(WalletError::InvalidInput(
            "idempotencyKey must contain 1 to 200 characters".into(),
        ));
    }
    if reason.is_some_and(|r| r.trim().is_empty() || r.chars().count() > 500) {
        return Err(WalletError::InvalidInput(
            "reason must contain 1 to 500 characters".into(),
        ));
    }
    Ok(())
}
pub(crate) async fn replay(
    tx: &mut Transaction<'_, Postgres>,
    scope: Uuid,
    actor: Uuid,
    key: &str,
    action: &str,
    request: &Value,
) -> Result<Option<Uuid>, WalletError> {
    let row=sqlx::query("SELECT mutation_id,action,request FROM oxsum.business_mutations WHERE scope_id=$1 AND actor_id=$2 AND idempotency_key=$3").bind(scope).bind(actor).bind(key).fetch_optional(&mut **tx).await?;
    let Some(row) = row else { return Ok(None) };
    if row.try_get::<String, _>("action")? != action
        || row.try_get::<Value, _>("request")? != *request
    {
        return Err(WalletError::Conflict(
            "idempotencyKey was used for different security terms".into(),
        ));
    }
    Ok(Some(row.try_get("mutation_id")?))
}
pub(crate) async fn record(
    tx: &mut Transaction<'_, Postgres>,
    scope: Uuid,
    actor: Uuid,
    key: &str,
    action: &str,
    request: &Value,
) -> Result<BusinessMutationResult, WalletError> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.business_mutations (mutation_id,scope_id,actor_id,idempotency_key,action,request) VALUES ($1,$2,$3,$4,$5,$6)").bind(id).bind(scope).bind(actor).bind(key).bind(action).bind(request).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO oxsum.admin_audit (action,actor,target,detail,idempotency_key) VALUES ($1,$2,$3,$4,$5)").bind(action).bind(format!("user:{actor}")).bind(scope.to_string()).bind(request).bind(format!("security:{id}")).execute(&mut **tx).await?;
    Ok(BusinessMutationResult { id })
}

impl Db {
    /// Personal account settings are independent of the selected commercial
    /// context. This resolution grants no customer or business permissions.
    pub async fn session_user(&self, token: &str) -> Result<User, WalletError> {
        let mut tx = self.pool().begin().await?;
        Ok(identity(&mut tx, token).await?.user)
    }
    pub async fn session_context(&self, token: &str) -> Result<SessionContextInfo, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let actor = identity(&mut tx, token).await?;
        context_info(&mut tx, business, actor).await
    }
    pub async fn switch_session_context(
        &self,
        token: &str,
        request: &SessionContextRequest,
    ) -> Result<SessionContextInfo, WalletError> {
        validate(&request.idempotency_key, None)?;
        if request.context == SessionContext::Business && request.organization_id.is_some() {
            return Err(WalletError::InvalidInput(
                "business context cannot name a customer".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let mut actor = identity(&mut tx, token).await?;
        let detail = json!(request);
        if replay(
            &mut tx,
            actor.session,
            actor.user.id,
            &request.idempotency_key,
            "session.context",
            &detail,
        )
        .await?
        .is_none()
        {
            match request.context {
                SessionContext::Business => {
                    if !actor.second_factor_satisfied {
                        return Err(WalletError::Forbidden(
                            "business access requires a login verified with your second factor"
                                .into(),
                        ));
                    }
                    staff(&mut tx, business, actor.user.id).await?;
                }
                SessionContext::Customer => {
                    actor.organization = request.organization_id.or(actor.organization);
                    let member:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.memberships WHERE user_id=$1 AND organization_id=$2)").bind(actor.user.id).bind(actor.organization).fetch_one(&mut *tx).await?;
                    if !member {
                        return Err(WalletError::NotFound(
                            "customer membership is not known".into(),
                        ));
                    }
                }
            }
            sqlx::query(
                "UPDATE oxsum.sessions SET context=$2,organization_id=$3 WHERE session_id=$1",
            )
            .bind(actor.session)
            .bind(request.context.as_str())
            .bind(actor.organization)
            .execute(&mut *tx)
            .await?;
            record(
                &mut tx,
                actor.session,
                actor.user.id,
                &request.idempotency_key,
                "session.context",
                &detail,
            )
            .await?;
            actor.context = request.context.as_str().into();
        }
        let result = context_info(&mut tx, business, actor).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Read-only permission probe. Mutating services must use require_capability
    /// within their own transaction; this snapshot must never authorize a later write.
    pub async fn business_capability(
        &self,
        token: &str,
        capability: BusinessCapability,
    ) -> Result<BusinessPrincipal, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        require_capability(&mut tx, business, token, capability).await
    }
    pub async fn business_staff(&self, token: &str) -> Result<Vec<BusinessStaff>, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        require_capability(&mut tx, business, token, BusinessCapability::StaffManage).await?;
        let rows=sqlx::query("SELECT s.user_id,u.email,s.role FROM oxsum.business_staff s JOIN oxsum.users u USING (user_id) WHERE s.business_id=$1 ORDER BY s.created_at,s.user_id").bind(business).fetch_all(&mut *tx).await?;
        rows.iter()
            .map(|r| {
                Ok(BusinessStaff {
                    user_id: r.try_get("user_id")?,
                    email: r.try_get("email")?,
                    role: BusinessRole::parse(&r.try_get::<String, _>("role")?)?,
                })
            })
            .collect()
    }
    pub async fn set_business_staff(
        &self,
        token: &str,
        request: &BusinessStaffRequest,
    ) -> Result<BusinessMutationResult, WalletError> {
        validate(&request.idempotency_key, Some(&request.reason))?;
        if request.role == BusinessRole::Owner {
            return Err(WalletError::InvalidInput(
                "use ownership transfer to appoint an owner".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let actor =
            require_capability(&mut tx, business, token, BusinessCapability::StaffManage).await?;
        let detail = json!(request);
        if let Some(id) = replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.staff.set",
            &detail,
        )
        .await?
        {
            return Ok(BusinessMutationResult { id });
        }
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.users WHERE user_id=$1 AND password_hash<>'!deleted')").bind(request.user_id).fetch_one(&mut *tx).await?;
        if !exists {
            return Err(WalletError::NotFound("user is not known".into()));
        }
        let changed=sqlx::query("INSERT INTO oxsum.business_staff (business_id,user_id,role) VALUES ($1,$2,$3) ON CONFLICT (business_id,user_id) DO UPDATE SET role=EXCLUDED.role WHERE business_staff.role<>'owner'").bind(business).bind(request.user_id).bind(request.role.as_str()).execute(&mut *tx).await?.rows_affected();
        if changed == 0 {
            return Err(WalletError::Conflict(
                "use ownership transfer before changing an owner".into(),
            ));
        }
        let result = record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.staff.set",
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn revoke_business_staff(
        &self,
        token: &str,
        request: &BusinessStaffRevokeRequest,
    ) -> Result<BusinessMutationResult, WalletError> {
        validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let actor =
            require_capability(&mut tx, business, token, BusinessCapability::StaffManage).await?;
        let detail = json!(request);
        if let Some(id) = replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.staff.revoke",
            &detail,
        )
        .await?
        {
            return Ok(BusinessMutationResult { id });
        }
        let role: Option<String> = sqlx::query_scalar(
            "SELECT role FROM oxsum.business_staff WHERE business_id=$1 AND user_id=$2",
        )
        .bind(business)
        .bind(request.user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if role.as_deref() == Some("owner") {
            return Err(WalletError::Conflict(
                "transfer ownership before revoking an owner".into(),
            ));
        }
        if role.is_none() {
            return Err(WalletError::NotFound("staff member is not known".into()));
        }
        sqlx::query("DELETE FROM oxsum.business_staff WHERE business_id=$1 AND user_id=$2")
            .bind(business)
            .bind(request.user_id)
            .execute(&mut *tx)
            .await?;
        let result = record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.staff.revoke",
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn transfer_business_ownership(
        &self,
        token: &str,
        request: &BusinessOwnershipRequest,
    ) -> Result<BusinessMutationResult, WalletError> {
        validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let actor = require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::OwnershipTransfer,
        )
        .await?;
        let detail = json!(request);
        if let Some(id) = replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.ownership.transfer",
            &detail,
        )
        .await?
        {
            return Ok(BusinessMutationResult { id });
        }
        if actor.user_id == request.user_id {
            return Err(WalletError::InvalidInput(
                "select another staff member".into(),
            ));
        }
        staff(&mut tx, business, request.user_id).await?;
        sqlx::query("UPDATE oxsum.business_staff SET role=CASE WHEN user_id=$2 THEN 'owner' ELSE 'admin' END WHERE business_id=$1 AND user_id IN ($2,$3)").bind(business).bind(request.user_id).bind(actor.user_id).execute(&mut *tx).await?;
        let result = record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.ownership.transfer",
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn set_business_registration(
        &self,
        token: &str,
        request: &BusinessRegistrationRequest,
    ) -> Result<BusinessMutationResult, WalletError> {
        validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let actor = require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::RegistrationWrite,
        )
        .await?;
        let detail = json!(request);
        if let Some(id) = replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.registration.set",
            &detail,
        )
        .await?
        {
            return Ok(BusinessMutationResult { id });
        }
        sqlx::query("UPDATE oxsum.businesses SET registration_mode=$2 WHERE business_id=$1")
            .bind(business)
            .bind(request.mode.as_str())
            .execute(&mut *tx)
            .await?;
        let result = record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            "business.registration.set",
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
}
/// Held until account creation commits, so closing registration cannot race a
/// checked-but-not-yet-created password or OAuth account.
pub(crate) async fn require_open_registration(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), WalletError> {
    let open:bool=sqlx::query_scalar("SELECT initialized_at IS NOT NULL AND registration_mode='open' FROM oxsum.businesses WHERE singleton FOR SHARE").fetch_one(&mut **tx).await?;
    if !open {
        return Err(WalletError::Forbidden(
            "this business registers by invitation only".into(),
        ));
    }
    Ok(())
}
/// Same business-first order as setup, transfer and staff mutation. A business
/// owner must transfer before deleting their personal account.
pub(crate) async fn remove_staff_for_deletion(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
) -> Result<(), WalletError> {
    let business = lock_business(tx).await?;
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM oxsum.business_staff WHERE business_id=$1 AND user_id=$2",
    )
    .bind(business)
    .bind(user)
    .fetch_optional(&mut **tx)
    .await?;
    if role.as_deref() == Some("owner") {
        return Err(WalletError::Conflict(
            "transfer business ownership before deleting your account".into(),
        ));
    }
    if role.is_some() {
        sqlx::query("DELETE FROM oxsum.business_staff WHERE business_id=$1 AND user_id=$2")
            .bind(business)
            .bind(user)
            .execute(&mut **tx)
            .await?;
        record(
            tx,
            business,
            user,
            &format!("delete-account:{user}"),
            "business.staff.account_deleted",
            &json!({"userId":user}),
        )
        .await?;
    }
    Ok(())
}

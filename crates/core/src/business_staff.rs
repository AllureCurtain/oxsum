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

/// A staff role within the business; each role grants an explicit capability
/// set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BusinessRole {
    /// Full authority, including staff, secrets and ownership transfer.
    Owner,
    /// Manages customers, catalog and routing.
    Admin,
    /// Reads reports and performs commercial, funds and refund writes.
    Finance,
    /// Reads customers and requests and can suspend customers.
    Support,
    /// Read-only overview and reports access.
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
            | C::ProfileWrite
            | C::IntegrationsManage => self == Self::Owner,
        }
    }
    /// Returns every capability this role grants, in `BusinessCapability::ALL`
    /// order.
    #[must_use]
    pub fn capabilities(self) -> Vec<BusinessCapability> {
        BusinessCapability::ALL
            .iter()
            .copied()
            .filter(|c| self.allows(*c))
            .collect()
    }
}
/// One independently grantable business permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BusinessCapability {
    /// Read the business overview.
    OverviewRead,
    /// Read customer records.
    CustomersRead,
    /// Create and update customer records.
    CustomersWrite,
    /// Suspend or restore a customer.
    CustomersSuspend,
    /// Read request history.
    RequestsRead,
    /// Change models, prices and aliases.
    CatalogWrite,
    /// Change routing and fallback configuration.
    RoutingWrite,
    /// Read and change provider credentials.
    ProviderSecretsWrite,
    /// Change sale books and commercial terms.
    CommercialWrite,
    /// Move funds and adjust balances.
    FundsWrite,
    /// Resolve refunds and disputes.
    RefundsWrite,
    /// Read reports and finance lists.
    ReportsRead,
    /// Read and change merchant credentials.
    MerchantSecretsWrite,
    /// Manage staff roles.
    StaffManage,
    /// Transfer business ownership.
    OwnershipTransfer,
    /// Change the registration mode.
    RegistrationWrite,
    /// Manage business integrations.
    IntegrationsManage,
    /// Change the public business identity and help settings.
    ProfileWrite,
}
impl BusinessCapability {
    /// Every capability, in a stable order.
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
        Self::ProfileWrite,
    ];
}
/// The authenticated staff member acting in one business.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessPrincipal {
    /// Business whose authority applies.
    pub business_id: Uuid,
    /// Authenticated user.
    pub user_id: Uuid,
    /// Current staff role.
    pub role: BusinessRole,
    /// Capabilities the role grants.
    pub capabilities: Vec<BusinessCapability>,
}
/// Which authority a session acts under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionContext {
    /// Customer membership authority.
    Customer,
    /// Business staff authority.
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
/// The session's resolved context and available authorities.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextInfo {
    /// Authority the session currently acts under.
    pub context: SessionContext,
    /// Authenticated user.
    pub user: User,
    /// Customer organization selected in customer context, if any.
    pub organization_id: Option<Uuid>,
    /// Business principal and capabilities, when the user holds a staff seat.
    pub business: Option<BusinessPrincipal>,
}
/// Switches the session between customer and business context.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionContextRequest {
    /// Caller-chosen key; a replay answers the standing switch.
    pub idempotency_key: String,
    /// Context to select.
    pub context: SessionContext,
    /// Customer organization to select; must be absent in business context.
    pub organization_id: Option<Uuid>,
}
/// One staff member of the business.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessStaff {
    /// Staff user identifier.
    pub user_id: Uuid,
    /// Staff user's email address.
    pub email: String,
    /// Assigned business role.
    pub role: BusinessRole,
}
/// Sets one user's business role.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessStaffRequest {
    /// Caller-chosen key; a replay answers the standing mutation.
    pub idempotency_key: String,
    /// User to add or update.
    pub user_id: Uuid,
    /// Role to grant; `owner` is refused here.
    pub role: BusinessRole,
    /// Operator-facing reason recorded in the audit trail.
    pub reason: String,
}
/// Removes one user's staff membership.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessStaffRevokeRequest {
    /// Caller-chosen key; a replay answers the standing mutation.
    pub idempotency_key: String,
    /// Staff user to remove.
    pub user_id: Uuid,
    /// Operator-facing reason recorded in the audit trail.
    pub reason: String,
}
/// Transfers ownership to another staff member; the current owner becomes
/// admin.
pub type BusinessOwnershipRequest = BusinessStaffRevokeRequest;
/// How new users may join the business.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RegistrationMode {
    /// Only invited users may register.
    Invite,
    /// Anyone may register.
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
/// Changes the business's registration mode.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BusinessRegistrationRequest {
    /// Caller-chosen key; a replay answers the standing mutation.
    pub idempotency_key: String,
    /// Registration mode to set.
    pub mode: RegistrationMode,
    /// Operator-facing reason recorded in the audit trail.
    pub reason: String,
}
/// Receipt identifying one committed business mutation.
#[derive(Debug, Serialize)]
pub struct BusinessMutationResult {
    /// Mutation receipt id.
    pub id: Uuid,
}

pub(crate) async fn lock_business(tx: &mut Transaction<'_, Postgres>) -> Result<Uuid, WalletError> {
    Ok(
        sqlx::query_scalar("SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE")
            .fetch_one(&mut **tx)
            .await?,
    )
}
pub(crate) struct Identity {
    session: Uuid,
    pub(crate) user: User,
    pub(crate) organization: Option<Uuid>,
    pub(crate) context: String,
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
/// Personal consent and team writes use the current session inside the mutation.
pub(crate) async fn personal_identity(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
) -> Result<Identity, WalletError> {
    let actor = identity(tx, token).await?;
    if !actor.second_factor_satisfied {
        return Err(WalletError::Forbidden(
            "a second-factor verified login is required".into(),
        ));
    }
    Ok(actor)
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
    // The staff principal answers in either context — the console nav needs a
    // membership signal before the session selects business authority (#615).
    // `context` keeps naming which authority the session acts under, so a
    // customer-context answer carrying `business` means the staff seat is
    // claimable through `POST /session/context`, never that it is selected:
    // every `/business/*` call still demands the switch and its second factor.
    // This is a membership read, so it deliberately skips the second-factor
    // gate the business branch enforces.
    let seat: Option<String> = sqlx::query_scalar(
        "SELECT s.role FROM oxsum.business_staff s \
         JOIN oxsum.businesses b USING (business_id) \
         WHERE s.business_id=$1 AND s.user_id=$2 AND b.initialized_at IS NOT NULL",
    )
    .bind(business)
    .bind(actor.user.id)
    .fetch_optional(&mut **tx)
    .await?;
    let principal = seat
        .map(|role| {
            let role = BusinessRole::parse(&role)?;
            Ok::<BusinessPrincipal, WalletError>(BusinessPrincipal {
                business_id: business,
                user_id: actor.user.id,
                role,
                capabilities: role.capabilities(),
            })
        })
        .transpose()?;
    Ok(SessionContextInfo {
        context: SessionContext::Customer,
        user: actor.user,
        organization_id: actor.organization.filter(|_| member),
        business: principal,
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
    /// Resolves the session's current context, user and available authorities.
    pub async fn session_context(&self, token: &str) -> Result<SessionContextInfo, WalletError> {
        let mut tx = self.pool().begin().await?;
        let business = lock_business(&mut tx).await?;
        let actor = identity(&mut tx, token).await?;
        context_info(&mut tx, business, actor).await
    }
    /// Switches the session's context; entering business context requires a
    /// staff seat and a second-factor-verified login.
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
    /// Lists the business's staff members in seat order.
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
    /// Adds a staff member or changes a non-owner's role.
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
    /// Removes a non-owner staff member.
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
    /// Transfers ownership to another staff member, demoting the caller to
    /// admin.
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
    /// Sets whether the business registers by invitation or openly.
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

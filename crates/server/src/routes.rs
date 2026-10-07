use std::sync::Arc;

use axum::extract::{Extension, Path, Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router, middleware};
use oxsum_core::{
    ApiKey, Consistency, CreatedApiKey, CreatedInvitation, CreatedSession, CreatedWebhook,
    HeadSigningKey, KeyPublication, Member, NewUser, Organization, Ownership, Principal,
    Registration, Role, SESSION_COOKIE, Session, SessionPrincipal, SignedHead, Tenants, User,
    UserOrganization, Wallet, WalletError, WebhookDelivery, WebhookEndpoint, signing_key,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{require_principal, session_cookie_value};
use crate::error::{ApiError, ApiJson};
use crate::{AppState, Signup, today};

/// The `/api/v1` surface, plus the routes served outside it: health, the gateway, the
/// billing WebSocket, the bills export and the Leptos pages.
pub fn router(state: AppState) -> Router {
    // Behind a credential: everything that touches an organization, its ledger or its
    // credentials. The credential is an API key or a session cookie; both resolve to a
    // principal.
    let authenticated = Router::new()
        .route("/org", get(organization))
        .route("/orgs", get(my_organizations).post(create_organization))
        .route("/org/keys", get(list_keys).post(create_key))
        .route("/org/keys/{key_id}", delete(revoke_key).patch(patch_key))
        .route("/org/invitations", post(create_invitation))
        .route("/org/members", post(add_member))
        .route(
            "/org/members/{user_id}",
            delete(remove_member).patch(change_member_role),
        )
        .route("/org/ownership", post(transfer_ownership))
        .route("/webhooks", get(list_webhooks).post(create_webhook))
        .route("/webhooks/{endpoint_id}", delete(delete_webhook))
        .route(
            "/webhooks/{endpoint_id}/deliveries",
            get(webhook_deliveries),
        )
        .route("/session", get(session))
        .route("/session/organization", post(switch_organization))
        .route("/topups", post(top_up))
        .route("/redemptions", post(redeem))
        .route("/holds", post(hold))
        .route("/settlements", post(settle))
        .route("/statements", get(statements))
        .route("/statements/{statement_id}", get(statement_detail))
        .route("/balance", get(balance))
        .route("/entries/{entry_id}/proof", get(proof))
        .route("/log/head", get(log_head))
        .route("/log/consistency", get(log_consistency))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_principal,
        ));

    // No credential: signup while the deployment allows it, and the login/logout pair.
    // Logout sits outside the auth middleware on purpose: logging out twice is not an
    // error, so it must answer 200 with no session at all.
    let open = Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        // Registration through an invitation link: the token is the credential, so it
        // stays open in invite mode too — that mode exists for this.
        .route("/invitations/redeem", post(redeem_invitation))
        // The operator's tree-head verifying key: a public key, so no credential.
        .route("/log/key", get(log_key));

    // The platform admin: an operator token, not an organization key, and its own middleware.
    let admin = crate::admin::router(state.clone());

    // The metrics scrape sits at the conventional path rather than under /api/v1/admin, but
    // it answers to the same operator token — a Prometheus scrape config carries it as
    // `bearer_token`.
    let operator = Router::new()
        .route("/metrics", get(crate::metrics::scrape))
        // `route_layer`, not `layer`: a plain layer on a merged router also wraps the
        // combined fallback, so an unmatched path would answer 401 instead of 404.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::admin::require_admin,
        ));

    let router: Router<AppState> = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(operator)
        // Live billing progress for the dashboard: session-cookie auth, like the pages.
        .route("/ws/billing", get(crate::ws::billing_ws))
        // The bills export: the page's table as a file the browser saves, on the
        // dashboard's own surface (the session cookie), not `/api/v1` (docs/decisions.md).
        .route("/dashboard/bills/export.csv", get(crate::bills::export_csv))
        .route(
            "/dashboard/bills/export.json",
            get(crate::bills::export_json),
        )
        .nest("/api/v1", open.merge(authenticated).nest("/admin", admin))
        // The OpenAI-compatible surface, which brings its own auth and its own error format.
        .nest("/v1", crate::gateway::router(state.clone()));
    // The Leptos pages and their server functions, served by the same binary.
    // The request counter sits last so it sees every route the router answered, pages
    // included — labelled by the matched pattern, never the concrete path.
    crate::web::mount(router, &state)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::metrics::track,
        ))
        .with_state(state)
}

/// The uniform success envelope: { "data": ... }.
#[derive(Serialize)]
pub(crate) struct Data<T> {
    data: T,
}

pub(crate) type ApiResult<T> = Result<Json<Data<T>>, ApiError>;

pub(crate) fn ok<T>(data: T) -> ApiResult<T> {
    Ok(Json(Data { data }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterReq {
    email: String,
    password: String,
    organization_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginReq {
    email: String,
    password: String,
}

/// Who a login authenticated, as the API presents it: the user, the organization the
/// session acts as, the role in it, and the session itself.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionInfo {
    user: User,
    organization: Organization,
    role: Role,
    session: Session,
}

impl From<SessionPrincipal> for SessionInfo {
    fn from(principal: SessionPrincipal) -> Self {
        Self {
            user: principal.user,
            organization: principal.organization,
            role: principal.role,
            session: principal.session,
        }
    }
}

/// The logout answer: empty, because the logout is in the `Set-Cookie` header that clears
/// the cookie.
#[derive(Serialize)]
struct LogoutRes {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateKeyReq {
    name: Option<String>,
    /// RFC 3339; absent means a key that never expires.
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    /// Minor units; absent or null means unlimited.
    #[serde(default)]
    spend_limit_minor: Option<i64>,
    /// "daily" | "weekly" | "monthly"; absent keeps the limit cumulative.
    #[serde(default)]
    budget_duration: Option<String>,
    /// The gateway models this key may call; absent or null allows all of them.
    #[serde(default)]
    model_allowlist: Option<Vec<String>>,
    /// Requests inside a rolling minute; absent or null is uncapped.
    #[serde(default)]
    requests_per_minute: Option<i32>,
    /// Holds outstanding at once; absent or null is uncapped.
    #[serde(default)]
    max_concurrent_holds: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateKeyConstraintsReq {
    /// The new limit in minor units; null clears it back to unlimited.
    #[serde(default)]
    spend_limit_minor: Option<i64>,
    /// The window the limit applies to; null clears it back to cumulative.
    #[serde(default)]
    budget_duration: Option<String>,
    /// The models the key may call; null clears the list back to all.
    #[serde(default)]
    model_allowlist: Option<Vec<String>>,
    /// The per-minute request allowance; null clears it back to uncapped.
    #[serde(default)]
    requests_per_minute: Option<i32>,
    /// The outstanding-holds cap; null clears it back to uncapped.
    #[serde(default)]
    max_concurrent_holds: Option<i32>,
}

/// Builds the constraint set both key endpoints carry, parsing the duration's name.
fn key_constraints(r: &UpdateKeyConstraintsReq) -> Result<oxsum_core::KeyConstraints, WalletError> {
    Ok(oxsum_core::KeyConstraints {
        spend_limit_minor: r.spend_limit_minor,
        budget_duration: r
            .budget_duration
            .as_deref()
            .map(oxsum_core::BudgetDuration::parse)
            .transpose()?,
        model_allowlist: r.model_allowlist.clone(),
        requests_per_minute: r.requests_per_minute,
        max_concurrent_holds: r.max_concurrent_holds,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AmountReq {
    idempotency_key: String,
    amount_minor: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettleReq {
    hold_key: String,
    actual_minor: i64,
}

/// The body of `POST /api/v1/redemptions`: the code as minted.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RedeemCodeReq {
    code: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BalanceRes {
    /// Everything the organization can still reserve: own funds plus the
    /// undrawn part of its credit line.
    available_minor: i64,
    /// The credit limit the operator granted; 0 for an organization without one.
    credit_limit_minor: i64,
    /// The drawn plus reserved part of the credit line.
    credit_used_minor: i64,
}

/// The ledger facade of the organization the credential belongs to.
async fn wallet(tenants: &Tenants, organization: &Organization) -> Result<Arc<Wallet>, ApiError> {
    Ok(tenants.get(&organization.tenant_id).await?)
}

async fn register(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<RegisterReq>,
) -> ApiResult<Registration> {
    if state.config.signup() != Signup::Open {
        return Err(ApiError::Forbidden(
            "this deployment registers by invitation only".into(),
        ));
    }
    let registration = state
        .db
        .register(NewUser {
            email: r.email,
            password: r.password,
            organization_name: r.organization_name,
        })
        .await?;
    grant_signup_bonus(&state, &registration).await?;
    ok(registration)
}

/// The signup bonus: `OXSUM_SIGNUP_BONUS_MINOR` credits a brand-new organization's
/// wallet, booked as an adjustment with the registration itself as its reason. Only
/// self-registration grants one — an invitation's redeem joins an existing
/// organization and creates nothing to endow. Idempotent under the organization's
/// id, and a nonzero bonus is what makes registration the ledger's first use
/// rather than the first paid request.
async fn grant_signup_bonus(state: &AppState, registration: &Registration) -> Result<(), ApiError> {
    let bonus = state.config.signup_bonus_minor();
    if bonus == 0 {
        return Ok(());
    }
    let organization = &registration.organization;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    wallet
        .adjust(
            &format!("signup-bonus:{}", organization.id),
            "signup bonus",
            bonus,
            today(),
        )
        .await?;
    Ok(())
}

/// Logs in: verifies the password, mints a session, and answers it in a cookie.
///
/// An unknown email and a wrong password fail identically (401, one message), so neither
/// reveals whether an account exists.
async fn login(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<LoginReq>,
) -> Result<Response, ApiError> {
    let CreatedSession { principal, token } = state.db.login(&r.email, &r.password).await?;
    let mut response = Json(Data {
        data: SessionInfo::from(principal),
    })
    .into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session_cookie(
            &token,
            state.config.session_cookie_secure(),
        ))
        .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

/// Logs out: revokes the session the cookie names and clears the cookie.
///
/// Always 200: logging out twice is not an error, and neither is logging out without a
/// session at all.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some(token) = session_cookie_value(&headers) {
        state.db.logout(&token).await?;
    }
    let mut response = Json(Data { data: LogoutRes {} }).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&clear_session_cookie(state.config.session_cookie_secure()))
            .map_err(|_| ApiError::Internal)?,
    );
    Ok(response)
}

/// The `Set-Cookie` value for a session: `HttpOnly`, `SameSite=Lax`, `Path=/`, and `Secure`
/// when the deployment says so (`OXSUM_SESSION_COOKIE_SECURE`, documented in
/// docs/development.md).
fn session_cookie(token: &str, secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The `Set-Cookie` value that clears the session cookie: an empty value that expires
/// immediately. `Max-Age=0` rather than an `Expires` date in the past: one mechanism, no
/// date formatting.
fn clear_session_cookie(secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The current session: who is logged in, as which organization, in which role.
///
/// A key names no session, so for a key principal this is 404 rather than an empty answer.
async fn session(Extension(principal): Extension<Principal>) -> ApiResult<SessionInfo> {
    match principal {
        Principal::Session(principal) => ok(SessionInfo::from(principal)),
        Principal::Key(_) => Err(ApiError::not_found()),
    }
}

async fn organization(Extension(principal): Extension<Principal>) -> ApiResult<Organization> {
    ok(principal.organization().clone())
}

/// Who may act on memberships of *several* organizations: a person, never a key.
///
/// Listing, creating and switching between organizations are all a person's actions —
/// an API key belongs to exactly one organization and has no user to hold a second
/// membership, so it is refused the same way membership management refuses it
/// (docs/decisions.md, "membership management is a person's action").
fn person(principal: Principal) -> Result<SessionPrincipal, ApiError> {
    match principal {
        Principal::Session(principal) => Ok(principal),
        Principal::Key(_) => Err(ApiError::Forbidden(
            "organizations are a person's business: an API key belongs to one".into(),
        )),
    }
}

/// Every organization the session's user belongs to, with their role — the
/// switcher's list.
async fn my_organizations(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<UserOrganization>> {
    let principal = person(principal)?;
    ok(state.db.organizations_of(principal.user.id).await?)
}

/// The body of `POST /api/v1/orgs`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateOrgReq {
    name: String,
}

/// Creates a team organization and makes the session's user its owner.
async fn create_organization(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<CreateOrgReq>,
) -> ApiResult<Organization> {
    let principal = person(principal)?;
    ok(state
        .db
        .create_team_organization(principal.user.id, &request.name)
        .await?)
}

/// The body of `POST /api/v1/session/organization`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SwitchOrgReq {
    organization_id: Uuid,
}

/// Switches the acting organization: the session row is updated, so the choice
/// carries every request the cookie makes from now on.
async fn switch_organization(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(request): ApiJson<SwitchOrgReq>,
) -> ApiResult<SessionInfo> {
    let mut principal = person(principal)?;
    let (organization, role) = state
        .db
        .switch_organization(
            principal.session.id,
            principal.user.id,
            request.organization_id,
        )
        .await?;
    principal.organization = organization;
    principal.role = role;
    ok(SessionInfo::from(principal))
}

/// The body of `POST /api/v1/invitations/redeem`: the link's token plus the account
/// to create.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RedeemReq {
    token: String,
    email: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWebhookReq {
    url: String,
    events: Vec<String>,
}

/// Mints an invitation link into the session's organization: the token is answered
/// once, here. Owner or admin only — the same rule every membership write follows.
async fn create_invitation(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<CreatedInvitation> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .create_invitation(actor, principal.organization().id)
        .await?)
}

/// Registers through an invitation link. Open in every signup mode — an `invite`
/// deployment's registrations all come through here.
async fn redeem_invitation(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RedeemReq>,
) -> ApiResult<Registration> {
    ok(state
        .db
        .redeem_invitation(
            &request.token,
            NewUser {
                email: request.email,
                password: request.password,
                organization_name: None,
            },
        )
        .await?)
}

async fn create_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    body: Option<ApiJson<CreateKeyReq>>,
) -> ApiResult<CreatedApiKey> {
    let (name, expires_at, constraints) = body
        .map_or(
            Ok((None, None, oxsum_core::KeyConstraints::default())),
            |ApiJson(r)| {
                key_constraints(&UpdateKeyConstraintsReq {
                    spend_limit_minor: r.spend_limit_minor,
                    budget_duration: r.budget_duration,
                    model_allowlist: r.model_allowlist,
                    requests_per_minute: r.requests_per_minute,
                    max_concurrent_holds: r.max_concurrent_holds,
                })
                .map(|c| (r.name, r.expires_at, c))
            },
        )
        .map_err(ApiError::from)?;
    // A key minted through a session records who minted it, for product.md's per-member
    // rules; a key minted with an API key records no creator, because no person acts there.
    ok(state
        .db
        .create_key(
            principal.organization().id,
            name,
            expires_at,
            principal.user_id(),
            constraints,
        )
        .await?)
}

async fn list_keys(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<ApiKey>> {
    ok(state
        .db
        .list_keys(principal.organization().id, principal.key_scope())
        .await?)
}

async fn revoke_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key_id): Path<Uuid>,
) -> ApiResult<ApiKey> {
    // A key id of another organization — or, for a member, a key they did not create — is
    // not found, not forbidden: an id should not be probeable for existence.
    match state
        .db
        .revoke_key(principal.organization().id, key_id, principal.key_scope())
        .await?
    {
        Some(key) => ok(key),
        None => Err(ApiError::not_found()),
    }
}

async fn patch_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(key_id): Path<Uuid>,
    ApiJson(r): ApiJson<UpdateKeyConstraintsReq>,
) -> ApiResult<ApiKey> {
    // The scope rules are the revoke's: a member may change only the keys they created,
    // and any other key id answers 404 with the key untouched, so ids cannot be probed.
    match state
        .db
        .update_key_constraints(
            principal.organization().id,
            key_id,
            principal.key_scope(),
            key_constraints(&r)?,
        )
        .await?
    {
        Some(key) => ok(key),
        None => Err(ApiError::not_found()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddMemberReq {
    email: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangeMemberRoleReq {
    role: AssignableRole,
}

/// The roles a membership may be changed to.
///
/// `owner` is deliberately not a value: ownership is a single seat moved by
/// `POST /api/v1/org/ownership`, so no request body reaches this surface able to promote
/// anyone — the refusal is in the contract rather than in a branch (crates/server/openapi.yaml).
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum AssignableRole {
    Admin,
    Member,
}

impl From<AssignableRole> for Role {
    fn from(role: AssignableRole) -> Self {
        match role {
            AssignableRole::Admin => Self::Admin,
            AssignableRole::Member => Self::Member,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferOwnershipReq {
    user_id: Uuid,
}

/// Adds an existing account to the organization as a member.
///
/// The acting role is checked here and in the domain layer (crates/core/src/orgs.rs): a
/// membership write needs a session in the owner or admin role, so an API key is refused by
/// `membership_actor` before the request reaches a rule.
async fn add_member(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<AddMemberReq>,
) -> ApiResult<Member> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .add_member(principal.organization().id, actor, &r.email)
        .await?)
}

async fn remove_member(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Member> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .remove_member(principal.organization().id, actor, user_id)
        .await?)
}

async fn change_member_role(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(user_id): Path<Uuid>,
    ApiJson(r): ApiJson<ChangeMemberRoleReq>,
) -> ApiResult<Member> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .change_member_role(
            principal.organization().id,
            actor,
            user_id,
            Role::from(r.role),
        )
        .await?)
}

async fn transfer_ownership(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<TransferOwnershipReq>,
) -> ApiResult<Ownership> {
    let actor = principal.membership_actor()?;
    ok(state
        .db
        .transfer_ownership(principal.organization().id, actor, r.user_id)
        .await?)
}

/// Register a webhook endpoint; the signing secret is answered once, here.
async fn create_webhook(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<CreateWebhookReq>,
) -> ApiResult<CreatedWebhook> {
    // Signing needs the sealing key; a deployment without one cannot register an
    // endpoint it could never sign for.
    let Some(secret) = state.config.secret() else {
        return Err(WalletError::Misconfigured(
            "webhooks need OXSUM_SECRET_KEY to sign deliveries".to_owned(),
        )
        .into());
    };
    ok(state
        .db
        .create_webhook(principal.organization().id, &r.url, &r.events, secret)
        .await?)
}

/// The organization's webhook endpoints, newest first.
async fn list_webhooks(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<WebhookEndpoint>> {
    ok(state.db.list_webhooks(principal.organization().id).await?)
}

/// Delete an endpoint — an id of another organization is not found, not forbidden.
async fn delete_webhook(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(endpoint_id): Path<Uuid>,
) -> ApiResult<WebhookEndpoint> {
    match state
        .db
        .delete_webhook(principal.organization().id, endpoint_id)
        .await?
    {
        Some(endpoint) => ok(endpoint),
        None => Err(ApiError::not_found()),
    }
}

/// An endpoint's recent deliveries — an id of another organization is not found.
async fn webhook_deliveries(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(endpoint_id): Path<Uuid>,
) -> ApiResult<Vec<WebhookDelivery>> {
    match state
        .db
        .webhook_deliveries(principal.organization().id, endpoint_id)
        .await?
    {
        Some(deliveries) => ok(deliveries),
        None => Err(ApiError::not_found()),
    }
}

/// Top up the acting organization's wallet.
async fn top_up(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<AmountReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    let receipt = w
        .top_up(&r.idempotency_key, r.amount_minor, today())
        .await?;
    // The manual deposit rail: every funding event leaves a row on deposits for
    // reconciliation (decision D2). The caller's idempotency key is the payment
    // reference, so a replayed top-up sees its own row.
    state
        .db
        .record_manual_deposit(
            principal.organization().id,
            &r.idempotency_key,
            r.amount_minor,
            *receipt.entry_id.as_uuid(),
        )
        .await?;
    // Whatever the deposit repaid counts against the statement book: open
    // statements are repaid oldest first (issue #124).
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    ok(receipt)
}

/// Redeem an operator-minted code into the acting organization's wallet.
async fn redeem(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<RedeemCodeReq>,
) -> ApiResult<oxsum_core::Redemption> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    let redemption = state
        .db
        .redeem_code(&w, principal.organization().id, &r.code, today())
        .await?;
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    ok(redemption)
}

async fn hold(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<AmountReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    // No description: this endpoint takes an amount, not a reason. The gateway, which knows what the
    // hold is for, records one. A key's hold is attributed to the key and checked against its
    // spend limit; a session holds unattributed, with no limit to check.
    match principal.acting_key() {
        Some(key) => {
            // The rolling-minute allowance is spent at admission, the same as the
            // gateway's: a refused hold never reaches the wallet.
            if let Some(rpm) = key.requests_per_minute
                && rpm > 0
                && let Err(limited) =
                    state
                        .rate_limiter
                        .admit(key.key_id, rpm as u32, std::time::Instant::now())
            {
                crate::metrics::rate_limited(&state.metrics, "api");
                return Err(WalletError::RateLimited {
                    limit: i64::from(rpm),
                    retry_after_secs: limited.retry_after.as_secs(),
                }
                .into());
            }
            ok(
                w.hold_for_key(key, None, &r.idempotency_key, "", r.amount_minor, today())
                    .await?,
            )
        }
        None => ok(w
            .hold(&r.idempotency_key, "", r.amount_minor, today())
            .await?),
    }
}

async fn settle(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ApiJson(r): ApiJson<SettleReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    // No description: this endpoint takes a hold key, not a reason. The gateway, which knows what
    // the hold is for, records one.
    ok(w.settle(&r.hold_key, "", r.actual_minor, today()).await?)
}

/// A statement and its itemized lines, as the detail endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatementDetailRes {
    statement: oxsum_core::Statement,
    lines: Vec<oxsum_core::StatementLine>,
}

/// The organization's finalized statements, newest period first — the monthly
/// billing documents "borrow first, settle monthly" issues. Drafts are the
/// operator's working copy and never appear here. The read reconciles first, so
/// a repayment that landed since the last look is already counted, and a pending
/// statement past its due date answers `overdue`.
async fn statements(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<Vec<oxsum_core::Statement>> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    ok(state
        .db
        .organization_statements(principal.organization().id)
        .await?)
}

/// One of the organization's finalized statements, with its lines. A draft,
/// another organization's statement and a missing id all answer 404 — an
/// unissued document does not exist for the organization.
async fn statement_detail(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(statement_id): Path<Uuid>,
) -> ApiResult<StatementDetailRes> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    state
        .db
        .reconcile_statements(principal.organization().id, &w, today())
        .await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if statement.organization_id != principal.organization().id
        || statement.status != oxsum_core::StatementStatus::Finalized
    {
        return Err(ApiError::not_found());
    }
    ok(StatementDetailRes {
        lines: state.db.statement_lines(statement_id).await?,
        statement,
    })
}

async fn balance(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<BalanceRes> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    ok(BalanceRes {
        available_minor: w.available().await?,
        credit_limit_minor: w.credit_limit().await?,
        credit_used_minor: w.credit_used().await?,
    })
}

async fn proof(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(entry_id): Path<Uuid>,
) -> ApiResult<oxsum_core::ProofBundle> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    match w
        .receipt_proof(oxsum_core::EntryId::from_uuid(entry_id))
        .await?
    {
        Some(bundle) => ok(bundle),
        None => Err(ApiError::not_found()),
    }
}

/// A tree head as the API presents it: size plus the lowercase-hex root.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TreeHeadRes {
    size: u64,
    root: String,
}

impl From<oxsum_core::TreeHead> for TreeHeadRes {
    fn from(head: oxsum_core::TreeHead) -> Self {
        Self {
            size: head.size,
            root: head.root.to_hex(),
        }
    }
}

/// The operator's verifying key, as the API publishes it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyingKeyRes {
    key_name: String,
    public_key: String,
    key_hash: String,
}

impl VerifyingKeyRes {
    fn of(key: &KeyPublication) -> Self {
        use base64::Engine as _;
        Self {
            key_name: key.name.clone(),
            public_key: base64::engine::general_purpose::STANDARD.encode(key.public_key),
            key_hash: key.key_hash.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }
}

/// A signed tree head: the note text, the head it attests, and the key that signed it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignedTreeHeadRes {
    note: String,
    origin: String,
    #[serde(flatten)]
    key: VerifyingKeyRes,
    size: u64,
    root: String,
}

impl From<SignedHead> for SignedTreeHeadRes {
    fn from(signed: SignedHead) -> Self {
        Self {
            note: signed.note,
            origin: signed.origin,
            key: VerifyingKeyRes::of(&signed.key),
            size: signed.head.size,
            root: signed.head.root.to_hex(),
        }
    }
}

/// A consistency proof between a held head and the current one, with the new head signed.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsistencyRes {
    note: String,
    #[serde(flatten)]
    key: VerifyingKeyRes,
    old_head: TreeHeadRes,
    head: TreeHeadRes,
    proof: ConsistencyProofRes,
}

/// A consistency proof in doubleentry's order: the hashes that recompute the new root
/// from the old one.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsistencyProofRes {
    old_size: u64,
    new_size: u64,
    path: Vec<String>,
}

impl From<Consistency> for ConsistencyRes {
    fn from(c: Consistency) -> Self {
        Self {
            note: c.signed.note.clone(),
            key: VerifyingKeyRes::of(&c.signed.key),
            old_head: c.old_head.into(),
            head: c.signed.head.into(),
            proof: ConsistencyProofRes {
                old_size: c.proof.old_size,
                new_size: c.proof.new_size,
                path: c.proof.path.iter().map(|h| h.to_hex()).collect(),
            },
        }
    }
}

/// The operator's head-signing key for this request, or 503 when the deployment did not
/// configure one.
///
/// Built per request from the seed: an Ed25519 keypair derive is cheap, and this keeps the
/// key material in [`Config`]'s one place rather than threading a `SigningKey` — which is
/// deliberately not `Clone` — through `AppState`.
fn head_signing_key(state: &AppState) -> Result<HeadSigningKey, ApiError> {
    let seed = state.config.head_signing_seed().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "tree head signing is not configured for this deployment".into(),
        )
    })?;
    // The key name is a constant that satisfies the note rules; a failure here is not a
    // caller error.
    signing_key(seed).map_err(|_| ApiError::Internal)
}

/// The current tree head of the caller's ledger, signed by the operator.
async fn log_head(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<SignedTreeHeadRes> {
    let key = head_signing_key(&state)?;
    let w = wallet(&state.tenants, principal.organization()).await?;
    ok(w.signed_head(&key).await?.into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsistencyQuery {
    from: u64,
}

/// Prove the current head extends the head at `?from=`: the old head recomputed from the
/// log, the new head signed, and the proof between them.
async fn log_consistency(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(q): Query<ConsistencyQuery>,
) -> ApiResult<ConsistencyRes> {
    let key = head_signing_key(&state)?;
    let w = wallet(&state.tenants, principal.organization()).await?;
    ok(w.consistency(q.from, &key).await?.into())
}

/// The operator's tree-head verifying key. A public key, so no credential: fetching it from
/// the server is convenience, and a verifier must have chosen it through a channel the
/// operator does not control.
async fn log_key(State(state): State<AppState>) -> ApiResult<VerifyingKeyRes> {
    let key = head_signing_key(&state)?;
    ok(VerifyingKeyRes::of(&KeyPublication::of(&key)))
}

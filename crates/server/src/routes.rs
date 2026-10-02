use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router, middleware};
use oxsum_core::{
    ApiKey, CreatedApiKey, CreatedSession, NewUser, Organization, Principal, Registration, Role,
    SESSION_COOKIE, Session, SessionPrincipal, Tenants, User, Wallet,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::{require_principal, session_cookie_value};
use crate::error::ApiError;
use crate::{AppState, Signup, today};

/// The `/api/v1` surface, plus the two routes served outside it: health, and the gateway.
pub fn router(state: AppState) -> Router {
    // Behind a credential: everything that touches an organization, its ledger or its
    // credentials. The credential is an API key or a session cookie; both resolve to a
    // principal.
    let authenticated = Router::new()
        .route("/org", get(organization))
        .route("/org/keys", get(list_keys).post(create_key))
        .route("/org/keys/{key_id}", delete(revoke_key))
        .route("/session", get(session))
        .route("/topups", post(top_up))
        .route("/holds", post(hold))
        .route("/settlements", post(settle))
        .route("/balance", get(balance))
        .route("/entries/{entry_id}/proof", get(proof))
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
        .route("/auth/logout", post(logout));

    // The platform admin: an operator token, not an organization key, and its own middleware.
    let admin = crate::admin::router(state.clone());

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", open.merge(authenticated).nest("/admin", admin))
        // The OpenAI-compatible surface, which brings its own auth and its own error format.
        .nest("/v1", crate::gateway::router(state.clone()))
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BalanceRes {
    available_minor: i64,
}

/// The ledger facade of the organization the credential belongs to.
async fn wallet(tenants: &Tenants, organization: &Organization) -> Result<Arc<Wallet>, ApiError> {
    Ok(tenants.get(&organization.tenant_id).await?)
}

async fn register(
    State(state): State<AppState>,
    Json(r): Json<RegisterReq>,
) -> ApiResult<Registration> {
    if state.config.signup() != Signup::Open {
        return Err(ApiError::Forbidden(
            "this deployment registers by invitation only".into(),
        ));
    }
    ok(state
        .db
        .register(NewUser {
            email: r.email,
            password: r.password,
            organization_name: r.organization_name,
        })
        .await?)
}

/// Logs in: verifies the password, mints a session, and answers it in a cookie.
///
/// An unknown email and a wrong password fail identically (401, one message), so neither
/// reveals whether an account exists.
async fn login(
    State(state): State<AppState>,
    Json(r): Json<LoginReq>,
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
        Principal::Key(_) => Err(ApiError::NotFound),
    }
}

async fn organization(Extension(principal): Extension<Principal>) -> ApiResult<Organization> {
    ok(principal.organization().clone())
}

async fn create_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    body: Option<Json<CreateKeyReq>>,
) -> ApiResult<CreatedApiKey> {
    let (name, expires_at) = body.map_or((None, None), |Json(r)| (r.name, r.expires_at));
    // A key minted through a session records who minted it, for product.md's per-member
    // rules; a key minted with an API key records no creator, because no person acts there.
    ok(state
        .db
        .create_key(
            principal.organization().id,
            name,
            expires_at,
            principal.user_id(),
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
        None => Err(ApiError::NotFound),
    }
}

async fn top_up(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(r): Json<AmountReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    ok(w.top_up(&r.idempotency_key, r.amount_minor, today())
        .await?)
}

async fn hold(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(r): Json<AmountReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    // No description: this endpoint takes an amount, not a reason. The gateway, which knows what the
    // hold is for, records one.
    ok(w.hold(&r.idempotency_key, "", r.amount_minor, today())
        .await?)
}

async fn settle(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(r): Json<SettleReq>,
) -> ApiResult<oxsum_core::Receipt> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    // No description: this endpoint takes a hold key, not a reason. The gateway, which knows what
    // the hold is for, records one.
    ok(w.settle(&r.hold_key, "", r.actual_minor, today()).await?)
}

async fn balance(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<BalanceRes> {
    let w = wallet(&state.tenants, principal.organization()).await?;
    ok(BalanceRes {
        available_minor: w.available().await?,
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
        None => Err(ApiError::NotFound),
    }
}

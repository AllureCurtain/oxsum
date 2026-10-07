use std::sync::Arc;

use axum::extract::{Extension, Path, Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router, middleware};
use oxsum_core::{
    ApiKey, Consistency, CreatedApiKey, CreatedInvitation, CreatedSession, CreatedWebhook,
    HeadSigningKey, KeyPublication, Member, NewUser, Organization, Ownership, Principal,
    Registration, Role, SESSION_COOKIE, Session, SessionPrincipal, SignedHead, Tenants, User,
    UserOrganization, Wallet, WalletError, WebhookDelivery, WebhookEndpoint, signing_key,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
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
        // Requesting a verification mail is the session user's own action; the
        // handler refuses an API key, which names no user.
        .route("/auth/verify/request", post(request_verification))
        .route("/topups", post(top_up))
        .route("/redemptions", post(redeem))
        .route("/holds", post(hold))
        .route("/settlements", post(settle))
        .route("/statements", get(statements))
        .route("/statements/{statement_id}", get(statement_detail))
        .route("/balance", get(balance))
        .route("/usage", get(usage))
        .route("/billing-records", get(billing_records))
        .route("/estimate-price", post(estimate_price))
        .route("/pricing", get(pricing))
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
        // OAuth is a browser redirect, not a JSON endpoint: the provider sends
        // the browser here, so both answers are `Location` headers, and every
        // failure lands back on /login rather than on an error body.
        .route("/auth/oauth/github", get(oauth_github))
        .route("/auth/oauth/github/callback", get(oauth_github_callback))
        // Which login methods the deployment offers: the login page's only
        // unauthenticated read, so it knows whether to draw the GitHub button.
        .route("/auth/methods", get(auth_methods))
        // The email flows: verification consumes a mailed token, and the reset
        // pair is open by design — the token is the credential.
        .route("/auth/verify", post(verify_email))
        .route("/auth/password/forgot", post(forgot_password))
        .route("/auth/password/reset", post(reset_password))
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
    /// The Turnstile widget's answer — required when the deployment configures
    /// the anti-bot check, ignored when it does not (issue #154).
    turnstile_token: Option<String>,
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
    require_bot_check(&state, r.turnstile_token.as_deref()).await?;
    let mut registration = state
        .db
        .register(NewUser {
            email: r.email,
            password: r.password,
            organization_name: r.organization_name,
        })
        .await?;
    grant_signup_bonus(&state, &registration).await?;
    registration.verification_sent = send_verification(&state, registration.user.id).await;
    ok(registration)
}

/// Mints a verify token and mails it — when the deployment has a mailer. A send
/// failure logs and answers `false` rather than failing the request it rode in
/// on: the account stands, the mail is retried through `auth/verify/request`.
async fn send_verification(state: &AppState, user_id: Uuid) -> bool {
    let Some(mailer) = state.config.mailer() else {
        return false;
    };
    let Ok(Some(minted)) = state
        .db
        .mint_email_token(user_id, oxsum_core::EmailPurpose::Verify)
        .await
    else {
        return false;
    };
    if let Err(error) = mailer.send_verification(&minted.email, &minted.token).await {
        tracing::warn!(%error, "the verification mail was not sent");
        return false;
    }
    true
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

/// What `GET /api/v1/auth/methods` answers (issue #152).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthMethods {
    oauth_github: bool,
    turnstile_site_key: Option<String>,
}

/// The login page's discovery read: no credential, because it runs before the
/// user has one to present.
async fn auth_methods(State(state): State<AppState>) -> Json<Data<AuthMethods>> {
    Json(Data {
        data: AuthMethods {
            oauth_github: state.config.github().is_some(),
            turnstile_site_key: state.config.turnstile().map(|t| t.site_key().to_owned()),
        },
    })
}

/// The env-gated anti-bot check both account-creation endpoints run (issue
/// #154). Unconfigured, it is a no-op; configured, a missing answer or a
/// `success: false` verdict is `FORBIDDEN`, and a verifier that cannot be
/// reached is `SERVICE_UNAVAILABLE` — the check never fails open.
async fn require_bot_check(state: &AppState, token: Option<&str>) -> Result<(), ApiError> {
    let Some(turnstile) = state.config.turnstile() else {
        return Ok(());
    };
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Err(ApiError::Forbidden("the anti-bot check is required".into()));
    };
    match turnstile.verify(&state.http, token).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(ApiError::Forbidden(
            "the anti-bot check did not pass".into(),
        )),
        Err(why) => {
            tracing::warn!(error = %why.0, "the anti-bot verifier could not be reached");
            Err(ApiError::ServiceUnavailable(
                "the anti-bot check could not be reached".into(),
            ))
        }
    }
}

/// Starts a GitHub OAuth login (issue #152): mints the single-use CSRF state
/// and answers `303` to the provider's authorize page. A deployment without
/// the provider pair configured answers 404 — the login page draws no button
/// for it either, so this is the path a hand-typed URL takes.
async fn oauth_github(State(state): State<AppState>) -> Result<Response, ApiError> {
    let Some(github) = state.config.github() else {
        return Err(ApiError::NotFound("not found".into()));
    };
    let oauth_state = state.db.mint_oauth_state(crate::oauth::PROVIDER).await?;
    Ok(Redirect::to(&github.authorize_url(&oauth_state)).into_response())
}

/// What the provider's redirect carries: `code` and `state` on success,
/// `error` when it refuses (the user declined, the app was mis-registered).
#[derive(Deserialize)]
struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Finishes a GitHub OAuth login. The caller is a browser mid-redirect, so the
/// answer is always a `Location` — `/dashboard` with the session cookie on
/// success, `/login?error=oauth` on any failure; the detail goes to the logs,
/// never to the query string the page would show.
async fn oauth_github_callback(
    State(state): State<AppState>,
    Query(query): Query<OAuthCallbackQuery>,
) -> Response {
    match oauth_github_finish(&state, &query).await {
        Ok(response) => response,
        Err(why) => {
            tracing::warn!(error = %why, "github oauth login failed");
            Redirect::to("/login?error=oauth").into_response()
        }
    }
}

/// The callback's legs, each one a possible refusal: the provider's own
/// `error`, a spent or expired state, the token exchange, the verified-email
/// requirement, and the account resolution — which itself refuses when the
/// deployment registers by invitation only and the identity is new.
async fn oauth_github_finish(
    state: &AppState,
    query: &OAuthCallbackQuery,
) -> Result<Response, String> {
    let Some(github) = state.config.github() else {
        return Err("provider not configured".to_owned());
    };
    if let Some(error) = &query.error {
        return Err(format!("provider refused: {error}"));
    }
    let (Some(code), Some(oauth_state)) = (&query.code, &query.state) else {
        return Err("callback carried no code or state".to_owned());
    };
    let spent = state
        .db
        .consume_oauth_state(oauth_state, crate::oauth::PROVIDER)
        .await
        .map_err(|e| e.to_string())?;
    if !spent {
        return Err("unknown, spent or expired state".to_owned());
    }
    let token = github.exchange(&state.http, code).await.map_err(|e| e.0)?;
    let identity = github
        .identity(&state.http, &token)
        .await
        .map_err(|e| e.0)?;
    let created = state
        .db
        .oauth_login(&identity, state.config.signup() == Signup::Open)
        .await
        .map_err(|e| e.to_string())?;
    let mut response = Redirect::to("/dashboard").into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session_cookie(
            &created.token,
            state.config.session_cookie_secure(),
        ))
        .map_err(|_| "cookie header rejected the token".to_owned())?,
    );
    Ok(response)
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
    /// The Turnstile widget's answer — required when the deployment configures
    /// the anti-bot check, ignored when it does not (issue #154).
    turnstile_token: Option<String>,
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
    // Before touching the invitation: a forged token costs the check a
    // verifier call, which the verifier rate-limits far better than our
    // redemption table would take it.
    require_bot_check(&state, request.turnstile_token.as_deref()).await?;
    let mut registration = state
        .db
        .redeem_invitation(
            &request.token,
            NewUser {
                email: request.email,
                password: request.password,
                organization_name: None,
            },
        )
        .await?;
    registration.verification_sent = send_verification(&state, registration.user.id).await;
    ok(registration)
}

/// What `POST /api/v1/auth/verify/request` answers: whether a verification mail
/// went out, or that the address was verified already.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyRequestRes {
    sent: bool,
    already_verified: bool,
}

/// Mails the session user a verification link. The one email-flow endpoint that
/// reports a missing mailer: sending the mail is its whole job (`SERVICE_UNAVAILABLE`).
/// A resend inside the sixty-second cooldown mints and mails nothing (`sent: false`).
async fn request_verification(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<VerifyRequestRes> {
    let Some(user) = principal.user() else {
        return Err(ApiError::Forbidden(
            "verifying an email is a person's action: an API key names no user".into(),
        ));
    };
    let Some(mailer) = state.config.mailer() else {
        return Err(ApiError::ServiceUnavailable(
            "email is not configured on this deployment".into(),
        ));
    };
    if user.email_verified {
        return ok(VerifyRequestRes {
            sent: false,
            already_verified: true,
        });
    }
    let Some(minted) = state
        .db
        .mint_email_token(user.id, oxsum_core::EmailPurpose::Verify)
        .await?
    else {
        return ok(VerifyRequestRes {
            sent: false,
            already_verified: false,
        });
    };
    mailer
        .send_verification(&minted.email, &minted.token)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "the verification mail was not sent");
            ApiError::Internal
        })?;
    ok(VerifyRequestRes {
        sent: true,
        already_verified: false,
    })
}

/// The body of `POST /api/v1/auth/verify`: the mailed token.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VerifyEmailReq {
    token: String,
}

/// What `POST /api/v1/auth/verify` answers.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifiedRes {
    verified: bool,
}

/// Consumes a verification token and marks its user's email verified. An unknown,
/// spent or expired token is `NOT_FOUND` — which is wrong is the holder's business.
async fn verify_email(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<VerifyEmailReq>,
) -> ApiResult<VerifiedRes> {
    let Some(user_id) = state
        .db
        .consume_email_token(&r.token, oxsum_core::EmailPurpose::Verify)
        .await?
    else {
        return Err(ApiError::NotFound("not found".into()));
    };
    state.db.mark_email_verified(user_id).await?;
    ok(VerifiedRes { verified: true })
}

/// The body of `POST /api/v1/auth/password/forgot`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForgotPasswordReq {
    email: String,
}

/// Mails a reset link when the address has an account — and answers the same
/// empty 200 when it does not, so the endpoint enumerates no accounts.
async fn forgot_password(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<ForgotPasswordReq>,
) -> Result<Response, ApiError> {
    if let Some(mailer) = state.config.mailer()
        && let Some(minted) = state
            .db
            .mint_email_token_for_address(&r.email, oxsum_core::EmailPurpose::Reset)
            .await?
        && let Err(error) = mailer
            .send_password_reset(&minted.email, &minted.token)
            .await
    {
        // A send failure still answers 200: the answer must not say whether the
        // account exists, and the next request mints a fresh link.
        tracing::warn!(%error, "the reset mail was not sent");
    }
    Ok(Json(Data { data: json!({}) }).into_response())
}

/// The body of `POST /api/v1/auth/password/reset`: the mailed token and the new
/// password.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetPasswordReq {
    token: String,
    password: String,
}

/// What `POST /api/v1/auth/password/reset` answers: how many sessions died with
/// the old password.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResetPasswordRes {
    sessions_revoked: u64,
}

/// Consumes a reset token, sets the new password and revokes every session the
/// user holds — the admin reset's semantics, self-served through the mail.
async fn reset_password(
    State(state): State<AppState>,
    ApiJson(r): ApiJson<ResetPasswordReq>,
) -> ApiResult<ResetPasswordRes> {
    let Some(user_id) = state
        .db
        .consume_email_token(&r.token, oxsum_core::EmailPurpose::Reset)
        .await?
    else {
        return Err(ApiError::NotFound("not found".into()));
    };
    let sessions_revoked = state.db.reset_password(user_id, &r.password).await?;
    ok(ResetPasswordRes { sessions_revoked })
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

/// The widest window `GET /api/v1/usage` answers, in days — the rollup reads one
/// row per bucket, so the bound guards the scan, not the answer's size.
const USAGE_WINDOW_DAYS: i64 = 92;

/// The page bounds `GET /api/v1/billing-records` accepts.
const RECORDS_DEFAULT_LIMIT: usize = 50;
const RECORDS_MAX_LIMIT: usize = 200;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageQuery {
    from: Option<String>,
    to: Option<String>,
}

/// The keys this principal may attribute usage to: an API key or an owner/admin
/// session sees every key of the organization, a member only their own — the
/// dashboard's `key_scope` rule applied to the REST surface.
async fn scoped_key_ids(
    state: &AppState,
    principal: &Principal,
) -> Result<Option<Vec<String>>, ApiError> {
    let scope = principal.key_scope();
    if matches!(
        scope,
        oxsum_core::KeyScope::Organization | oxsum_core::KeyScope::All
    ) {
        return Ok(None);
    }
    let keys = state
        .db
        .list_keys(principal.organization().id, scope)
        .await?;
    Ok(Some(
        keys.iter()
            .map(|key| key.id.as_simple().to_string())
            .collect(),
    ))
}

/// A row's attribution visible to the principal: unattributed rows are
/// organization history everyone reads, a key outside the scope drops the row.
fn in_scope(key_id: Option<&str>, scoped: &Option<Vec<String>>) -> bool {
    match (key_id, scoped) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(id), Some(keys)) => keys.iter().any(|key| key == id),
    }
}

fn parse_day(value: &str) -> Result<time::Date, ApiError> {
    time::Date::parse(
        value,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|_| ApiError::Validation(format!("{value:?} is not a YYYY-MM-DD date")))
}

async fn usage(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<UsageQuery>,
) -> ApiResult<UsageRes> {
    let to = query
        .to
        .as_deref()
        .map(parse_day)
        .transpose()?
        .unwrap_or_else(today);
    let from = query
        .from
        .as_deref()
        .map(parse_day)
        .transpose()?
        .unwrap_or_else(|| to - time::Duration::days(USAGE_WINDOW_DAYS - 1));
    if from > to {
        return Err(ApiError::Validation("`from` must not be after `to`".into()));
    }
    if to - from >= time::Duration::days(USAGE_WINDOW_DAYS) {
        return Err(ApiError::Validation(format!(
            "the window is bounded to {USAGE_WINDOW_DAYS} days"
        )));
    }
    let organization = principal.organization();
    let days = state
        .db
        .usage_daily(&organization.tenant_id, from, to)
        .await?;
    let scoped = scoped_key_ids(&state, &principal).await?;
    let days = days
        .into_iter()
        .filter(|row| {
            in_scope(
                row.key_id.map(|id| id.as_simple().to_string()).as_deref(),
                &scoped,
            )
        })
        .collect();
    ok(UsageRes { days })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageRes {
    days: Vec<oxsum_core::UsageDay>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordsQuery {
    before: Option<u64>,
    limit: Option<usize>,
}

async fn billing_records(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Query(query): Query<RecordsQuery>,
) -> ApiResult<RecordsRes> {
    let limit = query.limit.unwrap_or(RECORDS_DEFAULT_LIMIT);
    if !(1..=RECORDS_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::Validation(format!(
            "limit must be 1 to {RECORDS_MAX_LIMIT}"
        )));
    }
    let w = wallet(&state.tenants, principal.organization()).await?;
    let page = w.requests_page(query.before, limit).await?;
    let scoped = scoped_key_ids(&state, &principal).await?;
    ok(RecordsRes {
        rows: page
            .rows
            .into_iter()
            .filter(|row| in_scope(row.key_id.as_deref(), &scoped))
            .collect(),
        next_cursor: page.next_cursor,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordsRes {
    rows: Vec<oxsum_core::RequestEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EstimateReq {
    model: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    service_tier: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EstimateRes {
    estimate_minor: i64,
    model: String,
    version: i64,
}

/// The freeze a request of this shape would take — `Price::estimate_minor`, the
/// same arithmetic the gateway's hold path runs, so the number is the settle's
/// ceiling rather than a guess. Nothing is held and nothing is charged.
async fn estimate_price(
    State(state): State<AppState>,
    Extension(_principal): Extension<Principal>,
    ApiJson(r): ApiJson<EstimateReq>,
) -> ApiResult<EstimateRes> {
    let Some(priced) = state.db.priced(&r.model).await? else {
        return Err(ApiError::Validation(format!(
            "no channel serves model {:?}",
            r.model
        )));
    };
    let estimate = priced.price.estimate_minor(
        r.input_tokens.unwrap_or(0),
        r.output_tokens,
        r.service_tier.as_deref(),
    )?;
    ok(EstimateRes {
        estimate_minor: estimate,
        model: r.model,
        version: priced.version,
    })
}

/// The public catalog: every model's current price version. The query never
/// selects channel credentials, so there is nothing to withhold here.
async fn pricing(
    State(state): State<AppState>,
    Extension(_principal): Extension<Principal>,
) -> ApiResult<PricingRes> {
    ok(PricingRes {
        models: state.db.catalog().await?,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PricingRes {
    models: Vec<oxsum_core::CatalogModel>,
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

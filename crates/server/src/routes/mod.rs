//! The `/api/v1` surface and the routes served outside it: health, the gateway,
//! the billing WebSocket, the bills export and the dashboard pages.
//!
//! One module per resource group; `mod` itself only wires the router and holds
//! the envelope types and helpers every module shares.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router, middleware};
use oxsum_core::{
    Organization, Principal, Role, SESSION_COOKIE, Session, SessionPrincipal, Tenants, User, Wallet,
};
use serde::{Deserialize, Serialize};
use tower_http::timeout::TimeoutLayer;
use uuid::Uuid;

use crate::AppState;
use crate::auth::require_principal;
use crate::error::ApiError;

mod auth;
pub(crate) mod billing;
mod device;
mod keys;
mod log;
mod org;
mod usage;
mod webhooks;

/// Backstop for every request on the API and gateway surfaces: no handler may
/// run longer than this. It bounds handler execution — the time until the
/// response headers are produced — not a streaming body, which keeps flowing
/// after the headers: a stalled mid-stream upstream is the gateway client's
/// own timeout policy, deliberately untouched here (see `gateway/mod.rs`).
/// The billing WebSocket, the pages and the health/metrics endpoints are
/// excluded on purpose: the socket is long-lived by design, the rest answer
/// in milliseconds and need no backstop.
///
/// On timeout the client gets a bare 408, not the uniform error envelope:
/// the envelope is built by handlers, and a handler that never answered in
/// fifteen minutes is past the point where envelope fidelity matters.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The `/api/v1` surface, plus the routes served outside it: health, the gateway, the
/// billing WebSocket, the bills export and the dashboard pages.
pub fn router(state: AppState) -> Router {
    // Behind a credential: everything that touches an organization, its ledger or its
    // credentials. The credential is an API key or a session cookie; both resolve to a
    // principal.
    let authenticated = Router::new()
        .route("/org", get(org::organization))
        .route(
            "/orgs",
            get(org::my_organizations).post(org::create_organization),
        )
        .route("/org/keys", get(keys::list_keys).post(keys::create_key))
        .route(
            "/org/keys/{key_id}",
            delete(keys::revoke_key).patch(keys::patch_key),
        )
        .route("/org/invitations", post(org::create_invitation))
        .route("/org/members", post(org::add_member))
        .route(
            "/org/members/{user_id}",
            delete(org::remove_member).patch(org::update_member),
        )
        .route("/org/ownership", post(org::transfer_ownership))
        .route(
            "/webhooks",
            get(webhooks::list_webhooks).post(webhooks::create_webhook),
        )
        .route("/webhooks/{endpoint_id}", delete(webhooks::delete_webhook))
        .route(
            "/webhooks/{endpoint_id}/deliveries",
            get(webhooks::webhook_deliveries),
        )
        .route("/session", get(org::session))
        .route("/session/organization", post(org::switch_organization))
        // Deleting the account is a person's action — the handler refuses an
        // API key, which names no user (issue #202).
        .route("/users/me", delete(org::delete_account))
        // Requesting a verification mail is the session user's own action; the
        // handler refuses an API key, which names no user.
        .route("/auth/verify/request", post(auth::request_verification))
        // The device grant's user leg — a person's verdict, so the handlers
        // refuse an API key, which names no user (issue #156).
        .route("/device/request", get(device::device_request))
        .route("/device/authorize", post(device::device_authorize))
        .route("/topups", post(billing::top_up))
        .route("/topups/stripe", post(billing::stripe_topup))
        .route("/redemptions", post(billing::redeem))
        .route("/holds", post(billing::hold))
        .route("/settlements", post(billing::settle))
        .route("/statements", get(billing::statements))
        .route("/statements/{statement_id}", get(billing::statement_detail))
        .route(
            "/statements/{statement_id}/export.pdf",
            get(billing::statement_pdf),
        )
        .route("/balance", get(billing::balance))
        .route("/usage", get(usage::usage))
        .route("/budget", get(usage::budget))
        .route("/billing-records", get(usage::billing_records))
        .route("/estimate-price", post(usage::estimate_price))
        .route("/pricing", get(usage::pricing))
        .route("/entries/{entry_id}/proof", get(log::proof))
        .route("/log/head", get(log::log_head))
        .route("/log/consistency", get(log::log_consistency))
        .route("/log/price-head", get(log::log_price_head))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_principal,
        ));

    // No credential: signup while the deployment allows it, and the login/logout pair.
    // Logout sits outside the auth middleware on purpose: logging out twice is not an
    // error, so it must answer 200 with no session at all.
    let open = Router::new()
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        // OAuth is a browser redirect, not a JSON endpoint: the provider sends
        // the browser here, so both answers are `Location` headers, and every
        // failure lands back on /login rather than on an error body.
        .route("/auth/oauth/github", get(auth::oauth_github))
        .route(
            "/auth/oauth/github/callback",
            get(auth::oauth_github_callback),
        )
        // Which login methods the deployment offers: the login page's only
        // unauthenticated read, so it knows whether to draw the GitHub button.
        .route("/auth/methods", get(auth::auth_methods))
        // The email flows: verification consumes a mailed token, and the reset
        // pair is open by design — the token is the credential.
        .route("/auth/verify", post(auth::verify_email))
        .route("/auth/password/forgot", post(auth::forgot_password))
        .route("/auth/password/reset", post(auth::reset_password))
        // Registration through an invitation link: the token is the credential, so it
        // stays open in invite mode too — that mode exists for this.
        .route("/invitations/redeem", post(org::redeem_invitation))
        // The device grant's tool leg: the device code is the credential, so
        // both endpoints stay open in every signup mode (issue #156).
        .route("/device/code", post(device::device_code))
        .route("/device/token", post(device::device_token))
        // The operator's tree-head verifying key: a public key, so no credential.
        .route("/log/key", get(log::log_key))
        // Stripe's webhook: the provider calls here, the `Stripe-Signature`
        // header is the credential — org-agnostic, so it must not sit behind
        // the session or the API key.
        .route("/webhooks/stripe", post(billing::stripe_webhook));

    // The platform admin: an operator token, not an organization key, and its own middleware.
    let admin = crate::admin::router(state.clone());
    // The metering surface's own credential: a service credential, never an
    // organization's key or a session — an organization can never report its
    // own usage (issue #172).
    let metering = crate::metering::router(state.clone());

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
        .nest(
            "/api/v1",
            open.merge(authenticated)
                .nest("/admin", admin)
                .nest("/metering", metering)
                // The backstop sits on the API and gateway nests, not the top
                // router: the billing WebSocket below is long-lived by design
                // and must never be cut by a request timeout.
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    REQUEST_TIMEOUT,
                )),
        )
        // The OpenAI-compatible surface, which brings its own auth and its own error format.
        .nest(
            "/v1",
            crate::gateway::router(state.clone()).layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                REQUEST_TIMEOUT,
            )),
        );
    // The dashboard pages, served by the same binary.
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
/// Who a login authenticated, as the API presents it: the user, the organization the
/// session acts as, the role in it, and the session itself.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionInfo {
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
pub(crate) async fn wallet(
    tenants: &Tenants,
    organization: &Organization,
) -> Result<Arc<Wallet>, ApiError> {
    Ok(tenants.get(&organization.tenant_id).await?)
}
pub(crate) fn session_cookie(token: &str, secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The `Set-Cookie` value that clears the session cookie: an empty value that expires
/// immediately. `Max-Age=0` rather than an `Expires` date in the past: one mechanism, no
/// date formatting.
pub(crate) fn clear_session_cookie(secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The device-grant person check both user-side handlers share: a session
/// names a user, an API key does not.
pub(crate) fn person_only(principal: &Principal) -> Result<Uuid, ApiError> {
    principal.user_id().ok_or_else(|| {
        ApiError::Forbidden(
            "a device request's verdict is a person's action: an API key names no user".into(),
        )
    })
}

/// Who may act on memberships of *several* organizations: a person, never a key.
///
/// Listing, creating and switching between organizations are all a person's actions —
/// an API key belongs to exactly one organization and has no user to hold a second
/// membership, so it is refused the same way membership management refuses it
/// (docs/decisions.md, "membership management is a person's action").
pub(crate) fn person(principal: Principal) -> Result<SessionPrincipal, ApiError> {
    match principal {
        Principal::Session(principal) => Ok(principal),
        Principal::Key(_) => Err(ApiError::Forbidden(
            "organizations are a person's business: an API key belongs to one".into(),
        )),
    }
}

/// The `/verify` URL the PDF's footer and QR point at: `OXSUM_PUBLIC_URL`
/// plus the path when the deployment names one, else the relative path — the
/// same rule `verification_uri` uses for the device page.
pub(crate) fn verify_url(state: &AppState) -> String {
    match state.config.public_url() {
        Some(base) => format!("{}/verify", base.trim_end_matches('/')),
        None => "/verify".to_owned(),
    }
}

pub(crate) fn parse_day(value: &str) -> Result<time::Date, ApiError> {
    time::Date::parse(
        value,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|_| ApiError::Validation(format!("{value:?} is not a YYYY-MM-DD date")))
}

/// Peels one option layer serde does not: a bare `Option<Option<i64>>` field
/// answers `None` for both an absent field and an explicit `null`, while the
/// PATCH needs `null` to mean "clear the budget" (issue #162).
pub(crate) fn double_option_i64<'de, D>(deserializer: D) -> Result<Option<Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<i64>::deserialize(deserializer).map(Some)
}

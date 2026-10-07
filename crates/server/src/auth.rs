//! Authentication: two credentials, one principal.
//!
//! A request may carry `Authorization: Bearer oxs-…` (an organization's API key) or the
//! session cookie (a logged-in user). Both resolve to a [`Principal`]: a key acts *as* the
//! organization, a session as the user, the organization they act as, and their role in it.
//! The middleware resolves the credential once and hands the principal to the handler through
//! the request extensions. Nothing else in the request can name an organization.
//!
//! Two middlewares, two resolutions: `/api/v1` answers in oxsum's error envelope and `/v1`
//! in OpenAI's, because each surface's client parses its own. The gateway surface takes an
//! API key only: a session cookie is never accepted there.

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::middleware::Next;
use axum::response::Response;
use oxsum_core::{ActingKey, KeyPrincipal, Organization, Principal, SESSION_COOKIE};

use crate::AppState;
use crate::error::ApiError;
use crate::gateway::error::GatewayError;

/// Rejects a request without a usable credential, and resolves the principal behind it.
///
/// Every failure looks the same to the caller — missing credential, malformed, unknown,
/// revoked, expired — so a probe cannot tell a live credential from a dead one. An
/// explicitly presented bearer token is resolved as a key and never falls through to the
/// cookie: a wrong credential fails, it does not get a second chance.
pub async fn require_principal(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let principal = resolve(&state, request.headers())
        .await
        .ok_or(ApiError::Unauthorized)?;
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

/// The same, for the OpenAI-compatible surface, where a refusal has to look like OpenAI's.
///
/// Keys only: the session cookie is never read here, so a logged-in browser cannot spend
/// through the gateway and the gateway's credential stays the API key alone. The acting key
/// goes in beside the organization: the gateway's hold path enforces its spend limit.
pub async fn require_key_gateway(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    let (organization, key) = resolve_key(&state, request.headers())
        .await
        .ok_or(GatewayError::Unauthorized)?;
    request.extensions_mut().insert(organization);
    request.extensions_mut().insert(key);
    Ok(next.run(request).await)
}

/// The principal the presented credential authenticates, if it authenticates one.
///
/// A bearer token wins over the cookie: the explicitly supplied credential beats the
/// ambient one. With no bearer credential at all, the session cookie is tried.
async fn resolve(state: &AppState, headers: &HeaderMap) -> Option<Principal> {
    if bearer(headers).is_some() {
        let (organization, key) = resolve_key(state, headers).await?;
        return Some(Principal::Key(KeyPrincipal { organization, key }));
    }
    let token = session_cookie_value(headers)?;
    state
        .db
        .authenticate_session(&token)
        .await
        .ok()
        .flatten()
        .map(Principal::Session)
}

/// The organization the presented API key spends for, and the key that acted, if the
/// credential is a usable one.
///
/// The credential arrives as `Authorization: Bearer`, or as `x-api-key` — the spelling an
/// Anthropic SDK sends; both name the same organization key on `/v1`.
async fn resolve_key(state: &AppState, headers: &HeaderMap) -> Option<(Organization, ActingKey)> {
    let presented = bearer(headers).or_else(|| api_key_header(headers))?;
    state.db.authenticate(presented).await.ok().flatten()
}

/// The `Bearer` credential, if the header is there and readable.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
}

/// The `x-api-key` credential, if the header is there and readable — Anthropic's spelling
/// of the same API key the Bearer scheme carries.
fn api_key_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-api-key")?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
}

/// The session cookie's value, if the `Cookie` header carries one. Shared with the logout
/// route, which reads the cookie outside the auth middleware.
pub(crate) fn session_cookie_value(headers: &HeaderMap) -> Option<String> {
    let cookies = headers.get(COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == SESSION_COOKIE)
            .then(|| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

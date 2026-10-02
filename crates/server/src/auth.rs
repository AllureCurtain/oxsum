//! API-key authentication.
//!
//! The credential decides which organization a request acts for, so the middleware resolves
//! the key once and hands the organization to the handler through the request extensions.
//! Nothing else in the request can name an organization.
//!
//! Two middlewares, one resolution: `/api/v1` answers in oxsum's error envelope and `/v1` in
//! OpenAI's, because each surface's client parses its own.

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use oxsum_core::Organization;

use crate::AppState;
use crate::error::ApiError;
use crate::gateway::error::GatewayError;

/// Rejects a request without a usable API key, and resolves the key's organization.
///
/// Every failure looks the same to the caller — missing header, malformed key, unknown key,
/// revoked key, expired key — so a probe cannot tell a live secret from a dead one.
pub async fn require_key(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let organization = resolve(&state, request.headers())
        .await
        .ok_or(ApiError::Unauthorized)?;
    request.extensions_mut().insert(organization);
    Ok(next.run(request).await)
}

/// The same, for the OpenAI-compatible surface, where a refusal has to look like OpenAI's.
pub async fn require_key_gateway(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, GatewayError> {
    let organization = resolve(&state, request.headers())
        .await
        .ok_or(GatewayError::Unauthorized)?;
    request.extensions_mut().insert(organization);
    Ok(next.run(request).await)
}

/// The organization the presented credential acts for, if it is a usable one.
async fn resolve(state: &AppState, headers: &HeaderMap) -> Option<Organization> {
    let presented = bearer(headers)?;
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

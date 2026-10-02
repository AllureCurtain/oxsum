//! API-key authentication.
//!
//! The credential decides which organization a request acts for, so the middleware resolves
//! the key once and hands the organization to the handler through the request extensions.
//! Nothing else in the request can name an organization.

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;
use crate::error::ApiError;

/// Rejects a request without a usable API key, and resolves the key's organization.
///
/// Every failure looks the same to the caller — missing header, malformed key, unknown key,
/// revoked key, expired key — so a probe cannot tell a live secret from a dead one.
pub async fn require_key(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = bearer(request.headers()).ok_or(ApiError::Unauthorized)?;
    let organization = state
        .db
        .authenticate(presented)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    request.extensions_mut().insert(organization);
    Ok(next.run(request).await)
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

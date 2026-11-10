//! Authentication: two credentials, one principal.
//!
//! A request may carry `Authorization: Bearer oxs-…` (an organization's API key) or the
//! session cookie (a logged-in user). Both resolve to a [`Principal`]: a consumption key
//! spends for its customer and reads its own records; a session carries a user's role.
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
        .await?
        .ok_or(ApiError::Unauthorized)?;
    let path = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str())
        .unwrap_or("");
    principal.require_customer(customer_route_capability(request.method().as_str(), path))?;
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
        .map_err(GatewayError::from)?
        .ok_or(GatewayError::Unauthorized)?;
    request.extensions_mut().insert(organization);
    request.extensions_mut().insert(key);
    Ok(next.run(request).await)
}

/// The metering surface accepts only current integration credentials. Scope
/// and selected customer are rechecked inside the core authorization transaction.
pub async fn require_service(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let service = match bearer(request.headers()) {
        Some(secret) => state.db.authenticate_service(secret).await?,
        None => None,
    }
    .ok_or(ApiError::Unauthorized)?;
    request.extensions_mut().insert(service);
    Ok(next.run(request).await)
}

/// The principal the presented credential authenticates, if it authenticates one.
///
/// A bearer token wins over the cookie: the explicitly supplied credential beats the
/// ambient one. Only the absence of every machine credential header permits cookie auth.
///
/// The credential lookups' errors propagate rather than folding into `None` —
/// `Ok(None)` is "this credential authenticates nothing", while a storage
/// failure is not a bad credential: answering `UNAUTHORIZED` for a saturated
/// pool would misreport the service's own 429-class failure as the caller's
/// 401 (the stray authentication refusal issue #486's probe saw). Only a lookup
/// that ran says anything about the credential.
async fn resolve(state: &AppState, headers: &HeaderMap) -> Result<Option<Principal>, ApiError> {
    if has_machine_credential(headers) {
        let Some((organization, key)) =
            resolve_key(state, headers).await.map_err(ApiError::from)?
        else {
            return Ok(None);
        };
        return Ok(Some(Principal::Key(KeyPrincipal { organization, key })));
    }
    let Some(token) = session_cookie_value(headers) else {
        return Ok(None);
    };
    Ok(state
        .db
        .authenticate_session(&token)
        .await?
        .map(Principal::Session))
}

/// The organization the presented API key spends for, and the key that acted, if the
/// credential is a usable one.
///
/// The credential arrives as `Authorization: Bearer`, as `x-api-key` — the spelling an
/// Anthropic SDK sends — or as `x-goog-api-key`, the spelling a Gemini SDK sends; all
/// three name the same organization key on `/v1`. The error type is `WalletError` so
/// each surface maps it into its own envelope — `/api/v1` and `/v1` refuse a
/// saturated pool in their own shape.
async fn resolve_key(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<(Organization, ActingKey)>, oxsum_core::WalletError> {
    let presented = if headers.contains_key(AUTHORIZATION) {
        bearer(headers)
    } else if headers.contains_key("x-api-key") {
        api_key_header(headers)
    } else {
        goog_api_key_header(headers)
    };
    let Some(presented) = presented else {
        return Ok(None);
    };
    state.db.authenticate(presented).await
}

/// The `Bearer` credential, if the header is there and readable.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
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

/// The `x-goog-api-key` credential, if the header is there and readable — the Gemini
/// SDKs' spelling of the same organization key (issue #494).
fn goog_api_key_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-goog-api-key")?
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

/// An explicitly selected machine identity cannot inherit ambient browser rights.
pub(crate) fn has_machine_credential(headers: &HeaderMap) -> bool {
    headers.contains_key(AUTHORIZATION)
        || headers.contains_key("x-api-key")
        || headers.contains_key("x-goog-api-key")
}

/// Explicit allowlists. Axum's matched route (not a caller-supplied path string)
/// selects the resource class; new paths fail closed until classified here.
fn customer_route_capability(method: &str, path: &str) -> oxsum_core::CustomerCapability {
    use oxsum_core::CustomerCapability as C;
    // Nested MatchedPath includes the API prefix in the production router.
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    match (method, path) {
        (
            "GET",
            "/org"
            | "/org/keys"
            | "/org/keys/{key_id}/entitlement"
            | "/holds"
            | "/bills"
            | "/org/promo-lots"
            | "/entries"
            | "/usage"
            | "/billing-records"
            | "/requests"
            | "/operations"
            | "/operations/{operation_id}"
            | "/bundles"
            | "/plans"
            | "/plans/{plan_version_id}"
            | "/pricing"
            | "/collection-terms"
            | "/entries/{entry_id}/proof"
            | "/log/head"
            | "/log/consistency"
            | "/log/checkpoint"
            | "/log/price-head"
            | "/log/sale-head",
        )
        | ("POST", "/estimate-price") => C::ScopedRead,
        ("POST", "/topups" | "/holds" | "/settlements" | "/org/members") => C::Retired,
        (
            "GET",
            "/webhooks"
            | "/webhooks/{endpoint_id}/deliveries"
            | "/statements"
            | "/statements/{statement_id}"
            | "/statements/{statement_id}/proof"
            | "/statements/{statement_id}/export.pdf"
            | "/org/usage-report"
            | "/org/usage-reports"
            | "/org/usage-reports/{report_id}"
            | "/org/usage-reports/links"
            | "/org/evidence-bundles"
            | "/org/evidence-bundles/{bundle_id}"
            | "/org/evidence-bundles/{bundle_id}/share"
            | "/org/statements/{statement_id}/evidence"
            | "/org/experiments"
            | "/org/experiments/{experiment_id}/results",
        )
        | (
            "POST",
            "/webhooks"
            | "/org/usage-reports"
            | "/org/usage-reports/links"
            | "/org/usage-reports/{report_id}/links"
            | "/org/evidence-bundles"
            | "/org/evidence-bundles/{bundle_id}/share"
            | "/org/experiments/{name}/end",
        )
        | (
            "DELETE",
            "/webhooks/{endpoint_id}"
            | "/org/usage-reports/links/{link_id}"
            | "/org/evidence-bundles/{bundle_id}/share",
        )
        | ("PUT", "/org/experiments/{name}") => C::AccountManage,
        (
            "GET",
            "/org/readiness/receipt"
            | "/orgs"
            | "/org/groups"
            | "/org/members"
            | "/org/teams"
            | "/org/subscription"
            | "/org/subscription/periods"
            | "/org/plan"
            | "/org/plan/payment-quote"
            | "/org/plan/journals"
            | "/org/plan/journals/{journal_id}"
            | "/org/statements"
            | "/org/statements/{statement_id}"
            | "/org/statements/{statement_id}/payment-quote"
            | "/orders"
            | "/checkout-options"
            | "/orders/{order_id}"
            | "/orders/{order_id}/fulfillment"
            | "/orders/{order_id}/checkout-session"
            | "/orders/{order_id}/refunds"
            | "/session"
            | "/device/request"
            | "/balance"
            | "/budget"
            | "/org/keys/{key_id}/mandates",
        ) => C::Personal,
        (
            "POST",
            "/orgs"
            | "/org/keys"
            | "/org/keys/{key_id}/mandates"
            | "/org/teams"
            | "/org/ownership"
            | "/org/subscription/checkout"
            | "/org/plan/cancel"
            | "/org/plan/change"
            | "/session/organization"
            | "/device/authorize"
            | "/topups/stripe"
            | "/topups/airwallex"
            | "/topups/hyperswitch"
            | "/topups/x402"
            | "/orders"
            | "/orders/{order_id}/refund-request"
            | "/redemptions",
        ) => C::Personal,
        (
            "PATCH",
            "/org" | "/org/keys/{key_id}" | "/org/members/{user_id}" | "/org/teams/{team_id}",
        )
        | ("DELETE", "/org/keys/{key_id}" | "/org/members/{user_id}" | "/org/teams/{team_id}") => {
            C::Personal
        }
        _ => C::Retired,
    }
}

#[cfg(test)]
mod scope_tests {
    use super::customer_route_capability;
    use oxsum_core::CustomerCapability;

    #[test]
    fn unknown_routes_and_methods_have_no_inherited_customer_authority() {
        for (method, path) in [
            ("POST", "/api/v1/entries"),
            ("GET", "/api/v1/webhooks/new-resource"),
            ("PATCH", "/api/v1/statements/{statement_id}"),
            ("GET", "/api/v1/org/future"),
        ] {
            assert!(
                matches!(
                    customer_route_capability(method, path),
                    CustomerCapability::Retired
                ),
                "{method} {path}"
            );
        }
    }
}

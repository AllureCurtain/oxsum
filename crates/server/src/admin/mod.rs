//! The platform admin surface: channels and their prices, organizations,
//! billing, statements, operations and service credentials.
//!
//! Whoever deploys oxsum is the platform admin (docs/product.md), so the credential here is an
//! operator token from the environment rather than an organization's API key: pointing the gateway
//! at another upstream and changing what a request costs is not something an organization does to
//! itself. The token is compared in constant time, and a deployment that sets none has no admin
//! surface at all rather than an open one.
//!
//! Writing a price appends a version; it never rewrites one (see [`oxsum_core::Channel`]). A request
//! resolves the version in force when it starts and carries it to its settlement, so a price change
//! lands on later requests and leaves in-flight turns and old bills where they are. Web sessions
//! (TODO item 4) and the dashboard (TODO item 5) drive these same endpoints.
//!
//! One module per resource group; `mod` itself wires the router, holds the
//! `require_admin` middleware and the helpers every module shares.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put};
use axum::{Router, middleware};
use serde::Deserialize;
use subtle::ConstantTimeEq as _;
use time::{Date, Month, OffsetDateTime};
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;

mod billing;
mod channels;
mod credentials;
mod fallbacks;
mod guardrails;
mod ops;
mod organizations;
mod statements;
mod subscriptions;

/// The `/api/v1/admin` surface. Nested by [`crate::routes`], which supplies the state.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/channels", get(channels::list).post(channels::set))
        .route(
            "/channels/{name}/prices",
            get(channels::history).post(channels::append),
        )
        .route(
            "/channels/{name}/aliases",
            get(channels::aliases).post(channels::set_alias),
        )
        .route(
            "/channels/{name}/aliases/{alias}",
            delete(channels::delete_alias),
        )
        .route(
            "/channels/{name}/credentials",
            post(channels::add_credential),
        )
        .route(
            "/channels/{name}/credentials/{credential_id}",
            patch(channels::set_credential_status).delete(channels::drop_credential),
        )
        .route("/organizations", get(organizations::organizations))
        .route(
            "/organizations/{organization_id}",
            patch(organizations::update_organization),
        )
        .route(
            "/organizations/{organization_id}/adjustments",
            post(organizations::adjust_organization),
        )
        .route(
            "/organizations/{organization_id}/credit-notes",
            get(organizations::credit_notes).post(organizations::issue_credit_note),
        )
        .route(
            "/users/{user_id}/password-reset",
            post(credentials::reset_password),
        )
        .route("/holds", get(ops::holds))
        .route("/guardrail-policies", get(guardrails::list_policies))
        .route(
            "/guardrail-policies/{name}",
            put(guardrails::set_policy).delete(guardrails::delete_policy),
        )
        .route("/guardrail-bindings", get(guardrails::list_bindings))
        .route(
            "/guardrail-bindings/{scope}/{target}",
            put(guardrails::set_binding).delete(guardrails::delete_binding),
        )
        .route(
            "/model-fallbacks/{model}",
            get(fallbacks::get_fallbacks)
                .put(fallbacks::set_fallbacks)
                .delete(fallbacks::delete_fallbacks),
        )
        .route("/deposits", get(ops::deposits))
        .route("/deposits/{deposit_id}/reverse", post(ops::reverse_deposit))
        .route("/anomalies", get(ops::anomalies))
        .route("/margin", get(ops::margin))
        .route("/reconciliation", get(ops::reconciliation))
        .route("/redemption-codes", post(billing::mint_codes))
        .route("/tiers", get(billing::tiers))
        .route(
            "/tiers/{tier}",
            put(billing::set_tier).delete(billing::delete_tier),
        )
        .route(
            "/discounts",
            get(billing::discounts).post(billing::create_discount),
        )
        .route("/discounts/{discount_id}", delete(billing::end_discount))
        .route("/audit", get(ops::audit))
        .route(
            "/service-credentials",
            get(credentials::service_credentials).post(credentials::mint_service_credential),
        )
        .route(
            "/service-credentials/{credential_id}",
            delete(credentials::revoke_service_credential),
        )
        .route(
            "/closings",
            get(statements::closings).post(statements::close_month),
        )
        .route(
            "/statements",
            get(statements::statements).post(statements::generate_statements),
        )
        .route("/statements/{statement_id}", get(statements::statement))
        .route(
            "/statements/{statement_id}/export.pdf",
            get(statements::statement_pdf),
        )
        .route(
            "/statements/{statement_id}/finalize",
            post(statements::finalize_statement),
        )
        .route(
            "/statements/{statement_id}/payments",
            post(statements::record_payment),
        )
        .route(
            "/statements/{statement_id}/suspend",
            post(statements::suspend_statement),
        )
        .route("/subscription-plans", get(subscriptions::plans))
        .route("/subscription-plans/{name}", put(subscriptions::set_plan))
        .route(
            "/organizations/{organization_id}/subscription",
            put(subscriptions::bind).delete(subscriptions::cancel),
        )
        .layer(middleware::from_fn_with_state(state, require_admin))
}

/// Records one admin write in the audit log, after the mutation it describes
/// has committed (issue #160). The row is awaited rather than spawned: the
/// writes it follows are all idempotent, so a client that retries a 500 lands
/// the same mutation and its audit row together.
pub(crate) async fn record(
    state: &AppState,
    action: &str,
    target: Option<String>,
    detail: serde_json::Value,
    idempotency_key: Option<&str>,
) -> Result<(), ApiError> {
    state
        .db
        .record_audit(action, target.as_deref(), detail, idempotency_key)
        .await?;
    Ok(())
}

/// The organizations cursor, encoded: `<unix nanos>:<organization id>`. The pair is
/// the keyset `Db::organizations_page` orders by — `created_at` alone cannot order
/// two organizations registered in the same instant — and the string is opaque to
/// the caller: echoed, never constructed (issue #93).
pub(crate) fn encode_organization_cursor((created_at, id): (OffsetDateTime, Uuid)) -> String {
    format!("{}:{}", created_at.unix_timestamp_nanos(), id)
}

pub(crate) fn decode_organization_cursor(text: &str) -> Result<(OffsetDateTime, Uuid), ApiError> {
    let bad = || ApiError::Validation("a malformed organizations cursor".into());
    let (nanos, id) = text.split_once(':').ok_or_else(bad)?;
    let nanos = nanos.parse::<i128>().map_err(|_| bad())?;
    let created_at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    let id = Uuid::parse_str(id).map_err(|_| bad())?;
    Ok((created_at, id))
}

/// `YYYY-MM` as the month's first day. A malformed month is a validation failure,
/// not an empty result.
pub(crate) fn parse_month(month: &str) -> Result<Date, ApiError> {
    let bad = || ApiError::Validation("month must be YYYY-MM".into());
    let (year, month) = month.split_once('-').ok_or_else(bad)?;
    let year = year.parse::<i32>().map_err(|_| bad())?;
    let month = Month::try_from(month.parse::<u8>().map_err(|_| bad())?).map_err(|_| bad())?;
    Date::from_calendar_date(year, month, 1).map_err(|_| bad())
}

/// Peels one option layer serde does not: a bare `Option<Option<T>>` field
/// answers `None` for both an absent field and an explicit `null`, while the
/// PATCH needs `null` to mean "clear the tier". With `default` covering the
/// absent case, a present field always yields `Some(…)` — `Some(None)` when
/// it was `null`.
pub(crate) fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Opens the surface for the operator token, and for nothing else.
///
/// A deployment with no token has no platform admin, and this answers 401 rather than 404: the
/// surface exists and is closed, which is a different thing from a route that is not there.
pub(crate) async fn require_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = bearer(&request);
    match state.config.admin_token() {
        Some(expected) if presented.is_some_and(|token| token_matches(token, expected)) => {
            Ok(next.run(request).await)
        }
        _ => Err(ApiError::Unauthorized),
    }
}

/// Whether a presented token is the configured one, in time that does not depend on how much of it
/// was right.
fn token_matches(presented: &str, expected: &str) -> bool {
    presented.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// The `Bearer` credential of a request, if the header is there and readable.
fn bearer(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// Only the tests below reach in.
#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::token_matches;

    #[test]
    fn a_token_matches_only_itself() {
        assert!(token_matches("operator-token-1234", "operator-token-1234"));
        assert!(!token_matches("operator-token-1235", "operator-token-1234"));
        assert!(!token_matches("", "operator-token-1234"));
        assert!(!token_matches(
            "operator-token-1234",
            "operator-token-12345"
        ));
    }
}

//! The platform admin surface: channels, and the prices they charge.
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

use axum::extract::{Path, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router, middleware};
use oxsum_core::{Channel, ModelPrice, Price};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;

use crate::AppState;
use crate::error::ApiError;
use crate::routes::{ApiResult, ok};

/// The `/api/v1/admin` surface. Nested by [`crate::routes`], which supplies the state.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/channels", get(list).post(set))
        .route("/channels/{name}/prices", get(history).post(append))
        .layer(middleware::from_fn_with_state(state, require_admin))
}

/// A channel's connection, as the admin API takes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChannelReq {
    name: String,
    base_url: String,
    /// The upstream credential. Stored sealed; only its last four characters are ever read back.
    api_key: String,
}

/// A price to append for one of a channel's models.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PriceReq {
    model: String,
    input_price_per_million: i64,
    output_price_per_million: i64,
    max_output_tokens: i64,
}

/// The version a price change wrote.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionRes {
    model: String,
    version: i64,
}

/// Every channel, with the current version of each of its models.
async fn list(State(state): State<AppState>) -> ApiResult<Vec<Channel>> {
    ok(state.db.channels().await?)
}

/// Creates a channel, or replaces the connection of a channel that already exists.
///
/// Its prices are untouched: a connection moves, a price history stays where it is.
async fn set(State(state): State<AppState>, Json(request): Json<ChannelReq>) -> ApiResult<Channel> {
    // A deployment with no sealing key cannot store a credential, and saying so beats storing one in
    // the clear. In practice this cannot happen: `prepare` refuses to start without the key once
    // channels exist, and a channel that was just created needs it too.
    let Some(secret) = state.config.secret().cloned() else {
        tracing::error!("a channel was configured without OXSUM_SECRET_KEY");
        return Err(ApiError::Internal);
    };
    state
        .db
        .set_channel(&request.name, &request.base_url, &request.api_key, &secret)
        .await?;
    let channel = state
        .db
        .channels()
        .await?
        .into_iter()
        .find(|channel| channel.name == request.name)
        .ok_or(ApiError::Internal)?;
    ok(channel)
}

/// Appends a price version for one of a channel's models, and reports the version.
///
/// The previous version stays readable through [`history`], which is what makes a bill that names a
/// version checkable later.
async fn append(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(request): Json<PriceReq>,
) -> ApiResult<VersionRes> {
    // An unknown channel is not found, rather than a validation failure: the request is well formed
    // and names something that is not there.
    if state.db.channel_prices(&name).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    let price = Price {
        input_per_million: request.input_price_per_million,
        output_per_million: request.output_price_per_million,
        max_output_tokens: request.max_output_tokens,
    };
    let version = state.db.append_price(&name, &request.model, price).await?;
    ok(VersionRes {
        model: request.model,
        version,
    })
}

/// Every version of every model of one channel, newest first.
///
/// A price change appends, so this is the price history rather than a list of what is in force.
async fn history(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Vec<ModelPrice>> {
    match state.db.channel_prices(&name).await? {
        Some(prices) => ok(prices),
        None => Err(ApiError::NotFound),
    }
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

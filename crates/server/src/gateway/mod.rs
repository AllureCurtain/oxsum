//! The OpenAI-compatible gateway: `POST /v1/chat/completions` and `GET /v1/models`.
//!
//! A request is frozen before upstream is contacted and settled after it answers, so the wallet
//! never has to trust the network. The order is the promise: the hold is taken first, upstream
//! second, the settlement third, and each step has one place where it happens.

pub(crate) mod error;
mod relay;
mod request;

use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Extension, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use oxsum_core::{Organization, SettlementKind, hold_description};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::auth::require_key_gateway;
use crate::gateway::error::{GatewayError, REQUEST_ID};
use crate::gateway::relay::{Charge, Turn};
use crate::gateway::request::ChatRequest;
use crate::today;

/// How long the connection to upstream may take to establish.
///
/// There is deliberately no read timeout: the gaps between stream chunks are upstream's guarantee,
/// and the hold timeout is the backstop (docs/decisions.md, "gateway HTTP client").
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The HTTP client every gateway request shares, so connections are pooled and the timeout is set
/// once. A client that cannot be built would have no TLS backend at all; the default is the same
/// client without the timeout, which is better than refusing to serve the wallet.
pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// The `/v1` surface. Nested under `/v1` by [`crate::routes`], which supplies the state.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/models", get(models))
        .route("/chat/completions", post(chat))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_key_gateway,
        ))
}

/// Lists the models this deployment can serve, in OpenAI's shape.
///
/// Only models with a configured price appear: a model the gateway cannot price cannot be frozen,
/// so it is not served at all. `created` is zero because an environment-configured model has no
/// creation time to report; TODO item 3's versioned rows will carry one.
async fn models(State(state): State<AppState>) -> Response {
    let data: Vec<Value> = match state.config.gateway() {
        Some(gateway) => gateway
            .book()
            .models()
            .map(|(model, _)| {
                json!({
                    "id": model,
                    "object": "model",
                    "created": 0,
                    "owned_by": gateway.book().channel(),
                })
            })
            .collect(),
        None => Vec::new(),
    };
    Json(json!({ "object": "list", "data": data })).into_response()
}

/// One chat completion: freeze, relay, settle.
async fn chat(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    body: Bytes,
) -> Response {
    // The id exists before anything else does, so even a malformed body answers with the header a
    // caller can quote.
    let request_id = Uuid::new_v4().to_string();
    let body = match serde_json::from_slice::<Value>(&body) {
        Ok(body) => body,
        Err(error) => {
            return with_request_id(
                GatewayError::Invalid {
                    message: format!("the request body must be JSON: {error}"),
                    param: None,
                }
                .into_response(),
                &request_id,
            );
        }
    };
    let response = match run(&state, &organization, body, &request_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    with_request_id(response, &request_id)
}

/// The turn itself, from the freeze to the settlement.
async fn run(
    state: &AppState,
    organization: &Organization,
    body: Value,
    request_id: &str,
) -> Result<Response, GatewayError> {
    let Some(gateway) = state.config.gateway() else {
        return Err(GatewayError::Invalid {
            message: "this deployment serves no models".to_owned(),
            param: Some("model"),
        });
    };
    let request = ChatRequest::parse(body)?;
    let Some(price) = gateway.book().get(&request.model).copied() else {
        return Err(GatewayError::model_not_served(&request.model));
    };
    let output_bound = price.output_upper_bound(request.max_tokens)?;
    let texts: Vec<&str> = request.texts.iter().map(String::as_str).collect();
    let freeze = price.freeze_minor(&texts, request.max_tokens)?;

    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let hold_key = format!("req-{request_id}:hold");
    let description = hold_description(request_id, &request.model, freeze)?;
    if let Err(error) = wallet.hold(&hold_key, &description, freeze, today()).await {
        return Err(match error {
            oxsum_core::WalletError::InsufficientFunds => {
                // Reading the balance again can fail; saying so beats reporting a balance of zero.
                let available = wallet.available().await.ok();
                GatewayError::freeze_refused(freeze, available, request.max_tokens)
            }
            other => other.into(),
        });
    }

    let mut turn = Turn::new(
        wallet,
        request_id,
        &request.model,
        price,
        freeze,
        request.texts.clone(),
    );
    let upstream = state
        .http
        .post(format!("{}/chat/completions", gateway.base_url()))
        .bearer_auth(gateway.api_key())
        .json(&request.forwarded(output_bound))
        .send()
        .await;

    let response = match upstream {
        Err(error) => {
            // Nothing was received, so nothing is charged and the whole freeze goes back.
            tracing::warn!(%error, "upstream is unreachable");
            turn.settle(SettlementKind::UpstreamUnreachable, Charge::Nothing)
                .await;
            return Err(GatewayError::upstream(
                StatusCode::BAD_GATEWAY,
                format!("upstream is unreachable: {error}"),
                None,
            ));
        }
        Ok(response) if !response.status().is_success() => {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            turn.settle(SettlementKind::UpstreamError, Charge::Nothing)
                .await;
            // Upstream's own refusal is the honest answer, and it is already in the caller's format.
            return Err(GatewayError::upstream(
                StatusCode::BAD_GATEWAY,
                format!("upstream answered {status}: {text}"),
                serde_json::from_str::<Value>(&text)
                    .ok()
                    .filter(|body| body.get("error").is_some()),
            ));
        }
        Ok(response) if request.stream => sse(Body::from_stream(relay::stream(turn, response))),
        Ok(response) => {
            let text = response.text().await.unwrap_or_default();
            // Upstream answered in full, so the turn is finished: from here the only work left is the
            // local write, whatever the client does with the response.
            turn.note_upstream_end();
            match serde_json::from_str::<Value>(&text) {
                Ok(value) => {
                    // A whole body is not a stream, but its usage report is read the same way.
                    if let Some(usage) = relay::usage_of(&value) {
                        turn.note_usage(usage);
                    }
                    turn.note_text(&relay::completion_text(&value));
                    let (kind, charge) = turn.closing();
                    turn.settle(kind, charge).await;
                    Json(value).into_response()
                }
                Err(error) => {
                    // A 200 that is not JSON is upstream breaking its own contract: price what came
                    // back rather than handing the caller a charge of nothing.
                    turn.note_text(&text);
                    turn.settle(SettlementKind::Estimated, Charge::Estimated)
                        .await;
                    return Err(GatewayError::upstream(
                        StatusCode::BAD_GATEWAY,
                        format!("upstream answered 200 with a body that is not JSON: {error}"),
                        None,
                    ));
                }
            }
        }
    };
    Ok(response)
}

/// An SSE response: upstream's bytes, streamed, with the content type an OpenAI client expects.
fn sse(body: Body) -> Response {
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Stamps the request id on a response, so every answer can be traced to its bill.
fn with_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID, value);
    }
    response
}

#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::config::{Config, Signup};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use oxsum_core::Db;
    use tower::ServiceExt;

    /// An app with no channel configured: the wallet is served, the gateway is not.
    fn wallet_only_app() -> Router {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused")
            .expect("the placeholder URL parses");
        crate::app(Db::from_pool(pool), Config::new(Signup::Invite, None))
    }

    #[tokio::test]
    async fn a_request_without_a_key_is_refused_in_openais_shape() {
        // The key is resolved before the database is reached, so this needs neither a connection
        // nor a channel: what it checks is that `/v1` answers in the format an OpenAI SDK parses.
        let response = wallet_only_app()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .expect("the test's own request"),
            )
            .await
            .expect("the router answers");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("collects the body")
            .to_bytes();
        let body: Value = serde_json::from_slice(&body).expect("the error body is JSON");
        assert_eq!(body["error"]["type"], "authentication_error");
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }
}

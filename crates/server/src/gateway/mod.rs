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
use oxsum_core::{ActingKey, OpenHold, Organization, Serving, SettlementKind, hold_description};
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
/// Only models with a price appear: a model the gateway cannot price cannot be frozen, so it is not
/// served at all. `created` is when the version in force was written — the closest thing a price row
/// has to a creation time — and `owned_by` is the channel that serves it.
async fn models(State(state): State<AppState>) -> Response {
    let channels = match state.db.channels().await {
        Ok(channels) => channels,
        // A listing that cannot be read does not touch the wallet, but it is answered in OpenAI's
        // shape: this is the surface an OpenAI client is pointed at.
        Err(error) => {
            tracing::error!(%error, "listing the served models failed");
            return GatewayError::upstream(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the model list could not be read".to_owned(),
                None,
            )
            .into_response();
        }
    };
    let data: Vec<Value> = channels
        .into_iter()
        .flat_map(|channel| {
            channel.models.into_iter().map(move |model| {
                json!({
                    "id": model.model,
                    "object": "model",
                    "created": model.created_at.unix_timestamp(),
                    "owned_by": channel.name,
                })
            })
        })
        .collect();
    Json(json!({ "object": "list", "data": data })).into_response()
}

/// The channel and price version that serve a model, or `None` when nothing does.
///
/// A deployment with no sealing key serves no channel at all: `app` refuses to start when channels
/// exist without one, so this is the "the wallet is running, the gateway is not configured" case.
/// A credential that will not open is [`oxsum_core::WalletError::Misconfigured`], which becomes a
/// 500: the operator's mistake, not the caller's.
async fn serving(state: &AppState, model: &str) -> Result<Option<Serving>, GatewayError> {
    let Some(secret) = state.config.secret() else {
        return Ok(None);
    };
    Ok(state.db.serving(model, secret).await?)
}

/// One chat completion: freeze, relay, settle.
async fn chat(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Extension(key): Extension<ActingKey>,
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
    let response = match run(&state, &organization, &key, body, &request_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    with_request_id(response, &request_id)
}

/// The turn itself, from the freeze to the settlement.
async fn run(
    state: &AppState,
    organization: &Organization,
    key: &ActingKey,
    body: Value,
    request_id: &str,
) -> Result<Response, GatewayError> {
    let request = ChatRequest::parse(body)?;
    // The channel and the price version are resolved once, here, before anything is frozen: this
    // turn is priced by the version in force when it starts, whatever happens to prices later.
    let Some(serving) = serving(state, &request.model).await? else {
        return Err(GatewayError::model_not_served(&request.model));
    };
    let price = serving.price;
    let output_bound = price.output_upper_bound(request.max_tokens)?;
    let texts: Vec<&str> = request.texts.iter().map(String::as_str).collect();
    let freeze = price.freeze_minor(&texts, request.max_tokens)?;

    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let hold_key = format!("req-{request_id}:hold");
    let description = hold_description(request_id, &request.model, freeze)?;
    // The watch row goes in before the hold is taken: a row without a hold heals itself — the
    // sweeper deletes it when the hold is not there — while a hold without a row would be
    // invisible to the sweeper if this process died.
    if let Err(error) = state
        .db
        .note_open_hold(&OpenHold {
            hold_key: hold_key.clone(),
            tenant_id: organization.tenant_id.clone(),
            request_id: request_id.to_owned(),
            model: request.model.clone(),
            channel: serving.channel.clone(),
            price_version: serving.version,
            input_price: price.input_per_million,
            output_price: price.output_per_million,
            freeze_minor: freeze,
        })
        .await
    {
        tracing::error!(%error, "recording the open hold failed");
        return Err(error.into());
    }
    if let Err(error) = wallet
        .hold_for_key(key, &hold_key, &description, freeze, today())
        .await
    {
        // The hold was refused, so there is nothing for the sweeper to watch.
        if let Err(clear) = state.db.clear_open_hold(&hold_key).await {
            tracing::error!(%clear, "clearing a refused hold's watch row failed");
        }
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
        state.db.clone(),
        wallet,
        request_id,
        &request.model,
        &serving,
        freeze,
        request.texts.clone(),
        &organization.tenant_id,
        state.billing.clone(),
    );
    // The dashboard's live section sees the turn from here: the hold is taken, upstream is
    // next. Best-effort — a missed event is a missed live update, not lost state.
    let _ = state
        .billing
        .send(crate::billing::BillingEvent::TurnStarted {
            tenant_id: organization.tenant_id.clone(),
            request_id: request_id.to_owned(),
            model: request.model.clone(),
            channel: serving.channel.clone(),
            freeze_minor: freeze,
        });
    let upstream = state
        .http
        .post(format!("{}/chat/completions", serving.base_url))
        .bearer_auth(&serving.api_key)
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

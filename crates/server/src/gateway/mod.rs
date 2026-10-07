//! The OpenAI-compatible gateway: `POST /v1/chat/completions` and `GET /v1/models`.
//!
//! A request is frozen before upstream is contacted and settled after it answers, so the wallet
//! never has to trust the network. The order is the promise: the hold is taken first, upstream
//! second, the settlement third, and each step has one place where it happens.

pub(crate) mod error;
mod relay;
mod request;

use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use http_body_util::{BodyExt, StreamBody};
use oxsum_core::{
    ActingKey, Claim, OpenHold, Organization, Serving, SettlementKind, WalletError, adapter_for,
    fingerprint, hold_description,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::auth::require_key_gateway;
use crate::gateway::error::{BALANCE_MINOR, CHARGED_MINOR, FREEZE_MINOR, GatewayError, REQUEST_ID};
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

/// The header a retry carries to claim the same turn, Stripe-shaped.
const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// Marks a response answered from the idempotency record rather than run again.
const IDEMPOTENT_REPLAYED: &str = "idempotent-replayed";

/// One chat completion: freeze, relay, settle.
async fn chat(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Extension(key): Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // The id exists before anything else does, so even a malformed body answers with the header a
    // caller can quote.
    let minted_id = Uuid::new_v4().to_string();
    // An `Idempotency-Key` claims the turn before it runs (issue #132): a retry under the
    // same key and body replays the stored answer instead of holding and charging twice,
    // and the claim is decided before the limiter, so a replay never burns the window.
    let idem_key = headers
        .get(IDEMPOTENCY_KEY)
        .map(|value| value.to_str().unwrap_or_default().to_owned());
    let claim = match &idem_key {
        Some(key) => {
            match state
                .db
                .claim_request(organization.id, key, &fingerprint(&body), &minted_id)
                .await
            {
                Ok(Claim::Fresh { request_id }) => {
                    crate::metrics::claim(&state.metrics, "fresh");
                    Some((key.clone(), request_id))
                }
                Ok(Claim::InFlight) => {
                    crate::metrics::claim(&state.metrics, "in_flight");
                    return with_request_id(
                        GatewayError::idempotency_in_flight().into_response(),
                        &minted_id,
                    );
                }
                Ok(Claim::Mismatch) => {
                    crate::metrics::claim(&state.metrics, "mismatch");
                    return with_request_id(
                        GatewayError::idempotency_mismatch().into_response(),
                        &minted_id,
                    );
                }
                Ok(Claim::Replay {
                    request_id,
                    status,
                    body,
                }) => {
                    crate::metrics::claim(&state.metrics, "replay");
                    return replay(status, body, &request_id);
                }
                Err(error) => {
                    return with_request_id(GatewayError::from(error).into_response(), &minted_id);
                }
            }
        }
        None => None,
    };
    let request_id = claim
        .as_ref()
        .map(|(_, id)| id.clone())
        .unwrap_or(minted_id);
    let body = match serde_json::from_slice::<Value>(&body) {
        Ok(body) => body,
        Err(error) => {
            let error = GatewayError::Invalid {
                message: format!("the request body must be JSON: {error}"),
                param: None,
            };
            return with_request_id(
                settle_claim(&state, &organization, claim, Err(error)).await,
                &request_id,
            );
        }
    };
    let outcome = run(&state, &organization, &key, body, &request_id).await;
    let response = settle_claim(&state, &organization, claim, outcome).await;
    with_request_id(response, &request_id)
}

/// Writes the idempotency record its answer, or releases the claim.
///
/// A streamed turn's bytes cannot be replayed, so its record is left `in_flight` here
/// and completed by the usage-row write (`Db::record_usage`) with the settled receipt —
/// which also covers a client that hung up and a turn the sweeper settled. A refusal
/// that never reached the wallet releases the claim instead: nothing was billed, and a
/// corrected retry should run rather than replay an old refusal.
async fn settle_claim(
    state: &AppState,
    organization: &Organization,
    claim: Option<(String, String)>,
    outcome: Result<Response, GatewayError>,
) -> Response {
    let Some((key, _)) = claim else {
        return match outcome {
            Ok(response) => response,
            Err(error) => error.into_response(),
        };
    };
    match outcome {
        Ok(response) if is_sse(&response) => response,
        Ok(response) => complete_claim(state, organization, &key, response).await,
        // The turn ran and settled — upstream failing after the hold is an answer worth
        // replaying, because running it again would freeze and charge again.
        Err(error @ GatewayError::Upstream { .. }) => {
            complete_claim(state, organization, &key, error.into_response()).await
        }
        Err(error) => {
            if let Err(error) = state.db.release_request(organization.id, &key).await {
                tracing::error!(%error, "releasing an idempotency claim failed");
            }
            error.into_response()
        }
    }
}

/// Stores a finished response on the claim and answers it. The body is already
/// materialized — JSON for both the completion and every error — so buffering it
/// for the record costs one copy, not a stream interruption.
async fn complete_claim(
    state: &AppState,
    organization: &Organization,
    key: &str,
    response: Response,
) -> Response {
    let (parts, body) = response.into_parts();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            // Nothing is released: the turn may already have billed, and a released
            // claim would let the retry bill again. The record expires on its own.
            tracing::error!(%error, "a finished gateway body could not be buffered");
            return GatewayError::Internal.into_response();
        }
    };
    if let Ok(body) = serde_json::from_slice::<Value>(&bytes)
        && let Err(error) = state
            .db
            .complete_request(
                organization.id,
                key,
                i32::from(parts.status.as_u16()),
                &body,
            )
            .await
    {
        tracing::error!(%error, "completing an idempotency record failed");
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Whether this response is the SSE stream — whose record completes at settlement.
fn is_sse(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"))
}

/// The stored answer to a claimed key: same status and body, the original request id,
/// and the replay marker.
fn replay(status: i32, body: Value, request_id: &str) -> Response {
    let mut response = (
        StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK),
        Json(body),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str("true") {
        response.headers_mut().insert(IDEMPOTENT_REPLAYED, value);
    }
    with_request_id(response, request_id)
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
    // The key's rolling-minute allowance is consumed at admission, before any
    // money moves: a refused request never reaches the wallet. A client retry is
    // a new request (the id is minted here), so it rightly spends another slot.
    if let Some(rpm) = key.requests_per_minute
        && rpm > 0
        && let Err(limited) =
            state
                .rate_limiter
                .admit(key.key_id, rpm as u32, std::time::Instant::now())
    {
        crate::metrics::rate_limited(&state.metrics, "gateway");
        return Err(GatewayError::RateLimited {
            message: format!("this API key is limited to {rpm} requests per minute"),
            code: "RATE_LIMITED",
            limit: i64::from(rpm),
            retry_after_secs: limited.retry_after.as_secs(),
        });
    }
    // The channel and the price version are resolved once, here, before anything is frozen: this
    // turn is priced by the version in force when it starts, whatever happens to prices later.
    let Some(serving) = serving(state, &request.model).await? else {
        return Err(GatewayError::model_not_served(&request.model));
    };
    // The channel's protocol selects the adapter that reads its usage reports. A name
    // the registry does not know is refused when the channel is written, so reaching
    // here means the row was edited by hand — a deployment problem, not the caller's.
    let Some(adapter) = adapter_for(&serving.protocol) else {
        tracing::error!(channel = %serving.channel, protocol = %serving.protocol,
            "the channel names a protocol this build has no usage adapter for");
        return Err(WalletError::Misconfigured(format!(
            "channel {} names an unknown protocol: {}",
            serving.channel, serving.protocol
        ))
        .into());
    };
    let price = serving.price.clone();
    let output_bound = price.output_upper_bound(request.max_tokens)?;
    let texts: Vec<&str> = request.texts.iter().map(String::as_str).collect();
    let freeze = price.freeze_minor(
        &texts,
        request.max_tokens,
        request.attribution.service_tier.as_deref(),
    )?;

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
            input_price: price.input_price_per_million,
            output_price: price.output_price_per_million,
            freeze_minor: freeze,
            key_id: Some(key.key_id),
            end_user: request.attribution.end_user.clone(),
            service_tier: request.attribution.service_tier.clone(),
            tags: request.attribution.tags.clone(),
            sweep_attempts: 0,
            last_error: None,
            dead_at: None,
        })
        .await
    {
        tracing::error!(%error, "recording the open hold failed");
        return Err(error.into());
    }
    if let Err(error) = wallet
        .hold_for_key(
            key,
            Some(request.model.as_str()),
            &hold_key,
            &description,
            freeze,
            today(),
        )
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

    // The cost head: the reserved bound and the runway left under it are both known now,
    // before upstream is asked — they ride every answer this turn produces, streamed or
    // refused (P4-3). A balance that could not be read omits the header rather than
    // reporting a made-up number.
    let balance = match wallet.available().await {
        Ok(balance) => Some(balance),
        Err(error) => {
            tracing::warn!(%error, "the balance runway could not be read");
            None
        }
    };
    let cost = CostHead { freeze, balance };

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
        key.key_id,
        request.attribution.clone(),
        adapter,
        state.metrics.clone(),
    );
    // The dashboard's live section sees the turn from here: the hold is taken, upstream is
    // next. Best-effort — a missed event is a missed live update, not lost state.
    crate::metrics::hold(&state.metrics, &serving.channel, &request.model);
    let _ = state
        .billing
        .send(crate::billing::BillingEvent::TurnStarted {
            tenant_id: organization.tenant_id.clone(),
            request_id: request_id.to_owned(),
            model: request.model.clone(),
            channel: serving.channel.clone(),
            freeze_minor: freeze,
        });
    let upstream_started = Instant::now();
    let upstream = state
        .http
        .post(format!("{}/chat/completions", serving.base_url))
        .bearer_auth(&serving.api_key)
        .json(&request.forwarded(output_bound))
        .send()
        .await;

    let response = match upstream {
        Err(error) => {
            crate::metrics::upstream_response(
                &state.metrics,
                "unreachable",
                upstream_started.elapsed().as_secs_f64(),
            );
            // Nothing was received, so nothing is charged and the whole freeze goes back.
            tracing::warn!(%error, "upstream is unreachable");
            let charged = turn
                .settle(SettlementKind::UpstreamUnreachable, Charge::Nothing)
                .await;
            // The answer is upstream's refusal shape with the cost head on it — returned as
            // a response rather than the error so the head can be stamped here, where the
            // numbers are. The claim still stores it: a finished non-streamed answer is
            // completed either way.
            let mut response = GatewayError::upstream(
                StatusCode::BAD_GATEWAY,
                format!("upstream is unreachable: {error}"),
                None,
            )
            .into_response();
            cost.stamp(&mut response);
            stamp_charged(&mut response, charged);
            return Ok(response);
        }
        Ok(response) if !response.status().is_success() => {
            let status = response.status();
            crate::metrics::upstream_response(
                &state.metrics,
                "error",
                upstream_started.elapsed().as_secs_f64(),
            );
            let text = response.text().await.unwrap_or_default();
            let charged = turn
                .settle(SettlementKind::UpstreamError, Charge::Nothing)
                .await;
            // Upstream's own refusal is the honest answer, and it is already in the caller's format.
            let mut response = GatewayError::upstream(
                StatusCode::BAD_GATEWAY,
                format!("upstream answered {status}: {text}"),
                serde_json::from_str::<Value>(&text)
                    .ok()
                    .filter(|body| body.get("error").is_some()),
            )
            .into_response();
            cost.stamp(&mut response);
            stamp_charged(&mut response, charged);
            return Ok(response);
        }
        Ok(response) => {
            crate::metrics::upstream_response(
                &state.metrics,
                "ok",
                upstream_started.elapsed().as_secs_f64(),
            );
            if request.stream {
                // StreamBody rather than from_stream: the items are Frames, so the turn's
                // settled charge can ride the end of the stream as an HTTP trailer.
                let mut response = sse(Body::new(StreamBody::new(relay::stream(turn, response))));
                cost.stamp(&mut response);
                response
            } else {
                let text = response.text().await.unwrap_or_default();
                // Upstream answered in full, so the turn is finished: from here the only work
                // left is the local write, whatever the client does with the response.
                turn.note_upstream_end();
                match serde_json::from_str::<Value>(&text) {
                    Ok(value) => {
                        // A whole body is not a stream, but its usage report is read the same
                        // way.
                        if let Some(usage) = adapter.usage(&value) {
                            turn.note_usage(usage);
                        }
                        turn.note_text(&relay::completion_text(&value));
                        let (kind, charge) = turn.closing();
                        let charged = turn.settle(kind, charge).await;
                        let mut response = Json(value).into_response();
                        cost.stamp(&mut response);
                        stamp_charged(&mut response, charged);
                        response
                    }
                    Err(error) => {
                        // A 200 that is not JSON is upstream breaking its own contract: price
                        // what came back rather than handing the caller a charge of nothing.
                        turn.note_text(&text);
                        let charged = turn
                            .settle(SettlementKind::Estimated, Charge::Estimated)
                            .await;
                        let mut response = GatewayError::upstream(
                            StatusCode::BAD_GATEWAY,
                            format!("upstream answered 200 with a body that is not JSON: {error}"),
                            None,
                        )
                        .into_response();
                        cost.stamp(&mut response);
                        stamp_charged(&mut response, charged);
                        return Ok(response);
                    }
                }
            }
        }
    };
    Ok(response)
}

/// An SSE response: upstream's bytes, streamed, with the content type an OpenAI client
/// expects. `Trailer` announces that the settled charge follows the frames as
/// `x-oxsum-charged-minor` — the head is already out when the turn settles.
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
        .headers_mut()
        .insert(header::TRAILER, HeaderValue::from_static(CHARGED_MINOR));
    response
}

/// What a response head can say about cost before the turn settles (P4-3): the freeze is the
/// most the request can be charged, and the balance is the organization's spendable runway
/// left under that reservation — read once, right after the hold landed.
struct CostHead {
    freeze: i64,
    balance: Option<i64>,
}

impl CostHead {
    /// Stamps `x-oxsum-freeze-minor` always and `x-oxsum-balance-minor` when the read worked.
    fn stamp(&self, response: &mut Response) {
        if let Ok(value) = HeaderValue::from_str(&self.freeze.to_string()) {
            response.headers_mut().insert(FREEZE_MINOR, value);
        }
        if let Some(balance) = self.balance
            && let Ok(value) = HeaderValue::from_str(&balance.to_string())
        {
            response.headers_mut().insert(BALANCE_MINOR, value);
        }
    }
}

/// Stamps `x-oxsum-charged-minor` on a head — the settled charge was known before the answer
/// went out. A streamed answer carries it as a trailer instead (relay::stream).
fn stamp_charged(response: &mut Response, charged: Option<i64>) {
    if let Some(charged) = charged
        && let Ok(value) = HeaderValue::from_str(&charged.to_string())
    {
        response.headers_mut().insert(CHARGED_MINOR, value);
    }
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

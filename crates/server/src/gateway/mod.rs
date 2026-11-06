//! The protocol-native gateway: `POST /v1/chat/completions`, `POST /v1/messages`,
//! `POST /v1/embeddings`, `POST /v1/rerank`, `GET /v1/models` — and, on the
//! versioned path a Gemini client spells, `POST
//! /v1beta/models/{model}:generateContent` and `:streamGenerateContent`.
//!
//! A request is frozen before upstream is contacted and settled after it answers, so the wallet
//! never has to trust the network. The order is the promise: the hold is taken first, upstream
//! second, the settlement third, and each step has one place where it happens. The POST
//! surfaces run that same pipeline in their own wire dialect — OpenAI's, Anthropic's,
//! Gemini's, or the input-only embeddings/rerank shapes — and each serves only the channels
//! that declare its protocol, priced under the surface's own billing mode.

pub(crate) mod error;
mod relay;
mod request;

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use futures_util::StreamExt;
use http_body_util::{BodyExt, StreamBody};
use oxsum_core::{
    ActingKey, BillingMode, Claim, OpenHold, Organization, Serving, SettlementKind, UsageRecord,
    UsageRow, WalletError, credential_fingerprint, hold_description, parse_presented,
    route_order_stats,
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::AppState;
use crate::auth::require_key_gateway;
use crate::gateway::error::{CHARGED_MINOR, FREEZE_MINOR, GatewayError, REQUEST_ID};
use crate::gateway::relay::{Charge, Turn};
use crate::gateway::request::{GatewayRequest, Surface};
use crate::today;

/// How long the connection to upstream may take to establish.
///
/// There is deliberately no read timeout: the gaps between stream chunks are upstream's guarantee,
/// and the hold timeout is the backstop (docs/decisions.md, "gateway HTTP client").
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The longest `Retry-After` the gateway holds a request for (issue #168): a
/// 429 that names a shorter wait gets one delayed retry on the last route —
/// failover first, a bounded wait last. A longer wait is answered as a
/// refusal: the caller's own retry policy is the right place for it.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(3);

/// The most fallback hops a request may take past its own model (issue #500):
/// a bound on a misconfigured chain, so a loop a write-time check missed still
/// terminates. The chain is de-duplicated at resolve time, so a cycle can never
/// spend more attempts than there are distinct models.
const MAX_FALLBACK_CHAIN: usize = 8;

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
        .route("/responses", post(responses))
        .route("/messages", post(messages))
        .route("/embeddings", post(embeddings))
        .route("/rerank", post(rerank))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_key_gateway,
        ))
}

/// The Gemini surface (issue #494): `/v1beta/models/{model}:generateContent`
/// and `:streamGenerateContent`, nested under `/v1beta` by [`crate::routes`].
/// axum allows one capture per path segment, and a Gemini route spells the
/// model and the colon-action in one segment, so the wildcard takes both and
/// the handler splits them.
pub fn beta_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/models/{*target}", post(gemini_generate))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_key_gateway,
        ))
}

/// Lists the models this deployment can serve, in OpenAI's shape.
///
/// Only models with a price appear: a model the gateway cannot price cannot be frozen, so it is not
/// served at all. An `event`-mode price is not a model at all — it is the billable code a service
/// reports on `/api/v1/metering`; it appears only for the separate generic task adapter.
/// Listing a task model does not certify a particular media provider. `created` is when
/// the version in force was written — the closest thing a price row has to a creation time — and
/// `owned_by` is the channel that serves it. A model served by several channels lists once, under
/// the route that leads it (issue #168).
async fn models(
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Extension(key): Extension<ActingKey>,
) -> Result<Response, GatewayError> {
    let access = state
        .db
        .serving_access(organization.id, Some(key.key_id))
        .await?;
    let catalog = state.db.catalog_for_access(&access).await?;
    let mut listed = std::collections::BTreeMap::new();
    // The core catalog orders equal public names by weight, after permission
    // filtering. No denied route can provide this row's channel identity.
    for route in catalog {
        if route.price.mode != BillingMode::Event || route.protocol == oxsum_core::TASKS {
            listed.entry(route.model.clone()).or_insert_with(|| {
                json!({
                    "id": route.model,
                    "object": "model",
                    "created": route.created_at.unix_timestamp(),
                    "owned_by": route.channel,
                })
            });
        }
    }
    Ok(
        Json(json!({"object":"list","data":listed.into_values().collect::<Vec<_>>()}))
            .into_response(),
    )
}

/// The routes a request for this model may take, plus the upstream name an
/// alias resolved to (issue #286) — `None` when the asked name is served
/// directly or when nothing serves it.
///
/// A deployment with no sealing key serves no channel at all: `app` refuses to start when channels
/// exist without one, so this is the "the wallet is running, the gateway is not configured" case.
/// A credential that will not open is [`oxsum_core::WalletError::Misconfigured`], which becomes a
/// 500: the operator's mistake, not the caller's.
async fn servings_named(
    state: &AppState,
    model: &str,
    access: &oxsum_core::ServingAccess,
) -> Result<(Vec<Serving>, Option<String>), GatewayError> {
    let Some(secret) = state.config.secret() else {
        return Ok((Vec::new(), None));
    };
    Ok(state
        .db
        .servings_named_for_access(model, secret, access)
        .await?)
}

/// The header a retry carries to claim the same turn, Stripe-shaped.
const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// The header a delegated caller presents a signed spend mandate in (issue
/// #262): `X-Oxsum-Mandate: <base64url(canonical body + sig)>`, minted by
/// `POST /api/v1/org/keys/{keyId}/mandates` and verified against the key's
/// public half.
pub(crate) const MANDATE_HEADER: &str = "x-oxsum-mandate";

/// The header a request opts into an experiment with (issue #509):
/// `X-Oxsum-Experiment: <name>` — the open experiment whose weighted variant
/// pick rewrites the served model, recorded on the settlement and the usage
/// row so a report groups spend by variant.
pub(crate) const EXPERIMENT_HEADER: &str = "x-oxsum-experiment";

/// The most of an upstream body the non-streaming path will buffer. A whole
/// body has to sit in memory to be parsed as JSON, so a runaway or hostile
/// upstream must not be able to grow it without bound — embeddings batches
/// are legitimately large, hence megabytes, not kilobytes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// How much of an upstream error body goes into the client-facing message: a
/// preview for the operator, never the whole body.
const ERROR_BODY_PREVIEW_CHARS: usize = 500;

/// Reads an upstream body with a hard size cap. Past the cap the upstream is
/// treated as faulty — the turn settles `UpstreamError` and the client gets a
/// 502 naming the limit, not a process that grew until the OOM killer noticed.
async fn read_body_limited(response: reqwest::Response) -> Result<String, GatewayError> {
    let mut buffered: usize = 0;
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            GatewayError::upstream(
                StatusCode::BAD_GATEWAY,
                format!("the upstream body could not be read: {error}"),
                None,
            )
        })?;
        buffered += chunk.len();
        if buffered > MAX_BODY_BYTES {
            return Err(GatewayError::upstream(
                StatusCode::BAD_GATEWAY,
                format!(
                    "the upstream body exceeded the {} MiB limit",
                    MAX_BODY_BYTES / (1024 * 1024)
                ),
                None,
            ));
        }
        body.extend_from_slice(&chunk);
    }
    // `text()` decoded lossily before; keep that behavior rather than failing
    // a turn on one bad byte in an otherwise usable JSON body.
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// A short preview of an upstream error body for the client-facing message,
/// marked when truncated. The full body never leaves the server: it used to
/// be pasted into the message verbatim, which both leaked upstream internals
/// and bloated logs on large error pages.
fn error_body_preview(text: &str) -> String {
    let preview: String = text.chars().take(ERROR_BODY_PREVIEW_CHARS).collect();
    if preview.len() < text.len() {
        format!("{preview}…")
    } else {
        preview
    }
}

/// Marks a response answered from the idempotency record rather than run again.
const IDEMPOTENT_REPLAYED: &str = "idempotent-replayed";

/// One Gemini `generateContent` call: freeze, relay, settle (issue #494). The
/// wildcard holds the model and the colon-action together — `gemini-2.5-flash`
/// in `models/gemini-2.5-flash:generateContent` — and the first `:` is the
/// split: a model name never carries one.
async fn gemini_generate(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    Path(target): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((model, action)) = target.split_once(':') else {
        let error = GatewayError::Invalid {
            message: "a Gemini turn is POST /v1beta/models/{model}:generateContent or \
                 :streamGenerateContent"
                .to_owned(),
            param: None,
        };
        return error_response(error, Surface::Gemini { streamed: false });
    };
    let streamed = match action {
        "generateContent" => false,
        "streamGenerateContent" => true,
        _ => {
            let error = GatewayError::Invalid {
                message: format!(
                    "the action {action:?} is not served on this surface; \
                     :generateContent and :streamGenerateContent are"
                ),
                param: None,
            };
            return error_response(error, Surface::Gemini { streamed: false });
        }
    };
    if model.is_empty() {
        let error = GatewayError::Invalid {
            message: "the path must name a model".to_owned(),
            param: Some("model"),
        };
        return error_response(error, Surface::Gemini { streamed: false });
    }
    gateway(
        Surface::Gemini { streamed },
        state,
        organization,
        key,
        headers,
        body,
        Some(model),
    )
    .await
}

/// One chat completion: freeze, relay, settle — OpenAI's surface.
async fn chat(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    gateway(
        Surface::OpenAi,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}

/// One Responses turn: freeze, relay, settle — OpenAI's successor wire (issue
/// #495). The same `openai` channels serve it; upstream is called at
/// `responses` rather than `chat/completions`.
async fn responses(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    gateway(
        Surface::Responses,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}

/// One messages turn: freeze, relay, settle — Anthropic's surface.
async fn messages(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    gateway(
        Surface::Anthropic,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}

/// One embeddings call: freeze the input bound, relay, settle — the first of
/// the input-only surfaces (issue #170).
async fn embeddings(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    gateway(
        Surface::Embeddings,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}

/// One rerank call: freeze the input bound, relay, settle — the second
/// input-only surface, in the Jina/Cohere shape (issue #170).
async fn rerank(
    state: State<AppState>,
    organization: Extension<Organization>,
    key: Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    gateway(
        Surface::Rerank,
        state,
        organization,
        key,
        headers,
        body,
        None,
    )
    .await
}

/// The shared turn handler: the same claim-freeze-settle pipeline under either surface's
/// wire dialect. `path_model` is Some on the Gemini surface, whose route names the model —
/// the parse inside `run` reads it from there rather than the body (issue #494).
async fn gateway(
    surface: Surface,
    State(state): State<AppState>,
    Extension(organization): Extension<Organization>,
    Extension(key): Extension<ActingKey>,
    headers: HeaderMap,
    body: Bytes,
    path_model: Option<&str>,
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
    let bound_fingerprint =
        credential_fingerprint(key.key_id, &format!("{surface:?}"), path_model, &body);
    let claim = match &idem_key {
        Some(key) => {
            match state
                .db
                .claim_request(organization.id, key, &bound_fingerprint, &minted_id)
                .await
            {
                Ok(Claim::Fresh { request_id }) => {
                    crate::metrics::claim(&state.metrics, "fresh");
                    Some((key.clone(), request_id))
                }
                Ok(Claim::InFlight) => {
                    crate::metrics::claim(&state.metrics, "in_flight");
                    return with_request_id(
                        error_response(GatewayError::idempotency_in_flight(), surface),
                        &minted_id,
                    );
                }
                Ok(Claim::Mismatch) => {
                    crate::metrics::claim(&state.metrics, "mismatch");
                    return with_request_id(
                        error_response(GatewayError::idempotency_mismatch(), surface),
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
                    return with_request_id(
                        error_response(GatewayError::from(error), surface),
                        &minted_id,
                    );
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
            record_refusal(&state, &organization, &key, &request_id, "", &error).await;
            return with_request_id(
                settle_claim(&state, &organization, claim, Err(error), surface).await,
                &request_id,
            );
        }
    };
    // The model the refusal record names: the path's segment on a surface that
    // carries it there, else the body's own `model` when it carries one.
    let model = path_model.map(str::to_owned).unwrap_or_else(|| {
        body.get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    });
    // The body parses into the surface's dialect here, before the turn starts:
    // a malformed body is a refusal like any other admission failure, so it is
    // recorded and answered the same way `run`'s own refusals are.
    let request = match path_model {
        Some(path_model) => GatewayRequest::parse_for_model(body, surface, path_model),
        None => GatewayRequest::parse(body, surface),
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            record_refusal(&state, &organization, &key, &request_id, &model, &error).await;
            return with_request_id(
                settle_claim(&state, &organization, claim, Err(error), surface).await,
                &request_id,
            );
        }
    };
    let outcome = run(
        &state,
        &organization,
        &key,
        request,
        &request_id,
        &headers,
        surface,
    )
    .await;
    if let Err(error) = &outcome {
        record_refusal(&state, &organization, &key, &request_id, &model, error).await;
    }
    let response = settle_claim(&state, &organization, claim, outcome, surface).await;
    with_request_id(response, &request_id)
}

/// A refused turn's billing record (issue #233): the marker entry under
/// `req-<request>:refused` plus the usage row beside it, so a request admission
/// turned away still lands on the billing page with its terminal status and
/// reason — not only in server logs. Best-effort: a record that cannot be
/// written is logged and never turns the refusal itself into a different
/// answer.
async fn record_refusal(
    state: &AppState,
    organization: &Organization,
    key: &ActingKey,
    request_id: &str,
    model: &str,
    error: &GatewayError,
) {
    let Some(reason) = error.refusal_reason() else {
        return;
    };
    let wallet = match state.tenants.get(&organization.tenant_id).await {
        Ok(wallet) => wallet,
        Err(open_error) => {
            tracing::warn!(%open_error, request = %request_id,
                "a refused request's wallet could not open; no record");
            return;
        }
    };
    let actor = format!("{}", key.key_id.simple());
    let entry_id = match wallet
        .record_refusal(request_id, model, "", reason, Some(&actor))
        .await
    {
        Ok(entry_id) => *entry_id.as_uuid(),
        Err(mark_error) => {
            tracing::warn!(%mark_error, request = %request_id,
                "the refusal marker did not append");
            return;
        }
    };
    let row = UsageRow {
        request_id: request_id.to_owned(),
        tenant_id: organization.tenant_id.clone(),
        key_id: Some(key.key_id),
        mandate_id: None,
        model: model.to_owned(),
        channel: String::new(),
        price_version: 0,
        kind: SettlementKind::Refused,
        entry_id,
        status: "refused".to_owned(),
        failure_reason: Some(reason.to_owned()),
        usage: UsageRecord::default(),
        // A refusal never counts tokens — the columns stay NULL, not zero.
        counts: oxsum_core::CountsProvenance::None,
        charged_minor: 0,
        freeze_minor: 0,
        upstream_cost_minor: None,
        upstream_attempts: 0,
    };
    if let Err(row_error) = state.db.record_usage(&row).await {
        tracing::warn!(%row_error, request = %request_id,
            "the refusal's usage row did not write");
    }
    // A refused admission still logs a request-log row (issue #505): the
    // analytics surface answers "which requests were turned away" from it —
    // `admission_outcome: refused`, zero charge, no latency, no entry.
    let log_row = oxsum_core::RequestLogRow {
        request_id: request_id.to_owned(),
        tenant_id: organization.tenant_id.clone(),
        organization_id: organization.id,
        entry_id: Some(entry_id),
        key_id: Some(key.key_id),
        model_requested: model.to_owned(),
        model_served: String::new(),
        channel: String::new(),
        admission_outcome: "refused".to_owned(),
        status: "refused".to_owned(),
        ttft_ms: None,
        total_ms: None,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        charge_minor: 0,
        upstream_attempts: 0,
        error_kind: Some(reason.to_owned()),
        http_status: Some(i64::from(error.http_status().as_u16())),
        created_at: time::OffsetDateTime::now_utc(),
    };
    if let Err(error) = state.db.record_request_log(&log_row).await {
        tracing::warn!(%error, request = %request_id,
            "the refusal's request-log row did not write");
    }
}

/// The error envelope the surface's SDK parses: OpenAI's `{"error": {…}}` on
/// `/v1/chat/completions`, Anthropic's `{"type": "error", "error": {…}}` on `/v1/messages`.
/// Gemini answers `{"error": {…}}` too — its SDK reads Google's standard error
/// object, which carries the same field — so it shares the OpenAI envelope
/// rather than getting a third one (issue #494).
fn error_response(error: GatewayError, surface: Surface) -> Response {
    match surface {
        Surface::OpenAi
        | Surface::Embeddings
        | Surface::Rerank
        | Surface::Gemini { .. }
        | Surface::Responses => error.into_response(),
        Surface::Anthropic => error.into_anthropic_response(),
    }
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
    surface: Surface,
) -> Response {
    let Some((key, _)) = claim else {
        return match outcome {
            Ok(response) => response,
            Err(error) => error_response(error, surface),
        };
    };
    match outcome {
        Ok(response) if is_sse(&response) => response,
        Ok(response) => complete_claim(state, organization, &key, response).await,
        // The turn ran and settled — upstream failing after the hold is an answer worth
        // replaying, because running it again would freeze and charge again.
        Err(error @ GatewayError::Upstream { .. }) => {
            complete_claim(state, organization, &key, error_response(error, surface)).await
        }
        Err(error) => {
            if let Err(error) = state.db.release_request(organization.id, &key).await {
                tracing::error!(%error, "releasing an idempotency claim failed");
            }
            error_response(error, surface)
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
    mut request: GatewayRequest,
    request_id: &str,
    headers: &HeaderMap,
    surface: Surface,
) -> Result<Response, GatewayError> {
    // The turn's OTLP span (issue #224): created once the model is known,
    // held by the settlement plan so it stays open until the charge is
    // written — a streamed turn's span outlives this function by exactly the
    // relay plus its ledger write. It is never entered: `Span::record` works
    // on the handle alone, and holding the guard across awaits would leave
    // the span current on whatever task next polls this thread. The
    // `gen_ai.usage.*` fields land at settle time, when counts exist.
    let turn_span = tracing::info_span!(
        "gen_ai.turn",
        gen_ai.operation.name = surface.gen_ai_operation(),
        gen_ai.system = surface.protocol(),
        gen_ai.request.model = request.model.as_str(),
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
    );
    let requested_model = request.model.clone();
    let access = state
        .db
        .serving_access(organization.id, Some(key.key_id))
        .await?;
    if !access.allows_model(&requested_model) {
        return Err(GatewayError::Forbidden(
            "the caller cannot access the requested model".into(),
        ));
    }
    // Validate the original name and channel even when an experiment will
    // choose a different model. An experiment is never an authority grant.
    let (original_candidates, original_upstream) =
        servings_named(state, &requested_model, &access).await?;
    if !access.allows_path(&requested_model, original_upstream.as_deref()) {
        return Err(GatewayError::Forbidden(
            "the caller cannot access the resolved model".into(),
        ));
    }
    if !original_candidates.iter().any(|route| {
        access.allows_channel(route.channel_id)
            && route.protocol == surface.protocol()
            && route.price.mode == surface.billing_mode()
    }) {
        return Err(GatewayError::model_not_served(&requested_model));
    }
    // The experiment opt-in (issue #509): `X-Oxsum-Experiment` names an open
    // experiment of the organization, and the variant its weighted pick lands
    // on rewrites the model the rest of admission resolves — a variant is just
    // a model name plus a weight, resolved through the same `servings_named`
    // and priced like any request, so the served model still carries the whole
    // bill's proof. The binding (`{id, variant}`) is what the settlement and
    // the usage row record, so a report can group spend by variant. An
    // unknown or ended name resolves the plain model instead — an experiment
    // is a routing hint, never an admission gate.
    let experiment: Option<oxsum_core::ExperimentBinding> =
        match headers.get(EXPERIMENT_HEADER).and_then(|v| v.to_str().ok()) {
            Some(name) => match state.db.experiment_named(organization.id, name).await? {
                Some(experiment) => {
                    match oxsum_core::pick_variant(&experiment.variants, rand::random::<f64>()) {
                        Some(variant) => {
                            request.model = variant.model.clone();
                            Some(oxsum_core::ExperimentBinding {
                                experiment_id: experiment.id,
                                variant: variant.model.clone(),
                            })
                        }
                        None => None,
                    }
                }
                None => None,
            },
            None => None,
        };
    let requested_upstream = original_upstream.clone();
    let (servings, discount) = tokio::join!(
        async {
            if request.model == requested_model {
                Ok((original_candidates, original_upstream))
            } else {
                servings_named(state, &request.model, &access).await
            }
        },
        state.db.discount_percent(organization.id, &request.model),
    );
    let (candidates, upstream_model) = servings?;
    if !access.allows_path(&request.model, upstream_model.as_deref()) {
        return Err(GatewayError::Forbidden(
            "the caller cannot access the selected model".into(),
        ));
    }
    request.upstream_model = upstream_model;
    // The fallback chain this model may serve under (issue #500): resolved at
    // admission alongside everything else, capped and de-duplicated so a
    // misconfigured `a→b→a` cannot cycle. The requested model leads, each
    // configured fallback follows in position order, and a name already in the
    // chain is dropped — `set_fallbacks` refuses a self-loop at write time, but
    // a multi-hop cycle (`a→b→c→a`) only closes at resolve time.
    let mut model_chain: Vec<String> = vec![request.model.clone()];
    let mut seen: std::collections::HashSet<String> = [request.model.clone()].into_iter().collect();
    for fallback in state
        .db
        .fallbacks_for(&request.model)
        .await?
        .into_iter()
        .take(MAX_FALLBACK_CHAIN)
    {
        if seen.insert(fallback.clone()) {
            model_chain.push(fallback);
        }
    }
    let discount_percent = discount?.map(i64::from);
    if let Some(rpm) = access.requests_per_minute
        && rpm > 0
        && let Err(limited) =
            state
                .rate_limiter
                .admit(organization.id, rpm as u32, std::time::Instant::now())
    {
        crate::metrics::rate_limited(&state.metrics, "gateway");
        return Err(GatewayError::RateLimited {
            message: format!("the organization's tier is limited to {rpm} requests per minute"),
            code: "RATE_LIMITED",
            limit: i64::from(rpm),
            retry_after_secs: limited.retry_after.as_secs(),
        });
    }
    // The key's rolling-minute request allowance is consumed at admission, before
    // any money moves: a refused request never reaches the wallet. A client retry
    // is a new request (the id is minted here), so it rightly spends another slot.
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
    // A presented spend mandate is verified before it may constrain anything:
    // the signature checks against the key's public half, so a blob that fails
    // any step — base64, JSON, the Ed25519 verify — is `invalid_mandate` and the
    // turn is refused before a hold is taken (issue #262). The window and cap
    // checks run later, inside `hold_for_key`'s lock, where the committed sum
    // they are judged against is read.
    let mandate = match headers.get(MANDATE_HEADER).and_then(|v| v.to_str().ok()) {
        Some(blob) => {
            // No configured secret means no key ever carried a seed, so a
            // presented mandate cannot verify — `invalid_mandate`, the same
            // refusal any other unverifiable blob takes.
            let secret = state.config.secret().ok_or(GatewayError::InvalidMandate)?;
            let verifying = state
                .db
                .mandate_verifying_key(&key.key_id, secret)
                .await?
                .ok_or(GatewayError::InvalidMandate)?;
            Some(parse_presented(blob, &verifying).ok_or(GatewayError::InvalidMandate)?)
        }
        None => None,
    };
    let model_permitted = |model: &str, upstream: Option<&str>| -> bool {
        access.allows_path(model, upstream)
            && mandate
                .as_ref()
                .and_then(|m| m.verified.models.as_ref())
                .is_none_or(|names| {
                    names.iter().any(|name| name == model)
                        && upstream.is_none_or(|target| names.iter().any(|name| name == target))
                })
    };
    if !model_permitted(&requested_model, requested_upstream.as_deref())
        || !model_permitted(&request.model, request.upstream_model.as_deref())
    {
        return Err(GatewayError::Forbidden(
            "the mandate does not allow this model path".into(),
        ));
    }
    // The routes for this model are resolved once, here, before anything is
    // frozen: this turn is priced by the versions in force when it starts,
    // whatever happens to prices later. A surface serves only the channels
    // that speak its protocol — an `anthropic` route is not a candidate on the
    // OpenAI surface, and vice versa (issue #168) — and only prices of the
    // surface's own billing mode: a chat-priced model is not an embeddings
    // route, because its rates do not mean the same counts (issue #170). The
    // servings were fetched above under the admitted access snapshot.
    let candidates: Vec<Serving> = candidates
        .into_iter()
        .filter(|serving| {
            serving.protocol == surface.protocol() && serving.price.mode == surface.billing_mode()
        })
        .collect();
    let candidates: Vec<Serving> = candidates
        .into_iter()
        .filter(|serving| access.allows_channel(serving.channel_id))
        .collect();
    if candidates.is_empty() {
        return Err(GatewayError::model_not_served(&request.model));
    }
    // One model's resolved routes: the ordered servings a request naming it may
    // take, plus the upstream name its channels relay (its own alias
    // resolution, `None` for a direct name). The attempt loop walks these in
    // chain order — every route of the requested model first, then each
    // fallback's in turn (issue #500).
    struct ModelRoutes {
        /// The served name — what the settlement's `servedModel` records.
        model: String,
        /// The upstream name this model's channels relay, when it is an alias.
        upstream_model: Option<String>,
        /// The channels serving the name, in attempt order.
        ordered: Vec<Serving>,
    }
    // The requested model leads its own route order, exactly as it always has.
    // The routing-health window (issue #499): the pick folds what upstream has
    // done lately into each serving's effective weight — a slow channel counts
    // as a fraction, an erroring one is skipped for its cool-down.
    let route_stats = state.db.route_stats();
    let mut model_routes: Vec<ModelRoutes> = vec![ModelRoutes {
        model: request.model.clone(),
        upstream_model: request.upstream_model.clone(),
        ordered: route_order_stats(candidates, rand::random::<f64>(), Some(route_stats)),
    }];
    // Each fallback resolves independently through `servings_named`: its own
    // channels, its own alias target, its own route order. A fallback the
    // caller may not reach is skipped at resolve time — the chain narrows to
    // the permitted models, so the attempt loop never sees a route it must not
    // serve. A fallback with no servable route on this surface contributes
    // nothing and is skipped the same way.
    for model in model_chain.iter().skip(1) {
        if !model_permitted(model, None) {
            tracing::info!(model = %model, request_model = %request.model,
                "fallback model denied by the caller's constraints; skipped");
            continue;
        }
        let (servings, upstream_model) = servings_named(state, model, &access).await?;
        if !model_permitted(model, upstream_model.as_deref()) {
            continue;
        }
        let ordered: Vec<Serving> = servings
            .into_iter()
            .filter(|serving| {
                serving.protocol == surface.protocol()
                    && serving.price.mode == surface.billing_mode()
            })
            .filter(|serving| access.allows_channel(serving.channel_id))
            .collect();
        if ordered.is_empty() {
            continue;
        }
        model_routes.push(ModelRoutes {
            model: model.clone(),
            upstream_model,
            ordered: route_order_stats(ordered, rand::random::<f64>(), Some(route_stats)),
        });
    }
    // The surface selects the adapter that reads usage reports — every
    // candidate speaks its protocol after the filter. A name the registry does
    // not know is refused when the channel is written, so reaching here means
    // a row was edited by hand — a deployment problem, not the caller's.
    let Some(adapter) =
        oxsum_core::adapter_for_endpoint(surface.protocol(), surface.upstream_path())
    else {
        tracing::error!(protocol = %surface.protocol(),
            "the surface names a protocol this build has no usage adapter for");
        return Err(WalletError::Misconfigured(format!(
            "surface {} names an unknown protocol",
            surface.protocol()
        ))
        .into());
    };
    // The discount was resolved above, beside the servings: the
    // turn settles under the percent in force when it started, whatever the
    // rows do later — the snapshot the settlement description writes
    // (issue #158).
    let texts: Vec<&str> = request.texts.iter().map(String::as_str).collect();
    // The freeze is the conservative bound across every route a failover could
    // land on: the dearest candidate's upper bound, and `max_tokens` forwards
    // at the tightest ceiling so no route can bill what its own price would
    // not have covered. A token-array request already counted its input —
    // `counted_input` is that exact figure, not a text estimate (issue #170).
    // The bound spans every model in the chain, not only the requested one: a
    // failover may land on a fallback's dearest route, so the freeze reserves
    // the dearest serving across the whole chain and `max_tokens` forwards at
    // the tightest output ceiling any route could bill under (issue #500).
    let freeze = model_routes
        .iter()
        .flat_map(|routes| routes.ordered.iter())
        .map(|serving| match request.counted_input {
            Some(count) => serving.price.estimate_minor(
                count,
                request.max_tokens,
                request.attribution.service_tier.as_deref(),
            ),
            None => serving.price.freeze_minor(
                &texts,
                request.max_tokens,
                request.attribution.service_tier.as_deref(),
            ),
        })
        .collect::<Result<Vec<i64>, _>>()?
        .into_iter()
        .max()
        .unwrap_or(0);
    let output_bound = model_routes
        .iter()
        .flat_map(|routes| routes.ordered.iter())
        .map(|serving| serving.price.output_upper_bound(request.max_tokens))
        .collect::<Result<Vec<i64>, _>>()?
        .into_iter()
        .min()
        .unwrap_or(0);
    // The token-window allowance is judged at admission, after the bounds that
    // price the turn are known but before any hold is taken (issue #203): the
    // check is `window-actual + this estimate`, where the estimate is the same
    // input bound plus output bound the freeze prices — so a turn admitted here
    // can never settle more than the window already granted it. A refused turn
    // writes nothing: the durable window moves only when a settle lands.
    if let Some(tpm) = key.tokens_per_minute
        && tpm > 0
    {
        let estimate = request
            .counted_input
            .unwrap_or_else(|| oxsum_core::input_upper_bound(&texts))
            + output_bound;
        let (settled, retry_after) = state.db.tpm_window(key.key_id).await?;
        if settled + estimate > i64::from(tpm) {
            crate::metrics::rate_limited(&state.metrics, "gateway");
            return Err(GatewayError::RateLimited {
                message: format!(
                    "this API key is limited to {tpm} tokens per minute \
                     ({settled} settled, this request needs up to {estimate})"
                ),
                code: "TOKENS_PER_MINUTE",
                limit: i64::from(tpm),
                retry_after_secs: retry_after,
            });
        }
    }
    // Tag-scoped buckets bound the key on top of the plain window (issue #209):
    // the turn's `metadata` picks which buckets it lands in, and each is checked
    // `window-actual + estimate ≤ cap` against the same estimate the plain window
    // used. A full per-value bucket refuses only the turns carrying that value —
    // the other values' buckets stay open.
    if let Some(scopes) = &key.tokens_per_minute_scopes {
        let estimate = request
            .counted_input
            .unwrap_or_else(|| oxsum_core::input_upper_bound(&texts))
            + output_bound;
        for (tag, scope) in scopes {
            let tag_scope = oxsum_core::TagScope {
                tag: tag.clone(),
                scope: scope.clone(),
            };
            let Some(name) = tag_scope.scope_name(&request.attribution.tags) else {
                continue;
            };
            let (settled, retry_after) = state.db.tpm_window_scoped(key.key_id, &name).await?;
            if settled + estimate > scope.cap() {
                crate::metrics::rate_limited(&state.metrics, "gateway");
                return Err(GatewayError::RateLimited {
                    message: format!(
                        "this API key is limited to {} tokens per minute on tag \
                         {tag} ({settled} settled, this request needs up to {estimate})",
                        scope.cap()
                    ),
                    code: "TOKENS_PER_MINUTE",
                    limit: scope.cap(),
                    retry_after_secs: retry_after,
                });
            }
        }
    }
    // The intended route: the requested model's leading channel — what the
    // watch row and the live event name, and what a swept turn's settlement
    // records. A failover reroutes the turn before it settles, so the bill
    // always names the channel that answered.
    let primary = &model_routes[0].ordered[0];

    // The guardrail pipeline's pre_call (issue #502): the turn's policies —
    // the channel's binding, the key's `requiredPolicies`, and a presented
    // mandate's narrowed subset — judge the request's texts before any money
    // moves. A `Block` refuses before the hold is taken, so a blocked request
    // costs nothing and writes only the `refused` marker; a `Flag` is
    // collected to land on the settlement. The policies resolve per request —
    // two indexed reads — so a rule the org writes or binds takes effect on
    // the next call, never on a restart.
    let mandate_policies = match &mandate {
        Some(m) => {
            state
                .db
                .mandate_required_policies(m.verified.mandate_id)
                .await?
        }
        None => Vec::new(),
    };
    let guardrails = state
        .guardrails_for(
            &primary.channel,
            key.key_id,
            key.required_policies.as_deref().unwrap_or(&[]),
            &mandate_policies,
        )
        .await?;
    let mut pending_flags: Vec<oxsum_core::SettlementGuardrailOwned> = Vec::new();
    if !guardrails.is_empty() {
        let ctx = oxsum_core::RequestContext {
            organization_id: organization.id,
            key_id: key.key_id,
            model: &request.model,
            texts: &request.texts,
            end_user: request.attribution.end_user.as_deref(),
            channel: &primary.channel,
        };
        for policy in &guardrails {
            match policy.evaluate_request(&ctx) {
                oxsum_core::Verdict::Allow => {}
                oxsum_core::Verdict::Flag { policy, reason } => {
                    pending_flags.push(oxsum_core::SettlementGuardrailOwned {
                        policy,
                        verdict: "flagged",
                        reason,
                        streamed: false,
                    });
                }
                oxsum_core::Verdict::Block { policy, reason } => {
                    // A blocked turn never takes a hold: refuse now, and the
                    // `refused` marker records it like every other admission
                    // refusal so the billing page sees the policy's verdict.
                    return Err(GatewayError::Forbidden(format!(
                        "guardrail {policy} refused the request: {reason}"
                    )));
                }
            }
        }
    }

    // The credential pick inside each channel's pool (issue #444): one channel
    // may carry several upstream keys, and a single account's 429 should
    // rotate to the next credential of the *same* channel before the turn
    // leaves it for a differently-priced route. `credential_order` leads with
    // the conversation's sticky hit when it is still healthy — the upstream's
    // prompt cache is warmest there — then least-recently-used. The
    // conversation key is the caller's `end_user`: the closest thing a request
    // has to a stable conversation, and absent it nothing sticks.
    let conversation = request.attribution.end_user.as_deref();
    // The attempt order flattens the whole chain (issue #500): every
    // `(model, route, credential)` triple, the requested model's routes and
    // their pools first, then each fallback's in turn. A single 429 still
    // rotates to the same channel's next credential before the turn leaves the
    // route, and a route's last credential still falls to the next route —
    // only now "the next route" may belong to the next model.
    let attempts_list: Vec<(usize, usize, oxsum_core::PoolCredential)> = model_routes
        .iter()
        .enumerate()
        .flat_map(|(model_index, routes)| {
            routes
                .ordered
                .iter()
                .enumerate()
                .flat_map(move |(route_index, serving)| {
                    let sticky = state.db.affinity().sticky(serving.channel_id, conversation);
                    oxsum_core::credential_order(&serving.credentials, sticky)
                        .into_iter()
                        .map(move |credential| (model_index, route_index, credential))
                })
        })
        .collect();

    // The semantic cache (issue #497): the primary route's channel is the one
    // a miss would be written under, so its `settings.semanticCache` decides
    // whether this request reads the store at all. A hit is served — and
    // billed at the cache-read rate — before any fresh-turn freeze is taken:
    // the answer is already paid for once, so the turn reserves only what the
    // replay itself costs. The lookup is org-scoped and keyed by
    // `(channel, model, credential-bound digest)`. Sibling keys cannot read
    // each other's answers, and legacy unbound digests miss after upgrade.
    // A streamed request never looks: the store holds whole bodies, and a
    // client that asked for SSE cannot be handed one — re-chunking a finished
    // answer is a feature, not this one's (whole-response hits only).
    let cache_policy = if request.stream {
        oxsum_core::CachePolicy::default()
    } else {
        oxsum_core::CachePolicy::of(Some(&primary.settings)).map_err(|error| {
            GatewayError::upstream(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the channel's semanticCache settings are unreadable: {error}"),
                None,
            )
        })?
    };
    let cache_key = if cache_policy.enabled {
        oxsum_core::prompt_digest(
            surface.protocol(),
            &request.model,
            &request.prompt_parts,
            cache_policy.exclude_system,
        )
        .map(|digest| {
            (
                credential_fingerprint(key.key_id, "semantic-cache", Some(&digest), &[]),
                cache_policy,
            )
        })
    } else {
        None
    };
    if let Some((digest, _policy)) = &cache_key
        && let Some(hit) = state
            .db
            .cache_lookup(organization.id, primary.channel_id, &request.model, digest)
            .await?
    {
        return serve_cache_hit(
            state,
            organization,
            key,
            &request,
            request_id,
            primary,
            hit,
            adapter,
            discount_percent,
            mandate,
            turn_span,
            surface,
            guardrails.clone(),
            std::mem::take(&mut pending_flags),
            experiment.clone(),
            &requested_model,
        )
        .await;
    }

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
            channel: primary.channel.clone(),
            price_version: primary.version,
            input_price: primary.price.input_price_per_million,
            output_price: primary.price.output_price_per_million,
            freeze_minor: freeze,
            key_id: Some(key.key_id),
            mandate_id: mandate.as_ref().map(|m| m.verified.mandate_id),
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
    // The subscription's share of the freeze, drawn before the wallet's: a
    // bound plan covers what its period still holds, and the wallet holds the
    // overflow. A plan that disallows overflow and could not cover the whole
    // freeze refuses the hold outright, before any money moves (issue #496).
    let subscription = match state
        .db
        .admit_subscription_draw(organization.id, &hold_key, freeze)
        .await
    {
        Ok(admission) => admission,
        Err(error) => {
            if let Err(clear) = state.db.clear_open_hold(&hold_key).await {
                tracing::error!(%clear, "clearing a refused hold's watch row failed");
            }
            return Err(match error {
                oxsum_core::WalletError::InsufficientFunds => {
                    GatewayError::freeze_refused(freeze, request.max_tokens)
                }
                other => other.into(),
            });
        }
    };
    let wallet_minor = freeze - subscription.drawn_minor;
    if let Err(error) = wallet
        .hold_for_key(
            key,
            Some(request.model.as_str()),
            &hold_key,
            &description,
            // A hold of zero would be refused by the ledger — a fully
            // subscription-covered turn takes a 1-minor wallet hold instead,
            // so the turn still carries a ledger hold for the settlement to
            // land on and the sweeper to watch. The difference is repaid in
            // the settlement: the wallet is charged only `charged - drawn`.
            wallet_minor.max(1),
            today(),
            mandate.as_ref(),
        )
        .await
    {
        // The hold was refused, so there is nothing for the sweeper to watch.
        if let Err(clear) = state.db.clear_open_hold(&hold_key).await {
            tracing::error!(%clear, "clearing a refused hold's watch row failed");
        }
        if matches!(error, oxsum_core::WalletError::InsufficientFunds) {
            // A credit-line organization's 402 announces itself
            // (issue #200) — best-effort, since the refusal left no
            // transaction to enqueue into.
            state
                .db
                .notify_credit_limit_exceeded(organization.id, &wallet)
                .await;
        }
        return Err(match error {
            oxsum_core::WalletError::InsufficientFunds => {
                GatewayError::freeze_refused(freeze, request.max_tokens)
            }
            other => other.into(),
        });
    }
    // The hold's landing is the turn's first phase boundary on the OTLP span:
    // span-start → this event is admission+freeze, this → the settle event is
    // relay (issue #224). `in_scope` logs the event *on* the span without making
    // it current — the handle still is not entered.
    turn_span.in_scope(|| {
        tracing::info!(oxsum.hold.freeze_minor = freeze, "hold taken");
    });

    // Only request-attributed cost leaves through the consumption credential.
    let cost = CostHead { freeze };

    let mut turn = Turn::new(
        state.db.clone(),
        wallet,
        request_id,
        &request.model,
        request.upstream_model.clone(),
        primary,
        freeze,
        discount_percent,
        request.texts.clone(),
        &organization.tenant_id,
        state.billing.clone(),
        key.key_id,
        request.attribution.clone(),
        adapter,
        state.metrics.clone(),
        turn_span,
        mandate,
        subscription,
        organization.id,
        guardrails.clone(),
    );
    turn.note_requested_model(&requested_model);
    turn.note_input_count(request.counted_input);
    // The pre_call flags the pipeline collected land on the settlement — the
    // turn proceeded under them, and the v11 description records which rules
    // saw the request (issue #502).
    for flag in pending_flags {
        turn.note_guardrail(flag);
    }
    // The experiment binding rides the turn to its settlement and usage row
    // (issue #509): `{id, variant}` on the v13 description and the row's
    // `usage_details`, so a report groups spend by variant.
    if let Some(binding) = experiment {
        turn.note_experiment(binding);
    }
    // The dashboard's live section sees the turn from here: the hold is taken, upstream is
    // next. Best-effort — a missed event is a missed live update, not lost state.
    crate::metrics::hold(&state.metrics, &primary.channel, &request.model);
    let _ = state
        .billing
        .send(crate::billing::BillingEvent::TurnStarted {
            key_id: key.key_id,
            tenant_id: organization.tenant_id.clone(),
            request_id: request_id.to_owned(),
            model: request.model.clone(),
            channel: primary.channel.clone(),
            freeze_minor: freeze,
        });
    // The attempt loop: a retriable failure before any answer bytes —
    // unreachable, 429, or 5xx — rotates to the next attempt under the same
    // hold. The attempts walk the flattened chain (issue #500): the requested
    // model's routes and their pools first, then each fallback model's. Inside
    // one route its credentials still lead (issue #444): a single account's
    // wall fails over onto the channel's next key before the turn leaves the
    // route, and the route's last attempt falls to the next route — which may
    // belong to the next model. A 4xx fails fast, since the same refusal would
    // come back from everywhere. When the last attempt's refusal is a 429
    // naming a short `Retry-After`, the wait is honored once rather than
    // answered on the spot: failover first, a bounded wait last (issue #168).
    // A failure after bytes started flowing is never retried — the client
    // already holds a partial answer, and replaying upstream could double-bill
    // the platform.
    let mut attempts: i64 = 0;
    let mut retried_429 = false;
    let mut index = 0;
    let (served_at, upstream) = loop {
        if attempts_list.is_empty() {
            // No healthy credential anywhere: every channel's pool sat out —
            // the answer is an upstream failure like any other, charged
            // `Nothing` under the route that would have led.
            break (
                (0_usize, 0_usize),
                Err((
                    GatewayError::upstream(
                        StatusCode::BAD_GATEWAY,
                        "every upstream credential is cooling or disabled".to_owned(),
                        None,
                    ),
                    SettlementKind::UpstreamError,
                )),
            );
        }
        let (model_index, route_index, credential) = &attempts_list[index];
        let routes = &model_routes[*model_index];
        let candidate = &routes.ordered[*route_index];
        attempts += 1;
        let attempt_started = Instant::now();
        // The body is rebuilt per attempt: the `model` it names is the serving
        // model's own upstream resolution — its alias target when it is one, its
        // own name otherwise — so a fallback's channel is told the name it
        // actually serves, not the requested one (issue #500). `model` and
        // `upstream_model` are repointed together: a direct serve writes back
        // the same name, an alias resolves to its target.
        let mut forwarded_request = request.clone();
        forwarded_request.model = routes.model.clone();
        forwarded_request.upstream_model = routes.upstream_model.clone();
        let forwarded = forwarded_request.forwarded(output_bound);
        // The credential goes upstream in the protocol's own spelling:
        // `Authorization: Bearer` for OpenAI, `x-api-key` for Anthropic — which
        // also wants its version header, forwarded when the caller sent one —
        // and `x-goog-api-key` for Gemini, the spelling both AI Studio and
        // Vertex accept (issue #494). Gemini also names its model in the path,
        // so the upstream name is encoded as path data rather than interpolated.
        let upstream_name = routes
            .upstream_model
            .as_deref()
            .unwrap_or(routes.model.as_str());
        let policy = oxsum_core::ProviderDestinationPolicy::from_settings(&candidate.settings);
        let segments = surface.upstream_segments(upstream_name);
        let segments: Vec<_> = segments.iter().map(String::as_str).collect();
        let call = match policy {
            Ok(policy) => {
                state
                    .provider_http
                    .request(
                        reqwest::Method::POST,
                        &candidate.base_url,
                        &policy,
                        &segments,
                    )
                    .await
            }
            Err(_) => Err(oxsum_integrations::provider_http::ProviderHttpError::Blocked),
        };
        let mut call = match call {
            Ok(call) => call.json(&forwarded),
            Err(error) => {
                if index + 1 < attempts_list.len() {
                    index += 1;
                    continue;
                }
                break (
                    (*model_index, *route_index),
                    Err((
                        GatewayError::upstream(StatusCode::BAD_GATEWAY, error.to_string(), None),
                        SettlementKind::UpstreamUnreachable,
                    )),
                );
            }
        };
        if matches!(surface, Surface::Gemini { streamed: true }) {
            call = call.query(&[("alt", "sse")]);
        }
        call = match surface {
            Surface::OpenAi | Surface::Embeddings | Surface::Rerank | Surface::Responses => {
                call.bearer_auth(&credential.api_key)
            }
            Surface::Anthropic => {
                let call = call
                    .header("x-api-key", &credential.api_key)
                    .header("anthropic-version", anthropic_version(headers));
                match headers.get("anthropic-beta").and_then(|v| v.to_str().ok()) {
                    Some(beta) => call.header("anthropic-beta", beta),
                    None => call,
                }
            }
            Surface::Gemini { .. } => call.header("x-goog-api-key", &credential.api_key),
        };
        match call.send().await {
            Ok(response) if response.status().is_success() => {
                crate::metrics::upstream_response(
                    &state.metrics,
                    "ok",
                    attempt_started.elapsed().as_secs_f64(),
                );
                // The routing window (issue #499): a served attempt feeds the
                // channel's latency sample and a healthy outcome.
                state.db.route_stats().note(
                    &candidate.channel,
                    Some(attempt_started.elapsed().as_secs_f64() * 1_000.0),
                    false,
                );
                break ((*model_index, *route_index), Ok(response));
            }
            Ok(response) => {
                let status = response.status();
                crate::metrics::upstream_response(
                    &state.metrics,
                    "error",
                    attempt_started.elapsed().as_secs_f64(),
                );
                // The routing window: an answered-but-refused attempt still
                // gives a latency sample, and counts as an error for the rate.
                state.db.route_stats().note(
                    &candidate.channel,
                    Some(attempt_started.elapsed().as_secs_f64() * 1_000.0),
                    true,
                );
                let retriable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                if retriable {
                    // A refusal the credential produced — its own rate wall or
                    // quota — cools it in the pool so the next request's pick
                    // skips it for a bounded window (issue #444). Best-effort:
                    // the write may race a concurrent pickup, and the worst
                    // case is one more attempt on a rested key.
                    let db = state.db.clone();
                    let credential_id = credential.credential_id;
                    let why = format!("upstream answered {status}");
                    tokio::spawn(async move {
                        if let Err(error) = db.cool_credential(credential_id, &why).await {
                            tracing::warn!(%error, "cooling a refused credential failed");
                        }
                    });
                }
                if retriable && index + 1 < attempts_list.len() {
                    tracing::warn!(channel = %candidate.channel, %status,
                        "upstream refused before answering; failing over");
                    index += 1;
                    continue;
                }
                // The last attempt's bounded 429 wait: upstream named the delay
                // and nothing else remains, so the wait is honored once.
                let retry_after = retry_after(&response);
                if status == StatusCode::TOO_MANY_REQUESTS
                    && !retried_429
                    && retry_after.is_some_and(|delay| delay <= RETRY_AFTER_MAX)
                {
                    retried_429 = true;
                    tokio::time::sleep(retry_after.unwrap_or_default()).await;
                    continue;
                }
                // The body is read with a cap: a runaway error page must not
                // grow the process, and only a preview reaches the client.
                let text = match read_body_limited(response).await {
                    Ok(text) => text,
                    Err(error) => {
                        break (
                            (*model_index, *route_index),
                            Err((error, SettlementKind::UpstreamError)),
                        );
                    }
                };
                let preview = error_body_preview(&text);
                break (
                    (*model_index, *route_index),
                    Err((
                        GatewayError::upstream(
                            StatusCode::BAD_GATEWAY,
                            format!("upstream answered {status}: {preview}"),
                            serde_json::from_str::<Value>(&text)
                                .ok()
                                .filter(|body| body.get("error").is_some()),
                        ),
                        SettlementKind::UpstreamError,
                    )),
                );
            }
            Err(error) => {
                crate::metrics::upstream_response(
                    &state.metrics,
                    "unreachable",
                    attempt_started.elapsed().as_secs_f64(),
                );
                // The routing window: an unreachable attempt gives no latency
                // sample (upstream never answered) but counts full as an error.
                state.db.route_stats().note(&candidate.channel, None, true);
                // An unreachable upstream is the channel's problem, not the
                // credential's: the key never got to refuse, so nothing cools.
                if index + 1 < attempts_list.len() {
                    tracing::warn!(channel = %candidate.channel, error = %error.without_url(),
                        "upstream is unreachable; failing over");
                    index += 1;
                    continue;
                }
                tracing::warn!("upstream is unreachable");
                break (
                    (*model_index, *route_index),
                    Err((
                        GatewayError::upstream(
                            StatusCode::BAD_GATEWAY,
                            "upstream is unreachable".to_owned(),
                            None,
                        ),
                        SettlementKind::UpstreamUnreachable,
                    )),
                );
            }
        }
    };
    turn.note_attempts(attempts);
    // The credential that answered is the one this conversation sticks to
    // (issue #444): its id lives on the attempt that succeeded, and the
    // affinity note is process-local — a missed write only means the next turn
    // picks fresh. `note_credential_used` feeds the least-recently-used rank
    // either way; both writes are best-effort.
    if upstream.is_ok() {
        let (model_index, route_index, credential) = &attempts_list[index];
        state.db.affinity().note(
            model_routes[*model_index].ordered[*route_index].channel_id,
            conversation,
            credential.credential_id,
        );
        let db = state.db.clone();
        let credential_id = credential.credential_id;
        tokio::spawn(async move {
            if let Err(error) = db.note_credential_used(credential_id).await {
                tracing::warn!(%error, "noting a served credential failed");
            }
        });
    }
    let (served_model_index, served_route_index) = served_at;
    if served_at != (0, 0) {
        // The settlement names the model and route that answered: a failover
        // settles under that channel's price and version, inside the same
        // freeze — and a cross-model failover settles under the served model's
        // name, so `servedModel` records what actually answered (issue #500).
        let served = &model_routes[served_model_index];
        turn.reroute(
            &served.ordered[served_route_index],
            &served.model,
            served.upstream_model.clone(),
        );
    }

    let response = match upstream {
        Err((error, kind)) => {
            // Nothing was received, so nothing is charged and the whole freeze goes back.
            let charged = turn.settle(kind, Charge::Nothing, error.reason()).await;
            // The answer is upstream's refusal shape with the cost head on it — returned as
            // a response rather than the error so the head can be stamped here, where the
            // numbers are. The claim still stores it: a finished non-streamed answer is
            // completed either way.
            let mut response = error_response(error, surface);
            cost.stamp(&mut response);
            stamp_charged(&mut response, charged);
            return Ok(response);
        }
        Ok(response) => {
            if request.stream {
                // StreamBody rather than from_stream: the items are Frames, so the turn's
                // settled charge can ride the end of the stream as an HTTP trailer.
                let mut response = sse(Body::new(StreamBody::new(relay::stream(turn, response))));
                cost.stamp(&mut response);
                response
            } else {
                let text = match read_body_limited(response).await {
                    Ok(text) => text,
                    Err(error) => {
                        // The body never arrived intact, so nothing was
                        // received: nothing is charged and the whole freeze
                        // goes back, the same settlement the refusal arm
                        // above performs.
                        let charged = turn
                            .settle(
                                SettlementKind::UpstreamError,
                                Charge::Nothing,
                                error.reason(),
                            )
                            .await;
                        let mut response = error_response(error, surface);
                        cost.stamp(&mut response);
                        stamp_charged(&mut response, charged);
                        return Ok(response);
                    }
                };
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
                        turn.note_text(&adapter.answer_text(&value));

                        // The guardrail pipeline's post_call (issue #502): the
                        // same policies judge the assembled answer before the
                        // client sees it. A `Block` refunds the whole freeze —
                        // upstream answered but the content is refused, so the
                        // turn settles `Nothing` — and answers the caller the
                        // policy's refusal. A `Flag` is recorded into the
                        // settlement's `guardrail` field, the audit the flag
                        // exists to leave.
                        if !guardrails.is_empty() {
                            let output_text = adapter.answer_text(&value);
                            let response_ctx = oxsum_core::ResponseContext {
                                organization_id: organization.id,
                                key_id: key.key_id,
                                model: &request.model,
                                channel: &model_routes[served_model_index].ordered
                                    [served_route_index]
                                    .channel,
                                output_text: &output_text,
                                request_texts: &request.texts,
                                streamed: false,
                            };
                            let mut blocked: Option<(String, String)> = None;
                            for policy in &guardrails {
                                match policy.evaluate_response(&response_ctx) {
                                    oxsum_core::Verdict::Allow => {}
                                    oxsum_core::Verdict::Flag { policy, reason } => {
                                        turn.note_guardrail(oxsum_core::SettlementGuardrailOwned {
                                            policy,
                                            verdict: "flagged",
                                            reason,
                                            streamed: false,
                                        });
                                    }
                                    oxsum_core::Verdict::Block { policy, reason } => {
                                        turn.note_guardrail(oxsum_core::SettlementGuardrailOwned {
                                            policy: policy.clone(),
                                            verdict: "blocked",
                                            reason: reason.clone(),
                                            streamed: false,
                                        });
                                        blocked = Some((policy, reason));
                                        break;
                                    }
                                }
                            }
                            if let Some((policy, reason)) = blocked {
                                // The hold refunds in full: the answer is
                                // refused, not served. The verdict rides the
                                // settlement's `guardrail` field under the
                                // `Nothing` charge, so the bill proves which
                                // rule acted and that nothing was charged.
                                let message =
                                    format!("guardrail {policy} refused the response: {reason}");
                                let charged = turn
                                    .settle(SettlementKind::Usage, Charge::Nothing, Some(&message))
                                    .await;
                                let mut response =
                                    error_response(GatewayError::Forbidden(message), surface);
                                cost.stamp(&mut response);
                                stamp_charged(&mut response, charged);
                                return Ok(response);
                            }
                        }

                        let (kind, charge) = turn.closing();
                        // The cache write needs the provider's own counts —
                        // what a later hit reprices — so they are lifted out
                        // of the charge before it moves into the settle. An
                        // estimated charge has no trustworthy counts to store,
                        // and a hit on one would bill an estimate at a real
                        // price: it is simply not cached.
                        let store_usage = match &charge {
                            Charge::Usage(usage) => Some((**usage).clone()),
                            _ => None,
                        };
                        let charged = turn.settle(kind, charge, None).await;
                        // The write is keyed under the primary route — the
                        // channel a later lookup will resolve — so a failovered
                        // answer is not stored: it was produced under another
                        // channel's upstream, and filing it under this one's
                        // key would serve it next time as this channel's own.
                        // A cross-model failover is failovered by definition —
                        // the served model is not the name the lookup keys on.
                        if served_at == (0, 0)
                            && let Some(((digest, policy), usage)) =
                                cache_key.as_ref().zip(store_usage.as_ref())
                            && charged.is_some()
                        {
                            // The source entry is the settlement the serving
                            // turn just wrote — derived from the hold key like
                            // the settlement derives it, so the store never
                            // waits on the ledger to learn the link.
                            cache_store_spawn(
                                state.db.clone(),
                                organization.id,
                                oxsum_core::CacheStore {
                                    channel_id: primary.channel_id,
                                    model: request.model.clone(),
                                    prompt_digest: digest.clone(),
                                    body: value.clone(),
                                    usage: usage.clone(),
                                    source_entry_id: *oxsum_core::entry_id_for(
                                        &oxsum_core::settlement_key_for(&hold_key),
                                    )
                                    .as_uuid(),
                                    ttl_seconds: policy.ttl_seconds,
                                },
                            );
                        }
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
                            .settle(
                                SettlementKind::Estimated,
                                Charge::Estimated,
                                Some("upstream answered with a body that is not JSON"),
                            )
                            .await;
                        let mut response = error_response(
                            GatewayError::upstream(
                                StatusCode::BAD_GATEWAY,
                                format!(
                                    "upstream answered 200 with a body that is not JSON: {error}"
                                ),
                                None,
                            ),
                            surface,
                        );
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

/// Serves a semantic-cache hit (issue #497): the stored body answers the
/// request, billed at the cache-read rate the original turn's usage prices
/// to under the serving channel's current book.
///
/// The hit runs the same hold-then-settle pipeline a fresh turn does, with
/// two differences an honest bill depends on. The freeze reserves only what
/// the replay can cost — the stored usage priced under the current book —
/// not the fresh bound a miss would have frozen against. And the settlement
/// is `Charge::Cached`: every input token counts as a cached read, and the
/// v10 description's `cache` block names the entry the answer was first
/// produced under, so the bill proves reuse rather than claiming a fresh
/// turn. The upstream call never happens — a hit is an answer the platform
/// already paid for once.
///
/// Everything the fresh path checks at admission still ran upstream of this
/// call: the rate windows, the mandate verification, the route pick. What a
/// hit skips is only the reservation sized for a fresh answer.
#[allow(clippy::too_many_arguments)]
async fn serve_cache_hit(
    state: &AppState,
    organization: &Organization,
    key: &ActingKey,
    request: &GatewayRequest,
    request_id: &str,
    primary: &Serving,
    hit: oxsum_core::CachedAnswer,
    adapter: &'static dyn oxsum_core::UsageAdapter,
    discount_percent: Option<i64>,
    mandate: Option<oxsum_core::PresentedMandate>,
    turn_span: tracing::Span,
    surface: Surface,
    response_guardrails: Vec<Arc<dyn oxsum_core::Guardrail>>,
    pending_flags: Vec<oxsum_core::SettlementGuardrailOwned>,
    experiment: Option<oxsum_core::ExperimentBinding>,
    requested_model: &str,
) -> Result<Response, GatewayError> {
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let hold_key = format!("req-{request_id}:hold");
    // The reservation is the stored usage priced under the channel's live
    // book — the most the hit can cost. `total_minor` ignores the discount:
    // a discounted charge lands under it, which is what a bound is for.
    let freeze = primary.price.itemize(&hit.usage, true)?.total_minor()?;
    let description = hold_description(request_id, &request.model, freeze)?;
    if let Err(error) = state
        .db
        .note_open_hold(&OpenHold {
            hold_key: hold_key.clone(),
            tenant_id: organization.tenant_id.clone(),
            request_id: request_id.to_owned(),
            model: request.model.clone(),
            channel: primary.channel.clone(),
            price_version: primary.version,
            input_price: primary.price.input_price_per_million,
            output_price: primary.price.output_price_per_million,
            freeze_minor: freeze,
            key_id: Some(key.key_id),
            mandate_id: mandate.as_ref().map(|m| m.verified.mandate_id),
            end_user: request.attribution.end_user.clone(),
            service_tier: request.attribution.service_tier.clone(),
            tags: request.attribution.tags.clone(),
            sweep_attempts: 0,
            last_error: None,
            dead_at: None,
        })
        .await
    {
        tracing::error!(%error, "recording a cache hit's open hold failed");
        return Err(error.into());
    }
    // A hit draws on the subscription like any other turn: the entitlement
    // pays the replay's price the same way it would a fresh one's, and a plan
    // that cannot cover it refuses before money moves (issue #496).
    let subscription = match state
        .db
        .admit_subscription_draw(organization.id, &hold_key, freeze)
        .await
    {
        Ok(admission) => admission,
        Err(error) => {
            if let Err(clear) = state.db.clear_open_hold(&hold_key).await {
                tracing::error!(%clear, "clearing a refused cache hold's watch row failed");
            }
            return Err(match error {
                oxsum_core::WalletError::InsufficientFunds => {
                    GatewayError::freeze_refused(freeze, request.max_tokens)
                }
                other => other.into(),
            });
        }
    };
    let wallet_minor = freeze - subscription.drawn_minor;
    if let Err(error) = wallet
        .hold_for_key(
            key,
            Some(request.model.as_str()),
            &hold_key,
            &description,
            wallet_minor.max(1),
            today(),
            mandate.as_ref(),
        )
        .await
    {
        if let Err(clear) = state.db.clear_open_hold(&hold_key).await {
            tracing::error!(%clear, "clearing a refused cache hold's watch row failed");
        }
        if matches!(error, oxsum_core::WalletError::InsufficientFunds) {
            state
                .db
                .notify_credit_limit_exceeded(organization.id, &wallet)
                .await;
        }
        return Err(match error {
            oxsum_core::WalletError::InsufficientFunds => {
                GatewayError::freeze_refused(freeze, request.max_tokens)
            }
            other => other.into(),
        });
    }
    turn_span.in_scope(|| {
        tracing::info!(oxsum.hold.freeze_minor = freeze, "cache hit hold taken");
    });
    let cost = CostHead { freeze };
    let mut turn = Turn::new(
        state.db.clone(),
        wallet,
        request_id,
        &request.model,
        request.upstream_model.clone(),
        primary,
        freeze,
        discount_percent,
        request.texts.clone(),
        &organization.tenant_id,
        state.billing.clone(),
        key.key_id,
        request.attribution.clone(),
        adapter,
        state.metrics.clone(),
        turn_span,
        mandate,
        subscription,
        organization.id,
        response_guardrails.clone(),
    );
    turn.note_requested_model(requested_model);
    turn.note_attempts(0);
    turn.note_upstream_end();
    for flag in pending_flags {
        turn.note_guardrail(flag);
    }
    // A cache-hit turn still ran under the experiment that picked its model
    // (issue #509): the binding rides to the settlement and usage row so a
    // report groups it with the variant's other serves.
    if let Some(binding) = experiment {
        turn.note_experiment(binding);
    }
    // A re-served answer still passes the post_call (issue #502): the stored
    // body is the response the client sees, and a policy that blocks or flags
    // it now acts the same as on a fresh turn — a cached answer carrying PII
    // flags like a new one. `streamed: false`: the whole body is held here,
    // so a `Block` can still withhold it and refund.
    if !response_guardrails.is_empty() {
        let output_text = adapter.answer_text(&hit.body);
        let response_ctx = oxsum_core::ResponseContext {
            organization_id: organization.id,
            key_id: key.key_id,
            model: &request.model,
            channel: &primary.channel,
            output_text: &output_text,
            request_texts: &request.texts,
            streamed: false,
        };
        let mut blocked: Option<(String, String)> = None;
        for policy in &response_guardrails {
            match policy.evaluate_response(&response_ctx) {
                oxsum_core::Verdict::Allow => {}
                oxsum_core::Verdict::Flag { policy, reason } => {
                    turn.note_guardrail(oxsum_core::SettlementGuardrailOwned {
                        policy,
                        verdict: "flagged",
                        reason,
                        streamed: false,
                    });
                }
                oxsum_core::Verdict::Block { policy, reason } => {
                    turn.note_guardrail(oxsum_core::SettlementGuardrailOwned {
                        policy: policy.clone(),
                        verdict: "blocked",
                        reason: reason.clone(),
                        streamed: false,
                    });
                    blocked = Some((policy, reason));
                    break;
                }
            }
        }
        if let Some((policy, reason)) = blocked {
            let message = format!("guardrail {policy} refused the response: {reason}");
            let charged = turn
                .settle(SettlementKind::Usage, Charge::Nothing, Some(&message))
                .await;
            let mut response = error_response(GatewayError::Forbidden(message), surface);
            cost.stamp(&mut response);
            stamp_charged(&mut response, charged);
            return Ok(response);
        }
    }
    // The served row's counter moves best-effort — a missed increment is a
    // wrong admin counter, never a wrong bill.
    let db = state.db.clone();
    let cache_id = hit.cache_id;
    tokio::spawn(async move {
        if let Err(error) = db.cache_hit(cache_id).await {
            tracing::warn!(%error, "counting a served cache hit failed");
        }
    });
    let charged = turn
        .settle(
            SettlementKind::Usage,
            Charge::Cached {
                usage: Box::new(hit.usage),
                source_entry_id: hit.source_entry_id,
            },
            None,
        )
        .await;
    let mut response = Json(hit.body).into_response();
    response.headers_mut().insert(
        crate::gateway::error::CACHE_STATUS,
        HeaderValue::from_static("hit"),
    );
    cost.stamp(&mut response);
    stamp_charged(&mut response, charged);
    Ok(response)
}

/// Writes a miss's answer into the semantic cache (issue #497), best-effort
/// after the turn has already settled: the store is a replay convenience, and
/// a dropped write only means the next identical ask goes upstream again.
fn cache_store_spawn(db: oxsum_core::Db, organization_id: Uuid, store: oxsum_core::CacheStore) {
    tokio::spawn(async move {
        if let Err(error) = db.cache_store(organization_id, &store).await {
            tracing::warn!(%error, "writing a gateway answer into the semantic cache failed");
        }
    });
}

/// The `Retry-After` an upstream refusal named, as a wait. Only the seconds
/// form is honored; an HTTP-date is declined rather than parsed loosely.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// The `anthropic-version` upstream is told: the caller's when it sent one, the version
/// this deployment speaks when it did not — Anthropic refuses a request without one.
fn anthropic_version(headers: &HeaderMap) -> &str {
    headers
        .get("anthropic-version")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|version| !version.is_empty())
        .unwrap_or("2023-06-01")
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

/// The request's authorized bound, available before its settlement.
struct CostHead {
    freeze: i64,
}

impl CostHead {
    /// Stamps the bound without exposing customer-wide money.
    fn stamp(&self, response: &mut Response) {
        if let Ok(value) = HeaderValue::from_str(&self.freeze.to_string()) {
            response.headers_mut().insert(FREEZE_MINOR, value);
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

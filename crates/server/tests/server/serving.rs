//! Serving and metering coverage (issue #604, roadmap T13): every billable
//! surface produces a durable `request_operations` row, the attempts under it,
//! and a settlement the usage rows price — verified by reading the rows, not
//! only the response status.
//!
//! A08 is the scripted provider matrix: chat, responses, messages, gemini,
//! embeddings and rerank each write an operation that goes `succeeded`, a
//! winning `answered` attempt carrying the verbatim usage, and a `usage_records`
//! row whose `cost_basis` names how the platform's own cost was derived. A14 is
//! the excessive/missing-usage paths; A15 is that the real-provider suites stay
//! opt-in and untouched (see `real_upstream*.rs` — nothing here gates on them).
//! The retry and cancellation halves are the pre-byte failover — one operation,
//! two attempts, one settle — and the post-byte disconnect, whose operation
//! goes `cancelled` while the consumed usage still prices.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

use std::time::Duration;
#[path = "../../../core/tests/support/integration.rs"]
mod integration;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use oxsum_core::{BillingMode, Db, Price, SecretKey, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tower::ServiceExt;
use uuid::Uuid;

const TOKEN: &str = "operator-token-0123456789";
const PASSWORD: &str = "correct horse battery";

macro_rules! world_or_skip {
    ($top_up:expr, $protocol:expr) => {
        match oxsum_testkit::suite_url!() {
            Some(u) => world(&u, $top_up, $protocol).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

// ── the scripted upstream ────────────────────────────────────────────────────

/// What upstream answers for one model, per protocol.
#[derive(Clone)]
enum Answer {
    /// A whole chat completion with a usage report.
    Completion { input: i64, output: i64 },
    /// A stream that emits one partial chunk and then stays open — the shape a
    /// post-byte client disconnect needs.
    Stall,
    /// A stream that ends cleanly with `[DONE]` but never reports usage — the
    /// degraded-policy missing-usage shape.
    StreamNoUsage,
    /// Refuses the first `refuse` requests with `status`, optionally naming a
    /// `Retry-After`, then answers a normal completion — the pre-byte
    /// retry/failover driver.
    Flaky {
        refuse: u32,
        status: u16,
        retry_after_secs: Option<u64>,
    },
    /// A whole Anthropic message.
    Messages { usage: Value },
    /// A whole Gemini `generateContent` answer; `usage` is `usageMetadata`.
    Gemini { usage: Value },
    /// A whole embeddings answer with a `prompt_tokens` report.
    Embeddings { tokens: i64 },
    /// A whole rerank answer with a `total_tokens` report.
    Rerank { tokens: i64 },
    /// A whole Responses answer with a `usage` object.
    Responses { usage: Value },
}

/// One request upstream received.
#[derive(Clone)]
struct Seen {
    model: String,
}

#[derive(Clone, Default)]
struct Script {
    answers: Arc<Mutex<HashMap<String, Answer>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}
impl Script {
    fn answers(&self, model: &str, answer: Answer) {
        self.answers
            .lock()
            .unwrap()
            .insert(model.to_owned(), answer);
    }
    fn seen_for(&self, model: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.model == model)
            .count()
    }
}

fn note(script: &Script, body: &Value) {
    script.seen.lock().unwrap().push(Seen {
        model: body["model"].as_str().unwrap_or_default().to_owned(),
    });
}

fn sse(body: Body) -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        body,
    )
        .into_response()
}

/// The `/v1/chat/completions` side.
async fn upstream(State(script): State<Script>, axum::Json(body): axum::Json<Value>) -> Response {
    note(&script, &body);
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    match script.answers.lock().unwrap().get(&model).cloned() {
        Some(Answer::Completion { input, output }) => Json(json!({
            "id": "chatcmpl-serving",
            "object": "chat.completion",
            "created": 1,
            "model": model,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output},
        }))
        .into_response(),
        Some(Answer::Stall) => sse(Body::from_stream(async_stream::stream! {
            yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}],\"usage\":null}\n\n",
            ));
            std::future::pending::<()>().await;
        })),
        Some(Answer::StreamNoUsage) => sse(Body::from(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":null}\n\n\
             data: [DONE]\n\n",
        )),
        Some(Answer::Flaky {
            refuse,
            status,
            retry_after_secs,
        }) => {
            let asked = script
                .seen
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.model == model)
                .count() as u32;
            if asked <= refuse {
                let mut refusal = (
                    StatusCode::from_u16(status).expect("status"),
                    Json(json!({"error": {"message": "scripted refusal"}})),
                )
                    .into_response();
                if let Some(secs) = retry_after_secs {
                    refusal.headers_mut().insert(
                        axum::http::header::RETRY_AFTER,
                        secs.to_string().parse().unwrap(),
                    );
                }
                refusal
            } else {
                Json(json!({
                    "id": "chatcmpl-serving",
                    "object": "chat.completion",
                    "created": 1,
                    "model": model,
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12},
                }))
                .into_response()
            }
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "a foreign answer was scripted on the chat path"}})),
        )
            .into_response(),
    }
}

/// The `/v1/messages` side, in Anthropic's shapes.
async fn messages_upstream(
    State(script): State<Script>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    note(&script, &body);
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    match script.answers.lock().unwrap().get(&model).cloned() {
        Some(Answer::Messages { usage }) => Json(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": "Hello there"}],
            "stop_reason": "end_turn",
            "usage": usage,
        }))
        .into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"type": "error", "error": {"type": "invalid_request_error", "message": "a foreign answer was scripted on the anthropic path"}})),
        )
            .into_response(),
    }
}

/// The `/v1beta` side, in Gemini's shapes; the wildcard carries `{model}:{action}`.
async fn gemini_upstream(
    State(script): State<Script>,
    axum::extract::Path(target): axum::extract::Path<String>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    note(&script, &body);
    let (model, _action) = target.split_once(':').unwrap_or((target.as_str(), ""));
    match script.answers.lock().unwrap().get(model).cloned() {
        Some(Answer::Gemini { usage }) => Json(json!({
            "candidates": [{"index": 0, "content": {"role": "model", "parts": [{"text": "Hello there"}]}, "finishReason": "STOP"}],
            "modelVersion": model,
            "usageMetadata": usage,
        }))
        .into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": 400, "message": "a foreign answer was scripted on the gemini path", "status": "INVALID_ARGUMENT"}})),
        )
            .into_response(),
    }
}

/// The `/v1/responses` side.
async fn responses_upstream(
    State(script): State<Script>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    note(&script, &body);
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    match script.answers.lock().unwrap().get(&model).cloned() {
        Some(Answer::Responses { usage }) => Json(json!({
            "id": "resp_1",
            "object": "response",
            "model": model,
            "status": "completed",
            "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Hello there"}]}],
            "usage": usage,
        }))
        .into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "a foreign answer was scripted on the responses path"}})),
        )
            .into_response(),
    }
}

/// The `/v1/embeddings` and `/v1/rerank` side — the input-only surfaces.
async fn aux_upstream(
    State(script): State<Script>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    note(&script, &body);
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    match script.answers.lock().unwrap().get(&model).cloned() {
        Some(Answer::Embeddings { tokens }) => Json(json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": [0.1, -0.2]}],
            "model": model,
            "usage": {"prompt_tokens": tokens, "total_tokens": tokens},
        }))
        .into_response(),
        Some(Answer::Rerank { tokens }) => Json(json!({
            "id": "rerank-1",
            "model": model,
            "results": [{"index": 0, "relevance_score": 0.98, "document": {"text": "a doc"}}],
            "usage": {"total_tokens": tokens},
        }))
        .into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "a foreign answer was scripted on the input-only path"}})),
        )
            .into_response(),
    }
}

async fn start_upstream(script: Script) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .route("/v1/messages", post(messages_upstream))
        .route("/v1/responses", post(responses_upstream))
        .route("/v1/embeddings", post(aux_upstream))
        .route("/v1/rerank", post(aux_upstream))
        .route("/v1/models/{*target}", post(gemini_upstream))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a free port");
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    address
}

// ── the world ────────────────────────────────────────────────────────────────

struct World {
    app: Router,
    pool: PgPool,
    tenants: Tenants,
    tenant_id: String,
    organization_id: String,
    key: String,
    /// The registering member's session — the owner credential the service
    /// credential mint needs.
    session: String,
    channel: String,
    suffix: String,
    script: Script,
    /// The scripted upstream's base URL, resent on channel settings writes —
    /// `set` re-credentials and re-addresses the channel wholesale.
    base_url: String,
}

impl World {
    fn model(&self, name: &str) -> String {
        format!("{name}-{}", self.suffix)
    }
    fn answers(&self, model: &str, answer: Answer) {
        self.script.answers(&self.model(model), answer);
    }
    async fn wallet(&self) -> Arc<oxsum_core::Wallet> {
        self.tenants
            .get(&self.tenant_id)
            .await
            .expect("the organization's ledger opens")
    }
    /// The settlement entry's description for a request id, parsed.
    async fn settlement(&self, request_id: &str) -> Option<Value> {
        let hold_key = format!("req-{request_id}:hold");
        let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
        let text = self
            .wallet()
            .await
            .receipt_proof(entry_id, None)
            .await
            .expect("the proof is built")
            .map(|bundle| bundle.entry.description().as_str().to_owned());
        text.map(|t| serde_json::from_str(&t).expect("the description is JSON"))
    }
    /// The operation row for a request id — `operation_id` is the request id.
    async fn operation(&self, request_id: &str) -> sqlx::postgres::PgRow {
        sqlx::query(
            "SELECT operation_id, state, charged_minor, outcome_kind, surface, bound_minor, hold_key \
             FROM oxsum.request_operations WHERE operation_id = $1",
        )
        .bind(Uuid::parse_str(request_id).unwrap())
        .fetch_one(&self.pool)
        .await
        .expect("the operation row reads")
    }
    /// The attempts under an operation, oldest first.
    async fn attempts(&self, operation_id: Uuid) -> Vec<sqlx::postgres::PgRow> {
        sqlx::query(
            "SELECT status, usage_json, external_id, channel FROM oxsum.request_attempts \
             WHERE operation_id = $1 ORDER BY started_at",
        )
        .bind(operation_id)
        .fetch_all(&self.pool)
        .await
        .expect("the attempt rows read")
    }
    /// The usage row for a request id: kind, charge, cost basis and provenance.
    async fn usage_row(&self, request_id: &str) -> sqlx::postgres::PgRow {
        sqlx::query(
            "SELECT kind, charged_minor, upstream_cost_minor, cost_basis, upstream_attempts \
             FROM oxsum.usage_records WHERE request_id = $1",
        )
        .bind(request_id)
        .fetch_one(&self.pool)
        .await
        .expect("the usage row reads")
    }
    /// Writes this world's channel `semanticCache` through the admin API — the
    /// same call an operator makes, so the test reads what the gateway reads.
    /// The connection fields are resent unchanged: `set` re-credentials and
    /// re-addresses the channel wholesale.
    async fn semantic_cache(&self, policy: Value) {
        let config = Config::new(Signup::Open, None)
            .with_secret(SecretKey::from_bytes([7; 32]))
            .with_admin_token(TOKEN);
        let app = crate::support::app(Db::from_pool(self.pool.clone()), config);
        let (status, body) = call(
            &app,
            "POST",
            "/api/v1/admin/channels",
            Some(json!({
                "name": self.channel,
                "baseUrl": self.base_url,
                "semanticCache": policy,
            })),
            Some(TOKEN),
        )
        .await;
        assert!(status.is_success(), "the policy write failed: {body}");
    }
}

/// An app over a real database: a funded organization behind one channel whose
/// upstream is scripted, priced flat at `price` minor per million tokens.
async fn world(database_url: &str, top_up: i64, protocol: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;

    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("srv-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
    let script = Script::default();
    let base_url = start_upstream(script.clone()).await;
    db.set_channel_pool_policy(
        &channel,
        &base_url,
        Some(&["upstream-secret".to_owned()]),
        protocol,
        &secret,
        Some(&oxsum_core::ProviderDestinationPolicy {
            network: oxsum_core::ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }),
    )
    .await
    .expect("the channel is written");

    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_admin_token(TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = crate::support::app(db.clone(), config);

    let email = format!("srv_{suffix}@example.com");
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let data = &body["data"];
    let key = data["apiKey"]["secret"].as_str().unwrap().to_owned();
    let organization_id = data["organization"]["id"].as_str().unwrap().to_owned();
    let tenant_id = data["organization"]["tenantId"]
        .as_str()
        .unwrap()
        .to_owned();
    let session = crate::support::login(&app, &email, PASSWORD).await;
    if top_up > 0 {
        let (status, body) = call(
            &app,
            "POST",
            "/__test/funding",
            Some(json!({"idempotencyKey": format!("top-{suffix}"), "amountMinor": top_up})),
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "top-up failed: {body}");
    }

    World {
        app,
        tenants: Tenants::new(pool.clone()),
        pool,
        tenant_id,
        organization_id,
        key,
        session,
        channel,
        suffix,
        script,
        base_url,
    }
}

/// Prices `name` on the world's channel under a billing mode and an optional
/// `upstream` block, at `per_million` minor per input token.
async fn price_model(
    world: &World,
    name: &str,
    mode: BillingMode,
    per_million: i64,
    upstream: Option<i64>,
) -> String {
    let db = Db::from_pool(world.pool.clone());
    let upstream_block = upstream.map(|rate| {
        serde_json::from_value::<oxsum_core::UpstreamPrices>(json!({
            "inputPricePerMillion": rate,
        }))
        .expect("the upstream block parses")
    });
    // The input-only modes carry no output side: their prices keep both output
    // fields zero — the price book refuses an output rate on them (issue #170).
    let output_side = mode.meters_output();
    let price = Price {
        input_price_per_million: per_million,
        output_price_per_million: if output_side { per_million } else { 0 },
        max_output_tokens: if output_side { 1000 } else { 0 },
        cache_read_price_per_million: None,
        cache_write_5m_price_per_million: None,
        cache_write_1h_price_per_million: None,
        reasoning_price_per_million: None,
        cost_per_request: None,
        upstream: upstream_block,
        mode,
        rules: vec![],
    };
    let model = world.model(name);
    db.append_price(&world.channel, &model, price, 100)
        .await
        .expect("the price is written");
    model
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(key) = key {
        request = crate::support::credential(request, key);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                .expect("the test's own request"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A gateway call that returns the raw response (for the request-id header).
/// `header` names the credential spelling the surface reads — `authorization`
/// (OpenAI, sent `Bearer`), `x-api-key` (Anthropic) or `x-goog-api-key`
/// (Gemini); the value is prefixed `Bearer ` only on the `authorization` arm.
async fn gateway_call(app: &Router, key: &str, path: &str, body: Value, header: &str) -> Response {
    let value = if header == "authorization" {
        format!("Bearer {key}")
    } else {
        key.to_owned()
    };
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header(header, value)
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the router answers")
}

/// A gateway call whose body is sent verbatim — the cache's prompt digest reads
/// the *parsed* body while the operation fingerprint hashes the *raw* bytes, so
/// a semantically identical but differently-spelled body shares the cache row
/// yet opens its own operation (issue #604).
async fn gateway_call_raw(app: &Router, key: &str, path: &str, body: String) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(body))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the router answers")
}

fn request_id(response: &Response) -> String {
    response
        .headers()
        .get("x-oxsum-request-id")
        .expect("every gateway response carries a request id")
        .to_str()
        .expect("the request id is ASCII")
        .to_owned()
}

/// Asserts the whole A08 contract for one served turn: the operation went
/// `succeeded` under `surface`, one `answered` attempt carries the verbatim
/// usage the settlement priced, and the usage row's `cost_basis` is
/// `real_attempt` when the price tracked upstream or `missing` when it did not.
async fn assert_served(
    world: &World,
    request_id: &str,
    surface: &str,
    expected_cost_basis: &str,
    expected_charge: i64,
) {
    let operation = world.operation(request_id).await;
    assert_eq!(operation.get::<String, _>("surface"), surface);
    assert_eq!(operation.get::<String, _>("state"), "succeeded");
    assert_eq!(operation.get::<i64, _>("charged_minor"), expected_charge);
    let operation_id: Uuid = operation.get("operation_id");

    let attempts = world.attempts(operation_id).await;
    assert_eq!(attempts.len(), 1, "one upstream attempt served the turn");
    assert_eq!(attempts[0].get::<String, _>("status"), "answered");
    assert!(
        attempts[0].get::<Option<Value>, _>("usage_json").is_some(),
        "the winning attempt carries the usage the settle priced"
    );

    let settlement = world.settlement(request_id).await;
    assert!(settlement.is_some(), "the settlement entry exists");
    assert_eq!(
        settlement.unwrap()["kind"].as_str().unwrap(),
        "usage",
        "the settlement kind is a priced usage turn"
    );

    let usage = world.usage_row(request_id).await;
    assert_eq!(usage.get::<String, _>("cost_basis"), expected_cost_basis);
}

// ── A08: the scripted provider matrix ────────────────────────────────────────

/// Each serving surface produces an operation, an answered attempt and a
/// settlement — verified by reading the rows. The chat turn's usage reports
/// 11 input + 7 output tokens at 1_000_000 minor per million each way = 18.
#[tokio::test]
async fn every_surface_produces_an_operation_and_settles() {
    let world = world_or_skip!(10_000_000, "openai");

    // The chat surface, priced with an `upstream` block so the cost basis is
    // `real_attempt`: 11+7 tokens at 1 minor/token each way = 18 minor.
    let chat_model = price_model(&world, "chat", BillingMode::Chat, 1_000_000, Some(500_000)).await;
    world.answers(
        "chat",
        Answer::Completion {
            input: 11,
            output: 7,
        },
    );
    let response = gateway_call(
        &world.app,
        &world.key,
        "/v1/chat/completions",
        json!({"model": chat_model, "messages": [{"role": "user", "content": "hi"}], "stream": false}),
        "authorization",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_served(&world, &request_id(&response), "chat", "real_attempt", 18).await;

    // The Responses surface — same openai channel, `responses` operation name.
    world.answers(
        "chat",
        Answer::Responses {
            usage: json!({"input_tokens": 11, "output_tokens": 7, "total_tokens": 18}),
        },
    );
    let response = gateway_call(
        &world.app,
        &world.key,
        "/v1/responses",
        json!({"model": world.model("chat"), "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi"}]}], "max_output_tokens": 100}),
        "authorization",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_served(
        &world,
        &request_id(&response),
        "responses",
        "real_attempt",
        18,
    )
    .await;

    // The embeddings surface — input-only, flat 7-token report = 7 minor.
    let emb_model = price_model(
        &world,
        "emb",
        BillingMode::Embeddings,
        1_000_000,
        Some(500_000),
    )
    .await;
    world.answers("emb", Answer::Embeddings { tokens: 7 });
    let response = gateway_call(
        &world.app,
        &world.key,
        "/v1/embeddings",
        json!({"model": emb_model, "input": "hello"}),
        "authorization",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_served(
        &world,
        &request_id(&response),
        "embeddings",
        "real_attempt",
        7,
    )
    .await;

    // The rerank surface — input-only, 9-token report = 9 minor.
    let rank_model = price_model(
        &world,
        "rank",
        BillingMode::Rerank,
        1_000_000,
        Some(500_000),
    )
    .await;
    world.answers("rank", Answer::Rerank { tokens: 9 });
    let response = gateway_call(
        &world.app,
        &world.key,
        "/v1/rerank",
        json!({"model": rank_model, "query": "q", "documents": ["a", "b"]}),
        "authorization",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_served(&world, &request_id(&response), "rerank", "real_attempt", 9).await;
}

/// The Anthropic `/v1/messages` surface produces a `messages` operation that
/// settles the same way — verified by the row, not the status.
#[tokio::test]
async fn the_messages_surface_produces_an_operation() {
    let world = world_or_skip!(10_000_000, "anthropic");
    let model = price_model(&world, "msg", BillingMode::Chat, 1_000_000, Some(500_000)).await;
    world.answers(
        "msg",
        Answer::Messages {
            usage: json!({"input_tokens": 11, "output_tokens": 7}),
        },
    );
    let response = gateway_call(
        &world.app,
        &world.key,
        "/v1/messages",
        json!({"model": model, "max_tokens": 100, "messages": [{"role": "user", "content": "hi"}]}),
        "x-api-key",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_served(
        &world,
        &request_id(&response),
        "messages",
        "real_attempt",
        18,
    )
    .await;
}

/// The Gemini `generateContent` surface produces a `chat` operation (a Gemini
/// turn *is* a chat turn to the row) that settles the same way.
#[tokio::test]
async fn the_gemini_surface_produces_an_operation() {
    let world = world_or_skip!(10_000_000, "gemini");
    let model = price_model(&world, "gem", BillingMode::Chat, 1_000_000, Some(500_000)).await;
    world.answers("gem", Answer::Gemini {
        usage: json!({"promptTokenCount": 11, "candidatesTokenCount": 7, "totalTokenCount": 18}),
    });
    let response = gateway_call(
        &world.app,
        &world.key,
        &format!("/v1beta/models/{model}:generateContent"),
        json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}], "generationConfig": {"maxOutputTokens": 100}}),
        "x-goog-api-key",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_served(&world, &request_id(&response), "chat", "real_attempt", 18).await;
}

// ── the cache hit ────────────────────────────────────────────────────────────

/// A semantic-cache hit answers off the stored body, makes no upstream call and
/// records `outcome_kind = 'cache_hit'` with `cost_basis = 'cache_reuse'` — so a
/// cost read never confuses a re-served answer with a fresh upstream attempt.
#[tokio::test]
async fn a_cache_hit_records_cache_reuse_and_no_upstream_call() {
    let world = world_or_skip!(10_000_000, "openai");
    let model = price_model(
        &world,
        "cached",
        BillingMode::Chat,
        1_000_000,
        Some(500_000),
    )
    .await;
    // The semantic cache only reads the store when the channel opts in — the
    // same `semanticCache` settings write an operator makes.
    world.semantic_cache(json!({"enabled": true})).await;
    world.answers(
        "cached",
        Answer::Completion {
            input: 11,
            output: 7,
        },
    );
    let body =
        json!({"model": model, "messages": [{"role": "user", "content": "hi"}], "stream": false});

    // The first turn caches the answer upstream produced.
    let first = gateway_call(
        &world.app,
        &world.key,
        "/v1/chat/completions",
        body.clone(),
        "authorization",
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let _ = first.into_body().collect().await;
    let upstream_calls = world.script.seen_for(&world.model("cached"));
    assert_eq!(upstream_calls, 1, "the miss called upstream once");

    // A second request that is the same ask spelled differently: the parsed
    // body — and with it the cache's prompt digest — is identical, so it hits
    // the stored answer, while the raw bytes differ, so the operation
    // fingerprint opens a fresh operation row rather than replaying the first.
    let respelled = format!(
        "{{\"stream\":false,\"messages\":[{{\"content\":\"hi\",\"role\":\"user\"}}],\"model\":\"{model}\"}}"
    );
    let second = gateway_call_raw(&world.app, &world.key, "/v1/chat/completions", respelled).await;
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second
            .headers()
            .get("x-oxsum-cache")
            .and_then(|v| v.to_str().ok()),
        Some("hit"),
        "the second identical ask served off the cache"
    );
    let request_id = request_id(&second);
    let _ = second.into_body().collect().await;
    assert_eq!(
        world.script.seen_for(&world.model("cached")),
        1,
        "the hit never re-called upstream"
    );

    let operation = world.operation(&request_id).await;
    assert_eq!(operation.get::<String, _>("outcome_kind"), "cache_hit");
    assert_eq!(operation.get::<String, _>("state"), "succeeded");
    let usage = world.usage_row(&request_id).await;
    assert_eq!(usage.get::<String, _>("cost_basis"), "cache_reuse");
    assert_eq!(usage.get::<i32, _>("upstream_attempts"), 0);
}

// ── pre-byte retry and post-byte cancellation ────────────────────────────────

/// A refused first attempt fails over under the same operation: two `attempt`
/// rows — one `refused`, one `answered` — and a single settle, verified by the
/// operation's one terminal row.
#[tokio::test]
async fn a_prebyte_refusal_retries_under_one_operation() {
    let world = world_or_skip!(10_000_000, "openai");
    let model = price_model(&world, "flaky", BillingMode::Chat, 1_000_000, Some(500_000)).await;
    // The first attempt refuses before any answer byte with a bounded
    // `Retry-After`: the lone route waits the delay once and retries, so the
    // operation records two attempts — a `refused` and the `answered` retry.
    world.answers(
        "flaky",
        Answer::Flaky {
            refuse: 1,
            status: 429,
            retry_after_secs: Some(0),
        },
    );
    let response = gateway_call(
        &world.app,
        &world.key,
        "/v1/chat/completions",
        json!({"model": model, "messages": [{"role": "user", "content": "hi"}], "stream": false}),
        "authorization",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = request_id(&response);

    let operation = world.operation(&request_id).await;
    assert_eq!(operation.get::<String, _>("state"), "succeeded");
    let operation_id: Uuid = operation.get("operation_id");
    let attempts = world.attempts(operation_id).await;
    assert_eq!(
        attempts.len(),
        2,
        "the refusal and the retry are both attempts"
    );
    assert_eq!(attempts[0].get::<String, _>("status"), "refused");
    assert_eq!(attempts[1].get::<String, _>("status"), "answered");
    // One settle: the operation is terminal once, the usage row prices the
    // winning attempt's report.
    let usage = world.usage_row(&request_id).await;
    assert_eq!(usage.get::<i32, _>("upstream_attempts"), 2);
    assert_eq!(usage.get::<String, _>("cost_basis"), "real_attempt");
}

/// A client that disconnects mid-stream ends the operation `cancelled`, and
/// the usage upstream already produced still prices — the turn owes what it
/// consumed, not a fabricated zero.
#[tokio::test]
async fn a_postbyte_disconnect_cancels_but_still_prices() {
    let world = world_or_skip!(10_000_000, "openai");
    let model = price_model(&world, "stall", BillingMode::Chat, 1_000_000, Some(500_000)).await;
    world.answers("stall", Answer::Stall);
    let response = gateway_call(
        &world.app,
        &world.key,
        "/v1/chat/completions",
        json!({"model": model, "messages": [{"role": "user", "content": "hi"}], "stream": true}),
        "authorization",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = request_id(&response);
    // Drop the body after the first chunk: the client walked away mid-stream.
    drop(response.into_body());

    // The dropped turn settles off the request path — wait for the operation
    // to go terminal, then read the rows.
    let mut operation;
    for _ in 0..250 {
        operation = world.operation(&request_id).await;
        if operation.get::<String, _>("state") == "cancelled" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let operation = world.operation(&request_id).await;
    assert_eq!(operation.get::<String, _>("state"), "cancelled");
    assert_eq!(
        operation.get::<String, _>("outcome_kind"),
        "client_cancelled"
    );
    // A stream that reported nothing estimates the forwarded prefix — the cost
    // basis is `estimate`, and the consumed usage still settled, never zero.
    let usage = world.usage_row(&request_id).await;
    assert_eq!(usage.get::<String, _>("cost_basis"), "estimate");
    let settlement = world.settlement(&request_id).await.expect("settled");
    assert_eq!(settlement["kind"].as_str().unwrap(), "client_cancelled");
}

// ── A14: missing and excessive usage ─────────────────────────────────────────

/// A completion upstream never reported usage for settles `estimated`, priced
/// from the local estimate — the usage row's `cost_basis` is `estimate`, never
/// a fabricated `real_attempt`.
#[tokio::test]
async fn a_missing_usage_report_records_an_estimate_basis() {
    let world = world_or_skip!(10_000_000, "openai");
    let model = price_model(&world, "nou", BillingMode::Chat, 1_000_000, Some(500_000)).await;
    // A stream that ends cleanly on `[DONE]` but reports no usage: the degraded
    // policy prices the forwarded prefix from the local estimate.
    world.answers("nou", Answer::StreamNoUsage);
    let response = gateway_call(
        &world.app,
        &world.key,
        "/v1/chat/completions",
        json!({"model": model, "messages": [{"role": "user", "content": "hi"}], "stream": true}),
        "authorization",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = request_id(&response);
    let _ = response.into_body().collect().await;

    let operation = world.operation(&request_id).await;
    assert_eq!(operation.get::<String, _>("state"), "succeeded");
    assert_eq!(operation.get::<String, _>("outcome_kind"), "estimated");
    let settlement = world.settlement(&request_id).await.expect("settled");
    assert_eq!(settlement["kind"].as_str().unwrap(), "estimated");
    // The local estimate produced the price, so the cost basis is `estimate`
    // even though the price carries an `upstream` block (issue #604, A14).
    let usage = world.usage_row(&request_id).await;
    assert_eq!(usage.get::<String, _>("cost_basis"), "estimate");
}

// ── the metering surface ─────────────────────────────────────────────────────

/// A privileged service-credential metering event produces an operation marked
/// `metering`/`metered`, distinct from every customer surface — so a cost
/// read never confuses a staff-triggered internal event with billable usage.
#[tokio::test]
async fn a_metered_event_marks_its_operation() {
    let world = world_or_skip!(10_000_000, "openai");
    // An `event`-mode price on the world's channel for a billable code.
    let code = format!("mail.send-{}", world.suffix);
    let db = Db::from_pool(world.pool.clone());
    db.append_price(
        &world.channel,
        &code,
        Price {
            input_price_per_million: 1_000_000,
            output_price_per_million: 0,
            max_output_tokens: 0,
            cache_read_price_per_million: None,
            cache_write_5m_price_per_million: None,
            cache_write_1h_price_per_million: None,
            reasoning_price_per_million: None,
            cost_per_request: None,
            upstream: Some(
                serde_json::from_value(json!({"inputPricePerMillion": 500_000})).unwrap(),
            ),
            mode: BillingMode::Event,
            rules: vec![],
        },
        100,
    )
    .await
    .expect("the event price is written");
    // Mint a scoped service credential with meteringWrite over the org. The
    // registered member is made a business owner directly — the fixture already
    // initialized the deployment's singleton business for open registration, so
    // the invite-style `accept_business_setup` owner path is closed — then the
    // session selects business context and mints through the business surface,
    // the same credential the real metering intake presents on
    // `/metering/settlements`.
    let user_id = db
        .session_user(&world.session)
        .await
        .expect("the session resolves a user")
        .id;
    sqlx::query(
        "INSERT INTO oxsum.business_staff (business_id, user_id, role) \
         VALUES ((SELECT business_id FROM oxsum.businesses WHERE singleton), $1, 'owner')",
    )
    .bind(user_id)
    .execute(&world.pool)
    .await
    .expect("the member becomes a business owner");
    db.switch_session_context(
        &world.session,
        &oxsum_core::SessionContextRequest {
            idempotency_key: format!("ctx-{}", world.suffix),
            context: oxsum_core::SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .expect("the session selects business context");
    let minted = db
        .create_integration_credential(
            &world.session,
            &integration::request(
                vec![Uuid::parse_str(&world.organization_id).unwrap()],
                vec![oxsum_core::IntegrationScope::MeteringWrite],
            ),
        )
        .await
        .expect("the credential mints");
    let secret = minted.secret.expect("the mint answers a secret");

    // One-shot settle: a metered event that already ran.
    let (status, outcome) = call(
        &world.app,
        "POST",
        "/api/v1/metering/settlements",
        Some(json!({
            "idempotencyKey": format!("meter-{suffix}", suffix = world.suffix),
            "organizationId": world.organization_id,
            "channel": world.channel,
            "billableCode": code,
            "usage": {"inputTokens": 10, "eventType": "mail.send"},
        })),
        Some(&secret),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the metered settle failed: {outcome}"
    );

    // The operation row: `metering` surface, `metered` outcome, `succeeded`.
    let operation = sqlx::query(
        "SELECT surface, state, outcome_kind FROM oxsum.request_operations \
         WHERE organization_id = $1 AND surface = 'metering'",
    )
    .bind(Uuid::parse_str(&world.organization_id).unwrap())
    .fetch_one(&world.pool)
    .await
    .expect("the metered operation reads");
    assert_eq!(operation.get::<String, _>("surface"), "metering");
    assert_eq!(operation.get::<String, _>("state"), "succeeded");
    assert_eq!(operation.get::<String, _>("outcome_kind"), "metered");

    // The usage row it produced carries a `real_attempt` cost basis — the
    // service attested real consumption the price's `upstream` block priced.
    let usage = sqlx::query(
        "SELECT cost_basis FROM oxsum.usage_records \
         WHERE tenant_id = $1 AND event_type = 'mail.send'",
    )
    .bind(&world.tenant_id)
    .fetch_one(&world.pool)
    .await
    .expect("the metered usage row reads");
    assert_eq!(usage.get::<String, _>("cost_basis"), "real_attempt");
}

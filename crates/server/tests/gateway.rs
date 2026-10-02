//! Gateway end-to-end tests: a real wallet, a real HTTP client, and a scripted upstream.
//!
//! The upstream is a second axum server on a free port that answers whatever the test told it to,
//! selected by the model name in the request. That keeps the tests independent of the network and
//! lets one of them stall, refuse, or break its own contract on purpose.
//!
//! Everything a turn leaves behind is checked against the ledger itself: what was reserved, what
//! settled, and what the settlement entry says it was. The tests need DATABASE_URL and skip without
//! it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::BodyExt;
use oxsum_core::{Db, Tenants, input_upper_bound, sweep_stale_holds};
use oxsum_server::{BillingEvent, Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tower::ServiceExt;

/// The operator token the admin app in these tests is configured with.
const OPERATOR_TOKEN: &str = "operator-token-0123456789";

// ── the scripted upstream ────────────────────────────────────────────────────

/// What upstream answers for one model.
#[derive(Clone)]
enum Answer {
    /// A whole completion, with a usage report (`prompt_tokens`, `completion_tokens`) or without.
    Completion { usage: Option<(i64, i64)> },
    /// A streamed completion; `terminated` says whether upstream closes it with `[DONE]`.
    Stream { usage: bool, terminated: bool },
    /// A stream that emits one chunk and then stays open, for as long as the client is there.
    Stall,
    /// Upstream refuses, in OpenAI's own error shape.
    Refuse { status: u16, body: Value },
    /// A 200 whose body is not JSON at all.
    Garbage,
}

/// One request upstream received.
#[derive(Clone)]
struct Seen {
    authorization: Option<String>,
    body: Value,
}

/// The upstream's state: what to answer, and what it has been asked.
#[derive(Clone, Default)]
struct Script {
    answers: Arc<Mutex<HashMap<String, Answer>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Script {
    /// Teaches upstream how to answer for one model.
    fn answers(&self, model: &str, answer: Answer) {
        self.answers
            .lock()
            .unwrap()
            .insert(model.to_owned(), answer);
    }

    /// The requests upstream has received.
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// Starts the scripted upstream and returns its base URL, as a deployment would configure it.
///
/// The base URL ends in `/v1` because that is what an OpenAI-compatible provider publishes, and the
/// gateway appends `/chat/completions` to whatever it is given.
async fn start_upstream(script: Script) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a free port");
    let address = listener.local_addr().expect("the listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}/v1")
}

async fn upstream(
    State(script): State<Script>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    script.seen.lock().unwrap().push(Seen {
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: body.clone(),
    });
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    let answer = script.answers.lock().unwrap().get(&model).cloned();
    let Some(answer) = answer else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": {"message": "no script for this model"}})),
        )
            .into_response();
    };
    match answer {
        Answer::Completion { usage } => {
            let mut completion = json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "Hello there"},
                    "finish_reason": "stop",
                }],
            });
            if let Some((input, output)) = usage {
                completion["usage"] = json!({
                    "prompt_tokens": input,
                    "completion_tokens": output,
                    "total_tokens": input + output,
                });
            }
            axum::Json(completion).into_response()
        }
        Answer::Stream { usage, terminated } => {
            let mut frames = String::from(
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}],\"usage\":null}\n\n\
                 data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}],\"usage\":null}\n\n\
                 data: {\"choices\":[{\"delta\":{\"content\":\" there\"}}],\"usage\":null}\n\n",
            );
            if usage {
                frames.push_str(
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n",
                );
            }
            if terminated {
                frames.push_str("data: [DONE]\n\n");
            }
            sse(Body::from(frames))
        }
        Answer::Stall => sse(Body::from_stream(async_stream::stream! {
            yield Ok::<Bytes, std::io::Error>(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}],\"usage\":null}\n\n",
            ));
            // Never finishes: the client stays connected until it gives up, which is the point.
            std::future::pending::<()>().await;
        })),
        Answer::Refuse { status, body } => (
            StatusCode::from_u16(status).expect("the test's own status"),
            axum::Json(body),
        )
            .into_response(),
        Answer::Garbage => (
            StatusCode::OK,
            [("content-type", "text/plain")],
            "not json at all",
        )
            .into_response(),
    }
}

/// An SSE response carrying frames upstream prepared.
fn sse(body: Body) -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        body,
    )
        .into_response()
}

// ── the world a test runs in ─────────────────────────────────────────────────

/// Every model these tests use, all at one minor unit per token.
///
/// With `inputPricePerMillion` and `outputPricePerMillion` both 1_000_000, a token costs exactly
/// one minor unit, so the arithmetic in the assertions is the token count itself.
///
/// The names carry a per-world suffix because the database is shared between tests: a model belongs
/// to exactly one channel (docs/product.md), so two worlds running at once must not want the same
/// model name — each world has its own upstream, and a model has to resolve to the right one.
fn price_list(suffix: &str) -> String {
    let price = json!({
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 1_000_000,
        "maxOutputTokens": 1000,
    });
    let mut models = serde_json::Map::new();
    for model in [
        "ok",
        "no-usage",
        "stream",
        "stream-open",
        "stall",
        "refuse",
        "garbage",
        "over",
    ] {
        models.insert(format!("{model}-{suffix}"), price.clone());
    }
    Value::Object(models).to_string()
}

/// An app wired to a scripted upstream, one funded organization, and the handles to check it.
struct World {
    app: Router,
    tenants: Tenants,
    tenant_id: String,
    key: String,
    /// The first key's id, for the key-management endpoints.
    key_id: String,
    script: Script,
    /// The channel row this world put its upstream behind.
    channel: String,
    /// What its model names end in.
    suffix: String,
    /// The broadcast sender the gateway publishes billing events to: tests subscribe to
    /// assert what a turn publishes, without opening a dashboard socket.
    billing: broadcast::Sender<BillingEvent>,
    #[allow(dead_code)]
    pool: PgPool,
}

impl World {
    /// One of this world's models, as the gateway and its upstream see it.
    fn model(&self, name: &str) -> String {
        format!("{name}-{}", self.suffix)
    }

    /// Teaches this world's upstream how to answer for one of its models.
    fn answers(&self, model: &str, answer: Answer) {
        self.script.answers(&self.model(model), answer);
    }

    /// The ledger of the organization this world registered.
    async fn wallet(&self) -> Arc<oxsum_core::Wallet> {
        self.tenants
            .get(&self.tenant_id)
            .await
            .expect("the organization's ledger opens")
    }

    /// What the settlement entry for a request says, once it is there.
    async fn settlement(&self, request_id: &str) -> Option<Value> {
        // The settlement names the hold, and its idempotency key — and so its entry id — is
        // derived from the hold's key.
        let hold_key = format!("req-{request_id}:hold");
        let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
        let bundle = self
            .wallet()
            .await
            .receipt_proof(entry_id)
            .await
            .expect("the proof is built")
            .map(|bundle| bundle.entry);
        bundle.map(|entry| {
            serde_json::from_str(entry.description().as_str()).expect("the description is JSON")
        })
    }

    /// Waits for a settlement that happens off the request path, like a cancelled turn's.
    async fn settlement_within(&self, request_id: &str) -> Value {
        for _ in 0..250 {
            if let Some(record) = self.settlement(request_id).await {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the turn never settled");
    }

    /// The hold entry for a request, used to check what was frozen.
    async fn hold(&self, request_id: &str) -> Option<Value> {
        let entry_id = oxsum_core::entry_id_for(&format!("req-{request_id}:hold"));
        let bundle = self
            .wallet()
            .await
            .receipt_proof(entry_id)
            .await
            .expect("the proof is built")
            .map(|bundle| bundle.entry);
        bundle.map(|entry| {
            serde_json::from_str(entry.description().as_str()).expect("the description is JSON")
        })
    }
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

/// A world with `top_up` minor units of credit, or an early return when there is no database.
macro_rules! world {
    ($top_up:expr) => {
        match url() {
            Some(url) => world_for(&url, $top_up).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

async fn world_for(url: &str, top_up: i64) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");

    let suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("mock-{suffix}");
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let script = Script::default();
    let base_url = start_upstream(script.clone()).await;
    let book = oxsum_core::PriceBook::from_json(&channel, &price_list(&suffix))
        .expect("the test's own prices parse");
    // The channel and its prices are rows, the way an operator's would be: the gateway reads the
    // database, and the environment is only what a brand-new deployment seeds its first channel with.
    db.set_channel(&channel, &base_url, "upstream-secret", &secret)
        .await
        .expect("the channel is written");
    for (model, price) in book.models() {
        db.append_price(&channel, model, *price)
            .await
            .expect("the price is written");
    }
    let config = Config::new(Signup::Open, None).with_secret(secret);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, billing) = oxsum_server::app_with_billing(db, config);
    let tenants = Tenants::new(pool.clone());

    let (registration, key) = register(&app).await;
    let tenant_id = registration["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();
    let key_id = registration["apiKey"]["id"]
        .as_str()
        .expect("registration names the key")
        .to_owned();
    if top_up > 0 {
        call(
            &app,
            "POST",
            "/api/v1/topups",
            Some(json!({"idempotencyKey": "top-1", "amountMinor": top_up})),
            Some(&key),
        )
        .await;
    }
    World {
        app,
        tenants,
        tenant_id,
        key,
        key_id,
        script,
        channel,
        suffix,
        billing,
        pool,
    }
}

/// An app over the same database with the operator token configured: the platform admin, changing a
/// price.
fn admin_app(world: &World) -> Router {
    let config = Config::new(Signup::Open, None)
        .with_secret(oxsum_core::SecretKey::from_bytes([7; 32]))
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::app(Db::from_pool(world.pool.clone()), config)
}

/// Registers an organization and returns its document and its first API key.
async fn register(app: &Router) -> (Value, String) {
    let email = format!(
        "gateway_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let key = body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    (body["data"].clone(), key)
}

/// One JSON call against the wallet API.
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
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let request = request
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("the test's own request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// A chat request, answered but not read, so a test can stream it or drop it.
async fn chat(app: &Router, key: &str, body: Value) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the router answers")
}

/// A chat request with a message that costs a known little: two bytes of input.
fn simple(model: &str) -> Value {
    json!({"model": model, "messages": [{"role": "user", "content": "hi"}]})
}

/// The freeze `simple()` should produce, computed the way the request path computes it.
fn expected_freeze(max_tokens: Option<i64>) -> i64 {
    input_upper_bound(&["hi"]) + max_tokens.unwrap_or(1000)
}

/// The request id of a response, which is how its bill is found.
fn request_id(response: &Response) -> String {
    response
        .headers()
        .get("x-oxsum-request-id")
        .expect("every gateway response carries a request id")
        .to_str()
        .expect("the request id is ASCII")
        .to_owned()
}

/// Serialises the test sweepers across test binaries: production runs one sweeper, so the tests
/// take an advisory lock around aging and sweeping, and no two test sweepers run at once. Shared
/// with `sweep.rs`, which takes the same lock.
const SWEEP_LOCK: i64 = i64::from_be_bytes(*b"oxsumswp");

/// Takes the sweep lock, returning the connection that holds it. A previous test that failed
/// while holding the lock fails this one loudly instead of hanging the suite.
async fn lock_sweeper(pool: &PgPool) -> sqlx::pool::PoolConnection<sqlx::Postgres> {
    let mut conn = pool
        .acquire()
        .await
        .expect("a connection for the sweep lock");
    for _ in 0..600 {
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(SWEEP_LOCK)
            .fetch_one(&mut *conn)
            .await
            .expect("tries the sweep lock");
        if locked {
            return conn;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("could not take the sweep lock within a minute");
}

/// Releases the sweep lock.
async fn unlock_sweeper(mut conn: sqlx::pool::PoolConnection<sqlx::Postgres>) {
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(SWEEP_LOCK)
        .execute(&mut *conn)
        .await
        .expect("releases the sweep lock");
}

async fn json_of(response: Response) -> (StatusCode, Value) {
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

async fn text_of(response: Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

// ── the tests ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_whole_answer_is_charged_from_upstreams_usage() {
    let world = world!(1_000_000);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );

    let response = chat(&world.app, &world.key, simple(&world.model("ok"))).await;
    let id = request_id(&response);
    let (status, body) = json_of(response).await;
    assert_eq!(status, StatusCode::OK);
    // The completion is passed through untouched: the caller's SDK parses what it always parsed.
    assert_eq!(body["choices"][0]["message"]["content"], "Hello there");

    // Upstream saw the output ceiling written in, and the caller's own key.
    let seen = world.script.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].authorization.as_deref(),
        Some("Bearer upstream-secret")
    );
    assert_eq!(seen[0].body["max_tokens"], 1000);

    // Ten input tokens and two output tokens, at one minor unit each.
    let record = world.settlement(&id).await.expect("the turn settled");
    assert_eq!(record["kind"], "usage");
    assert_eq!(record["inputTokens"], 10);
    assert_eq!(record["outputTokens"], 2);
    assert_eq!(record["charged"], 12);
    assert_eq!(record["model"], world.model("ok"));
    assert_eq!(record["request"], id);

    // The freeze went back whole, less the charge.
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000 - 12);
    let hold = world.hold(&id).await.expect("the hold is recorded");
    assert_eq!(hold["freeze"], expected_freeze(None));
}

/// A turn publishes its life to the billing broadcast: started when the hold is taken,
/// settled when the charge is written. The dashboard's socket forwards these.
#[tokio::test]
async fn a_turn_publishes_its_billing_events() {
    let world = world!(1_000_000);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );
    let mut events = world.billing.subscribe();

    let response = chat(&world.app, &world.key, simple(&world.model("ok"))).await;
    let id = request_id(&response);
    assert_eq!(response.status(), StatusCode::OK);

    let started = events.recv().await.expect("a started event arrives");
    let BillingEvent::TurnStarted {
        tenant_id,
        request_id,
        model,
        freeze_minor,
        ..
    } = started
    else {
        panic!("the first event is the turn starting, got {started:?}");
    };
    assert_eq!(tenant_id, world.tenant_id);
    assert_eq!(request_id, id);
    assert_eq!(model, world.model("ok"));
    assert_eq!(freeze_minor, expected_freeze(None));

    let settled = events.recv().await.expect("a settled event arrives");
    let BillingEvent::TurnSettled {
        tenant_id,
        request_id,
        charged_minor,
        input_tokens,
        output_tokens,
        ..
    } = settled
    else {
        panic!("the second event is the turn settling, got {settled:?}");
    };
    assert_eq!(tenant_id, world.tenant_id);
    assert_eq!(request_id, id);
    // Ten input tokens and two output tokens, at one minor unit each.
    assert_eq!(input_tokens, 10);
    assert_eq!(output_tokens, 2);
    assert_eq!(charged_minor, 12);
}

#[tokio::test]
async fn a_streamed_turn_charges_the_usage_of_its_last_chunk() {
    let world = world!(1_000_000);
    world.answers(
        "stream",
        Answer::Stream {
            usage: true,
            terminated: true,
        },
    );

    let response = chat(
        &world.app,
        &world.key,
        json!({
            "model": world.model("stream"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    let id = request_id(&response);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let body = text_of(response).await;
    // The frames are passed through, terminator included, exactly once.
    assert!(body.contains("\"content\":\"Hello\""), "{body}");
    assert!(body.contains("data: [DONE]"), "{body}");
    assert_eq!(body.matches("[DONE]").count(), 1, "{body}");
    // Upstream was asked to report usage: without it the last chunk carries no numbers.
    assert_eq!(
        world.script.seen()[0].body["stream_options"]["include_usage"],
        true
    );

    // The stream only ends after the settlement is written, so the balance is already right here.
    let record = world.settlement(&id).await.expect("the turn settled");
    assert_eq!(record["kind"], "usage");
    assert_eq!(record["charged"], 12);
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000 - 12);
}

#[tokio::test]
async fn a_stream_that_ends_without_its_terminator_is_closed_for_the_client() {
    let world = world!(1_000_000);
    world.answers(
        "stream-open",
        Answer::Stream {
            usage: true,
            terminated: false,
        },
    );

    let response = chat(
        &world.app,
        &world.key,
        json!({
            "model": world.model("stream-open"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    let id = request_id(&response);
    let body = text_of(response).await;
    // An SSE reader waits for an event that is not coming unless the stream is closed for it.
    assert_eq!(body.matches("[DONE]").count(), 1, "{body}");
    assert_eq!(world.settlement(&id).await.unwrap()["kind"], "usage");
}

#[tokio::test]
async fn a_missing_usage_report_falls_back_to_a_local_estimate() {
    let world = world!(1_000_000);
    world.answers("no-usage", Answer::Completion { usage: None });

    let response = chat(&world.app, &world.key, simple(&world.model("no-usage"))).await;
    let id = request_id(&response);
    let (status, body) = json_of(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let record = world.settlement(&id).await.expect("the turn settled");
    assert_eq!(record["kind"], "estimated");
    let charged = record["charged"].as_i64().expect("a charge");
    // The estimate prices the prompt and the answer text; it is never more than the freeze.
    assert!(charged > 0, "{record}");
    assert!(charged <= record["freeze"].as_i64().unwrap(), "{record}");
    assert!(record["outputTokens"].as_i64().unwrap() > 0, "{record}");
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000 - charged);
}

#[tokio::test]
async fn usage_above_the_freeze_is_capped_at_it() {
    let world = world!(1_000_000);
    // Upstream reports 10 input and 500 output tokens, which at one minor unit each is far more than
    // a caller who asked for a single output token was frozen for.
    world.answers(
        "over",
        Answer::Completion {
            usage: Some((10, 500)),
        },
    );

    let mut body = simple(&world.model("over"));
    body["max_tokens"] = json!(1);
    let response = chat(&world.app, &world.key, body).await;
    let id = request_id(&response);
    let (status, _) = json_of(response).await;
    assert_eq!(status, StatusCode::OK);

    let record = world.settlement(&id).await.expect("the turn settled");
    let freeze = expected_freeze(Some(1));
    assert_eq!(record["freeze"], freeze);
    // The promise holds: the caller pays the freeze, not the usage that ran past it, and the
    // anomaly is visible as its own kind rather than as a silent loss.
    assert_eq!(record["kind"], "capped");
    assert_eq!(record["charged"], freeze);
    assert_eq!(record["inputTokens"], 10);
    assert_eq!(record["outputTokens"], 500);
    assert_eq!(world.wallet().await.reserved().await.unwrap(), 0);
}

#[tokio::test]
async fn an_upstream_refusal_charges_nothing_and_passes_the_reason_through() {
    let world = world!(1_000_000);
    world.answers(
        "refuse",
        Answer::Refuse {
            status: 500,
            body: json!({"error": {"message": "model overloaded", "type": "api_error"}}),
        },
    );

    let response = chat(&world.app, &world.key, simple(&world.model("refuse"))).await;
    let id = request_id(&response);
    let (status, body) = json_of(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    // Upstream's own words, in the shape the caller's SDK knows.
    assert_eq!(body["error"]["message"], "model overloaded");
    assert_eq!(body["error"]["type"], "api_error");

    let record = world.settlement(&id).await.expect("the turn settled");
    assert_eq!(record["kind"], "upstream_error");
    assert_eq!(record["charged"], 0);
    assert_eq!(record["inputTokens"], 0);
    // The whole freeze went back.
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000);
}

#[tokio::test]
async fn an_unreachable_upstream_charges_nothing() {
    let world = world!(1_000_000);
    // A port that was bound and released: nothing is listening on it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a free port");
    let dead = format!("http://{}/v1", listener.local_addr().unwrap());
    drop(listener);
    // Point this world's channel at the dead address and build the app again from the same rows:
    // that is what changing a channel's connection does, and the prices are untouched by it.
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let db = Db::from_pool(world.pool.clone());
    db.set_channel(&world.channel, &dead, "upstream-secret", &secret)
        .await
        .expect("the channel is repointed");
    let config = Config::new(Signup::Open, None).with_secret(secret);
    let app = oxsum_server::app(db, config);

    let response = chat(&app, &world.key, simple(&world.model("ok"))).await;
    let id = request_id(&response);
    let (status, body) = json_of(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "UPSTREAM_ERROR");

    let record = world.settlement(&id).await.expect("the turn settled");
    assert_eq!(record["kind"], "upstream_unreachable");
    assert_eq!(record["charged"], 0);
    assert_eq!(world.wallet().await.reserved().await.unwrap(), 0);
}

#[tokio::test]
async fn a_200_that_is_not_json_is_upstream_breaking_its_contract() {
    let world = world!(1_000_000);
    world.answers("garbage", Answer::Garbage);

    let response = chat(&world.app, &world.key, simple(&world.model("garbage"))).await;
    let id = request_id(&response);
    let (status, body) = json_of(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "UPSTREAM_ERROR");

    // What came back is still priced, rather than handed over for nothing.
    let record = world.settlement(&id).await.expect("the turn settled");
    assert_eq!(record["kind"], "estimated");
    assert!(record["charged"].as_i64().unwrap() > 0, "{record}");
    assert_eq!(world.wallet().await.reserved().await.unwrap(), 0);
}

#[tokio::test]
async fn a_freeze_beyond_the_balance_is_refused_before_upstream_is_contacted() {
    let world = world!(500);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );

    let response = chat(&world.app, &world.key, simple(&world.model("ok"))).await;
    let (status, body) = json_of(response).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["error"]["type"], "insufficient_quota");
    assert_eq!(body["error"]["code"], "INSUFFICIENT_FUNDS");
    // The message states the price, what is there, and what the caller can do about it.
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&expected_freeze(None).to_string()),
        "{message}"
    );
    assert!(message.contains("500"), "{message}");
    assert!(message.contains("max_tokens"), "{message}");
    // Nothing was frozen, and upstream never heard about it.
    assert!(world.script.seen().is_empty());
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.available().await.unwrap(), 500);
}

#[tokio::test]
async fn the_output_ceiling_scales_the_freeze() {
    let world = world!(1_000_000);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );

    let mut body = simple(&world.model("ok"));
    body["max_tokens"] = json!(7);
    let response = chat(&world.app, &world.key, body).await;
    let id = request_id(&response);
    let (status, _) = json_of(response).await;
    assert_eq!(status, StatusCode::OK);

    // A caller who asks for less output freezes less: two bytes of input, seven tokens, plus the
    // per-message overhead the input bound adds.
    let hold = world.hold(&id).await.expect("the hold is recorded");
    assert_eq!(hold["freeze"], expected_freeze(Some(7)));
    assert_eq!(world.script.seen()[0].body["max_tokens"], 7);
}

#[tokio::test]
async fn content_that_is_not_text_is_refused_without_freezing_anything() {
    let world = world!(1_000_000);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );

    let response = chat(
        &world.app,
        &world.key,
        json!({
            "model": world.model("ok"),
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is this"},
                    {"type": "image_url", "image_url": {"url": "https://example.test/x.png"}},
                ],
            }],
        }),
    )
    .await;
    let id = request_id(&response);
    let (status, body) = json_of(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["param"], "messages");
    assert!(world.script.seen().is_empty());
    assert!(world.hold(&id).await.is_none(), "nothing was frozen");
    assert_eq!(world.wallet().await.reserved().await.unwrap(), 0);
}

#[tokio::test]
async fn a_model_without_a_price_is_refused() {
    let world = world!(1_000_000);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );

    let (status, body) = json_of(chat(&world.app, &world.key, simple("not-served")).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(body["error"]["param"], "model");
    assert!(world.script.seen().is_empty());
}

#[tokio::test]
async fn a_client_that_goes_away_settles_what_it_received() {
    let world = world!(1_000_000);
    world.answers("stall", Answer::Stall);

    let response = chat(
        &world.app,
        &world.key,
        json!({
            "model": world.model("stall"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    let id = request_id(&response);
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    // Read the first frame, so part of the answer has been forwarded to this client.
    let frame = body
        .frame()
        .await
        .expect("upstream sent a frame")
        .expect("the frame is readable");
    let frame = frame.into_data().expect("the frame is data");
    assert!(String::from_utf8_lossy(&frame).contains("partial"));

    // The client goes away: hyper drops the body, which cancels the upstream call.
    drop(body);

    let record = world.settlement_within(&id).await;
    assert_eq!(record["kind"], "client_cancelled");
    let charged = record["charged"].as_i64().expect("a charge");
    assert!(charged > 0, "the forwarded part was priced: {record}");
    assert!(charged <= record["freeze"].as_i64().unwrap(), "{record}");
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000 - charged);
}

/// An OpenAI SDK client hangs up the moment it reads the terminator, and over a real socket that
/// lands while the gateway is appending the settlement. That used to cancel the write with the plan
/// already spent: no entry was written, and the freeze stayed reserved until the sweeper. The bill is
/// upstream's own usage, because the turn upstream finished is the turn the user received.
#[tokio::test]
async fn a_client_that_hangs_up_at_the_terminator_still_gets_its_usage_bill() {
    let world = world!(1_000_000);
    world.answers(
        "stream",
        Answer::Stream {
            usage: true,
            terminated: true,
        },
    );

    // The gateway on a real socket, so the client can hang up the way an SDK does.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a free port");
    let address = listener.local_addr().expect("the listener has an address");
    let app = world.app.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let mut response = reqwest::Client::new()
        .post(format!("http://{address}/v1/chat/completions"))
        .bearer_auth(&world.key)
        .json(&json!({
            "model": world.model("stream"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }))
        .send()
        .await
        .expect("the gateway answers");
    assert_eq!(response.status(), StatusCode::OK);
    let id = response
        .headers()
        .get("x-oxsum-request-id")
        .expect("every response carries a request id")
        .to_str()
        .expect("the request id is ASCII")
        .to_owned();

    // Read exactly as far as the terminator, then hang up.
    let mut frames = String::new();
    while let Some(chunk) = response.chunk().await.expect("the stream is readable") {
        frames.push_str(&String::from_utf8_lossy(&chunk));
        if frames.contains("[DONE]") {
            break;
        }
    }
    assert!(frames.contains("[DONE]"), "{frames}");
    drop(response);

    // The turn upstream had finished is billed as finished, from its own counts.
    let record = world.settlement_within(&id).await;
    assert_eq!(record["kind"], "usage", "{record}");
    assert_eq!(record["inputTokens"], 10);
    assert_eq!(record["outputTokens"], 2);
    assert_eq!(record["charged"], 12);
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000 - 12);
}

/// A turn that settles normally leaves no watch row behind: the sweeper only ever sees holds
/// whose request never settled.
#[tokio::test]
async fn a_settled_turn_leaves_no_watch_row() {
    let world = world!(1_000_000);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );

    let response = chat(&world.app, &world.key, simple(&world.model("ok"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let record = world.settlement(&id).await.expect("the turn settled");
    assert_eq!(record["kind"], "usage");

    // The settled turn left no watch row: other tests may have in-flight rows of their own, so
    // this checks the request's row rather than the whole table.
    let db = Db::from_pool(world.pool.clone());
    let rows = db
        .stale_open_holds(OffsetDateTime::now_utc() + Duration::from_secs(3600))
        .await
        .expect("lists the watch rows");
    assert!(
        !rows.iter().any(|row| row.request_id == id),
        "the settled turn left a watch row"
    );
}

/// A stalled turn's hold is swept once it passes the timeout: the whole freeze is released and
/// the settlement is recorded as the `swept` anomaly. This is the crash the sweeper exists for —
/// upstream never answered, so without it the freeze would sit reserved forever.
#[tokio::test]
async fn a_stalled_turn_is_swept_when_its_hold_times_out() {
    let world = world!(1_000_000);
    // One sweeper at a time across test binaries: the sweep below must not resolve another
    // binary's aged rows, nor have its own stolen.
    let sweeper = lock_sweeper(&world.pool).await;
    world.answers("stall", Answer::Stall);

    let response = chat(
        &world.app,
        &world.key,
        json!({
            "model": world.model("stall"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    let id = request_id(&response);
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let _ = body.frame().await.expect("a frame").expect("readable");

    // The turn is in flight and watched: the freeze is reserved and the row names the request.
    // Other tests may have in-flight rows of their own, so this checks the request's row.
    let db = Db::from_pool(world.pool.clone());
    let wallet = world.wallet().await;
    assert!(wallet.reserved().await.expect("reserved") > 0);
    let rows = db
        .stale_open_holds(OffsetDateTime::now_utc() + Duration::from_secs(3600))
        .await
        .expect("lists the watch rows");
    assert_eq!(
        rows.iter().filter(|row| row.request_id == id).count(),
        1,
        "the in-flight turn has no watch row"
    );

    // Past the timeout, the sweeper settles it at 0 with kind `swept`. The row is aged on
    // purpose: staleness is global, so the test scopes it with age and other tests' young rows
    // stay untouched.
    let hold_key = format!("req-{id}:hold");
    sqlx::query("UPDATE oxsum.open_holds SET opened_at = $1 WHERE hold_key = $2")
        .bind(OffsetDateTime::now_utc() - Duration::from_secs(3600))
        .bind(&hold_key)
        .execute(db.pool())
        .await
        .expect("ages the watch row");
    let resolved = sweep_stale_holds(
        &db,
        &world.tenants,
        OffsetDateTime::now_utc() - Duration::from_secs(30 * 60),
        OffsetDateTime::now_utc().date(),
    )
    .await
    .expect("sweeps");
    assert_eq!(resolved, 1);

    let record = world.settlement_within(&id).await;
    assert_eq!(record["kind"], "swept");
    assert_eq!(record["charged"], 0);
    assert_eq!(wallet.reserved().await.expect("reserved"), 0);
    assert_eq!(wallet.available().await.expect("available"), 1_000_000);
    unlock_sweeper(sweeper).await;
    drop(body);
}

#[tokio::test]
async fn concurrent_turns_cannot_overdraw_the_wallet() {
    let world = world!(1500);
    world.answers("stall", Answer::Stall);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );

    // The first turn is in flight and holding: its freeze alone is most of the balance.
    let first = chat(
        &world.app,
        &world.key,
        json!({
            "model": world.model("stall"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    let first_id = request_id(&first);
    assert_eq!(first.status(), StatusCode::OK);
    let mut body = first.into_body();
    let _ = body.frame().await.expect("a frame").expect("readable");

    // A second turn of the same size cannot be frozen on top of it.
    let second = chat(&world.app, &world.key, simple(&world.model("ok"))).await;
    let (status, refused) = json_of(second).await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{refused}");
    assert_eq!(refused["error"]["code"], "INSUFFICIENT_FUNDS");
    // And it never reached upstream.
    assert_eq!(world.script.seen().len(), 1);

    drop(body);
    let record = world.settlement_within(&first_id).await;
    assert_eq!(record["kind"], "client_cancelled");
    // The refused turn left nothing behind.
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
}

#[tokio::test]
async fn a_price_change_lands_on_later_requests_and_not_on_the_one_in_flight() {
    let world = world!(1_000_000);
    world.answers("stall", Answer::Stall);

    // A streamed turn on the stalling answer: frozen, in flight, and priced by version 1.
    let first = chat(
        &world.app,
        &world.key,
        json!({
            "model": world.model("stall"),
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    let first_id = request_id(&first);
    assert_eq!(first.status(), StatusCode::OK);
    let mut body = first.into_body();
    let _ = body.frame().await.expect("a frame").expect("readable");

    // The platform admin changes that model's price while the turn is in flight.
    let admin = admin_app(&world);
    let (status, changed) = call(
        &admin,
        "POST",
        &format!("/api/v1/admin/channels/{}/prices", world.channel),
        Some(json!({
            "model": world.model("stall"),
            "inputPricePerMillion": 10_000_000,
            "outputPricePerMillion": 10_000_000,
            "maxOutputTokens": 1000,
        })),
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["data"]["version"], 2);

    // The turn that was already in flight settles at the version it started on, and says which.
    drop(body);
    let record = world.settlement_within(&first_id).await;
    assert_eq!(record["channel"], world.channel.as_str());
    assert_eq!(record["priceVersion"], 1);
    assert_eq!(record["inputPrice"], 1_000_000);
    assert_eq!(record["outputPrice"], 1_000_000);

    // A turn that starts after the change is priced by version 2: the same call, ten times the price.
    world.answers(
        "stall",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );
    let second = chat(&world.app, &world.key, simple(&world.model("stall"))).await;
    let second_id = request_id(&second);
    let (status, forwarded) = json_of(second).await;
    assert_eq!(status, StatusCode::OK, "{forwarded}");
    let record = world
        .settlement(&second_id)
        .await
        .expect("the turn settled");
    assert_eq!(record["priceVersion"], 2);
    assert_eq!(record["inputPrice"], 10_000_000);
    assert_eq!(record["charged"], 120);
}

#[tokio::test]
async fn the_model_list_is_what_this_deployment_serves() {
    let world = world!(0);
    let request = Request::builder()
        .uri("/v1/models")
        .header("authorization", format!("Bearer {}", world.key))
        .body(Body::empty())
        .expect("the test's own request");
    let (status, body) = json_of(
        world
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["object"], "list");
    // The list is every priced model in the database, so this test looks for its own rather than
    // assuming it is the only world in there. Its channel is reported as the owner, and the creation
    // time is the version's, not a placeholder.
    let listed = body["data"].as_array().expect("a list of models");
    for name in ["ok", "stream"] {
        let mine = listed
            .iter()
            .find(|model| model["id"] == world.model(name).as_str())
            .unwrap_or_else(|| panic!("{} is listed in {listed:?}", world.model(name)));
        assert_eq!(mine["object"], "model");
        assert_eq!(mine["owned_by"], world.channel.as_str());
        assert!(mine["created"].as_i64().expect("a creation time") > 0);
    }
}

#[tokio::test]
async fn a_deployment_whose_key_is_missing_refuses_to_start() {
    let world = world!(1_000_000);
    // The database holds channels, and a deployment built without the key that seals their
    // credentials cannot open any of them. It says so at startup instead of answering every relayed
    // request with a 500 for a mistake of the operator's.
    let error = oxsum_server::prepare(
        &Db::from_pool(world.pool.clone()),
        &Config::new(Signup::Open, None),
    )
    .await
    .expect_err("a deployment that cannot open its channels must not start");
    let error = error.to_string();
    assert!(error.contains("OXSUM_SECRET_KEY"), "{error}");
}

#[tokio::test]
async fn a_key_spend_limit_is_refused_before_upstream_is_contacted() {
    let world = world!(1_000_000);
    world.answers(
        "ok",
        Answer::Completion {
            usage: Some((10, 2)),
        },
    );

    // Cap the key below what a turn freezes: the wallet could cover it, the key may not.
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({"spendLimitMinor": 100})),
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response = chat(&world.app, &world.key, simple(&world.model("ok"))).await;
    let (status, body) = json_of(response).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["type"], "insufficient_quota");
    assert_eq!(body["error"]["code"], "KEY_LIMIT_EXCEEDED");
    // The message names the numbers, so the caller can act on it.
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("100"), "{message}");
    // Nothing was frozen, and upstream never heard about it.
    assert!(world.script.seen().is_empty());
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
}

//! Gateway end-to-end tests for model-level fallbacks (issue #500): a request
//! naming `model` may serve under another model when its own channels all fail.
//!
//! The chain is configured per model through the admin surface and resolved
//! independently per model — each fallback's own channels, its own alias
//! target, its own route order. These tests check the failover path end to
//! end: the requested model's channels try first, each permitted fallback
//! follows in order, a fallback the caller may not reach is skipped rather
//! than refused, and the settlement's v12 `requestedModel`/`servedModel` fields
//! name the failover. The upstream is a scripted axum server on a free port, so a
//! refusal and an answer are deterministic.
//!
//! The tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::BodyExt;
use oxsum_core::{Db, Tenants};
use oxsum_server::{BillingEvent, Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::broadcast;
use tower::ServiceExt;

/// The operator token the admin app in these tests is configured with.
const OPERATOR_TOKEN: &str = "operator-token-0123456789";

// ── the scripted upstream ────────────────────────────────────────────────────

/// What upstream answers for one model.
#[derive(Clone)]
enum Answer {
    /// A whole completion with a usage report.
    Completion { usage: Option<(i64, i64)> },
    /// Upstream refuses, in OpenAI's own error shape.
    Refuse { status: u16, body: Value },
}

/// The upstream's state: what to answer, and what it has been asked.
#[derive(Clone, Default)]
struct Script {
    answers: Arc<Mutex<HashMap<String, Answer>>>,
    seen: Arc<Mutex<Vec<String>>>,
}
impl Script {
    /// Teaches upstream how to answer for one model.
    fn answers(&self, model: &str, answer: Answer) {
        self.answers
            .lock()
            .unwrap()
            .insert(model.to_owned(), answer);
    }

    /// The model each received request named, in order.
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

/// Starts the scripted upstream and returns its base URL, as a deployment
/// would configure it. The `/v1` tail is what a provider publishes; the
/// gateway appends the protocol's own path to it.
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

/// The `/v1/chat/completions` side of the scripted upstream: records the model
/// the body named, then answers what the script said for it.
async fn upstream(State(script): State<Script>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    script.seen.lock().unwrap().push(model.clone());
    match script.answers.lock().unwrap().get(&model).cloned() {
        Some(Answer::Completion { usage }) => axum::Json(completion(model, usage)).into_response(),
        Some(Answer::Refuse { status, body }) => {
            (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": {"message": "no scripted answer"}})),
        )
            .into_response(),
    }
}

/// A whole chat completion in OpenAI's shape, with whatever usage upstream
/// reports.
fn completion(model: String, usage: Option<(i64, i64)>) -> Value {
    let usage = usage.map(|(prompt_tokens, completion_tokens)| {
        json!({
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        })
    });
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop",
        }],
        "usage": usage,
    })
}

// ── the world a test runs in ─────────────────────────────────────────────────

/// An app wired to a scripted upstream, one funded organization, and the
/// handles to check it.
struct World {
    app: Router,
    tenants: Tenants,
    tenant_id: String,
    key: String,
    /// The key's id, for minting a mandate on it.
    key_id: String,
    script: Script,
    /// The channel row this world put its upstream behind.
    #[allow(dead_code)]
    channel: String,
    /// What its model names end in.
    suffix: String,
    /// The broadcast sender the gateway publishes billing events to.
    #[allow(dead_code)]
    billing: broadcast::Sender<BillingEvent>,
    /// The world's own pool — helpers poll the schema the API writes.
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

    /// The settlement entry's description for a request, verbatim.
    async fn settlement_description(&self, request_id: &str) -> Option<String> {
        let hold_key = format!("req-{request_id}:hold");
        let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
        let wallet = self
            .tenants
            .get(&self.tenant_id)
            .await
            .expect("the organization's ledger opens");
        let bundle = wallet
            .receipt_proof(entry_id, None)
            .await
            .expect("the proof is built")
            .map(|bundle| bundle.entry);
        bundle.map(|entry| entry.description().as_str().to_owned())
    }

    /// What the settlement entry for a request says, once it is there.
    async fn settlement(&self, request_id: &str) -> Option<Value> {
        self.settlement_description(request_id)
            .await
            .map(|text| serde_json::from_str(&text).expect("the description is JSON"))
    }

    /// Waits for a settlement that happens off the request path.
    async fn settlement_within(&self, request_id: &str) -> Value {
        for _ in 0..250 {
            if let Some(record) = self.settlement(request_id).await {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the turn never settled");
    }
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("fallbacks")
}

/// A world with `top_up` minor units of credit, or an early return when there
/// is no database.
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

/// With input and output both 1_000_000 per million, a token costs exactly one
/// minor unit. The model names carry the world's suffix because the database is
/// shared between tests: a model belongs to exactly one channel.
fn price_list(suffix: &str) -> String {
    let price = json!({
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 1_000_000,
        "maxOutputTokens": 1000,
    });
    let mut models = serde_json::Map::new();
    for model in ["req", "fall-a", "fall-b", "other"] {
        models.insert(format!("{model}-{suffix}"), price.clone());
    }
    Value::Object(models).to_string()
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
    db.set_channel(&channel, &base_url, "upstream-secret", "openai", &secret)
        .await
        .expect("the channel is written");
    for (model, price) in book.models() {
        db.append_price(&channel, model, price.clone(), 100)
            .await
            .expect("the price is written");
    }
    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, billing, _metrics) = oxsum_server::app_with_billing(db, config);
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
            None,
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

/// An app over the same database with the operator token configured: the
/// platform admin, writing a fallback chain.
fn admin_app(world: &World) -> Router {
    let config = Config::new(Signup::Open, None)
        .with_secret(oxsum_core::SecretKey::from_bytes([7; 32]))
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::app(Db::from_pool(world.pool.clone()), config)
}

/// Registers an organization and returns its document and its first API key.
async fn register(app: &Router) -> (Value, String) {
    let email = format!(
        "fallback_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
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

/// One JSON call against the API; the optional mandate rides the
/// `x-oxsum-mandate` header.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    mandate: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    if let Some(mandate) = mandate {
        request = request.header("x-oxsum-mandate", mandate);
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A chat request, answered but not read, so a test can stream it or drop it.
async fn chat(app: &Router, key: &str, mandate: Option<&str>, body: Value) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"));
    if let Some(mandate) = mandate {
        request = request.header("x-oxsum-mandate", mandate);
    }
    let request = request
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the router answers")
}

/// A chat request with a message that costs a known little.
fn simple(model: &str) -> Value {
    json!({"model": model, "messages": [{"role": "user", "content": "hi"}]})
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

/// Writes a fallback chain on `model` through the admin surface.
async fn set_fallbacks(world: &World, model: &str, fallbacks: &[&str]) -> StatusCode {
    let admin = admin_app(world);
    let (status, _body) = call(
        &admin,
        "PUT",
        &format!("/api/v1/admin/model-fallbacks/{model}"),
        Some(json!({ "fallbacks": fallbacks })),
        Some(OPERATOR_TOKEN),
        None,
    )
    .await;
    status
}

// ── the cases ────────────────────────────────────────────────────────────────

/// A requested model whose only channel refuses falls to its configured
/// fallback: the fallback answers, and the settlement records the served name.
#[tokio::test]
async fn a_failed_model_falls_back_to_the_next() {
    let world = world!(10_000_000);
    let requested = world.model("req");
    let fallback = world.model("fall-a");
    world.answers(
        "req",
        Answer::Refuse {
            status: 502,
            body: json!({"error": {"message": "bad gateway"}}),
        },
    );
    world.answers(
        "fall-a",
        Answer::Completion {
            usage: Some((5, 3)),
        },
    );
    assert_eq!(
        set_fallbacks(&world, &requested, &[&fallback]).await,
        StatusCode::OK,
        "the chain is written"
    );

    let response = chat(&world.app, &world.key, None, simple(&requested)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    // The body must be read so the turn settles.
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    // The requested model was tried, then the fallback answered.
    assert_eq!(
        world.script.seen(),
        vec![requested.clone(), fallback.clone()],
        "the chain walked in order"
    );
    let record = world.settlement_within(&id).await;
    assert_eq!(record["requestedModel"], requested);
    assert_eq!(record["servedModel"], fallback);
    assert_eq!(record["model"], fallback, "model stays the served name");
    assert!(
        record["upstreamAttempts"].as_i64().unwrap() >= 2,
        "every try counted: {record}"
    );
}

/// A mandate whose `models` names only the requested model keeps the fallback
/// out of reach: the fallback is skipped, not served, and the turn refuses
/// with the requested model's own upstream error.
#[tokio::test]
async fn a_mandate_denied_fallback_is_skipped_not_served() {
    let world = world!(10_000_000);
    let requested = world.model("req");
    let fallback = world.model("fall-a");
    world.answers(
        "req",
        Answer::Refuse {
            status: 502,
            body: json!({"error": {"message": "bad gateway"}}),
        },
    );
    world.answers(
        "fall-a",
        Answer::Completion {
            usage: Some((5, 3)),
        },
    );
    assert_eq!(
        set_fallbacks(&world, &requested, &[&fallback]).await,
        StatusCode::OK
    );

    // A mandate delegating only the requested model.
    let (status, created) = call(
        &world.app,
        "POST",
        &format!("/api/v1/org/keys/{}/mandates", world.key_id),
        Some(json!({
            "capMinor": 5_000_000,
            "models": [requested],
            "notAfter": rfc3339(
                time::OffsetDateTime::now_utc() + time::Duration::hours(1)
            ),
        })),
        Some(&world.key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mint failed: {created}");
    let blob = created["data"]["mandateBlob"]
        .as_str()
        .expect("the mint answers the signed blob")
        .to_owned();

    let response = chat(&world.app, &world.key, Some(&blob), simple(&requested)).await;
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    // The requested model was tried; the mandate-denied fallback was not.
    assert_eq!(
        world.script.seen(),
        vec![requested.clone()],
        "the denied fallback was never a route"
    );
}

/// A chain walks its configured order: `req` fails, `fall-a` fails, `fall-b`
/// answers, and the served model is the last one tried.
#[tokio::test]
async fn a_chain_walks_its_models_in_order() {
    let world = world!(10_000_000);
    let requested = world.model("req");
    let first = world.model("fall-a");
    let second = world.model("fall-b");
    let refuse = Answer::Refuse {
        status: 502,
        body: json!({"error": {"message": "bad gateway"}}),
    };
    world.answers("req", refuse.clone());
    world.answers("fall-a", refuse);
    world.answers(
        "fall-b",
        Answer::Completion {
            usage: Some((5, 3)),
        },
    );
    assert_eq!(
        set_fallbacks(&world, &requested, &[&first, &second]).await,
        StatusCode::OK
    );

    let response = chat(&world.app, &world.key, None, simple(&requested)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    assert_eq!(
        world.script.seen(),
        vec![requested.clone(), first.clone(), second.clone()],
        "the chain walked req → fall-a → fall-b"
    );
    let record = world.settlement_within(&id).await;
    assert_eq!(record["servedModel"], second);
    assert_eq!(record["requestedModel"], requested);
}

/// A chain naming a model nothing serves is refused at write time: an unserved
/// name would dead-end a request that already failed once, so it is a 400.
#[tokio::test]
async fn an_unserved_fallback_name_is_a_400() {
    let world = world!(10_000_000);
    let requested = world.model("req");
    let status = set_fallbacks(&world, &requested, &["no-such-model"]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an unserved name is a 400");
}

/// A clean serve under the requested model records `servedModel` — and
/// `model`, its long-standing alias — equal to the requested name, while
/// `requestedModel` is omitted: the fields only differ on a cross-model
/// failover, and omitting the redundant name keeps the record inside the
/// ledger's 512-char description cap.
#[tokio::test]
async fn a_clean_serve_records_the_served_model() {
    let world = world!(10_000_000);
    let requested = world.model("req");
    world.answers(
        "req",
        Answer::Completion {
            usage: Some((5, 3)),
        },
    );

    let response = chat(&world.app, &world.key, None, simple(&requested)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    let record = world.settlement_within(&id).await;
    assert_eq!(record["v"], 12);
    assert!(
        record.get("requestedModel").is_none(),
        "requestedModel is omitted on a clean serve: {record}"
    );
    assert_eq!(record["servedModel"], requested);
    assert_eq!(record["model"], requested);
}

/// The RFC 3339 timestamp the mandate-mint request parses.
fn rfc3339(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .expect("the timestamp formats")
}

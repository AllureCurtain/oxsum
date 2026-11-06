//! The durable request operation's end-to-end coverage (issue #601): a task
//! submit writes an operation, a settled task completes it, a replayed submit
//! replays it, and a streamed chat turn writes its attempt and settles.
//!
//! The world mirrors the tasks suite's — a scripted upstream, a `tasks`
//! channel for the relay, an `openai` channel for the chat surface — and the
//! poll pass is driven by `run_due` directly. Everything the operation layer
//! writes is checked against `request_operations`/`request_attempts` rows
//! themselves. Needs `DATABASE_URL` and skips without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use oxsum_core::{Db, Retention, SecretKey, kinds};
use oxsum_server::jobs::{JobContext, run_due};
use oxsum_server::{BillingEvent, Config, Metrics, Signup};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use tower::ServiceExt;
use uuid::Uuid;

const OPERATOR_TOKEN: &str = "operator-token-0123456789";

// ── the scripted upstream ────────────────────────────────────────────────

/// What upstream answers for `POST /tasks` — the submit leg.
#[derive(Clone)]
enum SubmitAnswer {
    Accept { task_id: String },
}

/// What upstream answers for `GET /tasks/{id}` — the poll leg.
#[derive(Clone)]
enum PollAnswer {
    Succeeded { result: Value, usage: Option<Value> },
}

/// The upstream's state: what to answer, and what it saw.
#[derive(Clone, Default)]
struct Script {
    submits: Arc<Mutex<HashMap<String, SubmitAnswer>>>,
    polls: Arc<Mutex<HashMap<String, PollAnswer>>>,
}

impl Script {
    fn submit(&self, model: &str, answer: SubmitAnswer) {
        self.submits
            .lock()
            .unwrap()
            .insert(model.to_owned(), answer);
    }
    fn poll(&self, external_id: &str, answer: PollAnswer) {
        self.polls
            .lock()
            .unwrap()
            .insert(external_id.to_owned(), answer);
    }
}

/// The scripted upstream: `POST /tasks` submits, `GET /tasks/{id}` polls, and
/// `POST /v1/chat/completions` answers a fixed chat body.
async fn start_upstream(script: Script) -> String {
    let app = Router::new()
        .route("/tasks", post(submit_upstream))
        .route("/tasks/{id}", get(poll_upstream))
        .route("/chat/completions", post(chat_upstream))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a free port");
    let address = listener.local_addr().expect("the listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

async fn submit_upstream(
    State(script): State<Script>,
    _headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    let answer = script.submits.lock().unwrap().get(&model).cloned();
    match answer {
        Some(SubmitAnswer::Accept { task_id }) => {
            axum::Json(json!({"id": task_id, "status": "submitted"})).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": {"message": "no submit script"}})),
        )
            .into_response(),
    }
}

async fn poll_upstream(
    State(script): State<Script>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let answer = script.polls.lock().unwrap().get(&id).cloned();
    match answer {
        Some(PollAnswer::Succeeded { result, usage }) => axum::Json(json!({
            "id": id, "status": "succeeded", "result": result, "usage": usage,
        }))
        .into_response(),
        None => axum::Json(json!({"id": id, "status": "running"})).into_response(),
    }
}

/// A fixed chat answer with a usage report — the settle's priced counts.
async fn chat_upstream() -> Response {
    axum::Json(json!({
        "id": "chatcmpl-fixture-9",
        "object": "chat.completion",
        "choices": [{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
    }))
    .into_response()
}

// ── the world ────────────────────────────────────────────────────────────

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("operations")
}

struct World {
    app: Router,
    pool: PgPool,
    db: Db,
    organization_id: Uuid,
    key: String,
    /// The login session — kept so `app_with_billing`'s wiring is the same as
    /// every other suite's; unused here because the member read takes the key.
    #[allow(dead_code)]
    session: String,
    script: Script,
    /// The broadcast sender — kept like the tasks suite keeps its own.
    #[allow(dead_code)]
    billing: broadcast::Sender<BillingEvent>,
}

/// An app wired to the scripted upstream, a funded organization, a `tasks`
/// channel serving `task-model` and an `openai` channel serving `chat-model`.
async fn world(url: &str, top_up: i64) -> World {
    let pool = PgPool::connect(url).await.expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let secret = SecretKey::from_bytes([7; 32]);
    let script = Script::default();
    let base_url = start_upstream(script.clone()).await;
    for (channel, protocol, model, price) in [
        (
            format!("tasks-{suffix}"),
            "tasks",
            "task-model",
            oxsum_core::Price {
                input_price_per_million: 0,
                output_price_per_million: 0,
                max_output_tokens: 0,
                cache_read_price_per_million: None,
                cache_write_5m_price_per_million: None,
                cache_write_1h_price_per_million: None,
                reasoning_price_per_million: None,
                cost_per_request: Some(500),
                upstream: None,
                mode: oxsum_core::BillingMode::Event,
                rules: Vec::new(),
            },
        ),
        (
            format!("chat-{suffix}"),
            "openai",
            "chat-model",
            oxsum_core::Price {
                input_price_per_million: 1_000_000,
                output_price_per_million: 1_000_000,
                max_output_tokens: 1_000_000,
                cache_read_price_per_million: None,
                cache_write_5m_price_per_million: None,
                cache_write_1h_price_per_million: None,
                reasoning_price_per_million: None,
                cost_per_request: None,
                upstream: None,
                mode: oxsum_core::BillingMode::Chat,
                rules: Vec::new(),
            },
        ),
    ] {
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
        db.append_price(&channel, model, price, 100)
            .await
            .expect("the price is written");
    }
    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, billing, _metrics) = support::app_with_billing(db.clone(), config);
    let (registration, key) = register(&app).await;
    let session = support::login(
        &app,
        registration["user"]["email"].as_str().unwrap(),
        "correct horse battery",
    )
    .await;
    let organization_id: Uuid = registration["organization"]["id"]
        .as_str()
        .expect("registration names the organization")
        .parse()
        .expect("the organization id is a uuid");
    if top_up > 0 {
        call(
            &app,
            "POST",
            "/__test/funding",
            Some(json!({"idempotencyKey": "top-1", "amountMinor": top_up})),
            Some(&key),
        )
        .await;
    }
    World {
        app,
        pool,
        db,
        organization_id,
        key,
        session,
        script,
        billing,
    }
}

fn ctx() -> JobContext {
    JobContext {
        hold_timeout: Duration::from_secs(60),
        metrics: Metrics::new(),
        webhook: Some((reqwest::Client::new(), SecretKey::from_bytes([7; 32]))),
        retention: Retention::default(),
        upstream_price_sync: false,
        witness: None,
        bill_provider: None,
    }
}

async fn register(app: &Router) -> (Value, String) {
    let email = format!(
        "ops_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
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
        request = support::credential(request, key);
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

/// `POST /v1/tasks`.
async fn submit_task(app: &Router, key: &str, body: Value, idem: Option<&str>) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/tasks")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"));
    if let Some(idem) = idem {
        request = request.header("idempotency-key", idem);
    }
    let request = request
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the router answers")
}

/// A due `poll-tasks` row, the way the startup pass enqueues it.
async fn enqueue_poll(db: &Db) {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO oxsum.jobs (kind, run_at, requires_leader) \
         VALUES ($1, now() - interval '1 second', false) \
         ON CONFLICT DO NOTHING RETURNING job_id",
    )
    .bind(kinds::POLL_TASKS)
    .fetch_optional(db.pool())
    .await
    .unwrap();
    assert!(inserted.is_some(), "a due poll-tasks run was enqueued");
}

/// The operation row for a request id — `operation_id` is the request/task
/// id the caller already quotes.
async fn operation_row(pool: &PgPool, operation_id: Uuid) -> Option<sqlx::postgres::PgRow> {
    sqlx::query(
        "SELECT state, hold_key, charged_minor, outcome_kind, surface, bound_minor \
         FROM oxsum.request_operations WHERE operation_id = $1",
    )
    .bind(operation_id)
    .fetch_optional(pool)
    .await
    .expect("the operation row reads")
}

// ── the tests ────────────────────────────────────────────────────────────

/// A task submit writes its operation row: surface `task:image`, the hold
/// bound, the lease stamped under this process's instance id.
#[tokio::test]
async fn a_submit_writes_an_operation() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: "up-task-op-1".to_owned(),
        },
    );
    let response = submit_task(
        &world.app,
        &world.key,
        json!({"kind": "image", "model": "task-model", "input": {"prompt": "a red cube"}}),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &response
            .into_body()
            .collect()
            .await
            .expect("collects")
            .to_bytes(),
    )
    .unwrap();
    let task_id: Uuid = body["taskId"].as_str().unwrap().parse().unwrap();

    let row = operation_row(&world.pool, task_id)
        .await
        .expect("the operation row exists");
    assert_eq!(row.get::<String, _>("surface"), "task:image");
    assert_eq!(row.get::<String, _>("state"), "running");
    assert_eq!(
        row.get::<String, _>("hold_key"),
        format!("req-{task_id}:hold")
    );
    assert_eq!(row.get::<i64, _>("bound_minor"), 500);
}

/// The settle lands the operation terminal: `succeeded`, `charged_minor`
/// matching the flat task price, `outcome_kind` the settlement's spelling.
#[tokio::test]
async fn a_settled_task_completes_its_operation() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    let external = "up-task-op-2";
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: external.to_owned(),
        },
    );
    world.script.poll(
        external,
        PollAnswer::Succeeded {
            result: json!({"images": ["https://cdn/1.png"]}),
            usage: Some(json!({"input_tokens": 3, "output_tokens": 0})),
        },
    );
    let response = submit_task(
        &world.app,
        &world.key,
        json!({"kind": "image", "model": "task-model", "input": {"prompt": "a red cube"}}),
        None,
    )
    .await;
    let body: Value = serde_json::from_slice(
        &response
            .into_body()
            .collect()
            .await
            .expect("collects")
            .to_bytes(),
    )
    .unwrap();
    let task_id: Uuid = body["taskId"].as_str().unwrap().parse().unwrap();

    enqueue_poll(&world.db).await;
    run_due(&world.db, &ctx()).await;

    let row = operation_row(&world.pool, task_id)
        .await
        .expect("the operation row exists");
    assert_eq!(row.get::<String, _>("state"), "succeeded");
    assert_eq!(row.get::<i64, _>("charged_minor"), 500);
    assert_eq!(row.get::<String, _>("outcome_kind"), "usage");
}

/// The same `Idempotency-Key` and body replays the submit — same `taskId`,
/// and the operation row the first write created is the one the replay reads:
/// one row, not two.
#[tokio::test]
async fn a_replayed_submit_replays_the_operation() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: "up-task-op-3".to_owned(),
        },
    );
    let submit = json!({"kind": "image", "model": "task-model", "input": {"prompt": "a red cube"}});
    let first = submit_task(&world.app, &world.key, submit.clone(), Some("idem-op-1")).await;
    assert_eq!(first.status(), StatusCode::OK);
    let first_body: Value = serde_json::from_slice(
        &first
            .into_body()
            .collect()
            .await
            .expect("collects")
            .to_bytes(),
    )
    .unwrap();
    let first_id: Uuid = first_body["taskId"].as_str().unwrap().parse().unwrap();

    let second = submit_task(&world.app, &world.key, submit, Some("idem-op-1")).await;
    assert_eq!(second.status(), StatusCode::OK);
    let second_body: Value = serde_json::from_slice(
        &second
            .into_body()
            .collect()
            .await
            .expect("collects")
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(
        second_body["taskId"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap(),
        first_id,
        "the replay names the same task"
    );
    // One operation row: the replay's insert lost to the first's fingerprint.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.request_operations WHERE organization_id = $1",
    )
    .bind(world.organization_id)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

/// A streamed chat turn: the operation goes `succeeded`, the winning attempt
/// carries upstream's usage verbatim, and upstream's own request `id` stamps
/// the attempt's `external_id`.
#[tokio::test]
async fn a_chat_turn_writes_attempts_and_settles() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    let (status, _body) = call(
        &world.app,
        "POST",
        "/v1/chat/completions",
        Some(json!({
            "model": "chat-model",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 8,
            "stream": false,
        })),
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The turn's request id is the operation's id — read it off the one row
    // this world's writes produced.
    let row = sqlx::query(
        "SELECT operation_id, state, charged_minor, outcome_kind, surface \
         FROM oxsum.request_operations WHERE organization_id = $1",
    )
    .bind(world.organization_id)
    .fetch_one(&world.pool)
    .await
    .expect("the operation row exists");
    let operation_id: Uuid = row.get("operation_id");
    assert_eq!(row.get::<String, _>("state"), "succeeded");
    assert_eq!(row.get::<String, _>("surface"), "chat");
    assert_eq!(row.get::<String, _>("outcome_kind"), "usage");
    // 11 input + 7 output tokens at 1_000_000 per million = 18 minor.
    assert_eq!(row.get::<i64, _>("charged_minor"), 18);

    // The winning attempt carries the verbatim usage and upstream's own id.
    let attempt = sqlx::query(
        "SELECT status, usage_json, external_id FROM oxsum.request_attempts \
         WHERE operation_id = $1",
    )
    .bind(operation_id)
    .fetch_one(&world.pool)
    .await
    .expect("the attempt row exists");
    assert_eq!(attempt.get::<String, _>("status"), "answered");
    assert_eq!(
        attempt.get::<Value, _>("usage_json")["inputTokens"],
        11,
        "the attempt carries the settled record's counts"
    );
    assert_eq!(
        attempt.get::<Option<String>, _>("external_id").as_deref(),
        Some("chatcmpl-fixture-9"),
        "upstream's own request id stamped the attempt"
    );
}

/// The member list reads the organization's own operations, newest first —
/// and the detail read carries the attempts the turn wrote. The credential
/// here is the org's own API key: a consumption key's read scopes to the
/// operations it admitted, which is this world's whole list.
#[tokio::test]
async fn the_member_operations_list_reads_back() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    call(
        &world.app,
        "POST",
        "/v1/chat/completions",
        Some(json!({
            "model": "chat-model",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 8,
            "stream": false,
        })),
        Some(&world.key),
    )
    .await;

    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/operations",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let operations = body["data"]["operations"].as_array().expect("a list");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0]["surface"], "chat");
    assert_eq!(operations[0]["state"], "succeeded");
    let operation_id = operations[0]["operationId"].as_str().unwrap();

    let (status, detail) = call(
        &world.app,
        "GET",
        &format!("/api/v1/operations/{operation_id}"),
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["data"]["operationId"], operation_id);
    assert_eq!(detail["data"]["attempts"].as_array().unwrap().len(), 1);
}

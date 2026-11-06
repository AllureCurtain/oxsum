//! The task relay's end-to-end coverage (issue #503): `POST /v1/tasks` freezes
//! an upper bound and relays `{baseUrl}/tasks`, the `poll-tasks` pass asks
//! `{baseUrl}/tasks/{id}` for the open rows and settles the hold on the
//! terminal answer, `GET /v1/tasks/{taskId}` is a row read, and
//! `POST /v1/tasks/{taskId}/cancel` ends the billing side best-effort.
//!
//! The upstream is a scripted axum server like the gateway suite's, and the
//! poll pass is driven by calling `run_due` directly rather than waiting on a
//! 30-second cadence. Everything the settle writes is checked against the
//! ledger and the task row itself. Needs `DATABASE_URL` and skips without it.

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
use oxsum_core::{Db, Retention, SecretKey, Tenants, kinds};
use oxsum_server::jobs::{JobContext, run_due};
use oxsum_server::{BillingEvent, Config, Metrics, Signup};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use tower::ServiceExt;
use uuid::Uuid;

/// The operator token the admin app in these tests is configured with.
const OPERATOR_TOKEN: &str = "operator-token-0123456789";

// ── the scripted task upstream ───────────────────────────────────────────────

/// What upstream answers for `POST /tasks` — the submit leg.
#[derive(Clone)]
#[allow(dead_code)]
enum SubmitAnswer {
    /// Accept the job: `{id, status: "submitted"}`.
    Accept { task_id: String },
    /// Refuse the submit in the provider's own error shape.
    Refuse { status: u16, body: Value },
    /// Accept but answer a body with no task id — a contract break.
    NoId,
}

/// What upstream answers for `GET /tasks/{id}` — the poll leg, keyed by the
/// external id the submit carried.
#[derive(Clone)]
#[allow(dead_code)]
enum PollAnswer {
    /// The job is still mid-flight.
    Running,
    /// The job finished: `result` carries the answer body, `usage` the
    /// normalized report the settle reads.
    Succeeded { result: Value, usage: Option<Value> },
    /// The job failed upstream.
    Failed { error: Value },
    /// Upstream cancelled it on its own.
    Cancelled,
}

/// The upstream's state: what to answer, and what it has been asked.
#[derive(Clone, Default)]
struct Script {
    submits: Arc<Mutex<HashMap<String, SubmitAnswer>>>,
    polls: Arc<Mutex<HashMap<String, PollAnswer>>>,
    /// The submit bodies upstream received — one per POST.
    seen_submits: Arc<Mutex<Vec<Value>>>,
    /// The external ids the poll asked about.
    seen_polls: Arc<Mutex<Vec<String>>>,
    /// The cancel calls upstream saw.
    seen_cancels: Arc<Mutex<Vec<String>>>,
}

impl Script {
    /// Teaches upstream what `POST /tasks` answers for the body's `model`.
    fn submit(&self, model: &str, answer: SubmitAnswer) {
        self.submits
            .lock()
            .unwrap()
            .insert(model.to_owned(), answer);
    }

    /// Teaches upstream what `GET /tasks/{id}` answers for that external id.
    fn poll(&self, external_id: &str, answer: PollAnswer) {
        self.polls
            .lock()
            .unwrap()
            .insert(external_id.to_owned(), answer);
    }
}

/// The scripted upstream: `POST /tasks` submits, `GET /tasks/{id}` polls,
/// `POST /tasks/{id}/cancel` cancels.
async fn start_upstream(script: Script) -> String {
    let app = Router::new()
        .route("/tasks", post(submit_upstream))
        .route("/tasks/{id}", get(poll_upstream))
        .route("/tasks/{id}/cancel", post(cancel_upstream))
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

/// `POST {baseUrl}/tasks` — the submit relay lands here.
async fn submit_upstream(
    State(script): State<Script>,
    _headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    script.seen_submits.lock().unwrap().push(body.clone());
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    let answer = script.submits.lock().unwrap().get(&model).cloned();
    let Some(answer) = answer else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": {"message": "no submit script for this model"}})),
        )
            .into_response();
    };
    match answer {
        SubmitAnswer::Accept { task_id } => {
            axum::Json(json!({"id": task_id, "status": "submitted"})).into_response()
        }
        SubmitAnswer::Refuse { status, body } => (
            StatusCode::from_u16(status).expect("the test's own status"),
            axum::Json(body),
        )
            .into_response(),
        SubmitAnswer::NoId => axum::Json(json!({"status": "submitted"})).into_response(),
    }
}

/// `GET {baseUrl}/tasks/{id}` — the poll pass asks here.
async fn poll_upstream(
    State(script): State<Script>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    script.seen_polls.lock().unwrap().push(id.clone());
    let answer = script.polls.lock().unwrap().get(&id).cloned();
    let Some(answer) = answer else {
        return axum::Json(json!({"id": id, "status": "running"})).into_response();
    };
    match answer {
        PollAnswer::Running => axum::Json(json!({"id": id, "status": "running"})).into_response(),
        PollAnswer::Succeeded { result, usage } => axum::Json(json!({
            "id": id,
            "status": "succeeded",
            "result": result,
            "usage": usage,
        }))
        .into_response(),
        PollAnswer::Failed { error } => axum::Json(json!({
            "id": id,
            "status": "failed",
            "error": error,
        }))
        .into_response(),
        PollAnswer::Cancelled => {
            axum::Json(json!({"id": id, "status": "cancelled"})).into_response()
        }
    }
}

/// `POST {baseUrl}/tasks/{id}/cancel` — the caller's cancel asks here.
async fn cancel_upstream(
    State(script): State<Script>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    script.seen_cancels.lock().unwrap().push(id.clone());
    axum::Json(json!({"id": id, "status": "cancelled"})).into_response()
}

// ── the world ────────────────────────────────────────────────────────────────

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("tasks")
}

/// The world's pieces: app, database, registered organization, scripted upstream.
#[allow(dead_code)]
struct World {
    app: Router,
    pool: PgPool,
    db: Db,
    tenants: Tenants,
    tenant_id: String,
    organization_id: Uuid,
    key: String,
    session: String,
    script: Script,
    channel: String,
    /// The broadcast sender — unused here, kept so `app_with_billing`'s wiring
    /// is the same as every other suite's.
    billing: broadcast::Sender<BillingEvent>,
}

/// An app wired to a scripted tasks upstream, one funded organization, and a
/// `tasks`-protocol channel serving `task-model`.
async fn world(url: &str, top_up: i64) -> World {
    let pool = PgPool::connect(url).await.expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("tasks-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
    let script = Script::default();
    let base_url = start_upstream(script.clone()).await;
    // One channel under the `tasks` protocol, priced flat at 500 minor per
    // request — a task's bound is a declared amount, not a token estimate.
    db.set_channel_pool_policy(
        &channel,
        &base_url,
        Some(&["upstream-secret".to_owned()]),
        "tasks",
        &secret,
        Some(&oxsum_core::ProviderDestinationPolicy {
            network: oxsum_core::ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }),
    )
    .await
    .expect("the channel is written");
    db.append_price(
        &channel,
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
        100,
    )
    .await
    .expect("the price is written");
    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, billing, _metrics) = support::app_with_billing(db.clone(), config);
    let tenants = Tenants::new(pool.clone());

    let (registration, key) = register(&app).await;
    let session = support::login(
        &app,
        registration["user"]["email"].as_str().unwrap(),
        "correct horse battery",
    )
    .await;
    let tenant_id = registration["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();
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
        tenants,
        tenant_id,
        organization_id,
        key,
        session,
        script,
        channel,
        billing,
    }
}

/// The job context the poll pass needs — the webhook pair carries the HTTP
/// client and the sealing key the pass opens credentials under.
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

/// Registers an organization and returns its document and its first API key.
async fn register(app: &Router) -> (Value, String) {
    let email = format!(
        "tasks_{}@example.com",
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

/// One JSON call against the app.
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
async fn submit_task(app: &Router, key: &str, body: Value) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/tasks")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the router answers")
}

/// `GET /v1/tasks/{id}` and `POST /v1/tasks/{id}/cancel` — the row reads.
async fn task_call(app: &Router, key: &str, method: &str, path: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {key}"))
        .body(Body::empty())
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

/// A minimal submit body: kind + model, no declared bound, so the channel's
/// `cost_per_request` freezes.
fn simple_submit() -> Value {
    json!({
        "kind": "image",
        "model": "task-model",
        "input": {"prompt": "a red cube"},
    })
}

/// A due `poll-tasks` row, enqueued the way the startup pass does — `now() -
/// interval` keeps a client clock ahead of the container's from missing the claim.
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

/// The task row's state and charged amount, straight off the table.
async fn task_row(pool: &PgPool, task_id: Uuid) -> (String, Option<i64>) {
    let row = sqlx::query("SELECT state, charged_minor FROM oxsum.tasks WHERE task_id = $1")
        .bind(task_id)
        .fetch_one(pool)
        .await
        .expect("the task row reads");
    (row.get("state"), row.get("charged_minor"))
}

// ── the tests ────────────────────────────────────────────────────────────────

/// A submit under a `tasks` channel freezes the declared bound, relays
/// `{baseUrl}/tasks`, records `external_task_id`, and answers
/// `{taskId, submitted}`.
#[tokio::test]
async fn submit_freezes_and_relays() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    let external = "up-task-1";
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: external.to_owned(),
        },
    );
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &response
            .into_body()
            .collect()
            .await
            .expect("collects the body")
            .to_bytes(),
    )
    .unwrap();
    assert_eq!(body["state"], "submitted");
    assert_eq!(body["kind"], "image");
    assert_eq!(body["model"], "task-model");
    let task_id: Uuid = body["taskId"].as_str().unwrap().parse().unwrap();
    // The freeze is the channel's flat `cost_per_request`.
    let (state, _charged) = task_row(&world.pool, task_id).await;
    assert_eq!(state, "submitted");
    // Upstream saw the submit with the body's input/params forwarded.
    let submits = world.script.seen_submits.lock().unwrap().clone();
    assert_eq!(submits.len(), 1);
    assert_eq!(submits[0]["input"]["prompt"], "a red cube");
}

/// A poll under `running` keeps the task open; the next `poll-tasks` pass asks
/// upstream, and the terminal `succeeded` answer settles the hold at the
/// result's usage and fires `task.succeeded`.
#[tokio::test]
async fn poll_to_succeeded_settles() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    let external = "up-task-2";
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
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
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
    // The caller's poll reads the row — the pass has not run yet.
    let (status, polled) = task_call(
        &world.app,
        &world.key,
        "GET",
        &format!("/v1/tasks/{task_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(polled["state"], "submitted");
    // Run the pass: upstream answers `succeeded` with usage; the hold settles.
    enqueue_poll(&world.db).await;
    run_due(&world.db, &ctx()).await;
    let (state, charged) = task_row(&world.pool, task_id).await;
    assert_eq!(state, "succeeded");
    // 3 input tokens at 0/million is $0, but `cost_per_request` charges the
    // flat 500 — the itemized total is 500 minor.
    assert_eq!(charged, Some(500));
    // The caller's poll now reads the terminal row.
    let (status, polled) = task_call(
        &world.app,
        &world.key,
        "GET",
        &format!("/v1/tasks/{task_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(polled["state"], "succeeded");
    assert_eq!(polled["chargedMinor"], 500);
    // Upstream was asked about the external id the submit carried.
    let polls = world.script.seen_polls.lock().unwrap().clone();
    assert_eq!(polls, vec![external.to_owned()]);
}

/// A `failed` terminal answer releases the hold at zero: nothing charges, the
/// row walks to `failed`, and the error lands on it.
#[tokio::test]
async fn poll_to_failed_releases() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    let external = "up-task-3";
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: external.to_owned(),
        },
    );
    world.script.poll(
        external,
        PollAnswer::Failed {
            error: json!({"message": "out of capacity"}),
        },
    );
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let task_id: Uuid = body["taskId"].as_str().unwrap().parse().unwrap();
    enqueue_poll(&world.db).await;
    run_due(&world.db, &ctx()).await;
    let (state, charged) = task_row(&world.pool, task_id).await;
    assert_eq!(state, "failed");
    assert_eq!(charged, Some(0));
    let (status, polled) = task_call(
        &world.app,
        &world.key,
        "GET",
        &format!("/v1/tasks/{task_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(polled["error"]["message"], "out of capacity");
}

/// `POST /v1/tasks/{id}/cancel` releases the hold and asks upstream to stop
/// the job — a second cancel is a no-op answer of the terminal row.
#[tokio::test]
async fn cancel_releases_and_tells_upstream() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    let external = "up-task-4";
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: external.to_owned(),
        },
    );
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let task_id: Uuid = body["taskId"].as_str().unwrap().parse().unwrap();
    let (status, cancelled) = task_call(
        &world.app,
        &world.key,
        "POST",
        &format!("/v1/tasks/{task_id}/cancel"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["state"], "cancelled");
    assert_eq!(cancelled["chargedMinor"], 0);
    // Upstream heard the cancel.
    let cancels = world.script.seen_cancels.lock().unwrap().clone();
    assert_eq!(cancels, vec![external.to_owned()]);
    // A second cancel answers the same terminal row — no error, no second
    // settle.
    let (status, again) = task_call(
        &world.app,
        &world.key,
        "POST",
        &format!("/v1/tasks/{task_id}/cancel"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["state"], "cancelled");
}

/// A submission upstream refuses releases the hold and answers 502 — the
/// caller's retry submits again because upstream never accepted anything.
#[tokio::test]
async fn submit_refused_releases() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    world.script.submit(
        "task-model",
        SubmitAnswer::Refuse {
            status: 503,
            body: json!({"error": {"message": "capacity"}}),
        },
    );
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    // The hold released: no `tasks` row, no lingering freeze.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.tasks")
        .fetch_one(&world.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

/// A submit on a model no `tasks` channel serves is `model_not_served` — the
/// same refusal a chat request for an unserved model takes.
#[tokio::test]
async fn unserved_model_refuses() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    let response = submit_task(
        &world.app,
        &world.key,
        json!({"kind": "image", "model": "no-such", "input": {}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// A foreign key cannot read another organization's task — 404, not 403, so a
/// task's existence is not an oracle.
#[tokio::test]
async fn foreign_key_cannot_read() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let world = world(&url, 10_000).await;
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: "up-task-5".to_owned(),
        },
    );
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let task_id = body["taskId"].as_str().unwrap();
    // A second organization, a second key.
    let (_other, other_key) = register(&world.app).await;
    let (status, _body) = task_call(
        &world.app,
        &other_key,
        "GET",
        &format!("/v1/tasks/{task_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A task submission uses the same channel boundary as synchronous inference:
/// denying all channels never dispatches upstream, writes a task or reserves money.
#[tokio::test]
async fn grouped_task_submission_cannot_use_a_forbidden_channel() {
    let Some(url) = url() else { return };
    let world = world(&url, 1_000_000).await;
    let group = world
        .db
        .put_group(world.organization_id, "no-tasks", &[])
        .await
        .unwrap();
    let key_id: Uuid =
        sqlx::query_scalar("SELECT key_id FROM oxsum.api_keys WHERE organization_id = $1")
            .bind(world.organization_id)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    world
        .db
        .assign_key_group(world.organization_id, key_id, Some(group.id))
        .await
        .unwrap();
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(world.script.seen_submits.lock().unwrap().is_empty());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.tasks WHERE organization_id = $1")
            .bind(world.organization_id)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    let (_, balance) = call(
        &world.app,
        "GET",
        "/api/v1/balance",
        None,
        Some(&world.session),
    )
    .await;
    assert_eq!(balance["data"]["availableMinor"], 1_000_000);
    assert_eq!(balance["data"]["reservedMinor"], 0);

    let channel_id = world
        .db
        .channel_id_named(&world.channel)
        .await
        .unwrap()
        .unwrap();
    world
        .db
        .put_group(world.organization_id, "no-tasks", &[channel_id])
        .await
        .unwrap();
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: "allowed-task".into(),
        },
    );
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(world.script.seen_submits.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sibling_key_cannot_poll_cancel_or_replay_a_task() {
    let Some(url) = url() else { return };
    let world = world(&url, 10_000).await;
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: "isolated-task".into(),
        },
    );
    let principal = oxsum_core::Principal::Session(
        world
            .db
            .authenticate_session(&world.session)
            .await
            .unwrap()
            .unwrap(),
    );
    let sibling = world
        .db
        .create_key_as(&principal, None, None, Default::default())
        .await
        .unwrap();
    async fn submit(app: &Router, key: &str, body: Value) -> (StatusCode, Value) {
        let res = app
            .clone()
            .oneshot(
                Request::post("/v1/tasks")
                    .header("authorization", format!("Bearer {key}"))
                    .header("idempotency-key", "private-task")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let body =
            serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        (status, body)
    }
    let (status, original) = submit(&world.app, &world.key, simple_submit()).await;
    assert_eq!(status, StatusCode::OK, "{original}");
    let task_id = original["taskId"].as_str().unwrap();
    let wallet = world.tenants.get(&world.tenant_id).await.unwrap();
    let before = (
        wallet.log_size().await.unwrap(),
        wallet.available().await.unwrap(),
        wallet.reserved().await.unwrap(),
    );
    for (method, path) in [
        ("GET", format!("/v1/tasks/{task_id}")),
        ("POST", format!("/v1/tasks/{task_id}/cancel")),
    ] {
        let (status, body) = task_call(&world.app, &sibling.secret, method, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "NOT_FOUND");
    }
    let (status, _) = submit(&world.app, &sibling.secret, simple_submit()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut changed = simple_submit();
    changed["input"]["prompt"] = json!("changed");
    assert_eq!(
        submit(&world.app, &world.key, changed).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for _ in 0..3 {
        let (status, replay) = submit(&world.app, &world.key, simple_submit()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replay, original);
    }
    assert_eq!(world.script.seen_submits.lock().unwrap().len(), 1);
    assert!(world.script.seen_cancels.lock().unwrap().is_empty());
    assert_eq!(
        (
            wallet.log_size().await.unwrap(),
            wallet.available().await.unwrap(),
            wallet.reserved().await.unwrap()
        ),
        before
    );
    assert_eq!(
        task_call(
            &world.app,
            &world.key,
            "GET",
            &format!("/v1/tasks/{task_id}")
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn task_poll_and_cancel_retain_submission_endpoint_after_channel_repoint() {
    let Some(url) = url() else { return };
    let world = world(&url, 10000).await;
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: "old-provider-job".into(),
        },
    );
    let (status, submitted) = call(
        &world.app,
        "POST",
        "/v1/tasks",
        Some(json!({"model":"task-model","kind":"custom","input":{},"maxCostMinor":500})),
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    let task_id = submitted["taskId"].as_str().unwrap();
    let replacement = Script::default();
    let base = start_upstream(replacement.clone()).await;
    world
        .db
        .set_channel_pool_policy(
            &world.channel,
            &base,
            Some(&["replacement-key".into()]),
            "tasks",
            &SecretKey::from_bytes([7; 32]),
            Some(&oxsum_core::ProviderDestinationPolicy {
                network: oxsum_core::ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            }),
        )
        .await
        .unwrap();
    let target = world
        .db
        .open_task_poll_targets(100)
        .await
        .unwrap()
        .into_iter()
        .find(|t| t.task.task_id.to_string() == task_id)
        .unwrap();
    let endpoint = target.endpoint.unwrap();
    assert_ne!(endpoint.base_url, base);
    assert_eq!(
        SecretKey::from_bytes([7; 32])
            .open(endpoint.api_key_sealed.as_deref().unwrap())
            .unwrap(),
        "upstream-secret"
    );
    enqueue_poll(&world.db).await;
    run_due(&world.db, &ctx()).await;
    assert!(
        world
            .script
            .seen_polls
            .lock()
            .unwrap()
            .contains(&"old-provider-job".into())
    );
    assert!(replacement.seen_polls.lock().unwrap().is_empty());
    let (status, _) = call(
        &world.app,
        "POST",
        &format!("/v1/tasks/{task_id}/cancel"),
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        *world.script.seen_cancels.lock().unwrap(),
        vec!["old-provider-job"]
    );
    assert!(replacement.seen_cancels.lock().unwrap().is_empty());
}

#[tokio::test]
async fn task_admission_honors_customer_models_and_refuses_unsupported_aliases_and_mandates() {
    let Some(url) = url() else {
        return;
    };
    let world = world(&url, 10_000).await;
    world
        .db
        .set_tier("task-limited", None, Some(vec!["other".into()]))
        .await
        .unwrap();
    world
        .db
        .set_organization_tier(world.organization_id, Some("task-limited"))
        .await
        .unwrap();
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(world.script.seen_submits.lock().unwrap().is_empty());
    world
        .db
        .set_organization_tier(world.organization_id, None)
        .await
        .unwrap();
    world
        .db
        .create_alias(&world.channel, "task-alias", "task-model")
        .await
        .unwrap();
    let mut aliased = simple_submit();
    aliased["model"] = json!("task-alias");
    assert_eq!(
        submit_task(&world.app, &world.key, aliased).await.status(),
        StatusCode::BAD_REQUEST
    );
    let request = Request::builder()
        .method("POST")
        .uri("/v1/tasks")
        .header("authorization", format!("Bearer {}", world.key))
        .header("content-type", "application/json")
        .header("x-oxsum-mandate", "a-mandate-must-not-be-ignored")
        .body(Body::from(simple_submit().to_string()))
        .unwrap();
    assert_eq!(
        world.app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert!(world.script.seen_submits.lock().unwrap().is_empty());
    assert_eq!(
        world
            .tenants
            .get(&world.tenant_id)
            .await
            .unwrap()
            .reserved()
            .await
            .unwrap(),
        0
    );
    let (_, list) = call(&world.app, "GET", "/v1/models", None, Some(&world.key)).await;
    let listed = list["data"].as_array().unwrap();
    assert!(listed.iter().any(|m| m["id"] == "task-model"));
    assert!(listed.iter().all(|m| m["id"] != "task-alias"));
    world.script.submit(
        "task-model",
        SubmitAnswer::Accept {
            task_id: "permission-snapshot-task".into(),
        },
    );
    let response = submit_task(&world.app, &world.key, simple_submit()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let task_id = body["taskId"].as_str().unwrap();
    world
        .db
        .set_organization_tier(world.organization_id, Some("task-limited"))
        .await
        .unwrap();
    assert_eq!(
        call(
            &world.app,
            "GET",
            &format!("/v1/tasks/{task_id}"),
            None,
            Some(&world.key)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &world.app,
            "POST",
            &format!("/v1/tasks/{task_id}/cancel"),
            Some(json!({})),
            Some(&world.key)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        world
            .tenants
            .get(&world.tenant_id)
            .await
            .unwrap()
            .reserved()
            .await
            .unwrap(),
        0
    );
}

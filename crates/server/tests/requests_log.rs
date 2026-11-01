//! Request analytics (issue #505): `oxsum.request_log` — one row per gateway
//! request, settled or refused — and the two reads over it: the member's
//! org-scoped `GET /api/v1/requests` and the operator's all-orgs
//! `GET /api/v1/admin/requests`.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("requests_log")
}

macro_rules! world_or_skip {
    () => {
        match url() {
            Some(u) => world(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// The operator token the admin app is configured with.
const OPERATOR_TOKEN: &str = "operator-token-request-log";

/// The scripted upstream — one completion reporting a usage shape the
/// request-log row can name.
async fn upstream(State(seen): State<Arc<Mutex<Vec<Value>>>>) -> Response {
    seen.lock().unwrap().push(json!({"served": true}));
    Json(json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1,
        "model": "served",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hi"},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
    }))
    .into_response()
}

async fn start_upstream() -> String {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(seen);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the upstream binds");
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    address
}

/// An app over a real database: one funded organization, one priced channel on
/// one model, the organization's first key, and its org id for the admin read.
struct World {
    app: Router,
    pool: PgPool,
    key: String,
    model: String,
    channel: String,
    organization_id: String,
}

async fn world(database_url: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");

    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("reqlog-{suffix}");
    let model = format!("ok-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
    let base_url = start_upstream().await;
    db.set_channel(&channel, &base_url, "upstream-secret", "openai", &secret)
        .await
        .expect("the channel is written");
    db.append_price(
        &channel,
        &model,
        oxsum_core::Price {
            input_price_per_million: 1_000_000,
            output_price_per_million: 1_000_000,
            max_output_tokens: 1000,
            cache_read_price_per_million: None,
            cache_write_5m_price_per_million: None,
            cache_write_1h_price_per_million: None,
            reasoning_price_per_million: None,
            cost_per_request: None,
            upstream: None,
            mode: oxsum_core::BillingMode::Chat,
            rules: vec![],
        },
        100,
    )
    .await
    .expect("the price is written");
    let config = Config::new(Signup::Open, None).with_secret(secret);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = oxsum_server::app(db.clone(), config);

    let email = format!(
        "reqlog_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body, _id) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let key = body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    let organization_id = body["data"]["organization"]["id"]
        .as_str()
        .expect("registration names the org")
        .to_owned();
    let (status, body, _id) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": format!("top-{suffix}"), "amountMinor": 100_000_000})),
        Some(&key),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "top-up failed: {body}");

    World {
        app,
        pool,
        key,
        model,
        channel,
        organization_id,
    }
}

/// An app over the same database with the operator token configured.
fn admin_app(world: &World) -> Router {
    let db = Db::from_pool(world.pool.clone());
    let config = Config::new(Signup::Open, None)
        .with_secret(SecretKey::from_bytes([7; 32]))
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::app(db, config)
}

/// One JSON call; `key` is the bearer credential and `headers` extra pairs.
/// Returns the status, the decoded body, and the gateway's minted request id
/// echoed on `x-oxsum-request-id` — the id the request-log row keys on.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value, String) {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
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
    let request_id = response
        .headers()
        .get("x-oxsum-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        request_id,
    )
}

/// A non-streamed chat completion through this world's key — the request the
/// request-log row records. Returns the minted request id the row keys on.
async fn chat(world: &World) -> (StatusCode, Value, String) {
    call(
        &world.app,
        "POST",
        "/v1/chat/completions",
        Some(json!({
            "model": world.model,
            "messages": [{"role": "user", "content": "hi"}],
        })),
        Some(&world.key),
        &[],
    )
    .await
}

/// The member's request-log read under whatever query the test shapes.
async fn requests(world: &World, query: &str) -> (StatusCode, Value) {
    let (status, body, _id) = call(
        &world.app,
        "GET",
        &format!("/api/v1/requests{query}"),
        None,
        Some(&world.key),
        &[],
    )
    .await;
    (status, body)
}

#[tokio::test]
async fn a_settled_turn_writes_a_request_log_row() {
    let world = world_or_skip!();
    let (status, _body, request_id) = chat(&world).await;
    assert_eq!(status, StatusCode::OK, "the turn failed");

    // The row writes post-settlement; give the spawned insert a beat.
    let mut row = Value::Null;
    for _ in 0..40 {
        let (status, body) = requests(&world, "?limit=50").await;
        assert_eq!(status, StatusCode::OK);
        if let Some(found) = body["data"]["rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["requestId"] == request_id))
        {
            row = found.clone();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_ne!(row, Value::Null, "no request-log row for the settled turn");
    assert_eq!(row["modelServed"], world.model);
    assert_eq!(row["modelRequested"], world.model);
    assert_eq!(row["channel"], world.channel);
    assert_eq!(row["admissionOutcome"], "admitted");
    assert_eq!(row["status"], "settled");
    assert_eq!(row["inputTokens"], 2);
    assert_eq!(row["outputTokens"], 3);
    assert!(row["chargeMinor"].as_i64().unwrap() > 0);
    assert!(row["entryId"].is_string(), "a settled row names its entry");
}

#[tokio::test]
async fn a_refused_admission_logs_a_refused_row() {
    let world = world_or_skip!();
    // A request naming a model the deployment does not serve is refused at
    // admission — before any hold — but still logs a row.
    let (status, _body, request_id) = call(
        &world.app,
        "POST",
        "/v1/chat/completions",
        Some(json!({
            "model": "no-such-model-xyz",
            "messages": [{"role": "user", "content": "hi"}],
        })),
        Some(&world.key),
        &[],
    )
    .await;
    assert!(status.is_client_error(), "an unserved model refuses");

    let mut row = Value::Null;
    for _ in 0..40 {
        let (_status, body) = requests(&world, "?status=refused").await;
        if let Some(found) = body["data"]["rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["requestId"] == request_id))
        {
            row = found.clone();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_ne!(
        row,
        Value::Null,
        "no request-log row for the refused request"
    );
    assert_eq!(row["admissionOutcome"], "refused");
    assert_eq!(row["status"], "refused");
    assert_eq!(row["chargeMinor"], 0);
    assert!(
        row["errorKind"].is_string(),
        "a refused row names its cause"
    );
}

#[tokio::test]
async fn the_filters_narrow_the_read() {
    let world = world_or_skip!();
    let (status, _body, request_id) = chat(&world).await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..40 {
        let (_s, body) = requests(&world, "?limit=200").await;
        if body["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["requestId"] == request_id))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // The model filter returns the row; a foreign model returns none of it.
    let (_s, body) = requests(&world, &format!("?model={}", world.model)).await;
    assert!(
        body["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["requestId"] == request_id))
    );
    let (_s, body) = requests(&world, "?model=never-served-zzz").await;
    assert!(
        !body["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["requestId"] == request_id))
    );

    // A high min-charge excludes the cheap turn; zero includes it.
    let (_s, body) = requests(&world, "?minCharge=999999999").await;
    assert!(
        !body["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["requestId"] == request_id))
    );
    let (_s, body) = requests(&world, "?minCharge=0&status=settled").await;
    assert!(
        body["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["requestId"] == request_id))
    );
}

#[tokio::test]
async fn a_member_sees_only_their_own_rows_and_admin_reads_all() {
    let world = world_or_skip!();
    let (status, _body, request_id) = chat(&world).await;
    assert_eq!(status, StatusCode::OK);

    // The admin endpoint reads every organization — and carries the org id.
    let admin = admin_app(&world);
    let mut admin_row = Value::Null;
    for _ in 0..40 {
        let (status, body, _id) = call(
            &admin,
            "GET",
            "/api/v1/admin/requests?limit=200",
            None,
            Some(OPERATOR_TOKEN),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if let Some(found) = body["data"]["rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["requestId"] == request_id))
        {
            admin_row = found.clone();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_ne!(admin_row, Value::Null, "the admin read missed the row");
    assert_eq!(admin_row["organizationId"], world.organization_id);

    // Narrowed to this org the row is still there; to a foreign org it is not.
    let (status, body, _id) = call(
        &admin,
        "GET",
        &format!(
            "/api/v1/admin/requests?organizationId={}",
            world.organization_id
        ),
        None,
        Some(OPERATOR_TOKEN),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["requestId"] == request_id))
    );
    let (_s, body, _id) = call(
        &admin,
        "GET",
        &format!("/api/v1/admin/requests?organizationId={}", Uuid::new_v4()),
        None,
        Some(OPERATOR_TOKEN),
        &[],
    )
    .await;
    assert!(
        !body["data"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["requestId"] == request_id))
    );

    // The member read never carries the org id on the wire.
    let (_s, body) = requests(&world, "?limit=200").await;
    let member_row = body["data"]["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["requestId"] == request_id))
        .expect("the member read has the row");
    assert!(
        member_row.get("organizationId").is_none(),
        "the member row must not carry the org id"
    );
}

#[tokio::test]
async fn the_cursor_walks_the_log_without_repeating_a_row() {
    let world = world_or_skip!();
    // Two settled turns, so a one-row page has somewhere to resume.
    let (_s1, _b1, id_a) = chat(&world).await;
    let (_s2, _b2, id_b) = chat(&world).await;
    for _ in 0..40 {
        let (_s, body) = requests(&world, "?limit=200").await;
        if body["data"]["rows"].as_array().is_some_and(|rows| {
            rows.iter().any(|r| r["requestId"] == id_a)
                && rows.iter().any(|r| r["requestId"] == id_b)
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Page one with limit=1 must hand back a cursor; page two must not repeat
    // page one's row and must reach the log's start (no further cursor).
    let (status, first) = requests(&world, "?limit=1").await;
    assert_eq!(status, StatusCode::OK);
    let first_rows = first["data"]["rows"].as_array().expect("a rows array");
    assert_eq!(first_rows.len(), 1);
    let first_id = first_rows[0]["requestId"].as_str().unwrap().to_owned();
    let cursor = first["data"]["nextCursor"]
        .as_str()
        .expect("a fuller log hands back a cursor")
        .to_owned();

    let (status, second) = requests(&world, &format!("?limit=1&cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK);
    let second_rows = second["data"]["rows"].as_array().expect("a rows array");
    assert_eq!(second_rows.len(), 1);
    let second_id = second_rows[0]["requestId"].as_str().unwrap();
    assert_ne!(first_id, second_id, "the cursor must not repeat a row");
    // The two turns are the only rows; the newest page holds one, the next the
    // other. Between them they name both turns.
    let mut walked = vec![first_id, second_id.to_owned()];
    walked.sort();
    let mut want = vec![id_a, id_b];
    want.sort();
    assert_eq!(walked, want, "the two pages must cover both turns");

    // A malformed cursor is a validation failure, not an empty page.
    let (status, _body) = requests(&world, "?cursor=not-a-cursor").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

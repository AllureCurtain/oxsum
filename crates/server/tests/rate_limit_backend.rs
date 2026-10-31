//! The shared rate-limit backend at the HTTP layer (issue #498): with
//! `OXSUM_RATE_LIMIT_BACKEND=postgres` the `requestsPerMinute` cap counts in
//! `oxsum.rate_windows`, so two admissions over the same key see one window —
//! here the second request inside the minute is refused 429 as under the
//! in-memory backend, but by the shared row, not process state.
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
use oxsum_core::{Db, RateLimiter, SecretKey};
use oxsum_server::{Config, RateLimitBackend, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("rate_limit_backend")
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

/// The scripted upstream — a model that answers one scripted completion.
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

async fn start_upstream() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(seen.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the upstream binds");
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (address, seen)
}

/// An app over a real database wired for the `postgres` rate-limit backend: one
/// funded organization, one priced channel on one model, the org's first key.
struct World {
    app: Router,
    pool: PgPool,
    key: String,
    key_id: String,
    model: String,
    seen: Arc<Mutex<Vec<Value>>>,
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
    let channel = format!("rl-{suffix}");
    let model = format!("ok-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
    let (base_url, seen) = start_upstream().await;
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
    // The difference this test exists to pin: the shared `postgres` backend,
    // not the default `memory`.
    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_rate_limit_backend(RateLimitBackend::Postgres);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = oxsum_server::app(db, config);

    let email = format!(
        "rl_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        &app,
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
    let key_id = body["data"]["apiKey"]["id"]
        .as_str()
        .expect("registration names the key")
        .to_owned();
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": format!("top-{suffix}"), "amountMinor": 100_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "top-up failed: {body}");

    World {
        app,
        pool,
        key,
        key_id,
        model,
        seen,
    }
}

/// One JSON call; `key` is the bearer credential when Some.
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

/// A gateway turn: a request under the organization's key.
async fn chat(world: &World) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", world.key))
        .body(Body::from(
            json!({"model": world.model, "messages": [{"role": "user", "content": "hi"}]})
                .to_string(),
        ))
        .expect("the test's own request");
    world
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("the gateway answers")
}

/// The `postgres` backend answers the same admission the memory one would — a
/// second request inside the minute is refused 429 before upstream is asked —
/// but the window lives in `oxsum.rate_windows`, so the same cap a second
/// instance would count is the one this process enforced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_postgres_backend_refuses_the_second_request_in_the_minute() {
    let world = world_or_skip!();
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({"requestsPerMinute": 1})),
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The first fits inside the shared window and flows end to end.
    let response = chat(&world).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(world.seen.lock().unwrap().len(), 1);

    // The shared row carries the hit: a second admit inside the minute is refused.
    let response = chat(&world).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().get("retry-after").is_some());
    // Upstream never heard about the refused turn.
    assert_eq!(world.seen.lock().unwrap().len(), 1);

    // And the window is durable in the shared table — a second instance reads
    // this same row rather than a process-local queue.
    let hits: i64 = sqlx::query_scalar(
        "SELECT cardinality(hits)::bigint FROM oxsum.rate_windows WHERE key_id = $1",
    )
    .bind(Uuid::parse_str(&world.key_id).unwrap())
    .fetch_one(&world.pool)
    .await
    .expect("the key's window row exists");
    assert_eq!(
        hits, 1,
        "the shared row counted exactly the admitted request"
    );
}

/// Two `PostgresRateLimiter`s over the pool the app runs on — standing in for
/// the second instance the feature exists for — admit the same key to
/// `per_minute` between them, the property `OXSUM_RATE_LIMIT_BACKEND=postgres`
/// buys a multi-instance deployment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_backend_handles_share_the_window_the_app_writes() {
    let world = world_or_skip!();
    let key = Uuid::parse_str(&world.key_id).unwrap();
    let now = std::time::Instant::now();
    // The app's own backend and a "second instance's" over the same pool count
    // one window.
    let instance = oxsum_core::PostgresRateLimiter::new(world.pool.clone());
    let other = oxsum_core::PostgresRateLimiter::new(world.pool.clone());

    instance.admit(key, 2, now).expect("the first fits");
    instance.admit(key, 2, now).expect("the second fits");
    // The "other instance" sees the same full window — not a fresh allowance.
    other
        .admit(key, 2, now)
        .expect_err("a second instance shares the count");
}

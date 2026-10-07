//! The `/metrics` scrape: gated on the operator token, and its counters move with traffic.
//!
//! Each `app()` carries its own registry (crates/server/src/metrics.rs), so a test that drove
//! a request can assert the exact count it caused rather than a shared process total.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// The operator token the apps in these tests are configured with.
const OPERATOR_TOKEN: &str = "operator-token-metrics-0123";

/// A pool that connects to nothing: auth refuses before the database is reached.
fn unused_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL")
}

/// An app over a real database, with the operator token configured.
async fn online_app(url: &str) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (
        oxsum_server::app(
            db,
            Config::new(Signup::Open, None).with_admin_token(OPERATOR_TOKEN),
        ),
        pool,
    )
}

/// The DATABASE_URL tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => online_app(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// One JSON call, the way the other suites make it.
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A scrape of `/metrics`: status, and the exposition document as text.
async fn scrape(app: &Router, token: Option<&str>) -> (StatusCode, String) {
    let mut request = Request::builder().method("GET").uri("/metrics");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).expect("the request"))
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The exposition line whose name and labels all appear — Prometheus renders one sample per
/// line, so a line carrying every fragment is the series the assertion wants.
fn sample<'a>(document: &'a str, name: &str, labels: &[&str]) -> Option<&'a str> {
    document.lines().find(|line| {
        line.starts_with(name)
            && labels.iter().all(|label| line.contains(label))
            && !line.starts_with("#")
    })
}

/// The value a sample line reports (everything after the last space).
fn value(line: &str) -> f64 {
    line.rsplit(' ')
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .expect("a sample ends in a number")
}

/// Registers an organization and answers its first key's secret.
async fn register(app: &Router) -> String {
    let email = format!(
        "metrics_{}@example.com",
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
    body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned()
}

/// Funds an organization through the manual rail, so its holds have something to freeze.
async fn top_up(app: &Router, key: &str, minor: i64) {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/topups",
        Some(json!({
            "idempotencyKey": format!("metrics-topup-{}", uuid::Uuid::new_v4().simple()),
            "amountMinor": minor,
        })),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the top-up failed: {body}");
}

#[tokio::test]
async fn the_scrape_answers_to_the_operator_token() {
    let app = oxsum_server::app(
        Db::from_pool(unused_pool()),
        Config::new(Signup::Open, None).with_admin_token(OPERATOR_TOKEN),
    );
    // No credential and a wrong credential both refuse, like every admin route.
    assert_eq!(scrape(&app, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        scrape(&app, Some("not-the-token")).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = scrape(&app, Some(OPERATOR_TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The exposition document explains itself: HELP lines and the oxsum prefix.
    assert!(body.contains("# HELP oxsum_http_requests_total"), "{body}");
    assert!(body.contains("# TYPE oxsum_http_requests_total"), "{body}");
}

#[tokio::test]
async fn a_scrape_counts_requests_and_reports_the_pool() {
    let (app, _pool) = app_or_skip!();
    let key = register(&app).await;
    top_up(&app, &key, 100).await;

    // A request the middleware saw, and a hold the wallet took — the hold endpoint counts
    // under its own route pattern.
    let (status, _) = call(&app, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, hold) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": "metrics-hold-1", "amountMinor": 5})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hold}");

    let (status, body) = scrape(&app, Some(OPERATOR_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let health = sample(
        &body,
        "oxsum_http_requests_total",
        &["route=\"/healthz\"", "status=\"200\""],
    )
    .expect("the health request was counted");
    assert_eq!(value(health), 1.0, "{health}");
    let holds = sample(
        &body,
        "oxsum_http_requests_total",
        &["route=\"/api/v1/holds\"", "status=\"200\""],
    )
    .expect("the hold request was counted under its pattern, not its URL");
    assert_eq!(value(holds), 1.0, "{holds}");
    // The scrape-time gauges are there. Their values are the shared database's, so the
    // assertion is on presence — the gateway suite's scrape pins what they say.
    assert!(
        sample(&body, "oxsum_open_holds", &[]).is_some(),
        "the open-holds gauge is reported:\n{body}"
    );
    assert!(
        sample(&body, "oxsum_dead_holds", &[]).is_some(),
        "the dead-holds gauge is reported:\n{body}"
    );
    let pool = sample(&body, "oxsum_db_pool_connections", &["state=\"open\""])
        .expect("the pool gauge is reported");
    assert!(value(pool) >= 1.0, "{pool}");
}

#[tokio::test]
async fn rate_limit_rejections_are_counted_by_surface() {
    let (app, _pool) = app_or_skip!();
    let key = register(&app).await;
    top_up(&app, &key, 100).await;

    // A key capped at one request a minute: the second hold inside the window is refused.
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "metered", "requestsPerMinute": 1})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let metered = created["data"]["secret"]
        .as_str()
        .expect("the key's secret");
    for n in 1..=2 {
        let (status, _) = call(
            &app,
            "POST",
            "/api/v1/holds",
            Some(json!({"idempotencyKey": format!("metrics-rl-{n}"), "amountMinor": 1})),
            Some(metered),
        )
        .await;
        if n == 1 {
            assert_eq!(status, StatusCode::OK);
        } else {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        }
    }

    let (_, body) = scrape(&app, Some(OPERATOR_TOKEN)).await;
    let rejected = sample(
        &body,
        "oxsum_rate_limit_rejections_total",
        &["surface=\"api\""],
    )
    .expect("the rejection was counted");
    assert_eq!(value(rejected), 1.0, "{rejected}");
}

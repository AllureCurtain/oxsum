//! Request bodies this server cannot read, and the envelope they are answered in.
//!
//! Every endpoint that takes a body declares 400 in `openapi.yaml`, but a body axum itself refuses
//! used to be answered in axum's own format instead: plain text, 415 when the `content-type` is
//! missing or is not JSON, 422 when the JSON does not match the request type (issue #51). These
//! tests pin the mapping on every such endpoint — register, login, top up, hold, settle, create key,
//! patch key, admin channels, admin prices — for a wrong content type, no content type, a body that
//! is not JSON, and JSON that is not this endpoint's shape.
//!
//! `POST /v1/chat/completions` is deliberately not here: the OpenAI-compatible surface answers in
//! OpenAI's error shape, which its clients parse, and a rejection there stays in that shape.
//!
//! The tests that reach an endpoint behind a credential need DATABASE_URL, see docs/development.md;
//! without it they skip instead of failing, so a bare `cargo test` still passes. A rejected body is
//! refused during extraction, before any handler runs, which is why the open endpoints need no
//! database at all.

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

/// The operator token these tests configure: the admin surface is opened with it, and a refused body
/// never gets past the middleware to a handler.
const TOKEN: &str = "operator-token-0123456789";

/// One way a client can get a request body wrong, as (what is wrong, content type, body).
type WrongBody = (&'static str, Option<&'static str>, &'static str);

/// The four ways a required body can be unreadable. The first two are axum's 415, the third its 400
/// and the fourth its 422; all four are the envelope's `VALIDATION_ERROR` at 400 here.
const WRONG_BODIES: [WrongBody; 4] = [
    (
        "a content type that is not JSON",
        Some("text/plain"),
        r#"{"email":"a@b.example"}"#,
    ),
    // No content type at all on an endpoint whose body is required: there is a body, and it is not
    // announced as JSON.
    ("no content type", None, r#"{"email":"a@b.example"}"#),
    (
        "a body that is not JSON",
        Some("application/json"),
        r#"{"email":"#,
    ),
    // Valid JSON that is not the request type: every request body here is a struct, and serde
    // rejects a string wherever a struct is expected.
    (
        "JSON that is not this endpoint's shape",
        Some("application/json"),
        r#""not a request""#,
    ),
];

/// An app over a pool that never connects, with the admin surface configured: a refused body is
/// answered before any handler reaches the database.
fn offline_app() -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL");
    oxsum_server::app(
        Db::from_pool(pool),
        Config::new(Signup::Open, None).with_admin_token(TOKEN),
    )
}

/// An app over a real database with oxsum's tables migrated, for the endpoints behind an API key.
async fn online_app(url: &str) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (oxsum_server::app(db, Config::new(Signup::Open, None)), pool)
}

/// The DATABASE_URL the credentialed tests need, or None to skip.
fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
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

/// One request whose content type and body are the point: a JSON content type only when the case
/// names one, and a credential only when the endpoint is behind one.
async fn raw_call(
    app: &Router,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: &str,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .body(Body::from(body.to_owned()))
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

/// Asserts that each wrong body answers 400 with the envelope and nothing else.
async fn assert_envelope_rejection(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    cases: &[WrongBody],
) {
    for &(what, content_type, body) in cases {
        let (status, value) = raw_call(app, method, path, content_type, body, token).await;
        let at = format!("{method} {path} with {what}");
        assert_eq!(status, StatusCode::BAD_REQUEST, "{at}: {value}");
        assert_eq!(value["error"]["code"], "VALIDATION_ERROR", "{at}: {value}");
        // A caller has to be told what was wrong with what it sent.
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "{at}: the message says what was wrong: {value}"
        );
        assert_eq!(value["error"]["details"], json!([]), "{at}: {value}");
    }
}

/// Registers a fresh organization and returns its first key's secret.
async fn register(app: &Router) -> String {
    let email = format!(
        "rejections_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let body = json!({"email": email, "password": "correct horse battery"}).to_string();
    let (status, value) = raw_call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some("application/json"),
        &body,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {value}");
    value["data"]["apiKey"]["secret"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn an_open_endpoint_refuses_a_bad_body_in_the_envelope() {
    // An unreadable body is refused before the handler — and therefore before the database — so
    // these three need no connection, and the test runs whether or not DATABASE_URL is set.
    let app = offline_app();
    assert_envelope_rejection(&app, "POST", "/api/v1/auth/register", None, &WRONG_BODIES).await;
    assert_envelope_rejection(&app, "POST", "/api/v1/auth/login", None, &WRONG_BODIES).await;

    let prices = format!("/api/v1/admin/channels/{}/prices", "rejections-channel");
    assert_envelope_rejection(
        &app,
        "POST",
        "/api/v1/admin/channels",
        Some(TOKEN),
        &WRONG_BODIES,
    )
    .await;
    assert_envelope_rejection(&app, "POST", &prices, Some(TOKEN), &WRONG_BODIES).await;
}

#[tokio::test]
async fn a_credentialed_endpoint_refuses_a_bad_body_in_the_envelope() {
    let (app, _pool) = app_or_skip!();
    let key = register(&app).await;

    for path in ["/api/v1/topups", "/api/v1/holds", "/api/v1/settlements"] {
        assert_envelope_rejection(&app, "POST", path, Some(&key), &WRONG_BODIES).await;
    }

    // The key's own surface: creating one, and changing one.
    let cases: Vec<WrongBody> = WRONG_BODIES
        .into_iter()
        .filter(|(_, content_type, _)| content_type.is_some())
        .collect();
    assert_envelope_rejection(&app, "POST", "/api/v1/org/keys", Some(&key), &cases).await;
    let key_path = format!("/api/v1/org/keys/{}", uuid::Uuid::new_v4());
    assert_envelope_rejection(&app, "PATCH", &key_path, Some(&key), &WRONG_BODIES).await;

    // The exception the filter above encodes: `POST /api/v1/org/keys` takes an optional body, so a
    // request without a content type is a request without a body — still a plain key, not a 400.
    // The new extractor delegates that rule to axum rather than reimplementing it.
    let (status, value) = raw_call(&app, "POST", "/api/v1/org/keys", None, "", Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert!(
        value["data"]["secret"].as_str().is_some(),
        "the new key carries its secret: {value}"
    );
}

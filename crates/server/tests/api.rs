//! HTTP smoke tests: routing, auth and error format. Requires DATABASE_URL.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Tenants;
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "test-token";

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn rejects_missing_or_wrong_token() {
    // No database needed: auth rejects the request before it ever reaches the ledger.
    let app = oxsum_server::app(Tenants::new("postgres://unused"), TOKEN);
    let (s, body) = call(&app, "GET", "/api/v1/tenants/acme/balance", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    let (s, _) = call(
        &app,
        "GET",
        "/api/v1/tenants/acme/balance",
        None,
        Some("nope"),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_needs_no_token() {
    let app = oxsum_server::app(Tenants::new("postgres://unused"), TOKEN);
    let res = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn wallet_round_trip() {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let app = oxsum_server::app(Tenants::new(url), TOKEN);
    let tenant = format!("http_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let base = format!("/api/v1/tenants/{tenant}");

    let (s, body) = call(
        &app,
        "POST",
        &format!("{base}/topups"),
        Some(json!({"idempotencyKey":"t1","amountMinor":5_000_000})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let entry_id = body["data"]["entryId"].as_str().unwrap().to_owned();

    let (s, body) = call(
        &app,
        "POST",
        &format!("{base}/holds"),
        Some(json!({"idempotencyKey":"h1","amountMinor":9_000_000})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["error"]["code"], "INSUFFICIENT_FUNDS");

    let (s, body) = call(&app, "GET", &format!("{base}/balance"), None, Some(TOKEN)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["data"]["availableMinor"], 5_000_000);

    let (s, body) = call(
        &app,
        "GET",
        &format!("{base}/entries/{entry_id}/proof"),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(body["data"]["proof"].is_object());

    let (s, body) = call(
        &app,
        "GET",
        "/api/v1/tenants/Bad-Name/balance",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

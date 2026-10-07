//! HTTP tests for the device grant (issue #156): the tool's two public legs and
//! the session's two — mint, lookup, verdict, poll — over the real router.
//! Needs DATABASE_URL and skips without it, like the other server suites.

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
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

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

struct Res {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> Res {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    if let Some(bearer) = bearer {
        req = req.header("authorization", format!("Bearer {bearer}"));
    }
    let res = app
        .clone()
        .oneshot(
            req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    Res {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        set_cookie,
    }
}

/// Registers a user and returns their session cookie.
async fn session(app: &Router) -> String {
    let email = format!(
        "device_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let res = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "register: {}", res.body);
    let res = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "login: {}", res.body);
    res.set_cookie
        .as_deref()
        .expect("a session cookie")
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// The minted grant: (device_code, user_code).
async fn mint(app: &Router) -> (String, String) {
    let res = call(app, "POST", "/api/v1/device/code", None, None, None).await;
    assert_eq!(res.status, StatusCode::OK, "mint: {}", res.body);
    let data = &res.body["data"];
    assert_eq!(data["interval"], 5);
    assert!(
        data["verificationUri"]
            .as_str()
            .unwrap()
            .ends_with("/device")
    );
    (
        data["deviceCode"].as_str().unwrap().to_owned(),
        data["userCode"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn the_full_grant_loop() {
    let (app, _pool) = app_or_skip!();
    let cookie = session(&app).await;
    let (device_code, user_code) = mint(&app).await;

    // The approval page's read: pending, and only with a session.
    let res = call(
        &app,
        "GET",
        &format!("/api/v1/device/request?code={}", user_code.to_lowercase()),
        None,
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["userCode"], user_code);

    // The tool polls pending, then inside the interval is slowed down.
    let res = call(
        &app,
        "POST",
        "/api/v1/device/token",
        Some(json!({"deviceCode": device_code})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body["data"]["status"], "pending");
    let res = call(
        &app,
        "POST",
        "/api/v1/device/token",
        Some(json!({"deviceCode": device_code})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "SLOW_DOWN");

    // The session approves; the next legal poll delivers the key — minted in
    // that poll's transaction, answered once.
    let res = call(
        &app,
        "POST",
        "/api/v1/device/authorize",
        Some(json!({"userCode": user_code, "approve": true})),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // Rewind the poll timestamp rather than sleeping five seconds.
    cool_down(&user_code).await;

    let res = call(
        &app,
        "POST",
        "/api/v1/device/token",
        Some(json!({"deviceCode": device_code})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let data = &res.body["data"];
    assert_eq!(data["status"], "approved");
    let secret = data["apiKey"]["secret"].as_str().expect("a key secret");
    assert!(secret.starts_with("oxs-"));

    // The delivered key works — a balance read against it authenticates.
    let res = call(&app, "GET", "/api/v1/balance", None, None, Some(secret)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    cool_down(&user_code).await;
    let res = call(
        &app,
        "POST",
        "/api/v1/device/token",
        Some(json!({"deviceCode": device_code})),
        None,
        None,
    )
    .await;
    assert_eq!(res.body["data"]["status"], "consumed");
}

async fn cool_down(user_code: &str) {
    sqlx::query(
        "UPDATE oxsum.device_codes SET last_poll_at = now() - interval '1 minute' \
         WHERE user_code = $1",
    )
    .bind(user_code)
    .execute(
        &sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url().unwrap())
            .await
            .unwrap(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn denial_is_terminal_and_invisible() {
    let (app, _pool) = app_or_skip!();
    let cookie = session(&app).await;
    let (device_code, user_code) = mint(&app).await;

    let res = call(
        &app,
        "POST",
        "/api/v1/device/authorize",
        Some(json!({"userCode": user_code, "approve": false})),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    let res = call(
        &app,
        "POST",
        "/api/v1/device/token",
        Some(json!({"deviceCode": device_code})),
        None,
        None,
    )
    .await;
    assert_eq!(res.body["data"]["status"], "denied");

    // Denied, unknown and malformed codes are the same 404 to the page.
    for code in ["bogus", "XXXX-XXXX", &user_code] {
        let res = call(
            &app,
            "GET",
            &format!("/api/v1/device/request?code={code}"),
            None,
            Some(&cookie),
            None,
        )
        .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{code}: {}", res.body);
    }
    // And a second verdict on the spent code is likewise nothing.
    let res = call(
        &app,
        "POST",
        "/api/v1/device/authorize",
        Some(json!({"userCode": user_code, "approve": true})),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_user_side_is_session_only() {
    let (app, _pool) = app_or_skip!();
    let cookie = session(&app).await;
    let (device_code, user_code) = mint(&app).await;

    // No credential at all → 401.
    for (method, path) in [
        ("GET", format!("/api/v1/device/request?code={user_code}")),
        ("POST", "/api/v1/device/authorize".to_owned()),
    ] {
        let res = call(&app, method, &path, None, None, None).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }

    // An API key is authenticated but is not a person → 403. Mint a fresh key
    // through the session — the org's keys endpoint answers the secret once.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "device-test"})),
        Some(&cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let key = res.body["data"]["secret"].as_str().unwrap().to_owned();

    let res = call(
        &app,
        "POST",
        "/api/v1/device/authorize",
        Some(json!({"userCode": user_code, "approve": true})),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // And the public legs take no credential at all.
    let res = call(
        &app,
        "POST",
        "/api/v1/device/token",
        Some(json!({"deviceCode": device_code})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
}

#[tokio::test]
async fn an_unknown_device_code_is_not_found() {
    let (app, _pool) = app_or_skip!();
    let res = call(
        &app,
        "POST",
        "/api/v1/device/token",
        Some(json!({"deviceCode": "oxd-nope"})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

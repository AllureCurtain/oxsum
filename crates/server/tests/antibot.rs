//! HTTP tests for the anti-bot check (issue #154): Turnstile's siteverify over
//! a stub verifier — a missing or refused token is `FORBIDDEN`, a verifier
//! that cannot answer is `SERVICE_UNAVAILABLE`, and an unconfigured deployment
//! runs both endpoints unchecked.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{Json, Router};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

/// The stub verifier: which tokens it calls genuine, and what it saw.
#[derive(Clone, Default)]
struct SiteVerify {
    inner: Arc<Mutex<SiteVerifyInner>>,
}

#[derive(Default)]
struct SiteVerifyInner {
    /// Tokens that answer `success: true`; anything else fails.
    good: Vec<String>,
    /// The `response` values the verifier received — the check ran through it.
    seen: Vec<String>,
}

/// Serves `POST /siteverify`, on a loopback port.
async fn siteverify_stub() -> (SiteVerify, String) {
    let stub = SiteVerify::default();
    let seen = stub.clone();
    let app = Router::new().route(
        "/siteverify",
        axum::routing::post(move |Json(body): Json<Value>| {
            let seen = seen.clone();
            async move {
                let response = body["response"].as_str().unwrap_or_default().to_owned();
                let mut inner = seen.inner.lock().unwrap();
                inner.seen.push(response.clone());
                Json(json!({"success": inner.good.contains(&response)}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a stub port");
    let url = format!("http://{}/siteverify", listener.local_addr().unwrap());
    tokio::spawn(axum::serve(listener, app).into_future());
    (stub, url)
}

/// An app over a real database, with the check wired to the stub when the
/// test gives one.
async fn online_app(
    url: &str,
    turnstile: Option<oxsum_server::antibot::Turnstile>,
) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let mut config = Config::new(Signup::Open, None);
    if let Some(turnstile) = turnstile {
        config = config.with_turnstile(turnstile);
    }
    (oxsum_server::app(db, config), pool)
}

/// The stub plus the app that verifies against it.
async fn checked_app(url: &str) -> (Router, PgPool, SiteVerify) {
    let (stub, verify_url) = siteverify_stub().await;
    let turnstile = oxsum_server::antibot::Turnstile::new(
        "site-key".to_owned(),
        "secret-key".to_owned(),
        verify_url,
    );
    let (app, pool) = online_app(url, Some(turnstile)).await;
    (app, pool, stub)
}

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => checked_app(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
    (unchecked) => {
        match url() {
            Some(u) => online_app(&u, None).await,
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

async fn post(app: &Router, path: &str, body: Value, cookie: Option<&str>) -> Res {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
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

fn fresh_email(tag: &str) -> String {
    format!(
        "{tag}_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    )
}

#[tokio::test]
async fn the_methods_endpoint_advertises_the_site_key() {
    let (app, _pool, _stub) = app_or_skip!();
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/methods")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["data"]["turnstileSiteKey"], "site-key");

    let (app, _pool) = match url() {
        Some(u) => online_app(&u, None).await,
        None => return,
    };
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/methods")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["data"]["turnstileSiteKey"], Value::Null);
}

#[tokio::test]
async fn registration_requires_the_check_when_configured() {
    let (app, _pool, stub) = app_or_skip!();
    stub.inner.lock().unwrap().good.push("good-token".into());

    // Missing, then refused — both answer the same FORBIDDEN, so a caller
    // learns nothing about which leg failed.
    for token in [None, Some("forged-token")] {
        let mut body = json!({"email": fresh_email("bot"), "password": PASSWORD});
        if let Some(token) = token {
            body["turnstileToken"] = json!(token);
        }
        let res = post(&app, "/api/v1/auth/register", body, None).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{token:?}");
    }

    let res = post(
        &app,
        "/api/v1/auth/register",
        json!({"email": fresh_email("human"), "password": PASSWORD, "turnstileToken": "good-token"}),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // Every judgment ran through the verifier — the refused forged token, and
    // the accepted one. The missing token never reached it.
    let seen = stub.inner.lock().unwrap().seen.clone();
    assert_eq!(
        seen,
        vec!["forged-token".to_owned(), "good-token".to_owned()]
    );
}

#[tokio::test]
async fn redemption_requires_the_check_when_configured() {
    let (app, _pool, stub) = app_or_skip!();
    stub.inner.lock().unwrap().good.push("good-token".into());

    // An invitation to redeem: register an owner (with the check passed), mint
    // the link through the session, then redeem it.
    let owner_email = fresh_email("owner");
    let res = post(
        &app,
        "/api/v1/auth/register",
        json!({"email": owner_email, "password": PASSWORD, "turnstileToken": "good-token"}),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = post(
        &app,
        "/api/v1/auth/login",
        json!({"email": owner_email, "password": PASSWORD}),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let cookie = res
        .set_cookie
        .as_deref()
        .expect("a session cookie")
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let res = post(&app, "/api/v1/org/invitations", Value::Null, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let token = res.body["data"]["token"]
        .as_str()
        .expect("an invitation token")
        .to_owned();

    for turnstile_token in [None, Some("forged")] {
        let mut body =
            json!({"token": token, "email": fresh_email("redeemer"), "password": PASSWORD});
        if let Some(t) = turnstile_token {
            body["turnstileToken"] = json!(t);
        }
        let res = post(&app, "/api/v1/invitations/redeem", body, None).await;
        assert_eq!(res.status, StatusCode::FORBIDDEN, "{turnstile_token:?}");
    }
    let res = post(
        &app,
        "/api/v1/invitations/redeem",
        json!({"token": token, "email": fresh_email("redeemer"), "password": PASSWORD, "turnstileToken": "good-token"}),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

#[tokio::test]
async fn an_unchecked_deployment_registers_unchecked() {
    let (app, _pool) = app_or_skip!(unchecked);

    // No token at all — the behavior the check's absence must preserve.
    let res = post(
        &app,
        "/api/v1/auth/register",
        json!({"email": fresh_email("plain"), "password": PASSWORD}),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

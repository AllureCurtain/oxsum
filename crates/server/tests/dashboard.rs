//! HTTP tests for the dashboard: the Leptos pages are mounted into the same binary as
//! the API, and `/ws/billing` pushes billing progress to a logged-in browser in real time.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use oxsum_core::{Db, OpenHold};
use oxsum_server::{BillingEvent, Config, Signup};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// An app over a real database with oxsum's tables migrated, plus the broadcast sender
/// the gateway publishes billing events to.
async fn online_app(url: &str) -> (Router, Db, broadcast::Sender<BillingEvent>) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None);
    let (app, billing) = oxsum_server::app_with_billing(db.clone(), config);
    (app, db, billing)
}

/// The DATABASE_URL the tests need, or None to skip.
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

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
    set_cookie: Option<String>,
}

/// One plain HTTP call against the app.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let set_cookie = headers
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        headers,
        body,
        set_cookie,
    }
}

/// Registers a fresh organization; returns the tenant id and the login cookie.
async fn logged_in(app: &Router, name: &str) -> (String, String) {
    let email = format!(
        "{name}_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let res = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "registration failed: {}",
        res.body
    );
    let tenant_id = res.body["data"]["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();
    let res = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "login failed: {}", res.body);
    let cookie = res
        .set_cookie
        .as_deref()
        .expect("a cookie was set")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned();
    (tenant_id, cookie)
}

/// The dashboard socket refuses a request with no session: a probe learns nothing.
#[tokio::test]
async fn billing_socket_refuses_a_missing_session() {
    let (app, _db, _billing) = app_or_skip!();
    let res = call(&app, "GET", "/ws/billing", None, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

/// The dashboard socket refuses a session it does not know, the same way.
#[tokio::test]
async fn billing_socket_refuses_an_unknown_session() {
    let (app, _db, _billing) = app_or_skip!();
    let res = call(
        &app,
        "GET",
        "/ws/billing",
        None,
        Some("oxsess-not-a-session"),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

/// The pages are mounted into the same binary as the API: the login page renders.
#[tokio::test]
async fn the_login_page_is_served() {
    let (app, _db, _billing) = app_or_skip!();
    let res = call(&app, "GET", "/login", None, None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let content_type = res
        .headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("text/html"),
        "the login page is HTML, got {content_type}"
    );
}

/// The dashboard shell renders server-side; the session guard itself runs in the browser.
#[tokio::test]
async fn the_dashboard_shell_is_served() {
    let (app, _db, _billing) = app_or_skip!();
    let res = call(&app, "GET", "/dashboard", None, None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

/// A logged-in socket gets a snapshot of the in-flight holds first, then its
/// organization's live events — and never another organization's.
#[tokio::test]
async fn billing_socket_sends_a_snapshot_then_live_events() {
    let (app, db, billing) = app_or_skip!();
    let (tenant_id, cookie) = logged_in(&app, "socket").await;

    // One hold in flight before the socket connects: the snapshot must carry it. The key
    // is unique per run: the tests share one database.
    let hold_key = format!(
        "req-snapshot-{}:hold",
        uuid::Uuid::new_v4().simple().to_string()
    );
    db.note_open_hold(&OpenHold {
        hold_key: hold_key.clone(),
        tenant_id: tenant_id.clone(),
        request_id: "snapshot-1".to_owned(),
        model: "mock-model".to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        input_price: 1,
        output_price: 2,
        freeze_minor: 1000,
    })
    .await
    .expect("the hold is watched");

    // The socket needs a real TCP upgrade, so the app serves on a loopback port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a loopback port");
    let addr = listener.local_addr().expect("the port is known");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serves the app");
    });
    let request = Request::builder()
        .uri(format!("ws://{addr}/ws/billing"))
        .header("host", addr.to_string())
        .header("cookie", format!("oxsum_session={cookie}"))
        // A hand-built upgrade: the headers a browser's WebSocket sends.
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header(
            "sec-websocket-key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header("sec-websocket-version", "13")
        .body(())
        .expect("the upgrade request");
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("the socket upgrades");

    // The first message is the snapshot: the hold that was already in flight.
    let snapshot: Value = next_json(&mut ws).await;
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["holds"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["holds"][0]["holdKey"], hold_key);
    assert_eq!(snapshot["holds"][0]["freezeMinor"], 1000);

    // A live event for this organization is forwarded.
    billing
        .send(BillingEvent::TurnProgress {
            tenant_id: tenant_id.clone(),
            request_id: "snapshot-1".to_owned(),
            output_chars: 2048,
        })
        .expect("the broadcast has a receiver");
    let event: Value = next_json(&mut ws).await;
    assert_eq!(event["type"], "turnProgress");
    assert_eq!(event["tenantId"], tenant_id);
    assert_eq!(event["requestId"], "snapshot-1");
    assert_eq!(event["outputChars"], 2048);

    // Another organization's event never reaches this socket.
    billing
        .send(BillingEvent::TurnStarted {
            tenant_id: "someone-else".to_owned(),
            request_id: "other-1".to_owned(),
            model: "mock-model".to_owned(),
            channel: "mock".to_owned(),
            freeze_minor: 500,
        })
        .expect("the broadcast has a receiver");
    let nothing = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
    assert!(
        nothing.is_err(),
        "another organization's event must not arrive"
    );
}

/// The next text message on the socket, parsed as JSON.
async fn next_json(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let message = ws
        .next()
        .await
        .expect("the socket stays open")
        .expect("the message arrives");
    let Message::Text(text) = message else {
        panic!("the dashboard only sends text messages");
    };
    serde_json::from_str(&text).expect("the message is JSON")
}

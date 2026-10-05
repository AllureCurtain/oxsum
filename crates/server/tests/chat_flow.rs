//! The chat page's flow, end to end at the HTTP layer: login → top-up → pick a model
//! → streaming chat turn → billing events on the WebSocket → proof verification.
//!
//! The page itself is Leptos (SSR + browser hydration); what this pins is the path
//! the page drives: every endpoint the page calls, in the order it calls them,
//! against a scripted upstream. The literal one-browser-session walkthrough stays a
//! manual check for the owner.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use oxsum_core::{Db, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

// ── the scripted upstream ────────────────────────────────────────────────────

/// One streamed answer with a usage report, the way the page's chat call expects it.
/// The answer is longer than the gateway's progress threshold, so the turn also
/// publishes progress events.
async fn upstream() -> Response {
    let mut frames = String::from(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}],\"usage\":null}\n\n",
    );
    for _ in 0..150 {
        frames.push_str(
            "data: {\"choices\":[{\"delta\":{\"content\":\"lorem ipsum dolor sit amet \"}}],\"usage\":null}\n\n",
        );
    }
    frames.push_str(
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n",
    );
    frames.push_str("data: [DONE]\n\n");
    (
        StatusCode::OK,
        [("content-type", "text/event-stream")],
        frames,
    )
        .into_response()
}

/// Starts the scripted upstream and returns its base URL, as a deployment would
/// configure it.
async fn start_upstream() -> String {
    let app = Router::new().route("/v1/chat/completions", post(upstream));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a free port");
    let address = listener.local_addr().expect("the listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}/v1")
}

// ── the world a test runs in ─────────────────────────────────────────────────

/// Everything the flow needs: the app, the session cookie the page logs in with, the
/// API key the page chats with, and the model the page picks.
struct World {
    app: Router,
    tenants: Tenants,
    tenant_id: String,
    cookie: String,
    key: String,
    model: String,
}

/// A world with a funded organization, or an early return when there is no database.
macro_rules! world {
    () => {
        match url() {
            Some(url) => world_for(&url).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

async fn world_for(url: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");

    // One model at one minor unit per token, on this world's own channel: the
    // database is shared between tests, and a model belongs to exactly one channel.
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("chat-e2e-{suffix}");
    let model = format!("chat-e2e-model-{suffix}");
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let base_url = start_upstream().await;
    let price = json!({
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 1_000_000,
        "maxOutputTokens": 1000,
    });
    let mut prices = serde_json::Map::new();
    prices.insert(model.clone(), price);
    let book = oxsum_core::PriceBook::from_json(&channel, &Value::Object(prices).to_string())
        .expect("prices parse");
    db.set_channel(&channel, &base_url, "upstream-secret", "openai", &secret)
        .await
        .expect("the channel is written");
    for (model, price) in book.models() {
        db.append_price(&channel, model, *price)
            .await
            .expect("the price is written");
    }
    let config = Config::new(Signup::Open, None).with_secret(secret);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, _billing) = oxsum_server::app_with_billing(db.clone(), config);
    let tenants = Tenants::new(pool.clone());

    let email = format!(
        "chat_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let key = body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    let tenant_id = body["data"]["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();

    let res = call_raw(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "login failed: {}", res.body);
    let cookie = session_cookie(&res);

    World {
        app,
        tenants,
        tenant_id,
        cookie,
        key,
        model,
    }
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

struct Res {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
}

/// One HTTP call against the app, with an API key and/or a session cookie.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let res = call_raw(app, method, path, body, key, cookie).await;
    (res.status, res.body)
}

async fn call_raw(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    cookie: Option<&str>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .expect("the test's own request");
    let res = app.clone().oneshot(req).await.expect("the router answers");
    let status = res.status();
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        body,
        set_cookie,
    }
}

/// The cookie value out of a login's `Set-Cookie` header.
fn session_cookie(res: &Res) -> String {
    res.set_cookie
        .as_deref()
        .expect("a cookie was set")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned()
}

/// A streaming chat request, the way the page sends it.
async fn chat(app: &Router, key: &str, body: Value) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the router answers")
}

/// Opens the billing socket the way the page does: a real TCP upgrade with the
/// session cookie.
async fn open_ws(
    app: &Router,
    cookie: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a loopback port");
    let addr = listener.local_addr().expect("the port is known");
    let app = app.clone();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serves the app");
    });
    let request = Request::builder()
        .uri(format!("ws://{addr}/ws/billing"))
        .header("host", addr.to_string())
        .header("cookie", format!("oxsum_session={cookie}"))
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header(
            "sec-websocket-key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header("sec-websocket-version", "13")
        .body(())
        .expect("the upgrade request");
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("the socket upgrades");
    ws
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
        panic!("the server only sends text messages");
    };
    serde_json::from_str(&text).expect("the message is JSON")
}

// ── the flow ─────────────────────────────────────────────────────────────────

/// Login → top-up → pick a model → streaming chat → live billing → proof verification:
/// the whole path the chat page drives, without touching the page itself.
#[tokio::test]
async fn topup_chat_billing_and_proof() {
    let world = world!();

    // The chat page renders for the session.
    let (status, _) = call(
        &world.app,
        "GET",
        "/dashboard/chat",
        None,
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the chat page renders");

    // Top-up through the session, the way the page's form does.
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "chat-e2e-topup", "amountMinor": 100_000_000})),
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the top-up lands: {body}");
    assert!(
        body["data"]["contentHash"].as_str().is_some(),
        "the receipt carries its content hash"
    );

    // The model picker lists the deployment's models, with the chat key.
    let (status, body) = call(
        &world.app,
        "GET",
        "/v1/models",
        None,
        Some(&world.key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the model list loads: {body}");
    let ids: Vec<&str> = body["data"]
        .as_array()
        .expect("the list shape")
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect();
    assert!(
        ids.contains(&world.model.as_str()),
        "the picked model is served: {ids:?}"
    );

    // The billing socket opens before the turn, the way the page holds it.
    let mut ws = open_ws(&world.app, &world.cookie).await;
    let snapshot: Value = next_json(&mut ws).await;
    assert_eq!(snapshot["type"], "snapshot");

    // The streaming chat turn, through the gateway with the chat key.
    let response = chat(
        &world.app,
        &world.key,
        json!({
            "model": world.model,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = response.headers()["x-oxsum-request-id"]
        .to_str()
        .expect("the turn is named")
        .to_owned();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the stream")
        .to_bytes();
    let text = String::from_utf8(bytes.to_vec()).expect("the stream is text");
    assert!(text.contains("lorem ipsum"), "the answer streams: {text}");
    assert!(
        text.contains("data: [DONE]"),
        "the stream terminates: {text}"
    );

    // The turn's billing plays live on the socket: freeze, progress, settlement.
    let mut saw_started = false;
    let mut saw_progress = false;
    let mut saw_settled = false;
    let deadline = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(deadline);
    while !(saw_started && saw_progress && saw_settled) {
        tokio::select! {
            _ = &mut deadline => panic!("the turn's billing never completed live"),
            message = ws.next() => {
                let message = message.expect("the socket stays open").expect("a message arrives");
                let Message::Text(text) = message else { continue };
                let event: Value = serde_json::from_str(&text).expect("the message is JSON");
                if event["requestId"].as_str() != Some(request_id.as_str()) {
                    continue;
                }
                match event["type"].as_str() {
                    Some("turnStarted") => {
                        saw_started = true;
                        assert!(
                            event["freezeMinor"].as_i64().unwrap_or(0) > 0,
                            "the freeze is named: {event}"
                        );
                    }
                    Some("turnProgress") => saw_progress = true,
                    Some("turnSettled") => saw_settled = true,
                    _ => {}
                }
            }
        }
    }

    // The bill: the settlement entry the request id names, through the proof endpoint
    // the page's server function reads.
    let hold_key = format!("req-{request_id}:hold");
    let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/entries/{entry_id}/proof"),
        None,
        None,
        Some(&world.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the bill is readable: {body}");
    let bundle_json = serde_json::to_string(&body["data"]).expect("the bundle serializes");
    let record: Value = serde_json::from_str(
        body["data"]["entry"]["description"]
            .as_str()
            .expect("a record"),
    )
    .expect("the record is JSON");
    assert_eq!(record["kind"], "usage");
    assert_eq!(record["inputTokens"], 10);
    assert_eq!(record["outputTokens"], 2);
    // One minor unit per token: the bill is the token count itself.
    assert_eq!(record["charged"], 12);

    // The verification the page links to: the bundle checks out under the wallet's
    // own logic, against the content hash the ledger recorded — the same two halves
    // the /verify link carries.
    let wallet = world
        .tenants
        .get(&world.tenant_id)
        .await
        .expect("the organization's ledger opens");
    let bundle = wallet
        .receipt_proof(entry_id)
        .await
        .expect("the proof is built")
        .expect("the settlement is there");
    let content_hash = bundle.entry.content_hash();
    assert!(
        oxsum_core::verify_bundle(&bundle_json, &content_hash).expect("the bundle parses"),
        "the bill verifies"
    );

    // And /verify renders with the prefilled link the page builds.
    let href = format!(
        "/verify?bundle={}&contentHash={content_hash}",
        urlencoding(&bundle_json)
    );
    let (status, _) = call(&world.app, "GET", &href, None, None, Some(&world.cookie)).await;
    assert_eq!(status, StatusCode::OK, "the prefilled verify page renders");
}

/// Percent-encodes a query value the way the page's link builder does.
fn urlencoding(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

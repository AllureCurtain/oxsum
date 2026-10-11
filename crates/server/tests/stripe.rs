//! Stripe rail tests (issue #201): the two endpoints, the signature
//! verification, and the full intent → session → webhook → credit flow.
//! The tests need DATABASE_URL and skip without it; a stub Stripe API stands
//! where `api.stripe.com` would be.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use hmac::{KeyInit, Mac, SimpleHmac};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const WEBHOOK_SECRET: &str = "whsec-test-secret-1234";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

fn sealing_key() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

/// The Stripe API stub: `POST /v1/checkout/sessions` answers a fixed session,
/// recording the `client_reference_id` the caller sent.
#[derive(Clone, Default)]
struct StripeStub {
    calls: Arc<Mutex<Vec<String>>>,
    answer_url: Arc<Mutex<String>>,
    answer_id: Arc<Mutex<String>>,
}

async fn stub_stripe() -> (String, StripeStub) {
    let stub = StripeStub::default();
    let app = Router::new()
        .route(
            "/v1/checkout/sessions",
            post(|State(stub): State<StripeStub>, body: String| async move {
                // The form-encoded body carries client_reference_id as
                // `client_reference_id=<uuid>` — read it back out.
                let ref_id = body
                    .split('&')
                    .find_map(|pair| {
                        let mut kv = pair.splitn(2, '=');
                        match (kv.next(), kv.next()) {
                            (Some("client_reference_id"), Some(v)) => Some(v.to_owned()),
                            _ => None,
                        }
                    })
                    .unwrap_or_default();
                stub.calls.lock().unwrap().push(ref_id);
                axum::Json(json!({
                    "id": *stub.answer_id.lock().unwrap(),
                    "url": *stub.answer_url.lock().unwrap(),
                }))
            }),
        )
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), stub)
}

/// A configured world: the Stripe stub answers checkout sessions, and the
/// config carries the rail's three variables.
async fn world() -> Option<(Router, Db, StripeStub)> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let (api_url, stub) = stub_stripe().await;
    *stub.answer_id.lock().unwrap() = "cs_test_123".to_owned();
    *stub.answer_url.lock().unwrap() = "https://checkout.stripe.com/pay/cs_test_123".to_owned();
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_stripe(oxsum_server::stripe::Stripe::new(
            "sk_test_123".to_owned(),
            WEBHOOK_SECRET.to_owned(),
            api_url,
            "http://localhost:3000".to_owned(),
        ));
    let (app, _events, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    Some((app, db, stub))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
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

/// A signed Stripe webhook: `Stripe-Signature` carries the test secret's tag.
fn signed_webhook(secret: &str, body: &str) -> (HeaderMap, String) {
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut mac = <SimpleHmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    let tag: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut headers = HeaderMap::new();
    headers.insert(
        "stripe-signature",
        format!("t={timestamp},v1={tag}").parse().unwrap(),
    );
    headers.insert("content-type", "application/json".parse().unwrap());
    (headers, body.to_owned())
}

async fn register(app: &Router) -> String {
    let email = format!(
        "stripe_{}@example.com",
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
    body["data"]["apiKey"]["secret"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The org's ledger balance, read back through the API.
async fn balance(app: &Router, key: &str) -> i64 {
    let (status, body) = call(app, "GET", "/api/v1/balance", None, Some(key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["data"]["availableMinor"].as_i64().unwrap()
}

/// A deposit the intent endpoint leaves: pending, the checkout URL answered.
#[tokio::test]
async fn the_intent_opens_a_pending_deposit_and_answers_the_url() {
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": "topup-1", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["checkoutUrl"].as_str().unwrap(),
        "https://checkout.stripe.com/pay/cs_test_123"
    );
    assert!(body["data"]["depositId"].is_string());
    // The stub saw the deposit id as client_reference_id.
    let calls = stub.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0], body["data"]["depositId"].as_str().unwrap());
}

/// A replayed intent answers the same checkout URL, not a second session.
#[tokio::test]
async fn a_replayed_intent_answers_the_same_url() {
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": "topup-2", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first_url = body["data"]["checkoutUrl"].as_str().unwrap().to_owned();

    // The Stripe stub changes its answer for a second call — a replay must not reach it.
    *stub.answer_url.lock().unwrap() = "https://checkout.stripe.com/pay/cs_test_456".to_owned();

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": "topup-2", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["checkoutUrl"].as_str().unwrap(), first_url);
}

/// The full flow: intent → session stub → signed webhook → purchased-pool credit.
#[tokio::test]
async fn the_full_flow_credits_the_purchased_pool() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": "topup-3", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let session_id = "cs_test_123";
    let deposit_id = body["data"]["depositId"].as_str().unwrap();

    let event = json!({
        "id": "evt_1",
        "type": "checkout.session.completed",
        "data": {
            "object": {
                "id": session_id,
                "client_reference_id": deposit_id,
                "amount_total": 5000,
            }
        }
    });
    let (headers, body_str) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/stripe")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let after = balance(&app, &key).await;
    assert_eq!(after - before, 5000);
}

/// A replayed webhook credits once — the confirm-and-credit is atomic.
#[tokio::test]
async fn a_replayed_webhook_credits_once() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": "topup-4", "amountMinor": 3000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event = json!({
        "type": "checkout.session.completed",
        "data": { "object": { "id": "cs_test_123", "amount_total": 3000 } }
    });
    for _ in 0..3 {
        let (headers, b) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/webhooks/stripe")
            .body(Body::from(b))
            .unwrap();
        let mut req = req;
        *req.headers_mut() = headers;
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    let after = balance(&app, &key).await;
    assert_eq!(after - before, 3000);
}

/// A tampered signature is refused — the webhook's only credential is the tag.
#[tokio::test]
async fn a_tampered_signature_is_refused() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register(&app).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": "topup-5", "amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Sign under a different secret — the tag is wrong.
    let event = json!({
        "type": "checkout.session.completed",
        "data": { "object": { "id": "cs_test_123", "amount_total": 1000 } }
    });
    let (headers, body_str) = signed_webhook("wrong-secret", &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/stripe")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// An unsigned webhook is refused — no header, no credit.
#[tokio::test]
async fn an_unsigned_webhook_is_refused() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let event = json!({
        "type": "checkout.session.completed",
        "data": { "object": { "id": "cs_test_123", "amount_total": 1000 } }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/stripe")
        .header("content-type", "application/json")
        .body(Body::from(event.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// An unconfigured deployment refuses the rail cleanly: the intent 503s and
/// the webhook 404s — no `OXSUM_STRIPE_*` means no rail to expose.
#[tokio::test]
async fn an_unconfigured_deployment_refuses_cleanly() {
    let u = url().unwrap();
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None).with_secret(sealing_key());
    let (app, _events, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    let key = register(&app).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": "topup-6", "amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let event = json!({
        "type": "checkout.session.completed",
        "data": { "object": { "id": "cs_x", "amount_total": 1000 } }
    });
    let (headers, body_str) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/stripe")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// `checkout.session.expired` closes the deposit: the wallet stays where it was.
#[tokio::test]
async fn an_expired_session_credits_nothing() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": "topup-7", "amountMinor": 2000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event = json!({
        "type": "checkout.session.expired",
        "data": { "object": { "id": "cs_test_123" } }
    });
    let (headers, body_str) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/stripe")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let after = balance(&app, &key).await;
    assert_eq!(after, before);
}

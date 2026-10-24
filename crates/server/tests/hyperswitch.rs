//! Hyperswitch rail tests (issue #319): the top-up intent, the HMAC-signed
//! webhook and the deposit walk. The tests need DATABASE_URL and skip without
//! it; a stub switch API stands where `sandbox.hyperswitch.io` would be.

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
use sha2::Sha512;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const WEBHOOK_SECRET: &str = "hyperswitch-whsec-test-1234";
const PUBLISHABLE_KEY: &str = "pk_snd_test_1234";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("hyperswitch")
}

fn sealing_key() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

/// The switch API stub: `POST /payments` records the deposit id it was told to
/// carry in `metadata` and answers a fixed payment id + client secret.
#[derive(Clone, Default)]
struct SwitchStub {
    deposits: Arc<Mutex<Vec<String>>>,
    answer_id: Arc<Mutex<String>>,
    answer_secret: Arc<Mutex<String>>,
}

async fn stub_switch() -> (String, SwitchStub) {
    let stub = SwitchStub::default();
    let app = Router::new()
        .route(
            "/payments",
            post(
                |State(stub): State<SwitchStub>, body: axum::Json<Value>| async move {
                    let deposit = body["metadata"]["deposit_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    stub.deposits.lock().unwrap().push(deposit);
                    axum::Json(json!({
                        "payment_id": *stub.answer_id.lock().unwrap(),
                        "client_secret": *stub.answer_secret.lock().unwrap(),
                        "status": "requires_payment_method",
                    }))
                },
            ),
        )
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), stub)
}

/// A configured world: the switch stub answers `POST /payments`, and the config
/// carries the rail's keys.
async fn world() -> Option<(Router, Db, SwitchStub)> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let (api_url, stub) = stub_switch().await;
    *stub.answer_id.lock().unwrap() = "pay_test_123".to_owned();
    *stub.answer_secret.lock().unwrap() = "pay_test_123_secret".to_owned();
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_hyperswitch(oxsum_server::hyperswitch::Hyperswitch::new(
            "snd_test_key".to_owned(),
            WEBHOOK_SECRET.to_owned(),
            api_url,
            PUBLISHABLE_KEY.to_owned(),
            "http://localhost:3000".to_owned(),
            "https://sdk.example.test/v1".to_owned(),
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

/// A signed switch webhook: `X-Webhook-Signature-512` carries the hex
/// HMAC-SHA512 of the raw body under the test business profile's hash key.
fn signed_webhook(secret: &str, body: &str) -> (HeaderMap, String) {
    let mut mac = <SimpleHmac<Sha512> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let tag: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut headers = HeaderMap::new();
    headers.insert("x-webhook-signature-512", tag.parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    (headers, body.to_owned())
}

/// Posts a webhook body with the given signature headers.
async fn post_webhook(app: &Router, headers: HeaderMap, body: String) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/hyperswitch")
        .body(Body::from(body))
        .unwrap();
    *req.headers_mut() = headers;
    app.clone().oneshot(req).await.unwrap().status()
}

async fn register(app: &Router) -> String {
    let email = format!(
        "hyperswitch_{}@example.com",
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

/// The switch names one payment: the intent opens a pending deposit, the stub
/// sees that deposit in `metadata`, and the answer carries the browser handles.
#[tokio::test]
async fn the_intent_creates_a_payment_and_answers_the_handles() {
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-1", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["paymentId"].as_str().unwrap(), "pay_test_123");
    assert_eq!(
        body["data"]["clientSecret"].as_str().unwrap(),
        "pay_test_123_secret"
    );
    assert_eq!(
        body["data"]["publishableKey"].as_str().unwrap(),
        PUBLISHABLE_KEY
    );
    // The checkout widget mounts HyperLoader from this origin (issue #320).
    assert_eq!(
        body["data"]["sdkUrl"].as_str().unwrap(),
        "https://sdk.example.test/v1"
    );
    let deposit_id = body["data"]["depositId"].as_str().unwrap();
    assert_eq!(
        stub.deposits.lock().unwrap().clone(),
        vec![deposit_id.to_owned()]
    );
}

/// A replayed intent answers the same handles, not a second switch payment.
#[tokio::test]
async fn a_replayed_intent_answers_the_same_handles() {
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-2", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first = body["data"]["clientSecret"].as_str().unwrap().to_owned();

    // The stub changes its answer for a second call — a replay must not reach it.
    *stub.answer_id.lock().unwrap() = "pay_other".to_owned();

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-2", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["clientSecret"].as_str().unwrap(), first);
    assert_eq!(body["data"]["paymentId"].as_str().unwrap(), "pay_test_123");
    // The replay answers the SDK origin too — the widget mounts from it.
    assert_eq!(
        body["data"]["sdkUrl"].as_str().unwrap(),
        "https://sdk.example.test/v1"
    );
    // Still exactly one upstream payment: the replay hit the deposit row.
    assert_eq!(stub.deposits.lock().unwrap().len(), 1);
}

/// A `payment_succeeded` webhook credits the wallet once, and a replayed
/// delivery (same `event_id`) credits nothing on top.
#[tokio::test]
async fn a_payment_succeeded_webhook_credits_once() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-3", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event = json!({
        "merchant_id": "merchant_test",
        "event_id": "evt_1",
        "event_type": "payment_succeeded",
        "content": {
            "type": "payment_details",
            "object": {
                "payment_id": "pay_test_123",
                "amount": 5000,
                "currency": "USD",
                "status": "succeeded",
            }
        }
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    assert_eq!(balance(&app, &key).await - before, 5000);

    // The same event again: the event id is the anchor.
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    // A different event id for the same payment: the deposit is already
    // credited, so it credits nothing either.
    let mut second = event.clone();
    second["event_id"] = json!("evt_2");
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &second.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    assert_eq!(balance(&app, &key).await - before, 5000);
}

/// A tampered signature is refused — the HMAC is the webhook's only credential.
#[tokio::test]
async fn a_tampered_signature_is_refused() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-4", "amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event = json!({
        "event_id": "evt_bad",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": "pay_test_123", "amount": 1000}}
    });
    let (headers, body) = signed_webhook("wrong-secret", &event.to_string());
    assert_eq!(
        post_webhook(&app, headers, body).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(balance(&app, &key).await, before);
}

/// An unconfigured deployment refuses the rail cleanly: the intent 503s and the
/// webhook 404s — no `OXSUM_HYPERSWITCH_*` means no rail to expose.
#[tokio::test]
async fn an_unconfigured_deployment_refuses_cleanly() {
    let Some(u) = url() else {
        return;
    };
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
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-5", "amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let event = json!({
        "event_id": "evt_unconfigured",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": "pay_x", "amount": 10}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(
        post_webhook(&app, headers, body).await,
        StatusCode::NOT_FOUND
    );
}

/// `payment_cancelled` closes the deposit: the wallet stays where it was.
#[tokio::test]
async fn a_cancelled_payment_credits_nothing() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-6", "amountMinor": 2000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event = json!({
        "event_id": "evt_cancel",
        "event_type": "payment_cancelled",
        "content": {"type": "payment_details", "object": {"payment_id": "pay_test_123"}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    assert_eq!(balance(&app, &key).await, before);

    // A later success cannot revive a cancelled deposit — the row is no longer
    // pending, so the webhook finds nothing to confirm.
    let event = json!({
        "event_id": "evt_late",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": "pay_test_123", "amount": 2000}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    assert_eq!(balance(&app, &key).await, before);
}

/// An event naming a payment no deposit knows is acknowledged, not an error:
/// the switch retries until it hears a 2xx, and a miss is not this route's
/// business.
#[tokio::test]
async fn an_unknown_payment_is_still_acknowledged() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let event = json!({
        "event_id": "evt_unknown",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": "pay_unknown", "amount": 100}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
}

/// The amount the switch reports is recorded, never used to credit: the
/// deposit's own amount is what the ledger moved, so a mismatch cannot
/// silently inflate the wallet.
#[tokio::test]
async fn an_amount_mismatch_credits_the_deposit_amount() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let before = balance(&app, &key).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-7", "amountMinor": 3000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event = json!({
        "event_id": "evt_mismatch",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": "pay_test_123", "amount": 9999}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    // The deposit's amount (3000) is what moved; 9999 is only recorded.
    assert_eq!(balance(&app, &key).await - before, 3000);
}

//! Hyperswitch rail tests (issue #319): the top-up intent, the HMAC-signed
//! webhook and the deposit walk. The tests need DATABASE_URL and skip without
//! it; a stub switch API stands where `sandbox.hyperswitch.io` would be.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

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
/// carry in `metadata` and answers a fixed payment id + client secret;
/// `POST /refunds` records the payment it was asked to refund and answers a
/// fixed refund id + status (or a refusal the test arms).
#[derive(Clone, Default)]
struct SwitchStub {
    deposits: Arc<Mutex<Vec<String>>>,
    refunds: Arc<Mutex<Vec<Value>>>,
    stored: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    answer_id: Arc<Mutex<String>>,
    answer_secret: Arc<Mutex<String>>,
    /// The refund id/status `POST /refunds` answers, and an optional refusal
    /// body that makes it answer a non-2xx instead.
    answer_refund: Arc<Mutex<Value>>,
    refund_refusal: Arc<Mutex<Option<(StatusCode, Value)>>>,
    stored_refunds: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    lost_refund_response: Arc<Mutex<bool>>,
    refund_reads_unavailable: Arc<Mutex<bool>>,
}

async fn stub_switch() -> (String, SwitchStub) {
    let stub = SwitchStub {
        answer_refund: Arc::new(Mutex::new(json!({
            "refund_id": "ref_test_1",
            "status": "succeeded",
        }))),
        ..SwitchStub::default()
    };
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
                    let id = body["payment_id"].as_str().unwrap().to_owned();
                    let value = json!({"payment_id":id,"client_secret":*stub.answer_secret.lock().unwrap(),"status":"requires_payment_method",
                        "merchant_id":"merchant_test","amount":body["amount"],"currency":body["currency"],"metadata":body["metadata"]});
                    stub.stored.lock().unwrap().insert(id, value.clone());
                    axum::Json(value)
                },
            ),
        )
        .route(
            "/refunds",
            post(
                |State(stub): State<SwitchStub>, body: axum::Json<Value>| async move {
                    stub.refunds.lock().unwrap().push(body.0.clone());
                    if let Some((status, refusal)) = stub.refund_refusal.lock().unwrap().clone() {
                        return (status, axum::Json(refusal));
                    }
                    let id=body["refund_id"].as_str().unwrap().to_owned();
                    let value=json!({"refund_id":id,"payment_id":body["payment_id"],"amount":body["amount"],
                        "currency":"USD","metadata":body["metadata"],"status":stub.answer_refund.lock().unwrap()["status"]});
                    if stub.stored_refunds.lock().unwrap().insert(id,value.clone()).is_some() {
                        return (StatusCode::BAD_REQUEST,axum::Json(json!({"error":{"code":"HE_01"}})));
                    }
                    if *stub.lost_refund_response.lock().unwrap() {
                        *stub.refund_reads_unavailable.lock().unwrap()=true;
                        return (StatusCode::BAD_GATEWAY,axum::Json(json!({"error":"response lost"})));
                    }
                    (StatusCode::OK,axum::Json(value))
                },
            ),
        )
        .route("/payments/{id}", axum::routing::get(|State(stub):State<SwitchStub>, axum::extract::Path(id):axum::extract::Path<String>| async move {
            let value = stub.stored.lock().unwrap().get(&id).cloned();
            value.map_or((StatusCode::NOT_FOUND, axum::Json(json!({"error":{"code":"HE_02"}}))), |v| (StatusCode::OK,axum::Json(v)))
        }))
        .route("/refunds/{id}",axum::routing::get(|State(stub):State<SwitchStub>,axum::extract::Path(id):axum::extract::Path<String>|async move {
            if *stub.refund_reads_unavailable.lock().unwrap() { return (StatusCode::SERVICE_UNAVAILABLE,axum::Json(json!({}))); }
            stub.stored_refunds.lock().unwrap().get(&id).cloned().map_or(
                (StatusCode::NOT_FOUND,axum::Json(json!({"error":{"code":"HE_02"}}))),
                |value|(StatusCode::OK,axum::Json(value)))
        }))
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
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let (api_url, stub) = stub_switch().await;
    *stub.answer_id.lock().unwrap() = "pay_test_123".to_owned();
    *stub.answer_secret.lock().unwrap() = "pay_test_123_secret".to_owned();
    let rail = oxsum_server::hyperswitch::Hyperswitch::new(
        "snd_test_key".to_owned(),
        WEBHOOK_SECRET.to_owned(),
        api_url,
        PUBLISHABLE_KEY.to_owned(),
        "http://localhost:3000".to_owned(),
        "https://sdk.example.test/v1".to_owned(),
    );
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_public_url("http://localhost:3000")
        .with_merchant_bootstrap(rail.merchant_bootstrap(
            "merchant_test".to_owned(),
            oxsum_core::MerchantMode::Test,
            oxsum_core::ProviderDestinationPolicy {
                network: oxsum_core::ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            },
        ))
        .with_hyperswitch(rail);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
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
        req = support::credential(req, key);
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

async fn register_session(app: &Router) -> String {
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
    support::login(app, &email, "correct horse battery").await
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
    let key = register_session(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "topup-1", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["paymentId"], body["data"]["depositId"]);
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
    let key = register_session(&app).await;

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
    assert_eq!(body["data"]["paymentId"], body["data"]["depositId"]);
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
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register_session(&app).await;
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
                "payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(),
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
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register_session(&app).await;
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
        "content": {"type": "payment_details", "object": {"payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(), "amount": 1000}}
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
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let config = Config::new(Signup::Open, None).with_secret(sealing_key());
    let (app, _events, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    let key = register_session(&app).await;

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

/// `payment_cancelled` closes the deposit: the wallet stays where it was. An
/// authoritative `payment_succeeded` delivered afterwards revives the row —
/// paid evidence outranks an earlier expiration notice, and the credit still
/// lands exactly once under the deposit's own amount.
#[tokio::test]
async fn a_cancelled_payment_is_revived_only_by_authoritative_success() {
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register_session(&app).await;
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
        "content": {"type": "payment_details", "object": {"payment_id": stub.deposits.lock().unwrap().last().unwrap().clone()}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    assert_eq!(balance(&app, &key).await, before);

    // The provider later reports the payment actually succeeded: the success is
    // authoritative, so the deposit revives and credits its pinned amount once.
    let event = json!({
        "event_id": "evt_late",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(), "amount": 2000, "currency": "USD"}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    assert_eq!(balance(&app, &key).await - before, 2000);
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

/// A reported amount that disagrees with the pinned expectation is rejected
/// before it can reach the ledger: the intake retains the event for review and
/// the wallet never moves on a mismatch.
#[tokio::test]
async fn an_amount_mismatch_never_credits() {
    let Some((app, db, stub)) = world().await else {
        return;
    };
    let key = register_session(&app).await;
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
        "content": {"type": "payment_details", "object": {"payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(), "amount": 9999, "currency": "USD"}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    // The mismatch cannot credit the wallet: the event is durably rejected.
    assert_eq!(balance(&app, &key).await, before);
    let state: String = sqlx::query_scalar(
        "SELECT state FROM oxsum.merchant_event_inbox WHERE event_id='evt_mismatch'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(state, "rejected");
}

const ADMIN_TOKEN: &str = "operator-token-refund";

/// A world with the admin surface configured too — the reversal path the
/// refund tests exercise lives on `/api/v1/admin`.
async fn admin_world() -> Option<(Router, Db, SwitchStub)> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let (api_url, stub) = stub_switch().await;
    *stub.answer_id.lock().unwrap() = "pay_test_123".to_owned();
    *stub.answer_secret.lock().unwrap() = "pay_test_123_secret".to_owned();
    let rail = oxsum_server::hyperswitch::Hyperswitch::new(
        "snd_test_key".to_owned(),
        WEBHOOK_SECRET.to_owned(),
        api_url,
        PUBLISHABLE_KEY.to_owned(),
        "http://localhost:3000".to_owned(),
        "https://sdk.example.test/v1".to_owned(),
    );
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_admin_token(ADMIN_TOKEN)
        .with_public_url("http://localhost:3000")
        .with_merchant_bootstrap(rail.merchant_bootstrap(
            "merchant_test".to_owned(),
            oxsum_core::MerchantMode::Test,
            oxsum_core::ProviderDestinationPolicy {
                network: oxsum_core::ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            },
        ))
        .with_hyperswitch(rail);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, _events, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    Some((app, db, stub))
}

/// Reversing a credited hyperswitch deposit issues the switch's refund and the
/// answer carries `refundStatus`.
#[tokio::test]
async fn a_reversal_issues_the_switch_refund() {
    let Some((app, _db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "ref-1", "amountMinor": 4000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deposit_id = body["data"]["depositId"].as_str().unwrap().to_owned();
    // Credit it.
    let event = json!({
        "event_id": "evt_credit_ref1",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(), "amount": 4000, "currency": "USD"}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);

    // Reverse it — the operator call.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/deposits/{deposit_id}/reverse"),
        Some(json!({"reason": "customer asked"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"].as_str().unwrap(), "reversed");
    // The HTTP answer is acknowledged, but only a signed event is terminal.
    assert_eq!(body["data"]["refundStatus"].as_str().unwrap(), "pending");
    // The refund call carried the payment id, the deposit's amount and the
    // deposit id in metadata.
    let refunds = stub.refunds.lock().unwrap().clone();
    assert_eq!(refunds.len(), 1);
    assert_eq!(
        refunds[0]["payment_id"].as_str().unwrap(),
        stub.deposits.lock().unwrap().last().unwrap()
    );
    assert_eq!(refunds[0]["amount"].as_i64().unwrap(), 4000);
    assert_eq!(
        refunds[0]["metadata"]["deposit_id"].as_str().unwrap(),
        deposit_id
    );

    // A replayed reversal answers the standing reversal and does not refund
    // again: its durable attempt lease prevents another provider request.
    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/deposits/{deposit_id}/reverse"),
        Some(json!({"reason": "customer asked"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stub.refunds.lock().unwrap().len(), 1);
}

/// An HTTP refusal cannot disprove creation. The reversal stands and the
/// unknown operation keeps its identity; retries respect the durable lease.
#[tokio::test]
async fn a_refused_refund_stays_unknown_and_retries_the_same_identity() {
    let Some((app, db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "ref-2", "amountMinor": 2000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deposit_id = body["data"]["depositId"].as_str().unwrap().to_owned();
    let event = json!({
        "event_id": "evt_credit_ref2",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(), "amount": 2000, "currency": "USD"}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);

    // The switch refuses the refund — the reversal still commits.
    *stub.refund_refusal.lock().unwrap() = Some((
        StatusCode::UNPROCESSABLE_ENTITY,
        json!({"error": {"type": "invalid_request", "message": "exceeds captured"}}),
    ));
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/deposits/{deposit_id}/reverse"),
        Some(json!({"reason": "chargeback"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"].as_str().unwrap(), "reversed");
    assert_eq!(body["data"]["refundStatus"].as_str().unwrap(), "unknown");

    // Immediate repeated submission cannot bypass the lease.
    let id = Uuid::parse_str(&deposit_id).unwrap();
    let target = db.deposit_refund(id).await.unwrap().unwrap();
    assert_eq!(target.attempt_count, 1);
    assert!(db.claim_deposit_refund(id).await.unwrap().is_none());
    sqlx::query("UPDATE oxsum.deposit_refunds SET next_attempt_at=now() WHERE deposit_id=$1")
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
    // The refusal cleared; a due retry uses the same body ID.
    *stub.refund_refusal.lock().unwrap() = None;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/deposits/{deposit_id}/reverse"),
        Some(json!({"reason": "chargeback"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["refundStatus"].as_str().unwrap(), "pending");
    // Two refund calls total: the refused one and the re-issued one.
    let calls = stub.refunds.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["refund_id"], calls[1]["refund_id"]);
}

/// A `refund_succeeded` webhook settles the standing the reversal wrote.
#[tokio::test]
async fn a_refund_webhook_settles_the_standing() {
    let Some((app, _db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "ref-3", "amountMinor": 1500})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deposit_id = body["data"]["depositId"].as_str().unwrap().to_owned();
    let event = json!({
        "event_id": "evt_credit_ref3",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(), "amount": 1500, "currency": "USD"}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);

    // The stub answers a pending refund.
    *stub.answer_refund.lock().unwrap() = json!({"refund_id": "ref_test_9", "status": "pending"});
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/deposits/{deposit_id}/reverse"),
        Some(json!({"reason": "manual"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["refundStatus"].as_str().unwrap(), "pending");

    // The rail's webhook walks the standing to succeeded — the deposit lists it.
    // The event must name the durable refund identity the server sent and the
    // durable payment identity the charge used.
    let refund_id = stub.refunds.lock().unwrap()[0]["refund_id"].clone();
    let event = json!({
        "event_id": "evt_refund_done",
        "event_type": "refund_succeeded",
        "content": {"type": "refund_details", "object": {
            "refund_id": refund_id,
            "payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(),
            "status": "succeeded",
            "amount": 1500, "currency":"USD"
        }}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);

    // Read the standing back through the admin list.
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/admin/deposits",
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["depositId"].as_str().unwrap() == deposit_id)
        .cloned()
        .unwrap();
    assert_eq!(row["refundStatus"].as_str().unwrap(), "succeeded");
}

/// A `dispute_opened` webhook records the dispute timestamp on the deposit.
#[tokio::test]
async fn a_dispute_opened_is_recorded() {
    let Some((app, _db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey": "disp-1", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deposit_id = body["data"]["depositId"].as_str().unwrap().to_owned();
    let event = json!({
        "event_id": "evt_credit_disp1",
        "event_type": "payment_succeeded",
        "content": {"type": "payment_details", "object": {"payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(), "amount": 5000, "currency": "USD"}}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);

    // A dispute opens on the payment.
    let event = json!({
        "event_id": "evt_dispute",
        "event_type": "dispute_opened",
        "content": {"type": "dispute_details", "object": {
            "payment_id": stub.deposits.lock().unwrap().last().unwrap().clone(),
            "dispute_id": "disp_abc",
            "amount": 5000,
            "currency": "USD"
        }}
    });
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);

    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/admin/deposits",
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["depositId"].as_str().unwrap() == deposit_id)
        .cloned()
        .unwrap();
    assert!(row["disputedAt"].as_str().is_some(), "{row}");
}

/// Lost responses, concurrent retries and late signed events share one external
/// identity while the original local reversal remains exactly once.
#[tokio::test]
async fn a_lost_deposit_refund_response_recovers_without_another_refund() {
    let Some((app, db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey":"lost-reversal","amountMinor":2000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = Uuid::parse_str(body["data"]["depositId"].as_str().unwrap()).unwrap();
    let event = json!({"event_id":"paid-lost","event_type":"payment_succeeded","content":{"type":"payment_details","object":{
        "payment_id":id.to_string(),"amount":2000,"currency":"USD"}}});
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    *stub.lost_refund_response.lock().unwrap() = true;
    let path = format!("/api/v1/admin/deposits/{id}/reverse");
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({"reason":"lost external reply"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["refundStatus"], "unknown");
    let entry = body["data"]["reversalEntryId"].clone();
    let target = db.deposit_refund(id).await.unwrap().unwrap();
    assert_eq!(target.attempt_count, 1);
    assert!(target.body_identity);
    assert_eq!(stub.refunds.lock().unwrap().len(), 1);
    *stub.refund_reads_unavailable.lock().unwrap() = false;
    sqlx::query("UPDATE oxsum.deposit_refunds SET next_attempt_at=now() WHERE deposit_id=$1")
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        call(
            &app,
            "POST",
            &path,
            Some(json!({"reason":"lost external reply"})),
            Some(ADMIN_TOKEN)
        ),
        call(
            &app,
            "POST",
            &path,
            Some(json!({"reason":"lost external reply"})),
            Some(ADMIN_TOKEN)
        )
    );
    assert_eq!(a.0, StatusCode::OK);
    assert_eq!(b.0, StatusCode::OK);
    assert_eq!(a.1["data"]["reversalEntryId"], entry);
    assert_eq!(b.1["data"]["reversalEntryId"], entry);
    let recovered = db.deposit_refund(id).await.unwrap().unwrap();
    assert_eq!(recovered.attempt_count, 2);
    assert_eq!(
        recovered.provider_refund_id.as_deref(),
        Some(target.request_id.to_string().as_str())
    );
    assert_eq!(stub.refunds.lock().unwrap().len(), 1);
    for (suffix, amount, refund_id, currency, expected) in [
        (
            "wrong-amount",
            1999,
            target.request_id.to_string(),
            "USD",
            StatusCode::CONFLICT,
        ),
        (
            "wrong-id",
            2000,
            Uuid::new_v4().to_string(),
            "USD",
            StatusCode::CONFLICT,
        ),
        (
            "wrong-currency",
            2000,
            target.request_id.to_string(),
            "EUR",
            StatusCode::CONFLICT,
        ),
        (
            "correct",
            2000,
            target.request_id.to_string(),
            "USD",
            StatusCode::OK,
        ),
    ] {
        let event = json!({"event_id":suffix,"event_type":"refund_succeeded","content":{"type":"refund_details","object":{
            "refund_id":refund_id,"payment_id":id.to_string(),"amount":amount,"currency":currency,"status":"succeeded"}}});
        let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
        assert_eq!(
            post_webhook(&app, headers, body).await,
            expected,
            "{suffix}"
        );
    }
    assert_eq!(
        db.deposit_refund(id).await.unwrap().unwrap().state,
        "succeeded"
    );
    assert_eq!(balance(&app, &key).await, 0);
    assert!(db.claim_deposit_refund(id).await.unwrap().is_none());
}

/// A pre-upgrade reversal can retrieve its known handle, but an unbound old
/// outcome and a missing old provider record must never authorize a new POST.
#[tokio::test]
async fn historical_deposit_reversals_are_fenced_or_read_only() {
    let Some((app, db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey":"old-reversal","amountMinor":2000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = Uuid::parse_str(body["data"]["depositId"].as_str().unwrap()).unwrap();
    let event = json!({"event_id":"paid-old","event_type":"payment_succeeded","content":{"type":"payment_details","object":{
        "payment_id":id.to_string(),"amount":2000,"currency":"USD"}}});
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    // Simulate an old binary's reversed row: it has no new recovery record.
    // The old provider status is intentionally retained but cannot establish
    // new terminal evidence under the upgraded projection.
    sqlx::query("UPDATE oxsum.deposits SET status='reversed',reversed_at=now(),meta=meta||'{\"refundStatus\":\"failed\",\"hyperswitchRefundId\":\"ref_historical\"}'::jsonb WHERE deposit_id=$1")
        .bind(id).execute(db.pool()).await.unwrap();
    stub.stored_refunds.lock().unwrap().insert("ref_historical".into(),json!({
        "refund_id":"ref_historical","payment_id":"pay_test_123","amount":2000,"currency":"USD","status":"succeeded",
        "metadata":{"deposit_id":id}
    }));
    let path = format!("/api/v1/admin/deposits/{id}/reverse");
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({"reason":"historical recovery"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["refundStatus"], "pending");
    let target = db.deposit_refund(id).await.unwrap().unwrap();
    assert!(!target.body_identity);
    assert!(stub.refunds.lock().unwrap().is_empty());
    let context = db
        .merchant_signing_context(
            target.merchant_id.unwrap(),
            target.version_id.unwrap(),
            &sealing_key(),
        )
        .await
        .unwrap()
        .unwrap();
    let http = oxsum_integrations::provider_http::ProviderHttp::default();
    let mut unbound = target.clone();
    unbound.provider_refund_id = None;
    assert!(
        oxsum_integrations::deposit_refund::recover(&context, &unbound, &http)
            .await
            .is_err()
    );
    stub.stored_refunds.lock().unwrap().remove("ref_historical");
    assert!(
        oxsum_integrations::deposit_refund::recover(&context, &target, &http)
            .await
            .is_err()
    );
    assert!(stub.refunds.lock().unwrap().is_empty());
    let original: Value = sqlx::query_scalar("SELECT meta FROM oxsum.deposits WHERE deposit_id=$1")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(original["refundStatus"], "failed");
    assert_eq!(original["hyperswitchRefundId"], "ref_historical");
    assert!(
        sqlx::query("UPDATE oxsum.deposit_refunds SET body_identity=true WHERE deposit_id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .is_err()
    );
    // No known handle and no merchant binding must also surface as unknown.
    let org = target.organization_id;
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.deposits(deposit_id,organization_id,rail,payment_ref,amount_minor,status) VALUES($1,$2,'hyperswitch','pre-upgrade-unbound',100,'reversed')")
        .bind(other).bind(org).execute(db.pool()).await.unwrap();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/deposits/{other}/reverse"),
        Some(json!({"reason":"reconcile unknown"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["refundStatus"], "unknown");
    let target = db.deposit_refund(other).await.unwrap().unwrap();
    assert!(!target.body_identity);
    assert!(target.next_attempt_at.is_none());
    assert!(stub.refunds.lock().unwrap().is_empty());
}

/// Exhaustion is an unresolved external outcome, not proof that money stayed
/// with the merchant. The local reversal and request ID remain immutable.
#[tokio::test]
async fn exhausted_deposit_recovery_accepts_exact_late_success_without_recreating() {
    let Some((app, db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/hyperswitch",
        Some(json!({"idempotencyKey":"exhausted-reversal","amountMinor":2000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = Uuid::parse_str(body["data"]["depositId"].as_str().unwrap()).unwrap();
    let event = json!({"event_id":"paid-exhausted","event_type":"payment_succeeded","content":{"type":"payment_details","object":{
        "payment_id":id.to_string(),"amount":2000,"currency":"USD"}}});
    let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    *stub.lost_refund_response.lock().unwrap() = true;
    let path = format!("/api/v1/admin/deposits/{id}/reverse");
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({"reason":"one reversal"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry = body["data"]["reversalEntryId"].clone();
    let initial = db.deposit_refund(id).await.unwrap().unwrap();
    for attempt in 2..=5 {
        sqlx::query("UPDATE oxsum.deposit_refunds SET next_attempt_at=now() WHERE deposit_id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
        let (a, b) = tokio::join!(
            call(
                &app,
                "POST",
                &path,
                Some(json!({"reason":"retry cannot replace the reason"})),
                Some(ADMIN_TOKEN)
            ),
            call(
                &app,
                "POST",
                &path,
                Some(json!({"reason":"one reversal"})),
                Some(ADMIN_TOKEN)
            )
        );
        assert_eq!(a.0, StatusCode::OK);
        assert_eq!(b.0, StatusCode::OK);
        assert_eq!(a.1["data"]["reversalEntryId"], entry);
        let standing = db.deposit_refund(id).await.unwrap().unwrap();
        assert_eq!(standing.attempt_count, attempt);
        assert_eq!(standing.request_id, initial.request_id);
        assert_eq!(standing.reason, "one reversal");
        assert_eq!(standing.state, "unknown");
        assert_eq!(standing.next_attempt_at.is_none(), attempt == 5);
    }
    assert!(db.claim_deposit_refund(id).await.unwrap().is_none());
    assert!(
        db.due_deposit_refunds(100)
            .await
            .unwrap()
            .iter()
            .all(|r| r.deposit_id != id)
    );
    assert_eq!(stub.refunds.lock().unwrap().len(), 1);
    for (suffix, kind) in [
        ("late-failure", "refund_failed"),
        ("late-success", "refund_succeeded"),
        ("older-failure", "refund_failed"),
    ] {
        let event = json!({"event_id":suffix,"event_type":kind,"content":{"type":"refund_details","object":{
            "refund_id":initial.request_id,"payment_id":id.to_string(),"amount":2000,"currency":"USD"}}});
        let (headers, body) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
        assert_eq!(post_webhook(&app, headers, body).await, StatusCode::OK);
    }
    let final_state = db.deposit_refund(id).await.unwrap().unwrap();
    assert_eq!(final_state.state, "succeeded");
    assert_eq!(final_state.attempt_count, 5);
    assert!(final_state.next_attempt_at.is_none());
    assert!(
        db.note_deposit_refund(id, "a-different-provider-handle")
            .await
            .is_err()
    );
    assert_eq!(balance(&app, &key).await, 0);
    assert_eq!(stub.refunds.lock().unwrap().len(), 1);
}

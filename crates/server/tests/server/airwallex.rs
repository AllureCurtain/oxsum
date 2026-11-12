//! Airwallex rail tests (issue #285): the two endpoints, the signature
//! verification, and the full intent → login → intent → webhook → credit
//! flow. The tests need DATABASE_URL and skip without it; a stub Airwallex
//! API stands where `api.airwallex.com` would be.

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

const WEBHOOK_SECRET: &str = "awx-whsec-test-1234";

fn sealing_key() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

/// The Airwallex API stub: `POST /api/v1/authentication/login` answers a
/// bearer token, `POST /api/v1/pa/payment_intents/create` records the
/// `merchant_order_id` and answers a fixed intent id + client_secret.
/// `POST /api/v1/pa/refunds/create` records the refund it was asked to issue
/// and answers a `rfd_…` handle; `GET /api/v1/pa/refunds/{id}` reads it back.
#[derive(Clone, Default)]
struct AirwallexStub {
    calls: Arc<Mutex<Vec<String>>>,
    logins: Arc<Mutex<u32>>,
    answer_id: Arc<Mutex<String>>,
    answer_secret: Arc<Mutex<String>>,
    /// Refund create bodies the recovery path sent, in order.
    refund_creates: Arc<Mutex<Vec<Value>>>,
    /// Refund objects the stub issued, keyed by `request_id`/`id`.
    stored_refunds: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    /// Armed refusal for the next refund create (status + error envelope).
    refund_refusal: Arc<Mutex<Option<(StatusCode, Value)>>>,
}

async fn stub_airwallex() -> (String, AirwallexStub) {
    let stub = AirwallexStub::default();
    let app = Router::new()
        .route(
            "/api/v1/authentication/login",
            post(|State(stub): State<AirwallexStub>| async move {
                *stub.logins.lock().unwrap() += 1;
                axum::Json(json!({
                    "token": "awx-test-token",
                    "expires_at": "2099-01-01T00:00:00Z",
                }))
            }),
        )
        .route(
            "/api/v1/pa/payment_intents/create",
            post(
                |State(stub): State<AirwallexStub>, body: axum::Json<Value>| async move {
                    let order_id = body["merchant_order_id"].as_str().unwrap_or_default();
                    stub.calls.lock().unwrap().push(order_id.to_owned());
                    axum::Json(json!({
                        "id": *stub.answer_id.lock().unwrap(),
                        "status": "REQUIRES_PAYMENT_METHOD",
                        "client_secret": *stub.answer_secret.lock().unwrap(),
                    }))
                },
            ),
        )
        .route(
            "/api/v1/pa/refunds/create",
            post(
                |State(stub): State<AirwallexStub>, body: axum::Json<Value>| async move {
                    stub.refund_creates.lock().unwrap().push(body.0.clone());
                    if let Some((status, refusal)) = stub.refund_refusal.lock().unwrap().clone() {
                        return (status, axum::Json(refusal));
                    }
                    let request_id = body["request_id"].as_str().unwrap_or_default();
                    let refund_id = format!("rfd_{request_id}");
                    let value = json!({
                        "id": refund_id,
                        "request_id": request_id,
                        "payment_intent_id": body["payment_intent_id"],
                        "amount": body["amount"],
                        "currency": body["currency"],
                        "status": "RECEIVED",
                        "metadata": body["metadata"],
                    });
                    stub.stored_refunds
                        .lock()
                        .unwrap()
                        .insert(request_id.to_owned(), value.clone());
                    stub.stored_refunds
                        .lock()
                        .unwrap()
                        .insert(refund_id, value.clone());
                    (StatusCode::OK, axum::Json(value))
                },
            ),
        )
        .route(
            "/api/v1/pa/refunds/{id}",
            axum::routing::get(
                |State(stub): State<AirwallexStub>,
                 axum::extract::Path(id): axum::extract::Path<String>| async move {
                    stub.stored_refunds.lock().unwrap().get(&id).cloned().map_or(
                        (
                            StatusCode::NOT_FOUND,
                            axum::Json(json!({"code":"resource_not_found","message":"refund not found"})),
                        ),
                        |value| (StatusCode::OK, axum::Json(value)),
                    )
                },
            ),
        )
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), stub)
}

/// A configured world: the Airwallex stub answers login and intents, and the
/// config carries the rail's four variables.
async fn world() -> Option<(Router, Db, AirwallexStub)> {
    let u = oxsum_testkit::suite_url!()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let (api_url, stub) = stub_airwallex().await;
    *stub.answer_id.lock().unwrap() = "int_test_123".to_owned();
    *stub.answer_secret.lock().unwrap() = "int_test_123_client_secret".to_owned();
    let rail = oxsum_server::airwallex::Airwallex::new(
        "awx-client-id".to_owned(),
        "awx-api-key".to_owned(),
        WEBHOOK_SECRET.to_owned(),
        api_url,
        "https://checkout.airwallex.com".to_owned(),
        "http://localhost:3000".to_owned(),
        reqwest::Client::new(),
    );
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_public_url("http://localhost:3000")
        .with_merchant_bootstrap(rail.merchant_bootstrap(
            "awx-merchant-account".to_owned(),
            oxsum_core::MerchantMode::Test,
            oxsum_core::ProviderDestinationPolicy {
                network: oxsum_core::ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            },
        ))
        .with_airwallex(rail);
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
        req = crate::support::credential(req, key);
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

/// A signed Airwallex webhook: `x-timestamp` (unix ms) and `x-signature`
/// carry the test secret's tag over `timestamp + body`.
fn signed_webhook(secret: &str, body: &str) -> (HeaderMap, String) {
    let timestamp = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    let timestamp = timestamp.to_string();
    let mut mac = <SimpleHmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(body.as_bytes());
    let tag: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut headers = HeaderMap::new();
    headers.insert("x-timestamp", timestamp.parse().unwrap());
    headers.insert("x-signature", tag.parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    (headers, body.to_owned())
}

async fn register_session(app: &Router) -> String {
    let email = format!(
        "airwallex_{}@example.com",
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
    crate::support::login(app, &email, "correct horse battery").await
}

/// The org's ledger balance, read back through the API.
async fn balance(app: &Router, key: &str) -> i64 {
    let (status, body) = call(app, "GET", "/api/v1/balance", None, Some(key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["data"]["availableMinor"].as_i64().unwrap()
}

/// A deposit the intent endpoint leaves: pending, the hosted-page URL answered.
#[tokio::test]
async fn the_intent_opens_a_pending_deposit_and_answers_the_url() {
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register_session(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": "topup-1", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["checkoutUrl"].as_str().unwrap(),
        "https://checkout.airwallex.com/?intent_id=int_test_123"
    );
    assert_eq!(body["data"]["intentId"].as_str().unwrap(), "int_test_123");
    assert_eq!(
        body["data"]["clientSecret"].as_str().unwrap(),
        "int_test_123_client_secret"
    );
    assert!(body["data"]["depositId"].is_string());
    // The stub saw the deposit id as merchant_order_id, after one login.
    let calls = stub.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0], body["data"]["depositId"].as_str().unwrap());
    assert_eq!(*stub.logins.lock().unwrap(), 1);
}

/// A replayed intent answers the same URL and secret, not a second intent.
#[tokio::test]
async fn a_replayed_intent_answers_the_same_url() {
    let Some((app, _db, stub)) = world().await else {
        return;
    };
    let key = register_session(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": "topup-2", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first_url = body["data"]["checkoutUrl"].as_str().unwrap().to_owned();

    // The stub changes its answer for a second call — a replay must not reach it.
    *stub.answer_id.lock().unwrap() = "int_test_456".to_owned();

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": "topup-2", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["checkoutUrl"].as_str().unwrap(), first_url);
    assert_eq!(
        body["data"]["clientSecret"].as_str().unwrap(),
        "int_test_123_client_secret"
    );
    // Still exactly one upstream intent call: the replay hit the deposit row.
    assert_eq!(stub.calls.lock().unwrap().len(), 1);
}

/// The full flow: intent → login stub → signed webhook → purchased-pool credit.
#[tokio::test]
async fn the_full_flow_credits_the_purchased_pool() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register_session(&app).await;
    let before = balance(&app, &key).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": "topup-3", "amountMinor": 5000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let intent_id = "int_test_123";
    let deposit_id = body["data"]["depositId"].as_str().unwrap();

    let event = json!({
        "id": "evt_1",
        "name": "payment_intent.succeeded",
        "data": {
            "object": {
                "id": intent_id,
                "merchant_order_id": deposit_id,
                "amount": 50.0,
                "currency": "USD",
                "status": "SUCCEEDED",
            }
        }
    });
    let (headers, body_str) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/airwallex")
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
    let key = register_session(&app).await;
    let before = balance(&app, &key).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": "topup-4", "amountMinor": 3000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event = json!({
        "id": "evt_replay_1",
        "name": "payment_intent.succeeded",
        "data": { "object": { "id": "int_test_123", "amount": 30.0, "currency": "USD" } }
    });
    for _ in 0..3 {
        let (headers, b) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/webhooks/airwallex")
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
    let key = register_session(&app).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": "topup-5", "amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Sign under a different secret — the tag is wrong.
    let event = json!({
        "name": "payment_intent.succeeded",
        "data": { "object": { "id": "int_test_123", "amount": 10.0 } }
    });
    let (headers, body_str) = signed_webhook("wrong-secret", &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/airwallex")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// An unsigned webhook is refused — no headers, no credit.
#[tokio::test]
async fn an_unsigned_webhook_is_refused() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let event = json!({
        "name": "payment_intent.succeeded",
        "data": { "object": { "id": "int_test_123", "amount": 10.0 } }
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/airwallex")
        .header("content-type", "application/json")
        .body(Body::from(event.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// An unconfigured deployment refuses the rail cleanly: the intent 503s and
/// the webhook 404s — no `OXSUM_AIRWALLEX_*` means no rail to expose.
#[tokio::test]
async fn an_unconfigured_deployment_refuses_cleanly() {
    let u = oxsum_testkit::suite_url!().unwrap();
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
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": "topup-6", "amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let event = json!({
        "name": "payment_intent.succeeded",
        "data": { "object": { "id": "int_x", "amount": 10.0 } }
    });
    let (headers, body_str) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/airwallex")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// `payment_intent.cancelled` closes the deposit: the wallet stays where it was.
#[tokio::test]
async fn a_cancelled_intent_credits_nothing() {
    let Some((app, _db, _stub)) = world().await else {
        return;
    };
    let key = register_session(&app).await;
    let before = balance(&app, &key).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": "topup-7", "amountMinor": 2000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let event = json!({
        "id": "evt_cancel_1",
        "name": "payment_intent.cancelled",
        "data": { "object": { "id": "int_test_123" } }
    });
    let (headers, body_str) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/airwallex")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let after = balance(&app, &key).await;
    assert_eq!(after, before);
}

const ADMIN_TOKEN: &str = "operator-token-airwallex-refund";

/// A world with the admin surface configured too — the reversal path the
/// deposit-refund recovery tests exercise lives on `/api/v1/admin`.
async fn admin_world() -> Option<(Router, Db, AirwallexStub)> {
    let u = oxsum_testkit::suite_url!()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let (api_url, stub) = stub_airwallex().await;
    *stub.answer_id.lock().unwrap() = "int_test_123".to_owned();
    *stub.answer_secret.lock().unwrap() = "int_test_123_client_secret".to_owned();
    let rail = oxsum_server::airwallex::Airwallex::new(
        "awx-client-id".to_owned(),
        "awx-api-key".to_owned(),
        WEBHOOK_SECRET.to_owned(),
        api_url,
        "https://checkout.airwallex.com".to_owned(),
        "http://localhost:3000".to_owned(),
        reqwest::Client::new(),
    );
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_admin_token(ADMIN_TOKEN)
        .with_public_url("http://localhost:3000")
        .with_merchant_bootstrap(rail.merchant_bootstrap(
            "awx-merchant-account".to_owned(),
            oxsum_core::MerchantMode::Test,
            oxsum_core::ProviderDestinationPolicy {
                network: oxsum_core::ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            },
        ))
        .with_airwallex(rail);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, _events, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    Some((app, db, stub))
}

/// Posts a signed Airwallex webhook body to the open intake.
async fn post_webhook(app: &Router, event: &Value) -> StatusCode {
    let (headers, body_str) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/airwallex")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    app.clone().oneshot(req).await.unwrap().status()
}

/// Opens a merchant-bound Airwallex deposit, credits it under a signed
/// `payment_intent.succeeded`, then reverses it through the admin surface.
/// Returns the deposit id, the credited amount and the provider intent id.
async fn credited_reversed_deposit(
    app: &Router,
    stub: &AirwallexStub,
    key: &str,
    amount_minor: i64,
) -> (Uuid, String) {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/topups/airwallex",
        Some(json!({"idempotencyKey": format!("dep-{}", Uuid::new_v4()), "amountMinor": amount_minor})),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let deposit_id = Uuid::parse_str(body["data"]["depositId"].as_str().unwrap()).unwrap();
    let intent_id = stub.answer_id.lock().unwrap().clone();
    // A signed success credits the wallet under the intent id.
    let event = json!({
        "id": format!("evt_paid_{deposit_id}"),
        "name": "payment_intent.succeeded",
        "data": { "object": {
            "id": intent_id,
            "merchant_order_id": deposit_id.to_string(),
            "amount": amount_minor as f64 / 100.0,
            "currency": "USD",
            "status": "SUCCEEDED",
        } }
    });
    assert_eq!(post_webhook(app, &event).await, StatusCode::OK);
    // Reverse it — the operator call queues the durable refund and recovers it.
    let (status, body) = call(
        app,
        "POST",
        &format!("/api/v1/admin/deposits/{deposit_id}/reverse"),
        Some(json!({"reason": "customer asked"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"].as_str().unwrap(), "reversed");
    (deposit_id, intent_id)
}

/// A reversed Airwallex deposit creates the provider refund once, records the
/// `rfd_…` handle, and a signed `refund.settled` event settles the row.
#[tokio::test]
async fn a_reversed_airwallex_deposit_recovers_and_settles() {
    let Some((app, db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;
    let (deposit_id, intent_id) = credited_reversed_deposit(&app, &stub, &key, 4000).await;

    // Recovery ran: exactly one refund create carrying the body identity and
    // the parent intent, after a login.
    let creates = stub.refund_creates.lock().unwrap().clone();
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0]["payment_intent_id"].as_str().unwrap(), intent_id);
    assert_eq!(creates[0]["amount"], json!(40.0));
    assert_eq!(creates[0]["currency"], "USD");
    let request_id = creates[0]["request_id"].as_str().unwrap().to_owned();
    assert!(*stub.logins.lock().unwrap() >= 1);

    // The durable row is pending, with the `rfd_…` provider handle recorded.
    let target = db.deposit_refund(deposit_id).await.unwrap().unwrap();
    assert_eq!(target.state, "pending");
    let provider_refund_id = target.provider_refund_id.clone().unwrap();
    assert_eq!(provider_refund_id, format!("rfd_{request_id}"));

    // An unsigned/wrong-signature `refund.settled` event does NOT settle.
    let wrong = json!({
        "id": "evt_wrong_sig",
        "name": "refund.settled",
        "data": { "object": {
            "id": provider_refund_id,
            "payment_intent_id": intent_id,
            "amount": 40.0,
            "currency": "USD",
            "metadata": { "deposit_id": deposit_id, "payment_id": deposit_id, "organization_id": target.organization_id },
        } }
    });
    let (headers, body_str) = signed_webhook("wrong-secret", &wrong.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/airwallex")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        db.deposit_refund(deposit_id).await.unwrap().unwrap().state,
        "pending"
    );

    // A signed `refund.settled` carrying the refund id + parent intent settles.
    let event = json!({
        "id": format!("evt_settled_{deposit_id}"),
        "name": "refund.settled",
        "data": { "object": {
            "id": provider_refund_id,
            "payment_intent_id": intent_id,
            "amount": 40.0,
            "currency": "USD",
            "status": "SETTLED",
            "metadata": { "deposit_id": deposit_id, "payment_id": deposit_id, "organization_id": target.organization_id },
        } }
    });
    assert_eq!(post_webhook(&app, &event).await, StatusCode::OK);
    let final_state = db.deposit_refund(deposit_id).await.unwrap().unwrap();
    assert_eq!(final_state.state, "succeeded");
    assert!(final_state.next_attempt_at.is_none());
}

/// A refund event that contradicts the pinned reversal cannot settle the row:
/// wrong amount is retained as a conflict, the standing stays pending.
#[tokio::test]
async fn a_mismatched_airwallex_refund_event_never_settles() {
    let Some((app, db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;
    let (deposit_id, intent_id) = credited_reversed_deposit(&app, &stub, &key, 2000).await;
    let target = db.deposit_refund(deposit_id).await.unwrap().unwrap();
    let provider_refund_id = target.provider_refund_id.clone().unwrap();

    // Signed but wrong amount — retained as a conflict, never settles. The
    // provider hears a 409 the way Hyperswitch's intake answers a term mismatch.
    let event = json!({
        "id": format!("evt_bad_amount_{deposit_id}"),
        "name": "refund.settled",
        "data": { "object": {
            "id": provider_refund_id,
            "payment_intent_id": intent_id,
            "amount": 99.99,
            "currency": "USD",
            "metadata": { "deposit_id": deposit_id, "payment_id": deposit_id, "organization_id": target.organization_id },
        } }
    });
    assert_eq!(post_webhook(&app, &event).await, StatusCode::CONFLICT);
    assert_eq!(
        db.deposit_refund(deposit_id).await.unwrap().unwrap().state,
        "pending"
    );

    // Correct amount, wrong refund id — also conflicts, never settles.
    let event = json!({
        "id": format!("evt_bad_id_{deposit_id}"),
        "name": "refund.settled",
        "data": { "object": {
            "id": "rfd_someone_else",
            "payment_intent_id": intent_id,
            "amount": 20.0,
            "currency": "USD",
            "metadata": { "deposit_id": deposit_id, "payment_id": deposit_id, "organization_id": target.organization_id },
        } }
    });
    assert_eq!(post_webhook(&app, &event).await, StatusCode::CONFLICT);
    assert_eq!(
        db.deposit_refund(deposit_id).await.unwrap().unwrap().state,
        "pending"
    );
}

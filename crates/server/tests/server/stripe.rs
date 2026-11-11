//! Stripe rail tests (issue #201): the two endpoints, the signature
//! verification, and the full intent → session → webhook → credit flow.
//! The tests need DATABASE_URL and skip without it; a stub Stripe API stands
//! where `api.stripe.com` would be.

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
    oxsum_testkit::suite_db_url("stripe")
}

fn sealing_key() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

/// The Stripe API stub: `POST /v1/checkout/sessions` answers a fixed session,
/// recording the `client_reference_id` the caller sent. `GET
/// /v1/checkout/sessions/{id}` resolves the session's `payment_intent` for the
/// refund path; `POST /v1/refunds` records the refund it was asked to issue and
/// answers a `re_…` handle; `GET /v1/refunds/{id}` reads it back.
#[derive(Clone, Default)]
struct StripeStub {
    calls: Arc<Mutex<Vec<String>>>,
    answer_url: Arc<Mutex<String>>,
    answer_id: Arc<Mutex<String>>,
    /// The `payment_intent` every session resolves to.
    payment_intent: Arc<Mutex<String>>,
    /// Refund create bodies the recovery path sent, in order.
    refund_creates: Arc<Mutex<Vec<Value>>>,
    /// `Idempotency-Key` headers the refund creates carried, in order.
    refund_idempotency: Arc<Mutex<Vec<String>>>,
    /// Refund objects the stub issued, keyed by `re_…` id.
    stored_refunds: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    /// Armed refusal for the next refund create (status + error envelope).
    refund_refusal: Arc<Mutex<Option<(StatusCode, Value)>>>,
}

/// Reads one form-encoded field out of an `application/x-www-form-urlencoded`
/// body — Stripe's wire shape. Keys arrive percent-encoded (`metadata[a]` is
/// `metadata%5Ba%5D`), so the name is encoded before comparing.
fn form_field(body: &str, name: &str) -> Option<String> {
    body.split('&').find_map(|pair| {
        let mut kv = pair.splitn(2, '=');
        let (Some(k), Some(v)) = (kv.next(), kv.next()) else {
            return None;
        };
        (decode_form(k) == name).then(|| decode_form(v))
    })
}

/// Percent-decodes a `application/x-www-form-urlencoded` token — enough of the
/// spec to read Stripe's `metadata[…]` keys and UUID values back.
fn decode_form(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b as char);
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

async fn stub_stripe() -> (String, StripeStub) {
    let stub = StripeStub::default();
    let app = Router::new()
        .route(
            "/v1/checkout/sessions",
            post(|State(stub): State<StripeStub>, body: String| async move {
                let ref_id = form_field(&body, "client_reference_id").unwrap_or_default();
                stub.calls.lock().unwrap().push(ref_id);
                axum::Json(json!({
                    "id": *stub.answer_id.lock().unwrap(),
                    "url": *stub.answer_url.lock().unwrap(),
                }))
            }),
        )
        .route(
            "/v1/checkout/sessions/{id}",
            axum::routing::get(
                |State(stub): State<StripeStub>,
                 axum::extract::Path(id): axum::extract::Path<String>| async move {
                    axum::Json(json!({
                        "id": id,
                        "payment_intent": *stub.payment_intent.lock().unwrap(),
                        "payment_status": "paid",
                    }))
                },
            ),
        )
        .route(
            "/v1/refunds",
            post(
                |State(stub): State<StripeStub>,
                 headers: HeaderMap,
                 body: String| async move {
                    let parsed = json!({
                        "payment_intent": form_field(&body, "payment_intent"),
                        "amount": form_field(&body, "amount").and_then(|a| a.parse::<i64>().ok()),
                        "metadata": {
                            "deposit_id": form_field(&body, "metadata[deposit_id]"),
                            "payment_id": form_field(&body, "metadata[payment_id]"),
                            "organization_id": form_field(&body, "metadata[organization_id]"),
                        },
                    });
                    stub.refund_creates.lock().unwrap().push(parsed.clone());
                    stub.refund_idempotency.lock().unwrap().push(
                        headers
                            .get("idempotency-key")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned(),
                    );
                    if let Some((status, refusal)) = stub.refund_refusal.lock().unwrap().clone() {
                        return (status, axum::Json(refusal));
                    }
                    let refund_id = format!(
                        "re_{}",
                        parsed["payment_intent"].as_str().unwrap_or_default().replace("pi_", "")
                    );
                    let value = json!({
                        "id": refund_id,
                        "object": "refund",
                        "amount": parsed["amount"],
                        "charge": "ch_stub",
                        "payment_intent": parsed["payment_intent"],
                        "status": "succeeded",
                        "currency": "usd",
                        "metadata": parsed["metadata"],
                    });
                    stub.stored_refunds
                        .lock()
                        .unwrap()
                        .insert(refund_id, value.clone());
                    (StatusCode::OK, axum::Json(value))
                },
            ),
        )
        .route(
            "/v1/refunds/{id}",
            axum::routing::get(
                |State(stub): State<StripeStub>,
                 axum::extract::Path(id): axum::extract::Path<String>| async move {
                    stub.stored_refunds.lock().unwrap().get(&id).cloned().map_or(
                        (
                            StatusCode::NOT_FOUND,
                            axum::Json(json!({"error":{"code":"resource_missing","message":"No such refund"}})),
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
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let (api_url, stub) = stub_stripe().await;
    *stub.answer_id.lock().unwrap() = "cs_test_123".to_owned();
    *stub.answer_url.lock().unwrap() = "https://checkout.stripe.com/pay/cs_test_123".to_owned();
    *stub.payment_intent.lock().unwrap() = "pi_test_123".to_owned();
    let rail = oxsum_server::stripe::Stripe::new(
        "sk_test_123".to_owned(),
        WEBHOOK_SECRET.to_owned(),
        api_url,
        "http://localhost:3000".to_owned(),
    );
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_public_url("http://localhost:3000")
        .with_merchant_bootstrap(rail.merchant_bootstrap(
            "acct_stripe_test".to_owned(),
            oxsum_core::MerchantMode::Test,
            oxsum_core::ProviderDestinationPolicy {
                network: oxsum_core::ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            },
        ))
        .with_stripe(rail);
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

async fn register_session(app: &Router) -> String {
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
    crate::support::login(app, &email, "correct horse battery").await
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
    let key = register_session(&app).await;

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
    let key = register_session(&app).await;

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
    let key = register_session(&app).await;
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
                "payment_status": "paid",
                "currency": "usd",
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
    let key = register_session(&app).await;
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
        "id": "evt_replay_1",
        "type": "checkout.session.completed",
        "data": { "object": { "id": "cs_test_123", "amount_total": 3000, "payment_status": "paid", "currency": "usd" } }
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
    let key = register_session(&app).await;

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
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let config = Config::new(Signup::Open, None).with_secret(sealing_key());
    let (app, _events, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    let key = register_session(&app).await;

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
    let key = register_session(&app).await;
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
        "id": "evt_expired_1",
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

const ADMIN_TOKEN: &str = "operator-token-stripe-refund";

/// A world with the admin surface configured too — the reversal path the
/// deposit-refund recovery tests exercise lives on `/api/v1/admin`.
async fn admin_world() -> Option<(Router, Db, StripeStub)> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let (api_url, stub) = stub_stripe().await;
    *stub.answer_id.lock().unwrap() = "cs_test_123".to_owned();
    *stub.answer_url.lock().unwrap() = "https://checkout.stripe.com/pay/cs_test_123".to_owned();
    *stub.payment_intent.lock().unwrap() = "pi_test_123".to_owned();
    let rail = oxsum_server::stripe::Stripe::new(
        "sk_test_123".to_owned(),
        WEBHOOK_SECRET.to_owned(),
        api_url,
        "http://localhost:3000".to_owned(),
    );
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_admin_token(ADMIN_TOKEN)
        .with_public_url("http://localhost:3000")
        .with_merchant_bootstrap(rail.merchant_bootstrap(
            "acct_stripe_test".to_owned(),
            oxsum_core::MerchantMode::Test,
            oxsum_core::ProviderDestinationPolicy {
                network: oxsum_core::ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            },
        ))
        .with_stripe(rail);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, _events, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    Some((app, db, stub))
}

/// Posts a signed Stripe webhook body to the open intake.
async fn post_webhook(app: &Router, event: &Value) -> StatusCode {
    let (headers, body_str) = signed_webhook(WEBHOOK_SECRET, &event.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/stripe")
        .body(Body::from(body_str))
        .unwrap();
    let mut req = req;
    *req.headers_mut() = headers;
    app.clone().oneshot(req).await.unwrap().status()
}

/// Opens a merchant-bound Stripe deposit, credits it under a signed
/// `checkout.session.completed`, then reverses it through the admin surface.
/// Returns the deposit id and the session's resolved `payment_intent`.
async fn credited_reversed_deposit(
    app: &Router,
    stub: &StripeStub,
    key: &str,
    amount_minor: i64,
) -> (Uuid, String) {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/topups/stripe",
        Some(json!({"idempotencyKey": format!("dep-{}", Uuid::new_v4()), "amountMinor": amount_minor})),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let deposit_id = Uuid::parse_str(body["data"]["depositId"].as_str().unwrap()).unwrap();
    let session_id = "cs_test_123";
    let payment_intent = stub.payment_intent.lock().unwrap().clone();
    // A signed success credits the wallet under the session id.
    let event = json!({
        "id": format!("evt_paid_{deposit_id}"),
        "type": "checkout.session.completed",
        "data": { "object": {
            "id": session_id,
            "client_reference_id": deposit_id.to_string(),
            "amount_total": amount_minor,
            "payment_status": "paid",
            "currency": "usd",
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
    (deposit_id, payment_intent)
}

/// A reversed Stripe deposit resolves `cs_…`→`pi_…`, creates the provider
/// refund once under the `Idempotency-Key` header, records the `re_…` handle,
/// and a signed `refund.updated` event settles the row.
#[tokio::test]
async fn a_reversed_stripe_deposit_recovers_and_settles() {
    let Some((app, db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;
    let (deposit_id, payment_intent) = credited_reversed_deposit(&app, &stub, &key, 4000).await;

    // Recovery ran: exactly one refund create, payment_intent-scoped, integer
    // minor units, under the durable request id in the Idempotency-Key header.
    let creates = stub.refund_creates.lock().unwrap().clone();
    assert_eq!(creates.len(), 1);
    assert_eq!(
        creates[0]["payment_intent"].as_str().unwrap(),
        payment_intent
    );
    assert_eq!(creates[0]["amount"], json!(4000));
    let idempotency = stub.refund_idempotency.lock().unwrap().clone();
    assert_eq!(idempotency.len(), 1);
    assert!(!idempotency[0].is_empty());

    // The durable row is pending with the `re_…` handle recorded, and the
    // deposit's mutable meta carries the resolved `pi_…` (the refund row's own
    // `provider_payment_id` stays its pinned `cs_…` session — that column is
    // immutable under `protect_deposit_refund`).
    let target = db.deposit_refund(deposit_id).await.unwrap().unwrap();
    assert_eq!(target.state, "pending");
    assert_eq!(target.provider_payment_id.as_deref(), Some("cs_test_123"));
    let provider_refund_id = target.provider_refund_id.clone().unwrap();
    assert!(provider_refund_id.starts_with("re_"));
    let intent: Option<String> = sqlx::query_scalar(
        "SELECT meta->>'stripePaymentIntent' FROM oxsum.deposits WHERE deposit_id=$1",
    )
    .bind(deposit_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(intent.as_deref(), Some(payment_intent.as_str()));

    // An unsigned/wrong-signature `refund.updated` event does NOT settle.
    let wrong = json!({
        "id": "evt_wrong_sig",
        "type": "refund.updated",
        "data": { "object": {
            "id": provider_refund_id,
            "payment_intent": payment_intent,
            "amount": 4000,
            "currency": "usd",
            "metadata": { "deposit_id": deposit_id, "payment_id": deposit_id, "organization_id": target.organization_id },
        } }
    });
    let (headers, body_str) = signed_webhook("wrong-secret", &wrong.to_string());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/stripe")
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

    // A signed `refund.updated` carrying the refund id + resolved intent settles.
    let event = json!({
        "id": format!("evt_settled_{deposit_id}"),
        "type": "refund.updated",
        "data": { "object": {
            "id": provider_refund_id,
            "object": "refund",
            "payment_intent": payment_intent,
            "amount": 4000,
            "currency": "usd",
            "status": "succeeded",
            "metadata": { "deposit_id": deposit_id, "payment_id": deposit_id, "organization_id": target.organization_id },
        } }
    });
    assert_eq!(post_webhook(&app, &event).await, StatusCode::OK);
    let final_state = db.deposit_refund(deposit_id).await.unwrap().unwrap();
    assert_eq!(final_state.state, "succeeded");
    assert!(final_state.next_attempt_at.is_none());
}

/// A `charge.refunded` event names the charge object, not the refund — it is
/// deliberately not a settle anchor, so it never settles the durable row.
#[tokio::test]
async fn a_charge_refunded_event_never_settles() {
    let Some((app, db, stub)) = admin_world().await else {
        return;
    };
    let key = register_session(&app).await;
    let (deposit_id, payment_intent) = credited_reversed_deposit(&app, &stub, &key, 2000).await;
    let target = db.deposit_refund(deposit_id).await.unwrap().unwrap();
    assert_eq!(target.state, "pending");

    // A signed `charge.refunded` whose object is the charge — it carries no
    // refund `re_…` and must not be normalized to a settle.
    let event = json!({
        "id": format!("evt_charge_refunded_{deposit_id}"),
        "type": "charge.refunded",
        "data": { "object": {
            "id": "ch_stub",
            "object": "charge",
            "payment_intent": payment_intent,
            "amount_refunded": 2000,
            "currency": "usd",
        } }
    });
    assert_eq!(post_webhook(&app, &event).await, StatusCode::OK);
    assert_eq!(
        db.deposit_refund(deposit_id).await.unwrap().unwrap().state,
        "pending"
    );
}

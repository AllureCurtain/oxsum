//! Payment orders over HTTP (issue #606): the customer pins an order, the stub
//! provider answers checkout handles, and only the signed versioned webhook can
//! mark it `paid` — a return-page visit or a second create never can. Paid and
//! fulfilled are deliberately separate: the reconcile sweep drains
//! paid-unfulfilled orders, and the order-derived ledger key keeps a recovery
//! retry to a single credit.
//!
//! The stub stands where `sandbox.hyperswitch.io` would be; the webhook goes to
//! the versioned merchant route T14.b mounted, which verifies the HMAC before
//! normalizing and drives `apply_order_event`. Needs DATABASE_URL; skips
//! without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core/tests/support/integration.rs"]
mod setup;
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use hmac::{KeyInit, Mac, SimpleHmac};
use http_body_util::BodyExt;
use oxsum_core::{
    Db, DisplayCurrency, MerchantMode, MerchantRail, ProviderDestinationPolicy, ProviderNetwork,
    Retention, SecretKey, Tenants, kinds,
};
use oxsum_server::jobs::{JobContext, run_due};
use oxsum_server::{Config, Metrics, Signup};
use serde_json::{Value, json};
use sha2::Sha512;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const WEBHOOK_SECRET: &str = "order-whsec-payments-1";
const ACCOUNT: &str = "acct_orders_test";
const PASSWORD: &str = "correct horse battery";
/// The rail's own currency the stub charges in — and the organization's display
/// unit, so the server-rated conversion is the order's `0.0014 USD`/credit.
const USD: &str = "USD";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("payments")
}

fn sealing_key() -> SecretKey {
    SecretKey::from_bytes([9; 32])
}

/// The switch API stub: `POST /payments` records the order id it was told to
/// carry in `metadata` and answers the payment id + client secret the test
/// armed — with more than one armed answer each call dequeues the front, so a
/// test can drive several orders through one stub; a single armed answer
/// repeats (the replay tests re-arm between calls). `fail` makes the stub
/// answer 500 — the provider-down case the checkout-session recovery drives.
#[derive(Clone, Default)]
struct SwitchStub {
    orders: Arc<Mutex<Vec<String>>>,
    answers: Arc<Mutex<Vec<(String, String)>>>,
    calls: Arc<Mutex<usize>>,
    fail: Arc<Mutex<bool>>,
    wrong_id: Arc<Mutex<bool>>,
    drop_response: Arc<Mutex<bool>>,
    reads_unavailable: Arc<Mutex<bool>>,
    stored: Arc<Mutex<std::collections::HashMap<String, Value>>>,
}

async fn stub_switch() -> (String, SwitchStub) {
    let stub = SwitchStub::default();
    let app = Router::new()
        .route(
            "/payments",
            post(
                |State(stub): State<SwitchStub>, body: axum::Json<Value>| async move {
                    *stub.calls.lock().unwrap() += 1;
                    if *stub.fail.lock().unwrap() {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            axum::Json(json!({"error": "provider down"})),
                        );
                    }
                    let order = body["metadata"]["order_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    stub.orders.lock().unwrap().push(order);
                    let mut armed = stub.answers.lock().unwrap();
                    let (_alias, secret) = if armed.len() > 1 {
                        armed.remove(0)
                    } else {
                        armed
                            .first()
                            .cloned()
                            .unwrap_or_else(|| ("pay_armed".into(), "secret_armed".into()))
                    };
                    let id = if *stub.wrong_id.lock().unwrap() { "pay_shared".to_owned() }
                        else { body["payment_id"].as_str().unwrap().to_owned() };
                    let value = json!({"payment_id":id,"client_secret":secret,"status":"requires_payment_method",
                        "merchant_id":ACCOUNT,"amount":body["amount"],"currency":body["currency"],"metadata":body["metadata"]});
                    stub.stored.lock().unwrap().insert(id, value.clone());
                    if *stub.drop_response.lock().unwrap() {
                        *stub.reads_unavailable.lock().unwrap() = true;
                        return (StatusCode::BAD_GATEWAY,axum::Json(json!({"error":"response lost after commit"})));
                    }
                    (StatusCode::OK, axum::Json(value))
                },
            ),
        )
        .route("/payments/{id}", axum::routing::get(|State(stub):State<SwitchStub>, axum::extract::Path(id):axum::extract::Path<String>| async move {
            if *stub.reads_unavailable.lock().unwrap() { return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({}))); }
            let value = stub.stored.lock().unwrap().get(&id).cloned();
            value.map_or((StatusCode::NOT_FOUND, axum::Json(json!({"error":{"code":"HE_02"}}))), |v| (StatusCode::OK,axum::Json(v)))
        }))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), stub)
}

/// A configured world: the switch stub answers `POST /payments`, the config
/// carries the rail's environment bootstrap, and `prepare` seeds the merchant.
/// The returned `(merchant_id, version_id)` locates the versioned webhook URL.
async fn world() -> Option<(Router, Db, SwitchStub, Uuid, Uuid)> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let (api_url, stub) = stub_switch().await;
    let rail = oxsum_server::hyperswitch::Hyperswitch::new(
        "snd_order_key".to_owned(),
        WEBHOOK_SECRET.to_owned(),
        api_url,
        "pk_snd_orders".to_owned(),
        "http://localhost:3000".to_owned(),
        "https://sdk.example.test/v1".to_owned(),
    );
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_public_url("http://localhost:3000")
        .with_merchant_bootstrap(rail.merchant_bootstrap(
            ACCOUNT.to_owned(),
            MerchantMode::Test,
            ProviderDestinationPolicy {
                network: ProviderNetwork::Internal,
                allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
            },
        ))
        .with_hyperswitch(rail);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let context = db
        .environment_merchant(MerchantRail::Hyperswitch, &sealing_key())
        .await
        .expect("the environment merchant resolves")
        .expect("the bootstrap seeded a merchant");
    let (app, _events, _metrics) = oxsum_server::app_with_billing(db.clone(), config);
    Some((app, db, stub, context.merchant_id, context.version.id))
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

/// A signed switch webhook: `x-webhook-signature-512` carries the hex
/// HMAC-SHA512 of the raw body under the environment merchant's webhook secret.
fn signed(secret: &str, body: &str) -> HeaderMap {
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
    headers
}

/// POSTs a webhook body to the versioned merchant route.
async fn post_webhook(
    app: &Router,
    merchant: Uuid,
    version: Uuid,
    headers: HeaderMap,
    body: String,
) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/webhooks/merchants/{merchant}/versions/{version}"
        ))
        .body(Body::from(body))
        .unwrap();
    *req.headers_mut() = headers;
    app.clone().oneshot(req).await.unwrap().status()
}

/// A Hyperswitch `payment_succeeded` event naming the pinned local order and
/// customer — the shape `normalize` reads (`metadata.payment_id`,
/// `metadata.organization_id`, the charged `amount`/`currency`).
fn succeeded(order: Uuid, org: Uuid, _provider: &str, amount: i64) -> String {
    json!({
        "merchant_id": ACCOUNT,
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "payment_succeeded",
        "livemode": false,
        "content": {"type":"payment_details","object":{
            "payment_id": order.to_string(),
            "amount": amount,
            "currency": USD,
            "metadata": {"payment_id": order, "order_id": order, "organization_id": org}
        }}
    })
    .to_string()
}

struct Account {
    organization_id: Uuid,
    cookie: String,
}

/// A registered customer and its personal session — order endpoints carry no
/// machine key, so the session cookie is the credential.
async fn register(app: &Router) -> Account {
    let email = format!(
        "orders_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let organization_id =
        Uuid::parse_str(body["data"]["organization"]["id"].as_str().unwrap()).unwrap();
    let cookie = support::login(app, &email, PASSWORD).await;
    Account {
        organization_id,
        cookie,
    }
}

/// The organization's effective display rate: `0.0014` USD per credit, so the
/// server rates a $2.10 order at 1_500_000_000 credit-minor.
async fn set_display(db: &Db, org: Uuid) {
    db.bootstrap_collection_terms(&oxsum_core::CollectionCurrency {
        code: USD.into(),
        rate_per_credit: "0.0014".into(),
    })
    .await
    .unwrap();
    db.set_display_currency(
        org,
        Some(DisplayCurrency {
            code: USD.into(),
            rate_per_credit: "0.0014".into(),
            decimals: 2,
        }),
    )
    .await
    .unwrap();
}

fn create_body(key: &str, purpose: &str, display_minor: i64) -> Value {
    json!({
        "idempotencyKey": key,
        "rail": "hyperswitch",
        "purpose": purpose,
        "displayAmountMinor": display_minor,
        "displayCurrency": USD,
    })
}

/// A paid order end-to-end: the create answers checkout handles, the signed
/// webhook marks `paid`, and the fulfillment read shows the honest mid-state —
/// then the sweep's grant lands the wallet cash exactly once.
#[tokio::test]
async fn a_paid_order_fulfills_promptly_through_the_worker() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_ord_1".into(), "secret_ord_1".into())];

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-1", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order_id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();
    assert_eq!(
        body["data"]["providerPaymentId"].as_str().unwrap(),
        body["data"]["order"]["orderId"].as_str().unwrap()
    );
    assert_eq!(
        body["data"]["clientSecret"].as_str().unwrap(),
        "secret_ord_1"
    );
    assert_eq!(
        body["data"]["order"]["status"].as_str().unwrap(),
        "provider_pending"
    );
    // The stub carried the order's own UUID in metadata — the idempotency anchor.
    assert_eq!(
        stub.orders.lock().unwrap().clone(),
        vec![order_id.to_string()]
    );

    // The return page (the order GET) cannot mark the order paid — only the
    // signed webhook can. Re-reading it still shows `providerPending`.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["status"].as_str().unwrap(), "provider_pending");

    // The signed success event marks the order paid — still not fulfilled.
    let body_text = succeeded(order_id, account.organization_id, "pay_ord_1", 210);
    let status = post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}/fulfillment"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"].as_str().unwrap(), "paid");
    assert!(body["data"]["paidAt"].is_string(), "{body}");
    assert!(body["data"]["fulfillment"].is_null());
    // Nothing credited yet — paid and fulfilled are deliberately separate.
    let wallet = Tenants::new(db.pool().clone())
        .get(&tenant(&db, account.organization_id).await)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);

    // Start the actual scheduler: no direct reconciliation call or test-side
    // SQL forces the fulfillment. A normal live deployment credits promptly.
    db.enqueue_job(
        kinds::RECONCILE,
        time::OffsetDateTime::now_utc() + time::Duration::days(1),
    )
    .await
    .unwrap();
    let worker = oxsum_server::jobs::spawn_worker(db.clone(), ctx());
    let completed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if wallet.available().await.unwrap() == 1_500_000_000 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    worker.abort();
    completed.expect("paid order must fulfill without waiting for daily reconciliation");
    assert_eq!(
        oxsum_core::next_run(kinds::PAYMENT_FULFILLMENT),
        Some(time::Duration::seconds(5))
    );
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}/fulfillment"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"].as_str().unwrap(), "fulfilled");
    assert_eq!(
        body["data"]["fulfillment"]["purpose"].as_str().unwrap(),
        "walletTopup"
    );
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    // A second sweep replays the standing grant — one credit either way.
    run_reconcile(&db).await;
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
}

/// The tenant an organization posts to — the order's wallet.
async fn tenant(db: &Db, org: Uuid) -> String {
    sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
        .bind(org)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Enqueue the dedicated payment recovery pass; daily reconciliation is not needed.
async fn run_reconcile(db: &Db) {
    // Same due-row insert the jobs suite uses: `run_at` is server-side now.
    sqlx::query(
        "INSERT INTO oxsum.jobs (kind, run_at) VALUES ($1, now() - interval '1 second') \
         ON CONFLICT DO NOTHING",
    )
    .bind(kinds::PAYMENT_FULFILLMENT)
    .execute(db.pool())
    .await
    .unwrap();
    run_due(db, &ctx()).await;
}

fn ctx() -> JobContext {
    JobContext {
        hold_timeout: Duration::from_secs(60),
        metrics: Metrics::new(),
        webhook: None,
        retention: Retention::default(),
        upstream_price_sync: false,
        witness: None,
        bill_provider: None,
    }
}

/// A replayed create answers the standing order and the same checkout handles —
/// the provider was never asked a second time, so a retry never issues a second
/// charge.
#[tokio::test]
async fn a_replayed_create_returns_the_same_checkout() {
    let Some((app, db, stub, _m, _v)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_ord_2".into(), "secret_ord_2".into())];

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-2", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order_id = body["data"]["order"]["orderId"]
        .as_str()
        .unwrap()
        .to_owned();
    let secret = body["data"]["clientSecret"].as_str().unwrap().to_owned();

    // The replay reuses the key; the stub would answer a different payment id,
    // so reaching it would prove a second charge — it must not be reached.
    *stub.answers.lock().unwrap() = vec![("pay_other".into(), "secret_other".into())];
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-2", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["order"]["orderId"].as_str().unwrap(), order_id);
    assert_eq!(body["data"]["clientSecret"].as_str().unwrap(), secret);
    assert_eq!(
        body["data"]["providerPaymentId"].as_str().unwrap(),
        body["data"]["order"]["orderId"].as_str().unwrap()
    );
    // Still exactly one upstream payment call.
    assert_eq!(*stub.calls.lock().unwrap(), 1);

    // Changed terms under the same idempotency key conflict.
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-2", "walletTopup", 999)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// A paid event whose amount disagrees with the pinned terms never credits —
/// the intake rejects the mismatch and the order can neither pay nor fulfill.
#[tokio::test]
async fn a_wrong_amount_event_never_marks_paid() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_ord_3".into(), "secret_ord_3".into())];

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-3", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let order_id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();

    // The signed event reports 999 against the pinned 210 — classify rejects it.
    let body_text = succeeded(order_id, account.organization_id, "pay_ord_3", 999);
    let status = post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Still provider_pending — the mismatch is durably rejected, never credited.
    assert_eq!(body["data"]["status"].as_str().unwrap(), "provider_pending");
    let state: String = sqlx::query_scalar(
        "SELECT state FROM oxsum.merchant_event_inbox WHERE merchant_id=$1 ORDER BY received_at DESC LIMIT 1",
    )
    .bind(merchant)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(state, "rejected");
    let wallet = Tenants::new(db.pool().clone())
        .get(&tenant(&db, account.organization_id).await)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// One provider payment can never fund two orders: a second order whose checkout
/// returns the same provider payment id is refused the moment it tries to bind
/// the reference — the shared `merchant_payment_refs` dedup holds.
#[tokio::test]
async fn a_provider_payment_id_cannot_bind_two_orders() {
    let Some((app, db, stub, _m, _v)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_shared".into(), "secret_shared".into())];

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-4a", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    *stub.wrong_id.lock().unwrap() = true;
    // A second order the provider answers with a conflicting payment id: binding it
    // through `merchant_payment_refs` conflicts — the create refuses rather
    // than let one provider charge fund two orders.
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-4b", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// A business-context session for the operator reads: the user registers, the
/// test owns its `business_staff` row, and the session switches to the business
/// context the report requires.
async fn operator(db: &Db, app: &Router) -> String {
    let email = format!(
        "operator_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let user = db
        .register(oxsum_core::NewUser {
            email: email.clone(),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap()
        .user;
    sqlx::query(
        "INSERT INTO oxsum.business_staff (business_id,user_id,role) \
                 SELECT business_id,$1,'owner' FROM oxsum.businesses WHERE singleton",
    )
    .bind(user.id)
    .execute(db.pool())
    .await
    .unwrap();
    let cookie = support::login(app, &email, PASSWORD).await;
    db.switch_session_context(
        &cookie,
        &oxsum_core::SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: oxsum_core::SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    cookie
}

/// The business reconciliation report reads paid against fulfilled and names
/// the `needs_attention` discrepancy set — the operator's view, behind the
/// personal session's reports capability.
#[tokio::test]
async fn the_reconciliation_reports_paid_against_fulfilled() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_ord_5".into(), "secret_ord_5".into())];

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-5", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let order_id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();
    let body_text = succeeded(order_id, account.organization_id, "pay_ord_5", 210);
    post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;

    // The operator's business session reads the report — a customer cookie never
    // carries the business capability.
    let owner = operator(&db, &app).await;
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/business/payment-reconciliation",
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["paid"].as_i64().unwrap() >= 1);
    assert!(
        body["data"]["paidUnfulfilled"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o.as_str().unwrap() == order_id.to_string())
    );

    run_reconcile(&db).await;
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/business/payment-reconciliation",
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body["data"]["paidUnfulfilled"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o.as_str().unwrap() == order_id.to_string())
    );
    assert!(body["data"]["fulfilled"].as_i64().unwrap() >= 1);
}

// ---------------------------------------------------------------------------
// T16 (issue #607): the customer checkout surface — the live checkout-session
// read, the order history's filters, the staff manual receipt and the
// cross-organization order view.
// ---------------------------------------------------------------------------

/// The checkout-session read answers the live handles and the lifecycle truth:
/// a returned browser polls it instead of guessing at the return URL, a
/// `provider_pending` order shows its standing handles, and `paid`/`fulfilled`
/// carry the same truth — never a promise this surface made.
#[tokio::test]
async fn the_checkout_session_recovers_the_live_handles() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_sess".into(), "secret_sess".into())];

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-sess", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order_id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();

    // The session answers the same handles the create minted — a page reload
    // or the provider's return lands on the same checkout.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}/checkout-session"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["providerPaymentId"].as_str().unwrap(),
        order_id.to_string()
    );
    assert_eq!(
        body["data"]["clientSecret"].as_str().unwrap(),
        "secret_sess"
    );
    assert_eq!(body["data"]["status"].as_str().unwrap(), "provider_pending");
    assert_eq!(
        body["data"]["publishableKey"].as_str().unwrap(),
        "pk_snd_orders"
    );
    assert_eq!(
        body["data"]["sdkUrl"].as_str().unwrap(),
        "https://sdk.example.test/v1"
    );

    // Paid: the session still answers the handles, status now `paid` — the
    // page reads truth, never infers it.
    let body_text = succeeded(order_id, account.organization_id, "pay_sess", 210);
    post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}/checkout-session"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"].as_str().unwrap(), "paid");
    // No second provider call: one upstream payment the whole way.
    assert_eq!(*stub.calls.lock().unwrap(), 1);
}

/// A provider outage at create time leaves the order `created` — the create
/// answers 503 — and the checkout-session read re-drives the provider call
/// under the same order UUID once the rail recovers. The retry can never
/// charge twice: the order UUID is the provider's own idempotency key.
#[tokio::test]
async fn a_failed_provider_call_recovers_through_the_session() {
    let Some((app, db, stub, _m, _v)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.fail.lock().unwrap() = true;

    // The provider is down: the order pins, the checkout drive refuses.
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-down", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // The order exists, still `created` — never a silent pending.
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/orders?status=created",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order_id = Uuid::parse_str(body["data"]["orders"][0]["orderId"].as_str().unwrap()).unwrap();
    assert_eq!(
        body["data"]["orders"][0]["status"].as_str().unwrap(),
        "created"
    );

    // The rail recovers; the session read re-issues under the same order UUID.
    *stub.fail.lock().unwrap() = false;
    *stub.answers.lock().unwrap() = vec![("pay_rec".into(), "secret_rec".into())];
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}/checkout-session"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["providerPaymentId"].as_str().unwrap(),
        order_id.to_string()
    );
    assert_eq!(body["data"]["status"].as_str().unwrap(), "provider_pending");
    // Two upstream calls total — the failed first and the recovery. The
    // recovery still carried the same order UUID as the provider's idempotency
    // key, so no second charge can exist.
    assert_eq!(*stub.calls.lock().unwrap(), 2);
    assert_eq!(
        stub.orders.lock().unwrap().clone(),
        vec![order_id.to_string()]
    );
}

/// A failed provider event walks the order to `failed` — the truth the page
/// and the list read, never a silent pending the customer cannot tell apart.
#[tokio::test]
async fn a_failed_event_surfaces_failed_not_pending() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_fail".into(), "secret_fail".into())];

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-fail", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let order_id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();

    // The switch reports payment_failed for the bound payment.
    let body_text = json!({
        "merchant_id": ACCOUNT,
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "payment_failed",
        "livemode": false,
        "content": {"type":"payment_details","object":{
            "payment_id": order_id.to_string(),
            "amount": 210,
            "currency": USD,
            "metadata": {"payment_id": order_id, "order_id": order_id, "organization_id": account.organization_id}
        }}
    })
    .to_string();
    let status = post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The order read, the session and the filtered list all say `failed`.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["status"].as_str().unwrap(), "failed");
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}/checkout-session"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["status"].as_str().unwrap(), "failed");
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/orders?status=failed",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["orders"].as_array().unwrap().len(), 1);
    let wallet = Tenants::new(db.pool().clone())
        .get(&tenant(&db, account.organization_id).await)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// A duplicate signed callback replays to the same order state — the event's
/// own dedup holds, and a second sweep of the same fulfillment still credits
/// once.
#[tokio::test]
async fn a_duplicate_callback_replays_to_the_same_state() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_dup".into(), "secret_dup".into())];

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-dup", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let order_id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();

    // The same signed event delivered twice: the inbox's dedup makes the
    // second delivery a no-op, and the order's `paid` is set exactly once.
    let event = json!({
        "merchant_id": ACCOUNT,
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "payment_succeeded",
        "livemode": false,
        "content": {"type":"payment_details","object":{
            "payment_id": order_id.to_string(),
            "amount": 210,
            "currency": USD,
            "metadata": {"payment_id": order_id, "order_id": order_id, "organization_id": account.organization_id}
        }}
    })
    .to_string();
    for _ in 0..2 {
        let status = post_webhook(
            &app,
            merchant,
            version,
            signed(WEBHOOK_SECRET, &event),
            event.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["status"].as_str().unwrap(), "paid");
    run_reconcile(&db).await;
    let wallet = Tenants::new(db.pool().clone())
        .get(&tenant(&db, account.organization_id).await)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
}

/// A double-clicked create rides the same idempotency key: the second browser
/// submission gets the standing order and its original checkout — the stub
/// never sees a second intent. (The replay contract, exercised at the session
/// surface this time.)
#[tokio::test]
async fn a_repeated_browser_submission_returns_the_standing_checkout() {
    let Some((app, db, stub, _m, _v)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_dbl".into(), "secret_dbl".into())];

    for _ in 0..2 {
        let (status, body) = call(
            &app,
            "POST",
            "/api/v1/orders",
            Some(create_body("ord-dbl", "walletTopup", 210)),
            Some(&account.cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["providerPaymentId"].as_str().unwrap(),
            body["data"]["order"]["orderId"].as_str().unwrap()
        );
    }
    assert_eq!(*stub.calls.lock().unwrap(), 1);
}

/// The order history filters: `purpose` and `status` narrow the page the
/// customer reads, and an unknown status is a validation error, not an empty
/// page that reads like "no orders".
#[tokio::test]
async fn the_order_list_filters_by_purpose_and_status() {
    let Some((app, db, stub, _m, _v)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![
        ("pay_flt".into(), "secret_flt".into()),
        ("pay_flt_b".into(), "secret_flt_b".into()),
    ];

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-flt-1", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let owner = operator(&db, &app).await;
    let offer = db
        .publish_bundle(
            &owner,
            &oxsum_core::BundlePublishRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "bundle filter fixture".into(),
                bundle_id: None,
                expected_version: 0,
                terms: oxsum_core::BundleTerms {
                    name: "filter bundle".into(),
                    display_currency: USD.into(),
                    display_amount_minor: 500,
                    purchased_minor: 5_000_000,
                    promo_minor: 50_000_000,
                    promo_lifetime_days: Some(30),
                },
            },
        )
        .await
        .unwrap();
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(json!({
            "idempotencyKey": "ord-flt-2",
            "purposeRef": offer.offer.bundle_version_id,
            "rail": "hyperswitch",
            "purpose": "bundle",
            "displayAmountMinor": 500,
            "displayCurrency": USD,
            "promoMinor": 50_000_000,
        })),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // purpose=walletTopup lists one; purpose=bundle lists the other.
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/orders?purpose=walletTopup",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let orders = body["data"]["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0]["purpose"].as_str().unwrap(), "walletTopup");

    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/orders?purpose=bundle",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["orders"].as_array().unwrap().len(), 1);

    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/orders?status=provider_pending",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["orders"].as_array().unwrap().len(), 2);

    let (status, _body) = call(
        &app,
        "GET",
        "/api/v1/orders?status=bogus",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A staff-recorded manual receipt funds the customer's wallet through the
/// same order + fulfillment path: the order lands `paid` on the receipt's own
/// evidence, the fulfillment books the top-up once, the customer sees the
/// manual order in their own history, and a replay or a reused reference
/// cannot credit twice.
#[tokio::test]
async fn a_manual_receipt_fulfills_once_with_unique_evidence() {
    let Some((app, db, _stub, _m, _v)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;

    let receipt = json!({
        "organizationId": account.organization_id,
        "idempotencyKey": "manual-1",
        "displayAmountMinor": 210,
        "displayCurrency": USD,
        "reference": "bank-txn-7f3a",
        "reason": "wire transfer received",
    });
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/business/orders",
        Some(receipt.clone()),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order_id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();
    assert_eq!(body["data"]["order"]["source"].as_str().unwrap(), "manual");
    assert_eq!(
        body["data"]["order"]["status"].as_str().unwrap(),
        "fulfilled"
    );
    assert!(body["data"]["order"]["merchantId"].is_null());
    assert_eq!(
        body["data"]["order"]["evidence"]["reference"]
            .as_str()
            .unwrap(),
        "bank-txn-7f3a"
    );
    assert_eq!(
        body["data"]["fulfillment"]["purpose"].as_str().unwrap(),
        "walletTopup"
    );
    // The wallet cash landed through the same grant path a webhook-paid order
    // takes — 210 USD minor at 0.0014 USD/credit is 1.5 credits.
    let wallet = Tenants::new(db.pool().clone())
        .get(&tenant(&db, account.organization_id).await)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    // The customer reads the manual order in their own history — a funded
    // top-up is a payment order like any other.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/orders/{order_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["source"].as_str().unwrap(), "manual");
    assert_eq!(body["data"]["status"].as_str().unwrap(), "fulfilled");

    // A replayed receipt returns the standing fulfilled order — never a
    // second grant.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/business/orders",
        Some(receipt.clone()),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["order"]["orderId"].as_str().unwrap(),
        order_id.to_string()
    );
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    // The same receipt reference funding a second order is refused — one
    // piece of evidence, one grant.
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/business/orders",
        Some(json!({
            "organizationId": account.organization_id,
            "idempotencyKey": "manual-2",
            "displayAmountMinor": 100,
            "displayCurrency": USD,
            "reference": "bank-txn-7f3a",
            "reason": "duplicate of the same wire",
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    // A customer session cannot write a manual order — the business
    // capability is refused outside staff context.
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/business/orders",
        Some(json!({
            "organizationId": account.organization_id,
            "idempotencyKey": "manual-3",
            "displayAmountMinor": 100,
            "displayCurrency": USD,
            "reference": "bank-txn-self",
            "reason": "self-credit attempt",
        })),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
}

/// The operator's cross-organization read lists manual and provider orders
/// together, filterable — the same truth the reconciliation report counts.
#[tokio::test]
async fn the_business_order_view_lists_across_customers() {
    let Some((app, db, stub, _m, _v)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.answers.lock().unwrap() = vec![("pay_biz".into(), "secret_biz".into())];
    let owner = operator(&db, &app).await;

    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("ord-biz", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/business/orders",
        Some(json!({
            "organizationId": account.organization_id,
            "idempotencyKey": "manual-biz",
            "displayAmountMinor": 500,
            "displayCurrency": USD,
            "reference": "wire-99",
            "reason": "received",
        })),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = call(&app, "GET", "/api/v1/business/orders", None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let orders = body["data"]["orders"].as_array().unwrap();
    assert!(orders.len() >= 2, "{body}");
    assert!(
        orders
            .iter()
            .any(|o| o["source"].as_str() == Some("manual"))
    );

    let (status, body) = call(
        &app,
        "GET",
        &format!(
            "/api/v1/business/orders?organizationId={}",
            account.organization_id
        ),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let orders = body["data"]["orders"].as_array().unwrap();
    assert!(
        orders
            .iter()
            .all(|o| o["organizationId"].as_str() == Some(&account.organization_id.to_string()))
    );

    // The customer session is refused the operator view entirely.
    let (status, _body) = call(
        &app,
        "GET",
        "/api/v1/business/orders",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A successful external create whose response disappears is recovered by its
/// body ID after restart, without another external create.
#[tokio::test]
async fn a_lost_checkout_response_recovers_the_existing_body_identity() {
    let Some((app, db, stub, _, _)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    *stub.drop_response.lock().unwrap() = true;
    let payload = create_body("lost-response", "walletTopup", 210);
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(payload.clone()),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(stub.stored.lock().unwrap().len(), 1);
    assert_eq!(*stub.calls.lock().unwrap(), 1);
    *stub.drop_response.lock().unwrap() = false;
    *stub.reads_unavailable.lock().unwrap() = false;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(payload),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(*stub.calls.lock().unwrap(), 1);
    let order = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();
    let reopened = Db::from_pool(db.pool().clone());
    let binding = reopened
        .merchant_payment_binding(order)
        .await
        .unwrap()
        .unwrap();
    assert!(binding.hyperswitch_body_id);
    assert_eq!(
        binding.provider_payment_id.as_deref(),
        Some(order.to_string().as_str())
    );
    assert_eq!(body["data"]["order"]["status"], "provider_pending");
    assert!(sqlx::query("UPDATE oxsum.merchant_payment_bindings SET hyperswitch_body_id=false WHERE payment_id=$1")
        .bind(order).execute(db.pool()).await.is_err());
}

/// An old payment without an external handle remains fenced. A known old
/// handle is read-only, including when its provider record is unavailable.
#[tokio::test]
async fn historical_payments_cannot_create_a_new_external_operation() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(create_body("historical-payment", "walletTopup", 210)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();
    let mut binding = db.merchant_payment_binding(id).await.unwrap().unwrap();
    binding.hyperswitch_body_id = false;
    let provider_id = binding.provider_payment_id.take().unwrap();
    let context = db
        .merchant_signing_context(merchant, version, &sealing_key())
        .await
        .unwrap()
        .unwrap();
    let http = oxsum_integrations::provider_http::ProviderHttp::default();
    let urls = oxsum_integrations::merchant_checkout::ReturnUrls::default();
    assert!(
        oxsum_integrations::merchant_checkout::create_order(
            &context,
            &binding,
            "historical",
            &http,
            "https://app.example",
            &urls
        )
        .await
        .is_err()
    );
    binding.provider_payment_id = Some(provider_id.clone());
    let result = oxsum_integrations::merchant_checkout::create_order(
        &context,
        &binding,
        "historical",
        &http,
        "https://app.example",
        &urls,
    )
    .await
    .unwrap();
    assert_eq!(result.provider_payment_id, provider_id);
    let mut value = stub.stored.lock().unwrap().remove(&provider_id).unwrap();
    assert!(
        oxsum_integrations::merchant_checkout::create_order(
            &context,
            &binding,
            "historical",
            &http,
            "https://app.example",
            &urls
        )
        .await
        .is_err()
    );
    value["metadata"]["organization_id"] = json!(Uuid::new_v4());
    stub.stored.lock().unwrap().insert(provider_id, value);
    assert!(
        oxsum_integrations::merchant_checkout::create_order(
            &context,
            &binding,
            "historical",
            &http,
            "https://app.example",
            &urls
        )
        .await
        .is_err()
    );
    assert_eq!(*stub.calls.lock().unwrap(), 1);
}
/// A personal display edit never changes purchased credits, and a stale quote
/// or mismatched currency is refused before the stub sees a provider request.
#[tokio::test]
async fn collection_terms_ignore_display_tampering_and_reject_mismatched_quotes() {
    let Some((app, db, stub, _, _)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    db.set_display_currency(
        account.organization_id,
        Some(DisplayCurrency {
            code: "EUR".into(),
            rate_per_credit: "0.00000001".into(),
            decimals: 6,
        }),
    )
    .await
    .unwrap();
    let (status, terms) = call(
        &app,
        "GET",
        "/api/v1/collection-terms",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(terms["data"]["currency"]["code"], "USD");
    assert_eq!(terms["data"]["currency"]["ratePerCredit"], "0.0014");
    let revision = terms["data"]["revision"].as_i64().unwrap();
    let mut body = create_body("display-cannot-rate", "walletTopup", 210);
    body["expectedCollectionRevision"] = json!(revision);
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(body.clone()),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["order"]["amountMinor"], 1_500_000_000_i64);
    let calls = *stub.calls.lock().unwrap();
    body["idempotencyKey"] = json!("wrong-collection-code");
    body["displayCurrency"] = json!("EUR");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/orders",
            Some(body.clone()),
            Some(&account.cookie)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    body["displayCurrency"] = json!("USD");
    body["expectedCollectionRevision"] = json!(revision + 1);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/orders",
            Some(body),
            Some(&account.cookie)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(*stub.calls.lock().unwrap(), calls);
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business/collection-terms",
            None,
            Some(&account.cookie)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let owner = operator(&db, &app).await;
    let (status, read) = call(
        &app,
        "GET",
        "/api/v1/business/collection-terms",
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    let update = json!({"idempotencyKey":"collection-http","reason":"owner fixture change","expectedRevision":revision,"currency":{"code":"USD","ratePerCredit":"1"}});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/collection-terms",
            Some(update.clone()),
            Some(&account.cookie)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, saved) = call(
        &app,
        "POST",
        "/api/v1/business/collection-terms",
        Some(update.clone()),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let replay = call(
        &app,
        "POST",
        "/api/v1/business/collection-terms",
        Some(update),
        Some(&owner),
    )
    .await;
    assert_eq!(replay.1, saved);
    let original = create_body("display-cannot-rate", "walletTopup", 210);
    let (status, replayed) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(original),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replayed["data"]["order"]["amountMinor"], 1_500_000_000_i64);
}

#[tokio::test]
async fn published_bundle_endpoints_reject_customer_grants_before_provider_io() {
    let Some((app, db, stub, _, _)) = world().await else {
        return;
    };
    let customer = register(&app).await;
    let owner = operator(&db, &app).await;
    let publish = json!({"idempotencyKey":"bundle-publish-http","reason":"bundle HTTP fixture","bundleId":null,"expectedVersion":0,"terms":{"name":"Local bundle","displayCurrency":"USD","displayAmountMinor":500,"purchasedMinor":5_000_000,"promoMinor":1_000_000,"promoLifetimeDays":30}});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/bundles",
            Some(publish.clone()),
            Some(&customer.cookie)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/business/bundles",
        Some(publish.clone()),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/bundles",
            Some(publish),
            Some(&owner)
        )
        .await
        .1,
        body
    );
    let id = body["data"]["offer"]["bundleVersionId"].as_str().unwrap();
    let (status, catalog) =
        call(&app, "GET", "/api/v1/bundles", None, Some(&customer.cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        call(&app, "GET", "/api/v1/business/bundles", None, Some(&owner))
            .await
            .0,
        StatusCode::OK
    );
    let base = json!({"idempotencyKey":"bundle-http-order","rail":"hyperswitch","purpose":"bundle","purposeRef":id,"displayCurrency":"USD","displayAmountMinor":500});
    let mut bad = base.clone();
    bad["promoMinor"] = json!(9_000_000);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/orders",
            Some(bad),
            Some(&customer.cookie)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(*stub.calls.lock().unwrap(), 0);
    let (status, order) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(base.clone()),
        Some(&customer.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(order["data"]["order"]["amountMinor"], 5_000_000);
    assert_eq!(order["data"]["order"]["promoMinor"], 1_000_000);
    let retire = json!({"idempotencyKey":"bundle-retire-http","reason":"stop new sales"});
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/v1/business/bundles/{id}/retire"),
            Some(retire),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/orders",
            Some(base.clone()),
            Some(&customer.cookie)
        )
        .await
        .1,
        order
    );
    assert_eq!(*stub.calls.lock().unwrap(), 1);
    let mut new = base;
    new["idempotencyKey"] = json!("bundle-new-retired");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/orders",
            Some(new),
            Some(&customer.cookie)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(*stub.calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn a_legacy_pending_bundle_cannot_resume_checkout_or_fulfill() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let customer = register(&app).await;
    let id = Uuid::new_v4();
    // Reproduce an already migrated legacy row. The separate core upgrade test
    // applies the real migration to old rows; here only the new-insert guard is
    // suspended inside this isolated test transaction to exercise the HTTP fence.
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("ALTER TABLE oxsum.payment_orders DISABLE TRIGGER payment_order_bundle_valid")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO oxsum.payment_orders(order_id,business_id,merchant_id,merchant_version_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,promo_minor,idempotency_key,status,checkout) VALUES($1,oxsum.default_business_id(),$2,$3,$4,'bundle',5000000,'USD','USD',500,1,0,1000000,'legacy-http','created','{\"clientSecret\":\"legacy-secret\"}')")
        .bind(id).bind(merchant).bind(version).bind(customer.organization_id).execute(&mut *tx).await.unwrap();
    sqlx::query("ALTER TABLE oxsum.payment_orders ENABLE TRIGGER payment_order_bundle_valid")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    for status in ["created", "provider_pending", "paid"] {
        sqlx::query("UPDATE oxsum.payment_orders SET status=$2 WHERE order_id=$1")
            .bind(id)
            .bind(status)
            .execute(db.pool())
            .await
            .unwrap();
        let (status, body) = call(
            &app,
            "GET",
            &format!("/api/v1/orders/{id}/checkout-session"),
            None,
            Some(&customer.cookie),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["data"]["clientSecret"].is_null());
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/api/v1/orders/{id}"),
                None,
                Some(&customer.cookie)
            )
            .await
            .0,
            StatusCode::OK,
            "history remains readable"
        );
    }
    let wallet = Tenants::new(db.pool().clone())
        .get(&tenant(&db, customer.organization_id).await)
        .await
        .unwrap();
    assert!(
        db.fulfill_payment_order(id, &wallet, time::OffsetDateTime::now_utc().date())
            .await
            .is_err()
    );
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(*stub.calls.lock().unwrap(), 0);
}

/// The customer pays the reviewed plan quote; forged values never reach the PSP.
#[tokio::test]
async fn a_plan_quote_drives_one_checkout_and_a_signed_payment_grants_only_its_period() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let customer = register(&app).await;
    db.bootstrap_collection_terms(&oxsum_core::CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: "HTTP exact paid plan".into(),
                price_minor: 1_000_001,
                currency: "USD".into(),
                quota_minor: Some(3_000_000),
                period_kind: oxsum_core::PeriodKind::CalendarMonth,
                overage_enabled: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    let plan = db
        .bind_customer_plan(
            customer.organization_id,
            sold.plan_version_id,
            oxsum_core::PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    let (status, quote) = call(
        &app,
        "GET",
        "/api/v1/org/plan/payment-quote",
        None,
        Some(&customer.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    let request = json!({
        "idempotencyKey":"http-plan-quoted","purpose":"planPeriod","rail":"hyperswitch",
        "purposeRef":plan.customer_plan_id,"expectedPlanQuote":quote["data"]["fingerprint"],
        "expectedCollectionRevision":quote["data"]["terms"]["collectionRevision"],
        "displayAmountMinor":quote["data"]["terms"]["displayAmountMinor"],"displayCurrency":"USD",
    });
    assert_eq!(request["displayAmountMinor"], 101);
    for field in ["displayAmountMinor", "purposeRef", "expectedPlanQuote"] {
        let mut forged = request.clone();
        forged[field] = match field {
            "displayAmountMinor" => json!(1),
            "purposeRef" => json!(Uuid::new_v4()),
            _ => json!("0".repeat(64)),
        };
        let (status, _) = call(
            &app,
            "POST",
            "/api/v1/orders",
            Some(forged),
            Some(&customer.cookie),
        )
        .await;
        assert!(status == StatusCode::CONFLICT || status == StatusCode::NOT_FOUND);
    }
    assert_eq!(
        *stub.calls.lock().unwrap(),
        0,
        "invalid terms fail before provider I/O"
    );
    let (status, result) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(request.clone()),
        Some(&customer.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let order = Uuid::parse_str(result["data"]["order"]["orderId"].as_str().unwrap()).unwrap();
    assert_eq!(result["data"]["order"]["amountMinor"], 1_000_001);
    assert_eq!(result["data"]["order"]["planPayment"]["periodIndex"], 0);
    let (status, replay) = call(
        &app,
        "POST",
        "/api/v1/orders",
        Some(request.clone()),
        Some(&customer.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["data"]["order"]["orderId"], order.to_string());
    let mut duplicate = request;
    duplicate["idempotencyKey"] = json!("second-intent-for-same-period");
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/orders",
            Some(duplicate),
            Some(&customer.cookie)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(*stub.calls.lock().unwrap(), 1);
    let (status, recover) = call(
        &app,
        "GET",
        "/api/v1/org/plan/payment-quote",
        None,
        Some(&customer.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recover["data"]["existingOrderId"], order.to_string());
    let payload = succeeded(order, customer.organization_id, "pay_armed", 101);
    assert_eq!(
        post_webhook(
            &app,
            merchant,
            version,
            signed(WEBHOOK_SECRET, &payload),
            payload.clone()
        )
        .await,
        StatusCode::OK
    );
    run_reconcile(&db).await;
    let periods = db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].authorization_ref, order);
    let wallet = Tenants::new(db.pool().clone())
        .get(&tenant(&db, customer.organization_id).await)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(
        post_webhook(
            &app,
            merchant,
            version,
            signed(WEBHOOK_SECRET, &payload),
            payload.clone()
        )
        .await,
        StatusCode::OK
    );
    run_reconcile(&db).await;
    assert_eq!(
        db.plan_periods(plan.customer_plan_id).await.unwrap().len(),
        1
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/org/plan/payment-quote",
            None,
            Some(&customer.cookie)
        )
        .await
        .0,
        StatusCode::CONFLICT,
        "early renewal is not a new checkout"
    );
}

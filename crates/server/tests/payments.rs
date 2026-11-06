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
/// armed — each successive payment id dequeues from `answers` so a test can
/// drive several orders through one stub.
#[derive(Clone, Default)]
struct SwitchStub {
    orders: Arc<Mutex<Vec<String>>>,
    answers: Arc<Mutex<Vec<(String, String)>>>,
    calls: Arc<Mutex<usize>>,
}

async fn stub_switch() -> (String, SwitchStub) {
    let stub = SwitchStub::default();
    let app = Router::new()
        .route(
            "/payments",
            post(
                |State(stub): State<SwitchStub>, body: axum::Json<Value>| async move {
                    *stub.calls.lock().unwrap() += 1;
                    let order = body["metadata"]["order_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    stub.orders.lock().unwrap().push(order);
                    let (id, secret) = stub
                        .answers
                        .lock()
                        .unwrap()
                        .first()
                        .cloned()
                        .unwrap_or_else(|| ("pay_armed".into(), "secret_armed".into()));
                    axum::Json(json!({
                        "payment_id": id,
                        "client_secret": secret,
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
fn succeeded(order: Uuid, org: Uuid, provider: &str, amount: i64) -> String {
    json!({
        "merchant_id": ACCOUNT,
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "payment_succeeded",
        "livemode": false,
        "content": {"type":"payment_details","object":{
            "payment_id": provider,
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
async fn a_paid_order_fulfills_once_through_the_sweep() {
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
        "pay_ord_1"
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

    // The sweep drains the paid-unfulfilled backlog: one grant under the
    // order-derived ledger key.
    run_reconcile(&db).await;
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

/// Enqueue and run the reconcile pass — the sweep `recover` rides on.
async fn run_reconcile(db: &Db) {
    // Same due-row insert the jobs suite uses: `run_at` is server-side now.
    sqlx::query(
        "INSERT INTO oxsum.jobs (kind, run_at) VALUES ($1, now() - interval '1 second') \
         ON CONFLICT DO NOTHING",
    )
    .bind(kinds::RECONCILE)
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
        "pay_ord_2"
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

    // A second order the provider answers with the same payment id: binding it
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
    assert_eq!(status, StatusCode::CONFLICT);
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

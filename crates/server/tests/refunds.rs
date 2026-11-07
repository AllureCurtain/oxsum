//! Corrections, partial refunds and disputes over HTTP (issue #608): the
//! customer's request is a review, never a self-grant — staff resolves under
//! `RefundsWrite`. Approve commits the durable `provider_pending` reservation
//! and the issued credit note *before* the PSP call; the signed `refund_*`/
//! `dispute_*` webhook is the only terminal truth. No path — concurrent
//! consume, spent funds, a duplicate or reordered event, a provider timeout —
//! can drive a wallet negative or grant money the order never funded.
//!
//! The stub stands where `sandbox.hyperswitch.io` would be; `/payments` answers
//! checkout handles, `/refunds` the refund issue. Needs DATABASE_URL; skips
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

const WEBHOOK_SECRET: &str = "order-whsec-refunds-1";
const ACCOUNT: &str = "acct_refunds_test";
const PASSWORD: &str = "correct horse battery";
const USD: &str = "USD";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("refunds_http")
}

fn sealing_key() -> SecretKey {
    SecretKey::from_bytes([9; 32])
}

/// The switch API stub: `POST /payments` records the order id it was told to
/// carry in `metadata` and answers the armed payment id; `POST /refunds`
/// answers the armed refund id. `refund_calls` counts PSP refund issues so a
/// test can prove the reservation committed before the provider was asked.
#[derive(Clone, Default)]
struct SwitchStub {
    orders: Arc<Mutex<Vec<String>>>,
    answers: Arc<Mutex<Vec<(String, String)>>>,
    refund_answers: Arc<Mutex<Vec<String>>>,
    refund_calls: Arc<Mutex<usize>>,
    calls: Arc<Mutex<usize>>,
    stored: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    stored_refunds: Arc<Mutex<std::collections::HashMap<String, Value>>>,
    drop_refund_response: Arc<Mutex<bool>>,
    refund_read_unavailable: Arc<Mutex<bool>>,
    refuse_refund: Arc<Mutex<bool>>,
    posts_after_store: Arc<Mutex<usize>>,
    mismatched_refund: Arc<Mutex<bool>>,
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
                    let (_alias, secret) = stub
                        .answers
                        .lock()
                        .unwrap()
                        .first()
                        .cloned()
                        .unwrap_or_else(|| ("pay_armed".into(), "secret_armed".into()));
                    let id = body["payment_id"].as_str().unwrap().to_owned();
                    let value = json!({"payment_id":id,"client_secret":secret,"status":"requires_payment_method",
                        "merchant_id":ACCOUNT,"amount":body["amount"],"currency":body["currency"],"metadata":body["metadata"]});
                    stub.stored.lock().unwrap().insert(id, value.clone());
                    axum::Json(value)
                },
            ),
        )
        .route(
            "/refunds",
            post(
                |State(stub): State<SwitchStub>, body: axum::Json<Value>| async move {
                    *stub.refund_calls.lock().unwrap() += 1;
                    if *stub.refuse_refund.lock().unwrap() { return (StatusCode::BAD_REQUEST, axum::Json(json!({"error":{"code":"IR_13"}}))); }
                    if stub.refund_answers.lock().unwrap().is_empty() { return (StatusCode::OK, axum::Json(json!({}))); }
                    let id = body["refund_id"].as_str().unwrap().to_owned();
                    let mut value = json!({"refund_id":id,"payment_id":body["payment_id"],"status":"pending",
                        "merchant_id":ACCOUNT,"amount":body["amount"],"currency":"USD","metadata":body["metadata"]});
                    let mut stored = stub.stored_refunds.lock().unwrap();
                    if stored.contains_key(&id) {
                        *stub.posts_after_store.lock().unwrap() += 1;
                        return (StatusCode::BAD_REQUEST, axum::Json(json!({"error":{"code":"HE_01"}})));
                    }
                    stored.insert(id, value.clone());
                    if *stub.drop_refund_response.lock().unwrap() {
                        *stub.refund_read_unavailable.lock().unwrap() = true;
                        return (StatusCode::BAD_GATEWAY, axum::Json(json!({"error":"response lost after commit"})));
                    }
                    if *stub.mismatched_refund.lock().unwrap() { value["amount"]=json!(99999); }
                    (StatusCode::OK, axum::Json(value))
                },
            ),
        )
        .route("/payments/{id}", axum::routing::get(|State(stub):State<SwitchStub>, axum::extract::Path(id):axum::extract::Path<String>| async move {
            let value = stub.stored.lock().unwrap().get(&id).cloned();
            value.map_or((StatusCode::NOT_FOUND, axum::Json(json!({"error":{"code":"HE_02"}}))), |v| (StatusCode::OK,axum::Json(v)))
        }))
        .route("/refunds/{id}", axum::routing::get(|State(stub):State<SwitchStub>, axum::extract::Path(id):axum::extract::Path<String>| async move {
            if *stub.refund_read_unavailable.lock().unwrap() { return (StatusCode::SERVICE_UNAVAILABLE,axum::Json(json!({}))); }
            let value = stub.stored_refunds.lock().unwrap().get(&id).cloned();
            value.map_or((StatusCode::NOT_FOUND, axum::Json(json!({"error":{"code":"HE_02"}}))), |v| (StatusCode::OK,axum::Json(v)))
        }))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), stub)
}

/// A configured world: the switch stub, the merchant bootstrap, the prepared
/// deployment, and the `(merchant_id, version_id)` the webhook route names.
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

/// A Hyperswitch `payment_succeeded` event naming the pinned order.
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

/// A Hyperswitch `refund_succeeded`/`refund_failed` event: `refund_id` names the
/// provider's refund handle (what `note_refund_provider` stored), `payment_id`
/// the order's provider payment — `metadata.payment_id` the pinned local order.
fn refund(
    kind: &str,
    order: Uuid,
    org: Uuid,
    provider: &str,
    refund_id: Option<&str>,
    amount: i64,
) -> String {
    json!({
        "merchant_id": ACCOUNT,
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": kind,
        "livemode": false,
        "content": {"type":"refund_details","object":{
            "refund_id": refund_id,
            "payment_id": provider,
            "amount": amount,
            "currency": USD,
            "metadata": {"payment_id": order, "organization_id": org}
        }}
    })
    .to_string()
}

/// A Hyperswitch `dispute_opened` event on the order.
fn dispute(order: Uuid, org: Uuid, provider: &str, dispute_id: &str, amount: i64) -> String {
    json!({
        "merchant_id": ACCOUNT,
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "dispute_opened",
        "livemode": false,
        "content": {"type":"dispute_details","object":{
            "dispute_id": dispute_id,
            "refund_id": dispute_id,
            "payment_id": provider,
            "amount": amount,
            "currency": USD,
            "metadata": {"payment_id": order, "organization_id": org}
        }}
    })
    .to_string()
}

struct Account {
    organization_id: Uuid,
    cookie: String,
}

/// A registered customer and its personal session.
async fn register(app: &Router) -> Account {
    let email = format!(
        "refunds_{}@example.com",
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

/// The organization's display rate: `0.0014` USD per credit, so $2.10 rates
/// at 1_500_000_000 credit-minor.
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

/// A business-context session for finance reads and `RefundsWrite` resolves.
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

async fn tenant(db: &Db, org: Uuid) -> String {
    sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
        .bind(org)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Enqueue and run the reconcile pass that fulfills `paid` orders.
async fn run_reconcile(db: &Db) {
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

/// Enqueue and run the refund-retry pass — the dispatch needs the sealing
/// secret the `webhook` pair carries to unseal the merchant credentials its
/// PSP calls sign under.
async fn run_refund_retries(db: &Db) {
    sqlx::query(
        "INSERT INTO oxsum.jobs (kind, run_at) VALUES ($1, now() - interval '1 second') \
         ON CONFLICT DO NOTHING",
    )
    .bind(kinds::REFUND_RETRIES)
    .execute(db.pool())
    .await
    .unwrap();
    let mut ctx = ctx();
    ctx.webhook = Some((reqwest::Client::new(), sealing_key()));
    run_due(db, &ctx).await;
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

fn create_body(key: &str, purpose: &str, display_minor: i64) -> Value {
    json!({
        "idempotencyKey": key,
        "rail": "hyperswitch",
        "purpose": purpose,
        "displayAmountMinor": display_minor,
        "displayCurrency": USD,
    })
}

/// A paid+fulfilled wallet_topup order end-to-end: the create, the signed
/// `payment_succeeded`, and the reconcile sweep's grant. Returns
/// `(order_id, provider_payment_id, wallet)`.
#[allow(clippy::too_many_arguments)]
async fn paid_order(
    app: &Router,
    db: &Db,
    stub: &SwitchStub,
    merchant: Uuid,
    version: Uuid,
    account: &Account,
    key: &str,
    display_minor: i64,
) -> (Uuid, String, Arc<oxsum_core::Wallet>) {
    *stub.answers.lock().unwrap() = vec![(format!("pay_{key}"), format!("secret_{key}"))];
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/orders",
        Some(create_body(key, "walletTopup", display_minor)),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let order_id = Uuid::parse_str(body["data"]["order"]["orderId"].as_str().unwrap()).unwrap();
    let provider = body["data"]["providerPaymentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let body_text = succeeded(order_id, account.organization_id, &provider, display_minor);
    let status = post_webhook(
        app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    run_reconcile(db).await;
    let wallet = Tenants::new(db.pool().clone())
        .get(&tenant(db, account.organization_id).await)
        .await
        .unwrap();
    (order_id, provider, wallet)
}

/// The customer asks for review, staff approves, the PSP is called once under
/// the committed reservation, and the signed `refund_succeeded` books the
/// take-back — the wallet shrinks by exactly the refunded credit, the note
/// applies, and a duplicate success books once.
#[tokio::test]
async fn a_refund_is_reviewed_then_the_signed_event_settles() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    let (order_id, provider, wallet) =
        paid_order(&app, &db, &stub, merchant, version, &account, "r-1", 210).await;
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    // The request is a review — `requested`, no money moved.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/orders/{order_id}/refund-request"),
        Some(json!({"idempotencyKey":"req-1","reason":"customer refund"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refund_id = Uuid::parse_str(body["data"]["refundId"].as_str().unwrap()).unwrap();
    assert_eq!(body["data"]["state"].as_str().unwrap(), "requested");
    assert_eq!(body["data"]["amountMinor"].as_i64().unwrap(), 1_500_000_000);
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    // The customer's own session can never grant the refund — the resolve is a
    // staff-only `RefundsWrite` surface.
    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/v1/business/refunds/{refund_id}/resolve"),
        Some(json!({"idempotencyKey":"res-1","decision":"approve"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Staff approves: the reservation commits `provider_pending` and the PSP is
    // asked exactly once under it.
    *stub.refund_answers.lock().unwrap() = vec!["re_1".into()];
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/business/refunds/{refund_id}/resolve"),
        Some(json!({"idempotencyKey":"res-1","decision":"approve","reason":"goodwill"})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "provider_pending");
    assert_eq!(*stub.refund_calls.lock().unwrap(), 1);

    // A replayed resolve answers the standing reservation — no second PSP call.
    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/v1/business/refunds/{refund_id}/resolve"),
        Some(json!({"idempotencyKey":"res-1","decision":"approve"})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(*stub.refund_calls.lock().unwrap(), 1);

    // The signed `refund_succeeded` books the take-back.
    let body_text = refund(
        "refund_succeeded",
        order_id,
        account.organization_id,
        &provider,
        Some(&refund_id.to_string()),
        210,
    );
    let status = post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(wallet.available().await.unwrap(), 0);

    // The finance read shows the succeeded refund and its applied note.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/business/refunds/{refund_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["state"].as_str().unwrap(), "succeeded");
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/business/credit-notes",
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let note = body["data"]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["refundId"].as_str().unwrap() == refund_id.to_string())
        .unwrap();
    assert_eq!(note["state"].as_str().unwrap(), "applied");
    assert!(note["entryId"].is_string(), "{note}");

    // A duplicate success event answers the standing state — booked once.
    let body_text = refund(
        "refund_succeeded",
        order_id,
        account.organization_id,
        &provider,
        Some(&refund_id.to_string()),
        210,
    );
    post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// A refund the customer already spent cannot book its take-back: the signed
/// success is fact — the refund lands `succeeded` — but the ledger refuses the
/// debit it cannot fund, so the wallet never goes negative and the audit
/// records the compensation the take-back owes.
#[tokio::test]
async fn a_refund_on_spent_credit_never_goes_negative() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    let (order_id, provider, wallet) =
        paid_order(&app, &db, &stub, merchant, version, &account, "r-2", 210).await;
    // The customer spends the funded credit before the refund lands.
    let tenant = tenant(&db, account.organization_id).await;
    let w = Tenants::new(db.pool().clone()).get(&tenant).await.unwrap();
    w.hold(
        "spend-h",
        "",
        1_500_000_000,
        time::OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    w.settle(
        "spend-h",
        "",
        1_500_000_000,
        time::OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);

    // Request + approve the full refund; the PSP is asked and answers success.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/orders/{order_id}/refund-request"),
        Some(json!({"idempotencyKey":"req-2","reason":"refund spent credit"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refund_id = Uuid::parse_str(body["data"]["refundId"].as_str().unwrap()).unwrap();
    *stub.refund_answers.lock().unwrap() = vec!["re_2".into()];
    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/v1/business/refunds/{refund_id}/resolve"),
        Some(json!({"idempotencyKey":"res-2","decision":"approve"})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The signed success is fact — the refund lands `succeeded` — but the
    // take-back refuses at the ledger: the wallet stays at zero, never
    // negative, and the note stays `issued` (uncompensated).
    let body_text = refund(
        "refund_succeeded",
        order_id,
        account.organization_id,
        &provider,
        Some(&refund_id.to_string()),
        210,
    );
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
        &format!("/api/v1/business/refunds/{refund_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["state"].as_str().unwrap(), "succeeded");
    // No silent negative: the purchased pool never dips below zero.
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert!(wallet.settled().await.unwrap() >= 0);
}

/// Two partial refunds convert pro-rata at the pinned rate and never exceed
/// the funded amount — the request over the remaining bound is refused.
#[tokio::test]
async fn partial_refunds_sum_to_the_funded_total() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    let (order_id, provider, wallet) =
        paid_order(&app, &db, &stub, merchant, version, &account, "r-3", 210).await;
    let total = wallet.available().await.unwrap();
    let half = total / 2;

    for (key, amount) in [("req-3a", half), ("req-3b", total - half)] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/v1/orders/{order_id}/refund-request"),
            Some(json!({"idempotencyKey":key,"amountMinor":amount,"reason":"partial"})),
            Some(&account.cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let refund_id = body["data"]["refundId"].as_str().unwrap().to_owned();
        *stub.refund_answers.lock().unwrap() = vec![format!("re_{key}")];
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/v1/business/refunds/{refund_id}/resolve"),
            Some(json!({"idempotencyKey":format!("res-{key}"),"decision":"approve"})),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let display = body["data"]["displayAmountMinor"].as_i64().unwrap();
        let body_text = refund(
            "refund_succeeded",
            order_id,
            account.organization_id,
            &provider,
            Some(&refund_id),
            display,
        );
        post_webhook(
            &app,
            merchant,
            version,
            signed(WEBHOOK_SECRET, &body_text),
            body_text,
        )
        .await;
    }
    // The two take-backs drained exactly the funded credit.
    assert_eq!(wallet.available().await.unwrap(), 0);

    // A refund beyond the funded total is refused outright — the bound held.
    let (status, _body) = call(
        &app,
        "POST",
        &format!("/api/v1/orders/{order_id}/refund-request"),
        Some(json!({"idempotencyKey":"req-3c","amountMinor":1,"reason":"over"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A provider timeout leaves the durable reservation for the retry pass — the
/// bounded sweep re-issues it, and a later success still settles, never a lost
/// refund and never a negative wallet.
#[tokio::test]
async fn a_provider_timeout_retries_then_settles() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    let (order_id, provider, wallet) =
        paid_order(&app, &db, &stub, merchant, version, &account, "r-4", 210).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/orders/{order_id}/refund-request"),
        Some(json!({"idempotencyKey":"req-4","amountMinor":500_000_000,"reason":"retry me"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refund_id = Uuid::parse_str(body["data"]["refundId"].as_str().unwrap()).unwrap();

    // The stub has no refund answer armed: the create_refund call's missing
    // `refund_id` field is a provider failure — the row stays `provider_pending`
    // for the retry pass rather than dropping the refund.
    *stub.refund_answers.lock().unwrap() = vec![];
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/business/refunds/{refund_id}/resolve"),
        Some(json!({"idempotencyKey":"res-4","decision":"approve"})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "provider_pending");

    // The failed provider call rescheduled the retry backoff; bring it due so
    // the sweep's work list takes it.
    sqlx::query(
        "UPDATE oxsum.refunds SET next_retry_at=now() - interval '1 second' WHERE refund_id=$1",
    )
    .bind(refund_id)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(db.due_refund_retries(10).await.unwrap().len(), 1);

    // Arm the provider's answer and run the retry pass — it re-issues the
    // reservation's PSP call, recovering the lost refund.
    *stub.refund_answers.lock().unwrap() = vec!["re_4".into()];
    run_refund_retries(&db).await;
    assert_eq!(*stub.refund_calls.lock().unwrap(), 2);

    // The late success still settles — the reservation was never lost.
    let body_text = refund(
        "refund_succeeded",
        order_id,
        account.organization_id,
        &provider,
        Some(&refund_id.to_string()),
        70,
    );
    post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    assert_eq!(wallet.available().await.unwrap(), 1_000_000_000);
}

/// A `dispute_opened` event records the contested charge — deliberately not a
/// refund — and `lost` re-charges the drawn receivable under the dispute's own
/// ledger key; `won` ends the record without moving money.
#[tokio::test]
async fn a_dispute_is_not_a_refund_and_lost_recharges() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    let (order_id, provider, wallet) =
        paid_order(&app, &db, &stub, merchant, version, &account, "r-5", 210).await;
    let on = time::OffsetDateTime::now_utc().date();
    // Draw the credit line so `lost` has a receivable to restore.
    wallet
        .set_credit_limit("d-line", 2_000_000_000, on)
        .await
        .unwrap();
    wallet.hold("d-hold", "", 2_000_000_000, on).await.unwrap();
    wallet
        .settle("d-hold", "", 2_000_000_000, on)
        .await
        .unwrap();
    assert_eq!(wallet.credit_used().await.unwrap(), 500_000_000);

    // The signed dispute event opens the contested record — no money moves.
    let body_text = dispute(order_id, account.organization_id, &provider, "dsp_5", 210);
    let status = post_webhook(
        &app,
        merchant,
        version,
        signed(WEBHOOK_SECRET, &body_text),
        body_text,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(&app, "GET", "/api/v1/business/disputes", None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let dispute_id = Uuid::parse_str(
        body["data"]["disputes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["orderId"].as_str().unwrap() == order_id.to_string())
            .unwrap()["disputeId"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(wallet.credit_used().await.unwrap(), 500_000_000);

    // `lost` re-charges the drawn receivable — capped at the 500_000_000 draw.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/business/disputes/{dispute_id}/resolve"),
        Some(json!({"idempotencyKey":"disp-5","resolution":"lost","reason":"chargeback upheld"})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "lost");
    assert!(body["data"]["rechargeEntryId"].is_string(), "{body}");
    // The contested 1_500_000_000 credit is capped at the 500_000_000 drawn —
    // the receivable restored in whole.
    assert_eq!(wallet.credit_used().await.unwrap(), 0);
}

/// Losing the post-commit response cannot create a second partial refund.
/// The next worker retrieves the retained body ID, and duplicate events apply once.
#[tokio::test]
async fn a_lost_refund_response_retrieves_without_reissuing() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    let (order, provider, wallet) = paid_order(
        &app,
        &db,
        &stub,
        merchant,
        version,
        &account,
        "lost-refund",
        210,
    )
    .await;
    let (status,body)=call(&app,"POST",&format!("/api/v1/orders/{order}/refund-request"),
        Some(json!({"idempotencyKey":"lost-refund","amountMinor":500_000_000,"reason":"lost response"})),Some(&account.cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refund = body["data"]["refundId"].as_str().unwrap().to_owned();
    *stub.refund_answers.lock().unwrap() = vec!["ready".into()];
    *stub.drop_refund_response.lock().unwrap() = true;
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/business/refunds/{refund}/resolve"),
        Some(json!({"idempotencyKey":"approve-lost","decision":"approve"})),
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(*stub.refund_calls.lock().unwrap(), 1);
    assert_eq!(stub.stored_refunds.lock().unwrap().len(), 1);
    let id = Uuid::parse_str(&refund).unwrap();
    assert!(
        db.refund_retry_target(id)
            .await
            .unwrap()
            .unwrap()
            .refund
            .provider_refund_id
            .is_none()
    );
    *stub.drop_refund_response.lock().unwrap() = false;
    *stub.refund_read_unavailable.lock().unwrap() = false;
    sqlx::query(
        "UPDATE oxsum.refunds SET next_retry_at=now()-interval '1 second' WHERE refund_id=$1",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    run_refund_retries(&Db::from_pool(db.pool().clone())).await;
    assert_eq!(*stub.refund_calls.lock().unwrap(), 1);
    assert_eq!(*stub.posts_after_store.lock().unwrap(), 0);
    let standing = db.refund_retry_target(id).await.unwrap().unwrap().refund;
    assert_eq!(
        standing.provider_refund_id.as_deref(),
        Some(refund.as_str())
    );
    assert_eq!(standing.retry_count, 2);
    let raw = refund_event_body(order, account.organization_id, &provider, &refund, 70);
    for _ in 0..2 {
        assert_eq!(
            post_webhook(
                &app,
                merchant,
                version,
                signed(WEBHOOK_SECRET, &raw),
                raw.clone()
            )
            .await,
            StatusCode::OK
        );
    }
    assert_eq!(wallet.available().await.unwrap(), 1_000_000_000);
    assert_eq!(
        db.refund_retry_target(id)
            .await
            .unwrap()
            .unwrap()
            .refund
            .state,
        "succeeded"
    );
}

fn refund_event_body(
    order: Uuid,
    org: Uuid,
    provider: &str,
    refund_id: &str,
    amount: i64,
) -> String {
    refund(
        "refund_succeeded",
        order,
        org,
        provider,
        Some(refund_id),
        amount,
    )
}

/// A pinned pre-insert refusal is safe to release on the first attempt;
/// a contradictory success response remains reserved for reconciliation.
#[tokio::test]
async fn first_refusal_and_mismatched_success_have_distinct_reservation_outcomes() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    for refused in [true, false] {
        let (order, _, wallet) = paid_order(
            &app,
            &db,
            &stub,
            merchant,
            version,
            &account,
            if refused { "refused" } else { "mismatch" },
            210,
        )
        .await;
        let (_,body)=call(&app,"POST",&format!("/api/v1/orders/{order}/refund-request"),
            Some(json!({"idempotencyKey":format!("outcome-{refused}"),"amountMinor":500_000_000,"reason":"outcome test"})),Some(&account.cookie)).await;
        let refund = body["data"]["refundId"].as_str().unwrap();
        *stub.refund_answers.lock().unwrap() = vec!["ready".into()];
        *stub.refuse_refund.lock().unwrap() = refused;
        *stub.mismatched_refund.lock().unwrap() = !refused;
        let (status, _) = call(
            &app,
            "POST",
            &format!("/api/v1/business/refunds/{refund}/resolve"),
            Some(json!({"idempotencyKey":format!("outcome-{refused}"),"decision":"approve"})),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let standing = db
            .refund_retry_target(Uuid::parse_str(refund).unwrap())
            .await
            .unwrap()
            .unwrap()
            .refund;
        assert_eq!(
            standing.state,
            if refused {
                "released"
            } else {
                "provider_pending"
            }
        );
        assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
    }
}

/// A historical operation with a known handle may only be retrieved; an
/// unbound legacy outcome never gets a new create ID during an upgrade.
#[tokio::test]
async fn historical_refund_handles_are_read_only_and_unknowns_are_fenced() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    let (order, provider, _) =
        paid_order(&app, &db, &stub, merchant, version, &account, "legacy", 210).await;
    let (_, body) = call(
        &app,
        "POST",
        &format!("/api/v1/orders/{order}/refund-request"),
        Some(
            json!({"idempotencyKey":"legacy","amountMinor":500_000_000,"reason":"legacy fixture"}),
        ),
        Some(&account.cookie),
    )
    .await;
    let id = Uuid::parse_str(body["data"]["refundId"].as_str().unwrap()).unwrap();
    let mut refund = db.refund_retry_target(id).await.unwrap().unwrap().refund;
    // Construct the in-memory view produced by migration's false default.
    refund.hyperswitch_body_id = false;
    let context = db
        .merchant_signing_context(merchant, version, &sealing_key())
        .await
        .unwrap()
        .unwrap();
    let http = oxsum_integrations::provider_http::ProviderHttp::default();
    assert!(
        oxsum_integrations::merchant_checkout::create_refund(&context, &refund, &provider, &http)
            .await
            .is_err()
    );
    assert_eq!(*stub.refund_calls.lock().unwrap(), 0);
    let legacy = "ref_pre_upgrade";
    refund.provider_refund_id = Some(legacy.into());
    stub.stored_refunds.lock().unwrap().insert(legacy.into(),json!({
        "refund_id":legacy,"payment_id":provider,"amount":refund.display_amount_minor,"currency":refund.display_currency,
        "status":"pending","metadata":{"refund_id":id.to_string(),"order_id":id.to_string()},
    }));
    let handle =
        oxsum_integrations::merchant_checkout::create_refund(&context, &refund, &provider, &http)
            .await
            .unwrap();
    assert_eq!(handle.provider_refund_id, legacy);
    assert_eq!(*stub.refund_calls.lock().unwrap(), 0);
    stub.stored_refunds.lock().unwrap().remove(legacy);
    assert!(
        oxsum_integrations::merchant_checkout::create_refund(&context, &refund, &provider, &http)
            .await
            .is_err()
    );
    assert_eq!(*stub.refund_calls.lock().unwrap(), 0);
    // A malformed or wrongly scoped read cannot become a successful handle.
    stub.stored_refunds.lock().unwrap().insert(
        legacy.into(),
        json!({
            "refund_id":legacy,"payment_id":"other_payment","amount":refund.display_amount_minor,
            "currency":refund.display_currency,"status":"succeeded",
        }),
    );
    assert!(
        oxsum_integrations::merchant_checkout::create_refund(&context, &refund, &provider, &http)
            .await
            .is_err()
    );
    assert_eq!(*stub.refund_calls.lock().unwrap(), 0);
    assert_eq!(
        db.business_refund(&owner, id).await.unwrap().unwrap().state,
        "requested"
    );
}

/// The actual request timeout is bounded; a switch that commits before hanging
/// is recovered by GET while the local reservation remains provider_pending.
#[tokio::test]
async fn a_refund_response_timeout_recovers_without_a_second_create() {
    let Some((app, db, stub, merchant, version)) = world().await else {
        return;
    };
    let account = register(&app).await;
    set_display(&db, account.organization_id).await;
    let owner = operator(&db, &app).await;
    let (order, provider, _) = paid_order(
        &app,
        &db,
        &stub,
        merchant,
        version,
        &account,
        "hanging-refund",
        210,
    )
    .await;
    let (_,body) = call(&app,"POST",&format!("/api/v1/orders/{order}/refund-request"),
        Some(json!({"idempotencyKey":"timeout","amountMinor":500_000_000,"reason":"timeout fixture"})),Some(&account.cookie)).await;
    let id = Uuid::parse_str(body["data"]["refundId"].as_str().unwrap()).unwrap();
    db.resolve_order_refund(
        &owner,
        id,
        oxsum_core::RefundDecision::Approve,
        None,
        "timeout-approve",
    )
    .await
    .unwrap();
    let target = db.claim_refund_attempt(id).await.unwrap().unwrap();
    let state = Arc::new(Mutex::new((0usize, None::<Value>)));
    let post_state = state.clone();
    let get_state = state.clone();
    let fixture = Router::new().route("/refunds",post(move |body:axum::Json<Value>| {
        let post_state = post_state.clone();
        async move {
            let result = json!({"refund_id":body["refund_id"],"payment_id":body["payment_id"],
                "amount":body["amount"],"currency":"USD","metadata":body["metadata"],"status":"pending"});
            {
                let mut stored = post_state.lock().unwrap();
                stored.0 += 1;
                stored.1 = Some(result.clone());
            }
            tokio::time::sleep(Duration::from_secs(12)).await;
            axum::Json(result)
        }
    })).route("/refunds/{id}",axum::routing::get(move || {
        let get_state = get_state.clone();
        async move {
            get_state.lock().unwrap().1.clone().map_or(
                (StatusCode::NOT_FOUND,axum::Json(json!({"error":{"code":"HE_02"}}))),
                |value| (StatusCode::OK,axum::Json(value)))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, fixture).await.unwrap();
    });
    let mut context = db
        .merchant_signing_context(merchant, version, &sealing_key())
        .await
        .unwrap()
        .unwrap();
    context.version.api_url = format!("http://{address}");
    let http = oxsum_integrations::provider_http::ProviderHttp::default();
    let started = std::time::Instant::now();
    let handle = oxsum_integrations::merchant_checkout::create_refund(
        &context,
        &target.refund,
        &provider,
        &http,
    )
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(9));
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(handle.provider_refund_id, id.to_string());
    db.note_refund_provider(id, &handle.provider_refund_id)
        .await
        .unwrap();
    let standing = db.refund_retry_target(id).await.unwrap().unwrap();
    let replay = oxsum_integrations::merchant_checkout::create_refund(
        &context,
        &standing.refund,
        &provider,
        &http,
    )
    .await
    .unwrap();
    assert_eq!(replay.provider_refund_id, handle.provider_refund_id);
    assert_eq!(state.lock().unwrap().0, 1);
    assert_eq!(standing.refund.state, "provider_pending");
    assert_eq!(standing.refund.retry_count, 1);
    server.abort();
}

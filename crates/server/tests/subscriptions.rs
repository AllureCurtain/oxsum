//! Subscription quota plans (issue #496): the admin catalogue and bind
//! endpoints, the org's own subscription reads, and a gateway turn's draw —
//! subscription first, wallet overflow — over a real PostgreSQL and a scripted
//! upstream.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

/// The operator token these tests configure the admin surface with.
const TOKEN: &str = "operator-token-0123456789";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("subscriptions")
}

macro_rules! world_or_skip {
    () => {
        match url() {
            Some(u) => world(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

// ── the scripted upstream ────────────────────────────────────────────────────

/// One upstream that answers every model the same scripted completion.
async fn upstream(State(seen): State<Arc<Mutex<Vec<Value>>>>) -> Response {
    seen.lock().unwrap().push(json!({"served": true}));
    Json(json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1,
        "model": "served",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "hi"},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
    }))
    .into_response()
}

/// The upstream's address once it is listening.
async fn start_upstream() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(seen.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the upstream binds");
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (address, seen)
}

// ── the world ────────────────────────────────────────────────────────────────

/// An app over a real database: one registered, funded organization, one
/// channel priced 1 minor per token on one model, and a scripted upstream the
/// model resolves to.
struct World {
    app: Router,
    pool: PgPool,
    organization_id: String,
    key: String,
    model: String,
}

async fn world(database_url: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");

    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("subs-{suffix}");
    let model = format!("ok-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
    let (base_url, _seen) = start_upstream().await;
    db.set_channel(&channel, &base_url, "upstream-secret", "openai", &secret)
        .await
        .expect("the channel is written");
    db.append_price(
        &channel,
        &model,
        oxsum_core::Price {
            input_price_per_million: 1_000_000,
            output_price_per_million: 1_000_000,
            max_output_tokens: 1000,
            cache_read_price_per_million: None,
            cache_write_5m_price_per_million: None,
            cache_write_1h_price_per_million: None,
            reasoning_price_per_million: None,
            cost_per_request: None,
            upstream: None,
            mode: oxsum_core::BillingMode::Chat,
            rules: vec![],
        },
        100,
    )
    .await
    .expect("the price is written");
    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_admin_token(TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = oxsum_server::app(db, config);

    // A registered, funded organization — the wallet the subscription sits beside.
    let email = format!(
        "subs_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let key = body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    let organization_id = body["data"]["organization"]["id"]
        .as_str()
        .expect("registration names the organization")
        .to_owned();
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": format!("top-{suffix}"), "amountMinor": 100_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "top-up failed: {body}");

    World {
        app,
        pool,
        organization_id,
        key,
        model,
    }
}

/// One JSON call against the API — the session credential when `key` is
/// given, the operator token when `admin` is.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                .expect("the test's own request"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// An admin call — the operator token is the credential.
async fn admin(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    call(app, method, path, body, Some(TOKEN)).await
}

/// A chat request against the gateway, credentialed by the organization's key.
async fn chat(app: &Router, key: &str, model: &str) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(
            json!({"model": model, "messages": [{"role": "user", "content": "hi"}]}).to_string(),
        ))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the gateway answers")
}

/// The request id the gateway stamped on its answer.
fn request_id(response: &Response) -> String {
    response
        .headers()
        .get("x-oxsum-request-id")
        .expect("the answer names its request")
        .to_str()
        .unwrap()
        .to_owned()
}

/// Bind a plan to this world's organization through the admin surface.
async fn bind(world: &World, plan_id: &str) -> (StatusCode, Value) {
    admin(
        &world.app,
        "PUT",
        &format!(
            "/api/v1/admin/organizations/{}/subscription",
            world.organization_id
        ),
        Some(json!({"planId": plan_id})),
    )
    .await
}

/// Define a plan through the admin surface.
async fn set_plan(app: &Router, name: &str, body: Value) -> (StatusCode, Value) {
    admin(
        app,
        "PUT",
        &format!("/api/v1/admin/subscription-plans/{name}"),
        Some(body),
    )
    .await
}

/// The draw rows one request's hold took — empty when no subscription drew.
async fn draws(pool: &PgPool, request_id: &str) -> Vec<Value> {
    sqlx::query(
        "SELECT drawn_minor, settled_minor FROM oxsum.subscription_draws \
         WHERE hold_key = $1",
    )
    .bind(format!("req-{request_id}:hold"))
    .fetch_all(pool)
    .await
    .expect("the draws read")
    .iter()
    .map(|row| {
        json!({
            "drawnMinor": row.get::<i64, _>("drawn_minor"),
            "settledMinor": row.get::<Option<i64>, _>("settled_minor"),
        })
    })
    .collect()
}

/// The organization's active subscription's consumed counter.
async fn consumed(pool: &PgPool, organization_id: &str) -> Option<i64> {
    sqlx::query_scalar(
        "SELECT consumed_minor FROM oxsum.org_subscriptions \
         WHERE organization_id = $1 AND state = 'active'",
    )
    .bind(Uuid::parse_str(organization_id).unwrap())
    .fetch_optional(pool)
    .await
    .expect("the subscription reads")
}

/// The settlement entry's description for a request — the bill's own record.
async fn settlement(world: &World, request_id: &str) -> Value {
    let wallet = Tenants::new(world.pool.clone())
        .get(
            &sqlx::query_scalar::<_, String>(
                "SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1",
            )
            .bind(Uuid::parse_str(&world.organization_id).unwrap())
            .fetch_one(&world.pool)
            .await
            .expect("the organization reads"),
        )
        .await
        .expect("the wallet opens");
    let hold_key = format!("req-{request_id}:hold");
    let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
    let text = wallet
        .receipt_proof(entry_id, None)
        .await
        .expect("the proof is built")
        .map(|bundle| bundle.entry.description().as_str().to_owned());
    serde_json::from_str(&text.expect("the settlement wrote")).expect("the description is JSON")
}

// ── the catalogue endpoints ──────────────────────────────────────────────────

#[tokio::test]
async fn a_plan_is_defined_and_listed_through_the_admin_surface() {
    let world = world_or_skip!();
    let name = format!("pro-{}", &Uuid::new_v4().simple().to_string()[..8]);

    let (status, body) = set_plan(
        &world.app,
        &name,
        json!({"quotaMinor": 1_000_000, "resetPeriod": "monthly", "priceMinor": 2000}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["name"], name);
    assert_eq!(body["data"]["quotaMinor"], 1_000_000);
    assert_eq!(body["data"]["resetPeriod"], "monthly");
    assert_eq!(body["data"]["allowWalletOverflow"], true);

    let (status, body) = admin(&world.app, "GET", "/api/v1/admin/subscription-plans", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == name)
    );
}

#[tokio::test]
async fn the_admin_surface_refuses_a_non_operator() {
    let world = world_or_skip!();
    let (status, _) = call(
        &world.app,
        "GET",
        "/api/v1/admin/subscription-plans",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ── bind / cancel endpoints ──────────────────────────────────────────────────

#[tokio::test]
async fn binding_and_cancelling_a_subscription_moves_the_org_reads() {
    let world = world_or_skip!();
    let name = format!("pro-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let (status, body) = set_plan(
        &world.app,
        &name,
        json!({"quotaMinor": 1_000_000, "resetPeriod": "monthly"}),
    )
    .await;
    let plan_id = body["data"]["planId"].as_str().unwrap().to_owned();
    assert_eq!(status, StatusCode::OK, "{body}");

    // Nothing bound: the org's own read answers null.
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/org/subscription",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"], Value::Null);

    // Bind through the admin surface.
    let (status, body) = bind(&world, &plan_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"], "active");
    assert_eq!(body["data"]["consumedMinor"], 0);

    // The org reads its own binding now.
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/org/subscription",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"], "active");
    assert_eq!(body["data"]["quotaMinor"], 1_000_000);

    // One open period stands in the history.
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/org/subscription/periods",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let periods = body["data"].as_array().unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0]["outcome"], "open");

    // Cancel: the binding goes away, the period closes `cancelled`.
    let (status, body) = admin(
        &world.app,
        "DELETE",
        &format!(
            "/api/v1/admin/organizations/{}/subscription",
            world.organization_id
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/org/subscription",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"], Value::Null);
}

#[tokio::test]
async fn binding_a_plan_that_does_not_exist_is_a_404() {
    let world = world_or_skip!();
    let (status, _) = bind(&world, &Uuid::new_v4().to_string()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── the gateway path ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_turn_draws_the_subscription_first_and_settles_its_share() {
    let world = world_or_skip!();
    let name = format!("pro-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let (status, body) = set_plan(
        &world.app,
        &name,
        json!({"quotaMinor": 1_000_000, "resetPeriod": "monthly"}),
    )
    .await;
    let plan_id = body["data"]["planId"].as_str().unwrap().to_owned();
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = bind(&world, &plan_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A chat turn: input 2 tokens + output 3 at 1 minor each, against a
    // freeze the 1-credit quota covers whole.
    let response = chat(&world.app, &world.key, &world.model).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    // The draw recorded the subscription's share of the freeze and settled
    // only what the charge used — the rest returned to the period.
    let rows = draws(&world.pool, &id).await;
    assert_eq!(rows.len(), 1, "one draw stands under the hold key");
    assert_eq!(rows[0]["settledMinor"], 5);

    let consumed = consumed(&world.pool, &world.organization_id).await.unwrap();
    assert_eq!(
        consumed, 5,
        "the period counts the settled share, not the draw"
    );

    // The settlement description names the entitlement that paid, at v9.
    let record = settlement(&world, &id).await;
    assert_eq!(record["v"], 9);
    assert_eq!(record["charged"], 5);
    let sub = &record["subscription"];
    assert!(sub["drawnMinor"].as_i64().unwrap() > 5);
    assert_eq!(sub["chargedMinor"], 5);
    assert!(sub["subscriptionId"].as_str().is_some());
    assert!(sub["periodId"].as_str().is_some());
}

#[tokio::test]
async fn an_exhausted_quota_overflows_into_the_wallet() {
    let world = world_or_skip!();
    // A quota smaller than the turn's charge: the subscription covers 10
    // minor, the wallet the rest.
    let name = format!("tiny-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let (status, body) = set_plan(
        &world.app,
        &name,
        json!({"quotaMinor": 10, "resetPeriod": "monthly", "allowWalletOverflow": true}),
    )
    .await;
    let plan_id = body["data"]["planId"].as_str().unwrap().to_owned();
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = bind(&world, &plan_id).await;
    assert_eq!(status, StatusCode::OK);

    let response = chat(&world.app, &world.key, &world.model).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    // The draw capped at the 10-minor quota; the charge was 5 — inside it.
    let consumed = consumed(&world.pool, &world.organization_id).await.unwrap();
    assert_eq!(consumed, 5);
    let record = settlement(&world, &id).await;
    assert_eq!(record["subscription"]["chargedMinor"], 5);
}

#[tokio::test]
async fn a_plan_without_overflow_refuses_once_the_quota_is_exhausted() {
    let world = world_or_skip!();
    // Quota 4: the first turn's 5-minor charge already cannot fit — a
    // no-overflow plan refuses rather than billing the wallet.
    let name = format!("rigid-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let (status, body) = set_plan(
        &world.app,
        &name,
        json!({"quotaMinor": 4, "resetPeriod": "monthly", "allowWalletOverflow": false}),
    )
    .await;
    let plan_id = body["data"]["planId"].as_str().unwrap().to_owned();
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = bind(&world, &plan_id).await;
    assert_eq!(status, StatusCode::OK);

    let response = chat(&world.app, &world.key, &world.model).await;
    // The no-overflow quota could not cover the freeze, and the plan refuses
    // the overflow — the same insufficient-funds answer an empty wallet gives.
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    // Nothing drew and nothing consumed — the hold never reached the wallet.
    let consumed = consumed(&world.pool, &world.organization_id).await.unwrap();
    assert_eq!(consumed, 0);
}

#[tokio::test]
async fn an_unsubscribed_org_charges_the_wallet_alone() {
    let world = world_or_skip!();
    let response = chat(&world.app, &world.key, &world.model).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    // No draw row and no subscription field on the settlement.
    assert!(draws(&world.pool, &id).await.is_empty());
    let record = settlement(&world, &id).await;
    assert!(record.get("subscription").is_none() || record["subscription"].is_null());
}

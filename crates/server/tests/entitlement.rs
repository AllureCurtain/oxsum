//! The entitlement read (issue #501): `GET /api/v1/org/keys/{keyId}/entitlement`
//! answers whether a request would be admitted now — the same checks admission
//! runs, composed read-only — and which constraint refuses, with the headroom
//! each one leaves.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("entitlement")
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

/// The scripted upstream — a model that answers one scripted completion.
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

async fn start_upstream() -> String {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(seen);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the upstream binds");
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    address
}

/// An app over a real database: one funded organization, one priced channel on
/// one model, and the organization's first key.
struct World {
    app: Router,
    pool: PgPool,
    key: String,
    key_id: String,
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
    let channel = format!("ent-{suffix}");
    let model = format!("ok-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
    let base_url = start_upstream().await;
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
    let config = Config::new(Signup::Open, None).with_secret(secret);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = oxsum_server::app(db, config);

    let email = format!(
        "ent_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let key = body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    let key_id = body["data"]["apiKey"]["id"]
        .as_str()
        .expect("registration names the key")
        .to_owned();
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": format!("top-{suffix}"), "amountMinor": 100_000_000})),
        Some(&key),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "top-up failed: {body}");

    World {
        app,
        pool,
        key,
        key_id,
        model,
    }
}

/// One JSON call; `key` is the bearer credential and `headers` extra
/// `[(name, value)]` pairs — the mandate blob when a test presents one.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
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

/// The entitlement answer for this world's key, under whatever query the test
/// shapes.
async fn entitlement(world: &World, query: &str) -> (StatusCode, Value) {
    call(
        &world.app,
        "GET",
        &format!("/api/v1/org/keys/{}/entitlement{query}", world.key_id),
        None,
        Some(&world.key),
        &[],
    )
    .await
}

/// The entitlement answer presenting a mandate blob.
async fn entitlement_with_mandate(world: &World, query: &str, blob: &str) -> (StatusCode, Value) {
    call(
        &world.app,
        "GET",
        &format!("/api/v1/org/keys/{}/entitlement{query}", world.key_id),
        None,
        Some(&world.key),
        &[("x-oxsum-mandate", blob)],
    )
    .await
}

/// Patch the key's whole constraint set; the body's absent fields clear.
async fn patch_key(world: &World, body: Value) -> (StatusCode, Value) {
    call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(body),
        Some(&world.key),
        &[],
    )
    .await
}

/// A gateway turn: a request under the organization's key.
async fn chat(world: &World, model: &str) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", world.key))
        .body(Body::from(
            json!({"model": model, "messages": [{"role": "user", "content": "hi"}]}).to_string(),
        ))
        .expect("the test's own request");
    world
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("the gateway answers")
}

// ── the answer's happy path ─────────────────────────────────────────────────

#[tokio::test]
async fn an_unconstrained_key_is_allowed_with_no_headroom_to_report() {
    let world = world_or_skip!();
    let (status, body) = entitlement(&world, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = &body["data"];
    assert_eq!(data["allowed"], true, "{data}");
    assert!(data["reason"].is_null(), "{data}");
    // Uncapped reads report the cap's absence, not a number.
    assert!(data["remainingRpm"].is_null(), "{data}");
    assert!(data["remainingTpm"].is_null(), "{data}");
    assert!(data["remainingBudgetMinor"].is_null(), "{data}");
    assert_eq!(data["concurrentHolds"], 0, "{data}");
    assert_eq!(data["checks"]["keyActive"], true, "{data}");
    assert_eq!(data["checks"]["orgSuspended"], false, "{data}");
}

#[tokio::test]
async fn a_model_inside_the_allowlist_is_allowed_and_another_is_not() {
    let world = world_or_skip!();
    let (status, _) = patch_key(&world, json!({"modelAllowlist": [world.model]})).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = entitlement(&world, &format!("?model={}", world.model)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["allowed"], true, "{body}");
    assert_eq!(body["data"]["checks"]["modelAllowed"], true, "{body}");

    let (status, body) = entitlement(&world, "?model=nope").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["allowed"], false, "{body}");
    assert_eq!(body["data"]["reason"], "model_not_allowed", "{body}");
    assert_eq!(body["data"]["checks"]["modelAllowed"], false, "{body}");
}

// ── the spend limit ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_spend_limit_reports_its_headroom_and_refuses_past_it() {
    let world = world_or_skip!();
    // A 1-credit cap with the funded wallet behind it.
    let (status, _) = patch_key(&world, json!({"spendLimitMinor": 1_000_000})).await;
    assert_eq!(status, StatusCode::OK);

    // Under the limit: allowed, and the headroom is the whole cap.
    let (status, body) = entitlement(&world, "?estimate=500000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = &body["data"];
    assert_eq!(data["allowed"], true, "{data}");
    assert_eq!(data["remainingBudgetMinor"], 1_000_000, "{data}");
    assert_eq!(data["checks"]["budgetOk"], true, "{data}");

    // Past the limit: budget_exhausted names it.
    let (status, body) = entitlement(&world, "?estimate=2000000").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = &body["data"];
    assert_eq!(data["allowed"], false, "{data}");
    assert_eq!(data["reason"], "budget_exhausted", "{data}");
    assert_eq!(data["checks"]["budgetOk"], false, "{data}");
    assert_eq!(data["remainingBudgetMinor"], 1_000_000, "{data}");
}

// ── the request and token windows ───────────────────────────────────────────

#[tokio::test]
async fn the_rpm_window_reports_headroom_and_refuses_when_full() {
    let world = world_or_skip!();
    let (status, _) = patch_key(&world, json!({"requestsPerMinute": 2})).await;
    assert_eq!(status, StatusCode::OK);

    // No requests yet: two slots free.
    let (status, body) = entitlement(&world, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["allowed"], true, "{body}");
    assert_eq!(body["data"]["remainingRpm"], 2, "{body}");

    // Two gateway turns spend the window; the third read sees it full — and the
    // peek itself spent nothing, or the count would be three by now.
    chat(&world, &world.model).await;
    chat(&world, &world.model).await;
    let (status, body) = entitlement(&world, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = &body["data"];
    assert_eq!(data["allowed"], false, "{data}");
    assert_eq!(data["reason"], "rpm_exceeded", "{data}");
    assert_eq!(data["checks"]["rpmOk"], false, "{data}");
    assert!(data["retryAfterSecs"].as_u64().is_some(), "{data}");
    // The peek spent nothing: the window still reads full on a second ask, not emptied.
    let (status, body) = entitlement(&world, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["reason"], "rpm_exceeded", "{body}");
}

#[tokio::test]
async fn the_tpm_window_reports_room_and_refuses_when_full() {
    let world = world_or_skip!();
    let (status, _) = patch_key(&world, json!({"tokensPerMinute": 10})).await;
    assert_eq!(status, StatusCode::OK);

    // A key that has settled nothing: the whole window is headroom.
    let (status, body) = entitlement(&world, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["allowed"], true, "{body}");
    assert_eq!(body["data"]["remainingTpm"], 10, "{body}");
    assert_eq!(body["data"]["checks"]["tpmOk"], true, "{body}");

    // Fill the window the way two settled turns would — the durable row the
    // settle writes in its own transaction, seeded straight.
    sqlx::query(
        "INSERT INTO oxsum.tpm_windows (key_id, scope, window_start, tokens) \
         VALUES ($1, '', date_trunc('second', now()), 10)",
    )
    .bind(Uuid::parse_str(&world.key_id).unwrap())
    .execute(&world.pool)
    .await
    .unwrap();

    // A full window refuses with its own reason and no headroom to report.
    let (status, body) = entitlement(&world, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = &body["data"];
    assert_eq!(data["allowed"], false, "{data}");
    assert_eq!(data["reason"], "tpm_exceeded", "{data}");
    assert_eq!(data["checks"]["tpmOk"], false, "{data}");
    assert_eq!(data["remainingTpm"], 0, "{data}");
    assert!(data["retryAfterSecs"].as_u64().is_some(), "{data}");
}

// ── the holds cap and scoped reads ──────────────────────────────────────────

#[tokio::test]
async fn an_open_hold_fills_the_concurrent_cap_and_reports_the_count() {
    let world = world_or_skip!();
    let (status, _) = patch_key(&world, json!({"maxConcurrentHolds": 1})).await;
    assert_eq!(status, StatusCode::OK);

    // One outstanding hold — `POST /api/v1/holds` reserves it.
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": format!("h-{}", &world.key_id[..8]), "amountMinor": 1_000_000})),
        Some(&world.key),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = entitlement(&world, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = &body["data"];
    assert_eq!(data["allowed"], false, "{data}");
    assert_eq!(data["reason"], "concurrent_holds", "{data}");
    assert_eq!(data["concurrentHolds"], 1, "{data}");
    assert_eq!(data["checks"]["holdsOk"], false, "{data}");
}

#[tokio::test]
async fn a_key_outside_the_callers_scope_is_not_found() {
    let world = world_or_skip!();
    // An id that is not this organization's key answers 404 — the scope rule
    // every key endpoint shares: ids cannot be probed.
    let foreign = Uuid::new_v4();
    let (status, _) = call(
        &world.app,
        "GET",
        &format!("/api/v1/org/keys/{foreign}/entitlement"),
        None,
        Some(&world.key),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── the mandate verdicts ────────────────────────────────────────────────────

#[tokio::test]
async fn a_presented_mandate_gets_its_own_verdicts() {
    let world = world_or_skip!();
    // A blob that cannot verify — no seed was ever minted on this key.
    let (status, body) = entitlement_with_mandate(&world, "", "not-a-real-blob").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["allowed"], false, "{body}");
    assert_eq!(body["data"]["reason"], "mandate_invalid", "{body}");
    assert_eq!(body["data"]["checks"]["mandateOk"], false, "{body}");

    // Mint a real mandate on the key, then ask with it: the delegation passes.
    let (status, body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/org/keys/{}/mandates", world.key_id),
        Some(json!({"capMinor": 5_000_000, "notAfter": "2030-01-01T00:00:00Z"})),
        Some(&world.key),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let blob = body["data"]["mandateBlob"]
        .as_str()
        .expect("a mandate blob");
    let (status, body) = entitlement_with_mandate(&world, "", blob).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["allowed"], true, "{body}");
    assert_eq!(body["data"]["checks"]["mandateOk"], true, "{body}");

    // An estimate past the mandate's cap names the delegation, not the key's
    // own headroom.
    let (status, body) = entitlement_with_mandate(&world, "?estimate=9000000", blob).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["allowed"], false, "{body}");
    assert_eq!(body["data"]["reason"], "mandate_cap", "{body}");
    assert_eq!(body["data"]["remainingBudgetMinor"], 5_000_000, "{body}");
}

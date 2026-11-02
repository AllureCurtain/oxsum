//! Request groups end to end (issue #508): a key/channel scoping dimension.
//!
//! A group names a subset of an organization's channels; a key assigned to it
//! may only serve through those channels, so one organization can split its
//! traffic without a second tenant. These tests check the admin CRUD, the
//! `groupId` assignment on key create/patch, the foreign-group refusal, and —
//! the load-bearing behavior — that a grouped key's request never routes
//! through a channel outside its group, while an ungrouped key reaches them
//! all.
//!
//! The tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::BodyExt;
use oxsum_core::{Db, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::broadcast;
use tower::ServiceExt;

const OPERATOR_TOKEN: &str = "operator-token-0123456789";

// ── the scripted upstream ────────────────────────────────────────────────────

/// One upstream's record of which model it was asked, plus the switch that
/// makes it refuse: `fail` flips the handler onto 429, the deterministic way a
/// test's failover walks to its next route.
#[derive(Clone, Default)]
struct Script {
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

async fn start_upstream(script: Script) -> String {
    async fn upstream(State(script): State<Script>, body: Bytes) -> Response {
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let model = body["model"].as_str().unwrap_or_default().to_owned();
        script.seen.lock().unwrap().push(model.clone());
        if script.fail.load(std::sync::atomic::Ordering::Relaxed) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                axum::Json(json!({"error": {"message": "rate limited"}})),
            )
                .into_response();
        }
        axum::Json(json!({
            "id": "chatcmpl-test", "object": "chat.completion", "model": model,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }))
        .into_response()
    }
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(script);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a free port");
    let address = listener.local_addr().expect("the listener has an address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}/v1")
}

// ── the world ────────────────────────────────────────────────────────────────

/// An app wired to two scripted channels — `chan_a` and `chan_b` serving the
/// same model — one funded organization, and the handles to check it.
struct World {
    app: Router,
    org_id: String,
    key: String,
    management_key: String,
    key_id: String,
    #[allow(dead_code)]
    chan_a: String,
    #[allow(dead_code)]
    chan_b: String,
    chan_a_id: String,
    #[allow(dead_code)]
    chan_b_id: String,
    script_a: Script,
    script_b: Script,
    /// The model both channels share — what a group test's request asks for.
    model: String,
    /// Channel A's private model — the fallback test's requested name.
    model_a: String,
    /// Channel B's private model — the fallback `model_a` resolves to.
    model_b: String,
    pool: PgPool,
    #[allow(dead_code)]
    tenants: Tenants,
    #[allow(dead_code)]
    billing: broadcast::Sender<oxsum_server::BillingEvent>,
}

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("groups")
}

macro_rules! world {
    () => {
        match url() {
            Some(url) => world_for(&url).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// With both token rates at 1_000_000 per million, a token costs one minor.
fn price_list(model: &str) -> String {
    let price = json!({
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 1_000_000,
        "maxOutputTokens": 1000,
    });
    let mut models = serde_json::Map::new();
    models.insert(model.to_owned(), price);
    Value::Object(models).to_string()
}

async fn world_for(url: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");

    let suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    // `model` is the model both channels share — what every test but the
    // fallback one asks for. `model_a` is channel A's alone and `model_b`
    // channel B's: the pair the fallback test chains so a pick between
    // channels needs no weight help.
    let model = format!("m-{suffix}");
    let model_a = format!("ma-{suffix}");
    let model_b = format!("mb-{suffix}");
    let chan_a = format!("chan-a-{suffix}");
    let chan_b = format!("chan-b-{suffix}");
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let script_a = Script::default();
    let script_b = Script::default();
    let base_a = start_upstream(script_a.clone()).await;
    let base_b = start_upstream(script_b.clone()).await;
    let book_a = oxsum_core::PriceBook::from_json(&chan_a, &price_list(&model))
        .expect("channel A's shared book parses");
    let book_a2 = oxsum_core::PriceBook::from_json(&chan_a, &price_list(&model_a))
        .expect("channel A's private book parses");
    let book_b = oxsum_core::PriceBook::from_json(&chan_b, &price_list(&model))
        .expect("channel B's shared book parses");
    let book_b2 = oxsum_core::PriceBook::from_json(&chan_b, &price_list(&model_b))
        .expect("channel B's private book parses");
    db.set_channel(&chan_a, &base_a, "upstream-secret", "openai", &secret)
        .await
        .expect("channel A written");
    db.set_channel(&chan_b, &base_b, "upstream-secret", "openai", &secret)
        .await
        .expect("channel B written");
    for (m, price) in book_a.models() {
        db.append_price(&chan_a, m, price.clone(), 100)
            .await
            .unwrap();
    }
    for (m, price) in book_a2.models() {
        db.append_price(&chan_a, m, price.clone(), 100)
            .await
            .unwrap();
    }
    for (m, price) in book_b.models() {
        db.append_price(&chan_b, m, price.clone(), 100)
            .await
            .unwrap();
    }
    for (m, price) in book_b2.models() {
        db.append_price(&chan_b, m, price.clone(), 100)
            .await
            .unwrap();
    }
    let chan_a_id = db
        .channel_id_named(&chan_a)
        .await
        .expect("channel A id")
        .expect("channel A exists")
        .to_string();
    let chan_b_id = db
        .channel_id_named(&chan_b)
        .await
        .expect("channel B id")
        .expect("channel B exists")
        .to_string();

    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, billing, _metrics) = oxsum_server::app_with_billing(db, config);
    let tenants = Tenants::new(pool.clone());

    let (registration, key) = register(&app).await;
    let org_id = registration["organization"]["id"]
        .as_str()
        .expect("registration names an org")
        .to_owned();
    let key_id = registration["apiKey"]["id"]
        .as_str()
        .expect("registration names a key")
        .to_owned();
    call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "top-1", "amountMinor": 10_000_000})),
        Some(&key),
    )
    .await;
    let (status, body) = call(&app, "POST", "/api/v1/org/keys", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    let management_key = body["data"]["secret"].as_str().unwrap().to_owned();
    World {
        app,
        org_id,
        key,
        management_key,
        key_id,
        chan_a,
        chan_b,
        chan_a_id,
        chan_b_id,
        script_a,
        script_b,
        model,
        model_a,
        model_b,
        pool,
        tenants,
        billing,
    }
}

async fn register(app: &Router) -> (Value, String) {
    let email = format!(
        "groups_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
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
    let key = body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    (body["data"].clone(), key)
}

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
    let request = request
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("the test's own request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A chat request; returns the gateway's HTTP status and which upstreams saw it.
async fn chat(world: &World, key: &str) -> StatusCode {
    chat_model(world, key, &world.model, None).await
}

/// A chat request asking for `model` specifically, pinning the attempt order
/// with `order` — channel A's `weight` 100 against channel B's 90 leads a
/// shared-model pick deterministically, where equal weights would be a coin
/// toss.
async fn chat_model(world: &World, key: &str, model: &str, order: Option<i64>) -> StatusCode {
    let mut body = json!({"model": model, "messages": [{"role": "user", "content": "hi"}]});
    if let Some(order) = order {
        body["metadata"] = json!({"ox_order": order});
    }
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .expect("the request");
    let response = world
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    response
        .into_body()
        .collect()
        .await
        .expect("complete response body");
    status
}

fn admin_app(world: &World) -> Router {
    let config = Config::new(Signup::Open, None)
        .with_secret(oxsum_core::SecretKey::from_bytes([7; 32]))
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::app(Db::from_pool(world.pool.clone()), config)
}

// ── the tests ────────────────────────────────────────────────────────────────

/// The admin surface round-trips a group's channel set, and a key's `groupId`
/// assigns and clears through the key patch.
#[tokio::test]
async fn a_group_is_written_and_a_key_assigned() {
    let world = world!();
    let admin = admin_app(&world);

    // Write a group holding only channel A.
    let (status, body) = call(
        &admin,
        "PUT",
        &format!("/api/v1/admin/organizations/{}/groups/team-a", world.org_id),
        Some(json!({ "channelIds": [world.chan_a_id] })),
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "group PUT failed: {body}");
    let group_id = body["data"]["id"].as_str().expect("group id").to_owned();
    assert_eq!(
        body["data"]["channelIds"].as_array().unwrap().len(),
        1,
        "the group holds its channel"
    );

    // It lists.
    let (status, body) = call(
        &admin,
        "GET",
        &format!("/api/v1/admin/groups?organizationId={}", world.org_id),
        None,
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "group list failed: {body}");
    assert_eq!(body["data"].as_array().unwrap().len(), 1);

    // Assign the key to it through the key patch.
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({ "groupId": group_id })),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "key group assign failed: {body}");
    assert_eq!(body["data"]["groupId"].as_str().unwrap(), group_id);
}

/// The load-bearing property: a grouped key only ever routes through its
/// group's channels; an ungrouped key reaches every serving.
#[tokio::test]
async fn a_grouped_key_stays_inside_its_channels() {
    let world = world!();
    let admin = admin_app(&world);

    // A group holding only channel A.
    let (status, body) = call(
        &admin,
        "PUT",
        &format!("/api/v1/admin/organizations/{}/groups/team-a", world.org_id),
        Some(json!({ "channelIds": [world.chan_a_id] })),
        Some(OPERATOR_TOKEN),
    )
    .await;
    let group_id = body["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(status, StatusCode::OK, "group PUT failed: {body}");

    // Assign the key — its requests may only reach channel A.
    let (status, _b) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({ "groupId": group_id })),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Drive several requests; every one must land on A, none on B.
    for _ in 0..4 {
        let status = chat(&world, &world.key).await;
        assert_eq!(status, StatusCode::OK, "a grouped request should serve");
    }
    assert!(
        !world.script_a.seen.lock().unwrap().is_empty(),
        "channel A should have served the grouped key"
    );
    assert!(
        world.script_b.seen.lock().unwrap().is_empty(),
        "a grouped key must never reach channel B"
    );
}

/// An ungrouped key still reaches every channel — the backward-compatible
/// default every key minted before groups landed has.
#[tokio::test]
async fn an_ungrouped_key_reaches_every_channel() {
    let world = world!();
    // Private models make reachability deterministic rather than relying on
    // random weighted picks eventually touching both routes.
    for model in [&world.model_a, &world.model_b] {
        assert_eq!(
            chat_model(&world, &world.key, model, None).await,
            StatusCode::OK
        );
    }
    assert_eq!(
        *world.script_a.seen.lock().unwrap(),
        vec![world.model_a.clone()]
    );
    assert_eq!(
        *world.script_b.seen.lock().unwrap(),
        vec![world.model_b.clone()]
    );
}

/// A group that excludes every serving of the requested model answers the
/// "model not served" refusal — a grouped key cannot escape to a channel its
/// group does not name.
#[tokio::test]
async fn a_group_with_no_serving_refuses_the_model() {
    let world = world!();
    let admin = admin_app(&world);

    // A group naming no channel leaves the model with no serving at all.
    let (status, body) = call(
        &admin,
        "PUT",
        &format!("/api/v1/admin/organizations/{}/groups/empty", world.org_id),
        Some(json!({ "channelIds": [] })),
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "empty group PUT failed: {body}");
    let group_id = body["data"]["id"].as_str().unwrap().to_owned();

    let (status, _b) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({ "groupId": group_id })),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The key's group names no channel, so the model resolves no serving.
    let status = chat(&world, &world.key).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, balance) = call(&world.app, "GET", "/api/v1/balance", None, Some(&world.key)).await;
    assert_eq!(balance["data"]["availableMinor"], 10_000_000);
    assert_eq!(balance["data"]["reservedMinor"], 0);
    assert!(world.script_a.seen.lock().unwrap().is_empty());
    assert!(world.script_b.seen.lock().unwrap().is_empty());
}

/// A foreign `groupId` — a group of another organization, or one that does not
/// exist — refuses the assignment rather than silently scoping the key.
#[tokio::test]
async fn a_foreign_group_id_refuses_the_assignment() {
    let world = world!();

    // A second organization's group — write it under a foreign org via core so
    // the key's own org cannot join it.
    let (foreign, _) = register(&world.app).await;
    let foreign_org = foreign["organization"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let group = oxsum_core::Db::from_pool(world.pool.clone())
        .put_group(
            foreign_org,
            "foreign",
            &[uuid::Uuid::parse_str(&world.chan_a_id).unwrap()],
        )
        .await
        .expect("a foreign group writes");

    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({ "groupId": group.id })),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// The same scope applies to a fallback: the requested model's channel is
/// exhausted (its one upstream refuses), the configured chain names a fallback
/// that only channel B serves, and a key grouped to channel A may not escape
/// to it — the fallback resolves through the group's channels exactly like the
/// primary model. (`model_a` rides on a channel `chan_a_only` B does not share,
/// so the pick between channels needs no weight help.)
#[tokio::test]
async fn a_fallback_resolves_only_the_groups_channels() {
    let world = world!();
    let admin = admin_app(&world);
    let db = Db::from_pool(world.pool.clone());

    // `model_a` is channel A's alone; `model_b` is channel B's fallback for it.
    // A group holding only channel A may not take the fallback's channel.
    db.set_fallbacks(&world.model_a, std::slice::from_ref(&world.model_b))
        .await
        .expect("the fallback chain writes");

    let (status, body) = call(
        &admin,
        "PUT",
        &format!("/api/v1/admin/organizations/{}/groups/team-a", world.org_id),
        Some(json!({ "channelIds": [world.chan_a_id] })),
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "group PUT failed: {body}");
    let group_id = body["data"]["id"].as_str().unwrap().to_owned();
    let (status, _b) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({ "groupId": group_id })),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Channel A serves `model_a` cleanly — the fallback is never needed.
    let status = chat_model(&world, &world.key, &world.model_a, None).await;
    assert_eq!(status, StatusCode::OK, "channel A serves the primary model");
    let primary = world.script_a.seen.lock().unwrap().len();
    assert!(primary > 0);
    assert!(world.script_b.seen.lock().unwrap().is_empty());

    // Channel A's upstream now refuses every call: the only remaining route
    // for the turn is `model_b` on channel B, which the group forbids — so the
    // turn refuses rather than escape, and channel B must never be asked it.
    world
        .script_a
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let status = chat_model(&world, &world.key, &world.model_a, None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let saw_model_b = world.script_b.seen.lock().unwrap().contains(&world.model_b);
    assert!(!saw_model_b, "channel B's fallback must never serve");

    // An ungrouped key proves the fallback itself works: channel A's refusal
    // walks the chain to `model_b`, which channel B then serves.
    let (_registration, other_key) = register(&world.app).await;
    call(
        &world.app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "top-2", "amountMinor": 10_000_000})),
        Some(&other_key),
    )
    .await;
    let status = chat_model(&world, &other_key, &world.model_a, None).await;
    assert_eq!(status, StatusCode::OK, "an ungrouped key walks the chain");
    assert!(
        world.script_b.seen.lock().unwrap().contains(&world.model_b),
        "channel B should have served the fallback model"
    );
}

/// Explicit null clears the group, omission preserves it, and invalid create
/// or patch leaves the entire key inventory/constraint set untouched.
#[tokio::test]
async fn group_assignment_is_atomic_and_null_is_distinct_from_omission() {
    let world = world!();
    let db = Db::from_pool(world.pool.clone());
    let group = db
        .put_group(world.org_id.parse().unwrap(), "atomic", &[])
        .await
        .unwrap();
    let path = format!("/api/v1/org/keys/{}", world.key_id);
    let (status, created) = call(
        &world.app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"groupId": group.id, "name": "born restricted"})),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["data"]["groupId"], group.id.to_string());
    assert_eq!(
        chat(&world, created["data"]["secret"].as_str().unwrap()).await,
        StatusCode::BAD_REQUEST
    );
    let (status, body) = call(
        &world.app,
        "PATCH",
        &path,
        Some(json!({"groupId": group.id, "spendLimitMinor": 1234})),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &world.app,
        "PATCH",
        &path,
        Some(json!({"spendLimitMinor": 5678})),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["groupId"], group.id.to_string());
    let invalid = uuid::Uuid::new_v4();
    let (status, _) = call(
        &world.app,
        "PATCH",
        &path,
        Some(json!({"groupId": invalid, "spendLimitMinor": 9999})),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, before) = call(
        &world.app,
        "GET",
        "/api/v1/org/keys",
        None,
        Some(&world.management_key),
    )
    .await;
    let current = before["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["id"] == world.key_id)
        .unwrap();
    assert_eq!(current["spendLimitMinor"], 5678);
    assert_eq!(current["groupId"], group.id.to_string());
    let (status, _) = call(
        &world.app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"groupId": invalid})),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, after) = call(
        &world.app,
        "GET",
        "/api/v1/org/keys",
        None,
        Some(&world.management_key),
    )
    .await;
    assert_eq!(before, after, "invalid create leaves no orphan key");
    let (status, body) = call(
        &world.app,
        "PATCH",
        &path,
        Some(json!({"groupId": null})),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["groupId"].is_null());
    assert_eq!(chat(&world, &world.key).await, StatusCode::OK);
}

#[tokio::test]
async fn grouped_bearer_cannot_escape_through_key_management_or_group_deletion() {
    let world = world!();
    let db = Db::from_pool(world.pool.clone());
    let group = db
        .put_group(world.org_id.parse().unwrap(), "restricted", &[])
        .await
        .unwrap();
    db.assign_key_group(
        world.org_id.parse().unwrap(),
        world.key_id.parse().unwrap(),
        Some(group.id),
    )
    .await
    .unwrap();
    for (method, path, body) in [
        ("POST", "/api/v1/org/keys".to_owned(), json!({})),
        (
            "POST",
            "/api/v1/org/keys".to_owned(),
            json!({"groupId": group.id}),
        ),
        (
            "PATCH",
            format!("/api/v1/org/keys/{}", world.key_id),
            json!({"groupId": null}),
        ),
    ] {
        let (status, response) =
            call(&world.app, method, &path, Some(body), Some(&world.key)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    }
    let (status, _) = call(
        &world.app,
        "DELETE",
        &format!(
            "/api/v1/admin/organizations/{}/groups/restricted",
            world.org_id
        ),
        None,
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(chat(&world, &world.key).await, StatusCode::BAD_REQUEST);
    let (status, groups) = call(
        &world.app,
        "GET",
        "/api/v1/org/groups",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(groups["data"][0]["id"], group.id.to_string());
    let (_, other) = register(&world.app).await;
    let (status, groups) = call(&world.app, "GET", "/api/v1/org/groups", None, Some(&other)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(groups["data"], json!([]));
    db.assign_key_group(
        world.org_id.parse().unwrap(),
        world.key_id.parse().unwrap(),
        None,
    )
    .await
    .unwrap();
    let (status, _) = call(
        &world.app,
        "DELETE",
        &format!(
            "/api/v1/admin/organizations/{}/groups/restricted",
            world.org_id
        ),
        None,
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn cached_answer_on_a_forbidden_channel_cannot_be_served_or_charged() {
    let world = world!();
    let db = Db::from_pool(world.pool.clone());
    db.set_semantic_cache(&world.chan_a, Some(&json!({"enabled": true})))
        .await
        .unwrap();
    assert_eq!(
        chat_model(&world, &world.key, &world.model_a, None).await,
        StatusCode::OK
    );
    // Wait for the normal asynchronous cache write, bounded by an observable row.
    let channel_id: uuid::Uuid = world.chan_a_id.parse().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.cached_responses WHERE organization_id = $1 AND channel_id = $2")
                .bind(world.org_id.parse::<uuid::Uuid>().unwrap()).bind(channel_id).fetch_one(&world.pool).await.unwrap();
            if count == 1 { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let (_, before) = call(&world.app, "GET", "/api/v1/balance", None, Some(&world.key)).await;
    let group = db
        .put_group(
            world.org_id.parse().unwrap(),
            "only-b",
            &[world.chan_b_id.parse().unwrap()],
        )
        .await
        .unwrap();
    db.assign_key_group(
        world.org_id.parse().unwrap(),
        world.key_id.parse().unwrap(),
        Some(group.id),
    )
    .await
    .unwrap();
    assert_eq!(
        chat_model(&world, &world.key, &world.model_a, None).await,
        StatusCode::BAD_REQUEST
    );
    let (_, after) = call(&world.app, "GET", "/api/v1/balance", None, Some(&world.key)).await;
    assert_eq!(before, after, "refusal creates no charge or leaked hold");
    assert_eq!(world.script_a.seen.lock().unwrap().len(), 1);
    assert!(world.script_b.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn same_model_retries_and_allowed_fallback_remain_inside_the_group() {
    let world = world!();
    let db = Db::from_pool(world.pool.clone());
    let org = world.org_id.parse().unwrap();
    let channels = [
        world.chan_a_id.parse().unwrap(),
        world.chan_b_id.parse().unwrap(),
    ];
    let group = db.put_group(org, "both", &channels).await.unwrap();
    db.assign_key_group(org, world.key_id.parse().unwrap(), Some(group.id))
        .await
        .unwrap();
    world
        .script_a
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(chat(&world, &world.key).await, StatusCode::OK);
    assert!(!world.script_b.seen.lock().unwrap().is_empty());
    db.set_fallbacks(&world.model_a, std::slice::from_ref(&world.model_b))
        .await
        .unwrap();
    assert_eq!(
        chat_model(&world, &world.key, &world.model_a, None).await,
        StatusCode::OK
    );
    assert!(world.script_b.seen.lock().unwrap().contains(&world.model_b));
}

#[tokio::test]
async fn grouped_access_does_not_bypass_a_model_allowlist_or_budget() {
    let world = world!();
    let db = Db::from_pool(world.pool.clone());
    let group = db
        .put_group(
            world.org_id.parse().unwrap(),
            "only-a",
            &[world.chan_a_id.parse().unwrap()],
        )
        .await
        .unwrap();
    let path = format!("/api/v1/org/keys/{}", world.key_id);
    let (status, _) = call(
        &world.app,
        "PATCH",
        &path,
        Some(json!({"groupId": group.id, "modelAllowlist": [world.model_b]})),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        chat_model(&world, &world.key, &world.model_a, None).await,
        StatusCode::FORBIDDEN
    );
    let (status, _) = call(
        &world.app,
        "PATCH",
        &path,
        Some(json!({"spendLimitMinor": 0})),
        Some(&world.management_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        chat_model(&world, &world.key, &world.model_a, None).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert!(world.script_a.seen.lock().unwrap().is_empty());
    assert!(world.script_b.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn session_group_assignment_requires_owner_or_admin() {
    let world = world!();
    let db = Db::from_pool(world.pool.clone());
    let (registered, _) = register(&world.app).await;
    let email = registered["user"]["email"].as_str().unwrap();
    let session = db
        .login(email, "correct horse battery")
        .await
        .unwrap()
        .into_session();
    let mut actor = db
        .authenticate_session(&session.token)
        .await
        .unwrap()
        .unwrap();
    let group = db
        .put_group(actor.organization.id, "session-scope", &[])
        .await
        .unwrap();
    let owner = oxsum_core::Principal::Session(actor.clone());
    let created = db
        .create_key_as(
            &owner,
            None,
            None,
            oxsum_core::KeyConstraints {
                group_id: Some(Some(group.id)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(created.key.group_id, Some(group.id));
    actor.role = oxsum_core::Role::Member;
    let member = oxsum_core::Principal::Session(actor);
    assert!(matches!(
        db.create_key_as(
            &member,
            None,
            None,
            oxsum_core::KeyConstraints {
                group_id: Some(Some(group.id)),
                ..Default::default()
            }
        )
        .await,
        Err(oxsum_core::WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.update_key_constraints_as(
            &member,
            created.key.id,
            oxsum_core::KeyConstraints {
                group_id: Some(None),
                ..Default::default()
            }
        )
        .await,
        Err(oxsum_core::WalletError::Forbidden(_))
    ));
    let kept = db
        .update_key_constraints_as(
            &member,
            created.key.id,
            oxsum_core::KeyConstraints {
                spend_limit_minor: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.group_id, Some(group.id));
    let cleared = db
        .update_key_constraints_as(
            &owner,
            created.key.id,
            oxsum_core::KeyConstraints {
                group_id: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cleared.group_id, None);
}

#[tokio::test]
async fn group_validation_is_atomic_and_query_errors_use_the_api_envelope() {
    let world = world!();
    let path = format!(
        "/api/v1/admin/organizations/{}/groups/validated",
        world.org_id
    );
    let (status, initial) = call(
        &world.app,
        "PUT",
        &path,
        Some(json!({"channelIds": [world.chan_a_id, world.chan_a_id]})),
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial["data"]["channelIds"].as_array().unwrap().len(), 1);
    let (status, _) = call(
        &world.app,
        "PUT",
        &path,
        Some(json!({"channelIds": [uuid::Uuid::new_v4()]})),
        Some(OPERATOR_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, groups) = call(
        &world.app,
        "GET",
        "/api/v1/org/groups",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(groups["data"][0], initial["data"]);
    for path in [
        "/api/v1/admin/groups",
        "/api/v1/admin/groups?organizationId=invalid",
    ] {
        let (status, body) = call(&world.app, "GET", path, None, Some(OPERATOR_TOKEN)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    }
}

//! The guardrail pipeline end to end (issue #502): admin-configured policies,
//! resolved per turn from the channel binding, the key's `requiredPolicies`
//! and a presented mandate's, judging the request before the hold
//! (`pre_call`) and the completed answer after it (`post_call`).
//!
//! The world is the gateway suite's own shape: a scripted upstream, a funded
//! organization, the admin surface under an operator token. Policies are
//! written through the admin API — the same route a real operator uses — so
//! each test exercises the write-validation and the runtime judgment
//! together. Requires DATABASE_URL; without it the tests skip.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use tower::ServiceExt;
use uuid::Uuid;

/// The operator token the admin calls authenticate with.
const TOKEN: &str = "operator-token-0123456789";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("guardrails")
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

/// The scripted upstream: one completion per model, plus a `seen` log so a
/// test can prove a blocked turn never reached it.
async fn upstream(State(seen): State<Arc<Mutex<Vec<Value>>>>) -> Response {
    seen.lock().unwrap().push(json!({"served": true}));
    axum::Json(json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 1,
        "model": "served",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "the answer's PII is 4111-1111-1111-1111"},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
    }))
    .into_response()
}

async fn start_upstream() -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the upstream binds");
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (address, seen)
}

/// The scripted moderation endpoint: answers the verdict the test staged.
async fn start_moderation(verdict: Value) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let calls = seen.clone();
    let app = Router::new()
        .route(
            "/moderate",
            post(move |axum::Json(body): axum::Json<Value>| {
                let calls = calls.clone();
                let verdict = verdict.clone();
                async move {
                    calls.lock().unwrap().push(body);
                    axum::Json(verdict).into_response()
                }
            }),
        )
        .with_state(());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the moderation endpoint binds");
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (address, seen)
}

/// The test world: an app over a real database, one funded organization, one
/// priced channel on one model — and the admin token the policy writes use.
struct World {
    app: Router,
    tenants: oxsum_core::Tenants,
    tenant_id: String,
    key: String,
    key_id: String,
    model: String,
    channel: String,
    seen: Arc<Mutex<Vec<Value>>>,
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
    let channel = format!("gr-{suffix}");
    let model = format!("ok-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
    let (base_url, seen) = start_upstream().await;
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

    let email = format!(
        "gr_{}@example.com",
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
    let key_id = body["data"]["apiKey"]["id"]
        .as_str()
        .expect("registration names the key")
        .to_owned();
    let tenant_id = body["data"]["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();
    let tenants = oxsum_core::Tenants::new(pool.clone());
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
        tenants,
        tenant_id,
        key,
        key_id,
        model,
        channel,
        seen,
    }
}

/// One JSON call; `key` is the bearer credential when Some — the operator
/// token on admin calls, the org key on org calls.
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

/// A gateway turn under the org's key; `text` is the prompt's content.
/// `Idempotency-Key` names the turn, which fixes the request id — and so the
/// ledger keys — rather than the server minting one.
async fn chat(world: &World, text: &str, idem: &str) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", world.key))
        .header("idempotency-key", idem)
        .body(Body::from(
            json!({"model": world.model, "messages": [{"role": "user", "content": text}]})
                .to_string(),
        ))
        .expect("the test's own request");
    world
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("the gateway answers")
}

/// Writes a keyword policy bound to this world's channel.
async fn keyword_policy(world: &World, name: &str, re: &str, action: &str) {
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/admin/guardrail-policies/{name}"),
        Some(json!({
            "kind": "keyword",
            "config": {"patterns": [{"re": re, "action": action}]},
        })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the policy writes: {body}");
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/admin/guardrail-bindings/channel/{}", world.channel),
        Some(json!({"policies": [name]})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the binding writes: {body}");
}

/// The turn's settled description, once the ledger holds it — the record the
/// `guardrail` field must appear in. Read through the wallet's own proof path,
/// the way `verify_charge` reads it.
async fn settled(world: &World, request_id: &str) -> Value {
    let wallet = world
        .tenants
        .get(&world.tenant_id)
        .await
        .expect("the organization's ledger opens");
    let hold_key = format!("req-{request_id}:hold");
    let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
    for _ in 0..250 {
        if let Some(bundle) = wallet
            .receipt_proof(entry_id, None)
            .await
            .expect("the proof is built")
        {
            return serde_json::from_str(bundle.entry.description().as_str())
                .expect("the description is JSON");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the turn never settled");
}

/// A keyword `block` policy's pre_call refusal: 403 before the hold — upstream
/// is never asked, and nothing was ever frozen to settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_prompt_is_refused_before_the_hold() {
    let world = world_or_skip!();
    keyword_policy(&world, "gr-block", "forbidden-phrase", "block").await;

    let response = chat(&world, "this contains the forbidden-phrase", "blk-1").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("the body collects")
        .to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("gr-block")),
        "the refusal names the policy: {body}"
    );
    // The block ran before the hold: upstream was never asked.
    assert_eq!(world.seen.lock().unwrap().len(), 0);
}

/// A keyword `flag` lets the turn through and the verdict lands on the
/// settlement's `guardrail` list — the audit a flag exists to leave.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flagged_prompt_settles_with_the_flag_on_record() {
    let world = world_or_skip!();
    keyword_policy(&world, "gr-flag", "watch-this", "flag").await;

    let response = chat(&world, "please watch-this", "flag-1").await;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = response
        .headers()
        .get("x-oxsum-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("the answer names its request")
        .to_owned();
    let description = settled(&world, &request_id).await;
    let flags = description["guardrail"]
        .as_array()
        .expect("the settlement carries a guardrail list");
    assert!(
        flags.iter().any(|f| f["policy"] == "gr-flag"
            && f["verdict"] == "flagged"
            && f["reason"]
                .as_str()
                .is_some_and(|r| r.contains("watch-this"))),
        "the flag landed: {description}"
    );
    // A flag never blocks: upstream answered and the turn charged.
    assert_eq!(world.seen.lock().unwrap().len(), 1);
    assert!(description["charged"].as_i64().unwrap_or(0) > 0);
}

/// A moderation endpoint whose verdict is `block` refuses `pre_call` — the
/// callout's own verdict, not a built-in rule's, is what turns the request
/// away.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moderation_endpoint_block_refuses_pre_call() {
    let world = world_or_skip!();
    let (moderation, calls) =
        start_moderation(json!({"verdict": "block", "reason": "hate speech"})).await;
    let (status, body) = call(
        &world.app,
        "PUT",
        "/api/v1/admin/guardrail-policies/gr-mod",
        Some(json!({
            "kind": "moderation",
            "config": {"endpoint": format!("{moderation}/moderate"), "timeoutMs": 5000},
            "apiKey": "bearer-secret-1",
        })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the moderation policy writes: {body}"
    );
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/admin/guardrail-bindings/channel/{}", world.channel),
        Some(json!({"policies": ["gr-mod"]})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response = chat(&world, "screen this", "mod-1").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(calls.lock().unwrap().len(), 1, "the endpoint was asked");
    // The endpoint's `block` ran before the hold: upstream never heard of it.
    assert_eq!(world.seen.lock().unwrap().len(), 0);
}

/// A `post_call` `block` on the completed answer: the turn is refused 403 but
/// the settlement settles `Charge::Nothing` — the upstream answer already
/// arrived and is paid for upstream's side of the ledger, so the org's hold is
/// returned whole rather than charged for a refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_response_block_refunds_the_hold() {
    let world = world_or_skip!();
    // The PII scanner flags card numbers on the response path — the scripted
    // upstream's answer carries one — and a keyword `block` on the same text
    // turns the flag into the refusal this test checks.
    let (status, body) = call(
        &world.app,
        "PUT",
        "/api/v1/admin/guardrail-policies/gr-pii-block",
        Some(json!({
            "kind": "keyword",
            "config": {"patterns": [{"re": "4111-1111-1111-1111", "action": "block"}]},
        })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/admin/guardrail-bindings/channel/{}", world.channel),
        Some(json!({"policies": ["gr-pii-block"]})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response = chat(&world, "go", "blk-2").await;
    let request_id = response
        .headers()
        .get("x-oxsum-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("the answer names its request")
        .to_owned();
    // The answer's card number matched the block pattern: upstream answered
    // (it was asked), but the turn is refused and charged nothing.
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(world.seen.lock().unwrap().len(), 1);
    let description = settled(&world, &request_id).await;
    assert_eq!(
        description["charged"].as_i64().unwrap_or(-1),
        0,
        "the blocked answer's hold was refunded: {description}"
    );
    let flags = description["guardrail"]
        .as_array()
        .expect("a guardrail list");
    assert!(
        flags
            .iter()
            .any(|f| f["policy"] == "gr-pii-block" && f["verdict"] == "blocked"),
        "the response block landed: {description}"
    );
}

/// The built-in PII scan on the response path: the answer's card number flags
/// without blocking — the turn settles normally with the flag on its record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pii_match_flags_the_response_without_blocking() {
    let world = world_or_skip!();
    let (status, body) = call(
        &world.app,
        "PUT",
        "/api/v1/admin/guardrail-policies/gr-pii",
        Some(json!({"kind": "pii", "config": {}})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/admin/guardrail-bindings/channel/{}", world.channel),
        Some(json!({"policies": ["gr-pii"]})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response = chat(&world, "go", "pii-1").await;
    let request_id = response
        .headers()
        .get("x-oxsum-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("the answer names its request")
        .to_owned();
    assert_eq!(response.status(), StatusCode::OK, "a flag never refuses");
    let description = settled(&world, &request_id).await;
    let flags = description["guardrail"]
        .as_array()
        .expect("a guardrail list");
    assert!(
        flags
            .iter()
            .any(|f| f["policy"] == "gr-pii" && f["verdict"] == "flagged"),
        "the PII flag landed: {description}"
    );
}

/// A mandate may only narrow its key's `requiredPolicies`: minting one that
/// names a policy the key does not require is refused 400 — the delegation
/// cannot add a rule the key never agreed to.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mandate_cannot_demand_a_policy_its_key_lacks() {
    let world = world_or_skip!();
    keyword_policy(&world, "gr-key-only", "anything", "flag").await;

    // The key requires nothing; a mandate demanding `gr-key-only` oversteps.
    let (status, body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/org/keys/{}/mandates", world.key_id),
        Some(json!({
            "capMinor": 1_000_000,
            "notAfter": "2099-01-01T00:00:00Z",
            "requiredPolicies": ["gr-key-only"],
        })),
        Some(&world.key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a mandate may only narrow the key's policies: {body}"
    );

    // Give the key the requirement and the same mandate mints: the delegate's
    // list is now a subset.
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({"requiredPolicies": ["gr-key-only"]})),
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/org/keys/{}/mandates", world.key_id),
        Some(json!({
            "capMinor": 1_000_000,
            "notAfter": "2099-01-01T00:00:00Z",
            "requiredPolicies": ["gr-key-only"],
        })),
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the narrowed mandate mints: {body}");
}

/// A `requiredPolicies` write that names a policy which does not exist is
/// refused 400 — a key cannot demand a rule that was never configured, or the
/// name would sit on the credential unguarding every turn it claims to
/// protect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_required_policy_that_does_not_exist_is_refused() {
    let world = world_or_skip!();
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({"requiredPolicies": ["no-such-policy"]})),
        Some(&world.key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a dangling requiredPolicies is refused: {body}"
    );
}

/// The admin write path's own validation: a keyword policy whose regex cannot
/// compile is a 400 at the PUT, not a stored rule every turn would skip.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_uncompilable_keyword_policy_is_refused_at_write() {
    let world = world_or_skip!();
    let (status, body) = call(
        &world.app,
        "PUT",
        "/api/v1/admin/guardrail-policies/gr-bad",
        Some(json!({
            "kind": "keyword",
            "config": {"patterns": [{"re": "[unclosed", "action": "block"}]},
        })),
        Some(TOKEN),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a regex that cannot compile never lands: {body}"
    );
}

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
async fn upstream(
    State(seen): State<Arc<Mutex<Vec<Value>>>>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    seen.lock().unwrap().push(json!({"served": true}));
    if body["messages"][0]["content"] == "zero-token-baseline" {
        return axum::Json(json!({
            "id": "chatcmpl-empty",
            "choices": [{"message": {"role": "assistant", "content": ""}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
        }))
        .into_response();
    }
    if body["messages"][0]["content"] == "missing-usage-block" {
        return axum::Json(json!({
            "id": "chatcmpl-no-usage",
            "choices": [{"message": {"role": "assistant", "content": "4111-1111-1111-1111"}}]
        }))
        .into_response();
    }
    if body["stream"] == true {
        let chunk = json!({"id":"chatcmpl-test","object":"chat.completion.chunk","created":1,"model":"served","choices":[{"index":0,"delta":{"content":"the answer's PII is 4111-1111-1111-1111"},"finish_reason":null}]});
        let end = json!({"id":"chatcmpl-test","object":"chat.completion.chunk","created":1,"model":"served","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}});
        return (
            [("content-type", "text/event-stream")],
            format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
        )
            .into_response();
    }
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
    start_moderation_sequence(vec![verdict]).await
}

/// Consecutive evaluations consume scripted answers; the last answer repeats.
async fn start_moderation_sequence(verdicts: Vec<Value>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let verdicts = Arc::new(verdicts);
    let calls = seen.clone();
    let app = Router::new()
        .route(
            "/moderate",
            post(move |axum::Json(body): axum::Json<Value>| {
                let calls = calls.clone();
                let verdicts = verdicts.clone();
                async move {
                    let verdict = {
                        let mut calls = calls.lock().unwrap();
                        let verdict = verdicts[calls.len().min(verdicts.len() - 1)].clone();
                        calls.push(body);
                        verdict
                    };
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
    db: Db,
    tenants: oxsum_core::Tenants,
    tenant_id: String,
    key: String,
    session: String,
    key_id: String,
    model: String,
    channel: String,
    seen: Arc<Mutex<Vec<Value>>>,
}

async fn world(database_url: &str) -> World {
    world_with_funding(database_url, true).await
}

async fn world_with_funding(database_url: &str, funded: bool) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;

    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("gr-{suffix}");
    let model = format!("ok-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
    let (base_url, seen) = start_upstream().await;
    db.set_channel_pool_policy(
        &channel,
        &base_url,
        Some(&["upstream-secret".to_owned()]),
        "openai",
        &secret,
        Some(&oxsum_core::ProviderDestinationPolicy {
            network: oxsum_core::ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }),
    )
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
            cost_per_request: Some(700),
            upstream: Some(oxsum_core::UpstreamPrices {
                input_price_per_million: Some(1_000_000),
                output_price_per_million: Some(1_000_000),
                cost_per_request: Some(200),
                ..Default::default()
            }),
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
    let app = crate::support::app(db.clone(), config);

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
    let session = crate::support::login(&app, &email, "correct horse battery").await;
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
    if funded {
        let (status, body) = call(
            &app,
            "POST",
            "/__test/funding",
            Some(json!({"idempotencyKey": format!("top-{suffix}"), "amountMinor": 100_000_000})),
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "top-up failed: {body}");
    }

    World {
        app,
        db,
        tenants,
        tenant_id,
        key,
        session,
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
        request = crate::support::credential(request, key);
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
    chat_with_stream(world, text, idem, false).await
}

async fn chat_with_stream(world: &World, text: &str, idem: &str, stream: bool) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", world.key))
        .header("idempotency-key", idem)
        .body(Body::from(
            json!({"model": world.model, "stream":stream, "messages": [{"role": "user", "content": text}]})
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
        Some(&world.session),
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
        Some(&world.session),
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
        Some(&world.session),
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
        Some(&world.session),
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

async fn bind_policies(world: &World, names: &[String]) {
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/admin/guardrail-bindings/channel/{}", world.channel),
        Some(json!({"policies":names})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn moderation_policy(world: &World, name: &str, endpoint: &str) {
    let (status,body) = call(&world.app,"PUT",
        &format!("/api/v1/admin/guardrail-policies/{name}"),
        Some(json!({"kind":"moderation","config":{"endpoint":format!("{endpoint}/moderate"),"timeoutMs":1000},"apiKey":"local-test-only"})),Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn request_id(response: &Response) -> String {
    response.headers()["x-oxsum-request-id"]
        .to_str()
        .unwrap()
        .to_owned()
}

async fn retained_reasons(world: &World, request: &str) -> Value {
    let details: Value = sqlx::query_scalar(
        "SELECT usage_details FROM oxsum.usage_records WHERE request_id=$1 AND tenant_id=$2",
    )
    .bind(request)
    .bind(&world.tenant_id)
    .fetch_one(world.db.pool())
    .await
    .unwrap();
    details["oxsumReasonEvidence"].clone()
}

fn check_exact_reason(field: &Value, full: &str) {
    assert!(
        field.get("reason").is_none(),
        "long text must not masquerade as shortened inline text"
    );
    let commitment: oxsum_verify::ReasonCommitment =
        serde_json::from_value(field["reasonCommitment"].clone()).unwrap();
    assert!(oxsum_verify::verify_reason_commitment(&commitment, full));
    assert!(!oxsum_verify::verify_reason_commitment(
        &commitment,
        &format!("{full} ")
    ));
}

async fn check_proof(world: &World, request: &str, refused: bool) {
    let key = if refused {
        format!("req-{request}:refused")
    } else {
        oxsum_core::settlement_key_for(&format!("req-{request}:hold"))
    };
    let wallet = world.tenants.get(&world.tenant_id).await.unwrap();
    let bundle = wallet
        .receipt_proof(oxsum_core::entry_id_for(&key), None)
        .await
        .unwrap()
        .unwrap();
    let hash = bundle.entry.content_hash();
    let mut value = serde_json::to_value(&bundle).unwrap();
    assert!(oxsum_verify::verify_bundle(&value.to_string(), &hash).unwrap());
    assert_eq!(
        oxsum_verify::verify_charge(bundle.entry.description().as_str()),
        oxsum_verify::ChargeCheck::Recomputed
    );
    let mut description: Value = serde_json::from_str(bundle.entry.description().as_str()).unwrap();
    description["guardrail"][0]["reasonCommitment"]["sha256"] = Value::from("0".repeat(64));
    value["entry"]["description"] = Value::from(description.to_string());
    assert!(!oxsum_verify::verify_bundle(&value.to_string(), &hash).unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_long_two_phase_reasons_survive_whole_cache_and_exact_replay() {
    let world = world_or_skip!();
    let full = "  🦀\n\"\\\u{0001}".repeat(200);
    let (endpoint, calls) = start_moderation(json!({"verdict":"flag","reason":full})).await;
    let names: Vec<String> = (0..8)
        .map(|i| format!("long-{i}-{}", world.channel))
        .collect();
    for name in &names {
        moderation_policy(&world, name, &endpoint).await;
    }
    bind_policies(&world, &names).await;
    world
        .db
        .set_semantic_cache(
            &world.channel,
            Some(&json!({"enabled":true,"ttlSeconds":60})),
        )
        .await
        .unwrap();
    for (idem, is_cache) in [("full-first", false), ("full-cache", true)] {
        if is_cache {
            tokio::time::timeout(Duration::from_secs(5),async {
                loop {
                    let stored:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.cached_responses c JOIN oxsum.channels h ON h.channel_id=c.channel_id WHERE h.name=$1)").bind(&world.channel).fetch_one(world.db.pool()).await.unwrap();
                    if stored { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("the asynchronously stored answer becomes visible");
        }
        let response = chat(&world, "exact cache prompt", idem).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("x-oxsum-cache").is_some(), is_cache);
        let request = request_id(&response);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let description = settled(&world, &request).await;
        assert_eq!(description["v"], 15);
        assert!(description.to_string().len() <= oxsum_core::GATEWAY_SETTLEMENT_JSON_BYTES);
        assert_eq!(description["guardrail"].as_array().unwrap().len(), 16);
        let retained = retained_reasons(&world, &request).await;
        for (index, note) in description["guardrail"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            check_exact_reason(note, &full);
            let stored = &retained["guardrails"][index];
            assert_eq!(stored["index"], index);
            assert_eq!(stored["reason"], full);
            assert_eq!(stored["phase"], note["phase"]);
            assert_eq!(stored["policy"], note["policy"]);
        }
        check_proof(&world, &request, false).await;
        if !is_cache && let Some(path) = std::env::var_os("OXSUM_GUARDRAIL_FIXTURE_OUT") {
            let wallet = world.tenants.get(&world.tenant_id).await.unwrap();
            let bundle = wallet
                .receipt_proof(
                    oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!(
                        "req-{request}:hold"
                    ))),
                    None,
                )
                .await
                .unwrap()
                .unwrap();
            std::fs::write(path,serde_json::to_vec_pretty(&json!({"bundle":bundle,"expectedHash":bundle.entry.content_hash().to_hex(),"reason":full})).unwrap()).unwrap();
        }
        let seen = calls.lock().unwrap().len();
        let replay = chat(&world, "exact cache prompt", idem).await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(request_id(&replay), request);
        assert_eq!(replay.into_body().collect().await.unwrap().to_bytes(), body);
        assert_eq!(calls.lock().unwrap().len(), seen);
        assert_eq!(settled(&world, &request).await, description);
        if is_cache {
            assert_eq!(description["upstreamAttempts"], 0);
            assert!(description["cache"].is_object());
        }
    }
    assert_eq!(world.seen.lock().unwrap().len(), 1);
    assert_eq!(calls.lock().unwrap().len(), 32);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_preblock_keeps_typed_full_reason_without_hold_or_operation() {
    let world = world_or_skip!();
    let full = "\"🦀\n".repeat(300);
    let (endpoint, calls) = start_moderation(json!({"verdict":"block","reason":full})).await;
    let name = format!("pre-{}", world.channel);
    moderation_policy(&world, &name, &endpoint).await;
    bind_policies(&world, std::slice::from_ref(&name)).await;
    let wallet = world.tenants.get(&world.tenant_id).await.unwrap();
    let before = wallet.available().await.unwrap();
    let response = chat(&world, "blocked", "long-preblock").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let request = request_id(&response);
    let bundle = wallet
        .receipt_proof(
            oxsum_core::entry_id_for(&format!("req-{request}:refused")),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let description: Value = serde_json::from_str(bundle.entry.description().as_str()).unwrap();
    check_exact_reason(&description["guardrail"][0], &full);
    assert_eq!(description["guardrail"][0]["phase"], "pre_call");
    let message = format!("guardrail {name} refused the request: {full}");
    check_exact_reason(&description, &message);
    let retained = retained_reasons(&world, &request).await;
    assert_eq!(retained["guardrails"][0]["reason"], full);
    assert_eq!(retained["failure"]["reason"], message);
    check_proof(&world, &request, true).await;
    let records = wallet.recent_requests(10).await.unwrap();
    let record = records
        .iter()
        .find(|record| record.request_id == request)
        .unwrap();
    assert!(record.failure_reason.is_none());
    let commitment = record.failure_reason_commitment.as_ref().unwrap();
    assert!(oxsum_verify::verify_reason_commitment(commitment, &message));
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/billing-records",
        None,
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = body["data"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["requestId"] == request)
        .unwrap();
    assert!(row.get("failureReason").is_none());
    assert_eq!(
        row["failureReasonCommitment"],
        serde_json::to_value(commitment).unwrap()
    );
    assert_eq!(wallet.available().await.unwrap(), before);
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    let operations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.request_operations WHERE key_id=$1")
            .bind(Uuid::parse_str(&world.key_id).unwrap())
            .fetch_one(world.db.pool())
            .await
            .unwrap();
    assert_eq!(operations, 0);
    assert_eq!(world.seen.lock().unwrap().len(), 0);
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_postblock_refunds_whole_but_streamed_block_still_bills_and_commits() {
    let world = world_or_skip!();
    let full = "post 🦀\n\"".repeat(300);
    let (endpoint, calls) = start_moderation_sequence(vec![
        json!({"verdict":"allow"}),
        json!({"verdict":"block","reason":full}),
        json!({"verdict":"allow"}),
        json!({"verdict":"block","reason":full}),
    ])
    .await;
    let name = format!("post-{}", world.channel);
    moderation_policy(&world, &name, &endpoint).await;
    bind_policies(&world, &[name]).await;
    let wallet = world.tenants.get(&world.tenant_id).await.unwrap();
    let before = wallet.available().await.unwrap();
    for (stream, idem) in [(false, "post-whole"), (true, "post-stream")] {
        let response = chat_with_stream(&world, "go", idem, stream).await;
        assert_eq!(
            response.status(),
            if stream {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            }
        );
        let request = request_id(&response);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        if stream {
            assert!(String::from_utf8_lossy(&bytes).contains("4111-1111"));
        }
        let description = settled(&world, &request).await;
        let note = &description["guardrail"][0];
        check_exact_reason(note, &full);
        assert_eq!(note["verdict"], "blocked");
        assert_eq!(note["phase"], "post_call");
        assert_eq!(note.get("streamed").is_some(), stream);
        assert_eq!(description["charged"].as_i64().unwrap() > 0, stream);
        assert_eq!(
            retained_reasons(&world, &request).await["guardrails"][0]["reason"],
            full
        );
        assert_eq!(wallet.reserved().await.unwrap(), 0);
        check_proof(&world, &request, false).await;
        if !stream {
            assert_eq!(wallet.available().await.unwrap(), before);
        }
    }
    assert_eq!(calls.lock().unwrap().len(), 4);
    assert_eq!(world.seen.lock().unwrap().len(), 2);
}

/// The same explicit refund releases both money and entitlement reservations.
/// A previously served cache source stays charged; replay cannot settle twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flat_fee_refunds_preserve_provider_cost_and_release_wallet_or_quota() {
    let Some(url) = url() else { return };
    for quota in [false, true] {
        let world = world_with_funding(&url, !quota).await;
        let wallet = world.tenants.get(&world.tenant_id).await.unwrap();
        let organization: Uuid = sqlx::query_scalar(
            "SELECT organization_id FROM oxsum.organizations WHERE tenant_id=$1",
        )
        .bind(&world.tenant_id)
        .fetch_one(world.db.pool())
        .await
        .unwrap();
        if quota {
            let (status, plan) = call(
                &world.app,
                "PUT",
                &format!("/api/v1/admin/subscription-plans/{}", world.channel),
                Some(json!({"quotaMinor":1_000_000,"resetPeriod":"monthly","allowWalletOverflow":false})),
                Some(TOKEN),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{plan}");
            let (status, body) = call(
                &world.app,
                "PUT",
                &format!("/api/v1/admin/organizations/{organization}/subscription"),
                Some(json!({"planId":plan["data"]["planId"]})),
                Some(TOKEN),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(wallet.available().await.unwrap(), 0);
        }
        let full = "complete refund reason 🦀\n".repeat(200);
        let (endpoint, calls) = start_moderation_sequence(vec![
            json!({"verdict":"allow"}),
            json!({"verdict":"allow"}),
            json!({"verdict":"allow"}),
            json!({"verdict":"block","reason":full}),
            json!({"verdict":"allow"}),
            json!({"verdict":"block","reason":full}),
            json!({"verdict":"allow"}),
            json!({"verdict":"block","reason":full}),
            json!({"verdict":"allow"}),
            json!({"verdict":"allow"}),
        ])
        .await;
        let name = format!("flat-{}", world.channel);
        moderation_policy(&world, &name, &endpoint).await;
        bind_policies(&world, &[name]).await;
        world
            .db
            .set_semantic_cache(
                &world.channel,
                Some(&json!({"enabled":true,"ttlSeconds":60})),
            )
            .await
            .unwrap();
        let before = wallet.available().await.unwrap();
        let mut total = 0;
        for (index, text, stream, blocked, charged) in [
            (0, "cache prompt", false, false, 705),
            (1, "cache prompt", false, true, 0),
            (2, "fresh blocked", false, true, 0),
            (3, "stream blocked", true, true, 705),
            (4, "zero-token-baseline", false, false, 700),
        ] {
            if index == 1 {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let stored: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.cached_responses c JOIN oxsum.channels h ON h.channel_id=c.channel_id WHERE h.name=$1)")
                            .bind(&world.channel).fetch_one(world.db.pool()).await.unwrap();
                        if stored { break; }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.unwrap();
            }
            let idem = format!("flat-{index}");
            let response = chat_with_stream(&world, text, &idem, stream).await;
            let status = if blocked && !stream {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::OK
            };
            assert_eq!(response.status(), status);
            let request = request_id(&response);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let description = settled(&world, &request).await;
            assert_eq!(description["charged"], charged, "{description}");
            let recorded_status: Option<i32> =
                sqlx::query_scalar("SELECT http_status FROM oxsum.request_log WHERE request_id=$1")
                    .bind(&request)
                    .fetch_one(world.db.pool())
                    .await
                    .unwrap();
            assert_eq!(recorded_status, Some(i32::from(status.as_u16())));
            assert_eq!(description["upstreamAttempts"], i64::from(index != 1));
            let (usage_charge, provider_cost, basis): (i64, Option<i64>, String) = sqlx::query_as(
                "SELECT charged_minor, upstream_cost_minor, cost_basis FROM oxsum.usage_records WHERE request_id=$1",
            ).bind(&request).fetch_one(world.db.pool()).await.unwrap();
            assert_eq!(usage_charge, charged);
            assert_eq!(
                provider_cost,
                Some(if index == 1 {
                    0
                } else if index == 4 {
                    200
                } else {
                    205
                })
            );
            assert_eq!(
                basis,
                if index == 1 {
                    "cache_reuse"
                } else {
                    "real_attempt"
                }
            );
            let operation_charge: i64 = sqlx::query_scalar(
                "SELECT charged_minor FROM oxsum.request_operations WHERE hold_key=$1",
            )
            .bind(format!("req-{request}:hold"))
            .fetch_one(world.db.pool())
            .await
            .unwrap();
            assert_eq!(operation_charge, charged);
            if index != 1 {
                let provider_usage: Value = sqlx::query_scalar("SELECT a.usage_json FROM oxsum.request_attempts a JOIN oxsum.request_operations o USING(operation_id) WHERE o.hold_key=$1")
                    .bind(format!("req-{request}:hold")).fetch_one(world.db.pool()).await.unwrap();
                assert_eq!(
                    provider_usage["inputTokens"].as_i64().unwrap_or(0),
                    if index == 4 { 0 } else { 2 }
                );
                assert_eq!(
                    provider_usage["outputTokens"].as_i64().unwrap_or(0),
                    if index == 4 { 0 } else { 3 }
                );
            }
            if blocked {
                check_exact_reason(&description["guardrail"][0], &full);
                assert_eq!(description["guardrail"][0]["verdict"], "blocked");
                assert_eq!(
                    retained_reasons(&world, &request).await["guardrails"][0]["reason"],
                    full
                );
                check_proof(&world, &request, false).await;
            }
            assert_eq!(wallet.reserved().await.unwrap(), 0);
            total += charged;
            assert_eq!(
                wallet.available().await.unwrap(),
                before - if quota { 0 } else { total }
            );
            if quota {
                let consumed: i64 = sqlx::query_scalar("SELECT consumed_minor FROM oxsum.org_subscriptions WHERE organization_id=$1 AND state='active'")
                    .bind(organization).fetch_one(world.db.pool()).await.unwrap();
                assert_eq!(consumed, total);
                let (drawn, settled): (i64, Option<i64>) = sqlx::query_as("SELECT drawn_minor,settled_minor FROM oxsum.subscription_draws WHERE hold_key=$1")
                    .bind(format!("req-{request}:hold")).fetch_one(world.db.pool()).await.unwrap();
                assert!(drawn > 0);
                assert_eq!(settled, Some(charged));
                assert_eq!(description["subscription"]["chargedMinor"], charged);
            }
            if !stream {
                let seen = world.seen.lock().unwrap().len();
                let moderated = calls.lock().unwrap().len();
                let replay = chat(&world, text, &idem).await;
                assert_eq!(replay.status(), status);
                assert_eq!(request_id(&replay), request);
                assert_eq!(replay.into_body().collect().await.unwrap().to_bytes(), body);
                assert_eq!(world.seen.lock().unwrap().len(), seen);
                assert_eq!(calls.lock().unwrap().len(), moderated);
                assert_eq!(settled(&world, &request).await, description);
            }
        }
        assert_eq!(world.seen.lock().unwrap().len(), 4);
        assert_eq!(calls.lock().unwrap().len(), 10);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_moderation_transport_uses_existing_phase_failure_rules() {
    let world = world_or_skip!();
    let (endpoint, calls) = start_moderation_sequence(vec![
        json!({"verdict":"flag","reason":"x".repeat(70_000)}),
        json!({"verdict":"allow"}),
        json!({"verdict":"block","reason":"x".repeat(70_000)}),
    ])
    .await;
    let name = format!("body-{}", world.channel);
    moderation_policy(&world, &name, &endpoint).await;
    bind_policies(&world, &[name]).await;
    let start = std::time::Instant::now();
    let refused = chat(&world, "go", "oversized-pre").await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(world.seen.lock().unwrap().len(), 0);
    let answered = chat(&world, "go", "oversized-post").await;
    assert_eq!(answered.status(), StatusCode::OK);
    let request = request_id(&answered);
    answered.into_body().collect().await.unwrap();
    let description = settled(&world, &request).await;
    assert!(description.get("guardrail").is_none());
    assert!(description["charged"].as_i64().unwrap() > 0);
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert_eq!(world.seen.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_new_and_historical_policy_sets_refuse_before_moderation() {
    let world = world_or_skip!();
    let (endpoint, calls) = start_moderation(json!({"verdict":"flag","reason":"short"})).await;
    let live = format!("live-{}", world.channel);
    moderation_policy(&world, &live, &endpoint).await;
    for policies in [
        vec![live.clone(); 2],
        (0..9).map(|i| format!("p{i}")).collect(),
    ] {
        let (status, _) = call(
            &world.app,
            "PUT",
            &format!("/api/v1/admin/guardrail-bindings/channel/{}", world.channel),
            Some(json!({"policies":policies})),
            Some(TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &world.app,
            "PATCH",
            &format!("/api/v1/org/keys/{}", world.key_id),
            Some(json!({"requiredPolicies":policies})),
            Some(&world.session),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let name = format!("keyword-{}", world.channel);
    for patterns in [
        vec![json!({"re":"🦀".repeat(1025),"action":"flag"})],
        vec![json!({"re":"x","action":"unknown"})],
        vec![json!({"re":"x","action":"flag"}); 65],
    ] {
        let (status, _) = call(
            &world.app,
            "PUT",
            &format!("/api/v1/admin/guardrail-policies/{name}"),
            Some(json!({"kind":"keyword","config":{"patterns":patterns}})),
            Some(TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    // Historical dangling names remain visible to the union bound.
    let mut legacy: Vec<String> = (0..8).map(|i| format!("missing{i}")).collect();
    legacy.push(live.clone());
    sqlx::query("INSERT INTO oxsum.guardrail_bindings(scope,target,policies) VALUES('channel',$1,$2) ON CONFLICT(scope,target) DO UPDATE SET policies=EXCLUDED.policies").bind(&world.channel).bind(&legacy).execute(world.db.pool()).await.unwrap();
    let response = chat(&world, "go", "legacy-union").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(world.seen.lock().unwrap().len(), 0);
    assert_eq!(calls.lock().unwrap().len(), 0);
    // Individually valid lists must still refuse when their complete union
    // exceeds eight. Clearing it allows the next independent configuration.
    let mut current = vec![live.clone()];
    for i in 0..8 {
        let name = format!("union-{i}-{}", world.channel);
        keyword_policy(&world, &name, "x", "flag").await;
        current.push(name);
    }
    bind_policies(&world, &current[..8]).await;
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({"requiredPolicies":[current[8]]})),
        Some(&world.session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        chat(&world, "go", "combined-union").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(calls.lock().unwrap().len(), 0);
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/keys/{}", world.key_id),
        Some(json!({"requiredPolicies":null})),
        Some(&world.session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    keyword_policy(&world, &name, "x", "flag").await;
    sqlx::query("UPDATE oxsum.guardrail_policies SET config=$2 WHERE name=$1")
        .bind(&name)
        .bind(json!({"patterns":[{"re":"x".repeat(4097),"action":"flag"}]}))
        .execute(world.db.pool())
        .await
        .unwrap();
    bind_policies(&world, &[live, name]).await;
    let response = chat(&world, "go", "legacy-keyword").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(world.seen.lock().unwrap().len(), 0);
    assert_eq!(calls.lock().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_fallback_evidence_refuses_even_when_primary_could_answer() {
    let world = world_or_skip!();
    let price:oxsum_core::Price=serde_json::from_value(json!({"inputPricePerMillion":1,"outputPricePerMillion":1,"maxOutputTokens":100,"rules":[{"match":{"serviceTier":"\u{0001}".repeat(3000)},"price":{"inputPricePerMillion":1,"outputPricePerMillion":1,"maxOutputTokens":100}}]})).unwrap();
    let fallback = format!("fallback-{}", world.channel);
    world
        .db
        .append_price(&world.channel, &fallback, price, 100)
        .await
        .unwrap();
    world
        .db
        .set_fallbacks(&world.model, &[fallback])
        .await
        .unwrap();
    let (endpoint, calls) = start_moderation(json!({"verdict":"allow"})).await;
    let name = format!("route-{}", world.channel);
    moderation_policy(&world, &name, &endpoint).await;
    bind_policies(&world, &[name]).await;
    let response = chat(&world, "go", "oversized-fallback").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(world.seen.lock().unwrap().len(), 0);
    assert_eq!(calls.lock().unwrap().len(), 0);
    let wallet = world.tenants.get(&world.tenant_id).await.unwrap();
    assert_eq!(wallet.reserved().await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_reason_usage_fault_rolls_back_ledger_settlement_and_terminal_state() {
    let world = world_or_skip!();
    let full = "fault \"🦀\n".repeat(300);
    let (endpoint, _) = start_moderation(json!({"verdict":"flag","reason":full})).await;
    let name = format!("fault-{}", world.channel);
    moderation_policy(&world, &name, &endpoint).await;
    bind_policies(&world, &[name]).await;
    // The trigger targets one generated tenant, so parallel suites cannot be
    // affected. It fails at the full-text write after the append and operation CAS.
    let suffix = Uuid::new_v4().simple().to_string();
    let function = format!("guardrail_fault_{suffix}");
    let trigger = format!("guardrail_fault_{suffix}");
    sqlx::raw_sql(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id=TG_ARGV[0] THEN RAISE EXCEPTION 'injected full reason write fault'; END IF; RETURN NEW; END $$; CREATE TRIGGER {trigger} BEFORE INSERT ON oxsum.usage_records FOR EACH ROW EXECUTE FUNCTION oxsum.{function}('{}');",world.tenant_id)).execute(world.db.pool()).await.unwrap();
    let response = chat(&world, "go", "reason-row-fault").await;
    let status = response.status();
    let request = request_id(&response);
    response.into_body().collect().await.unwrap();
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON oxsum.usage_records; DROP FUNCTION oxsum.{function}();"
    ))
    .execute(world.db.pool())
    .await
    .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "the provider response already arrived"
    );
    let wallet = world.tenants.get(&world.tenant_id).await.unwrap();
    let hold = format!("req-{request}:hold");
    assert!(
        wallet
            .receipt_proof(
                oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold)),
                None
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        wallet.reserved().await.unwrap() > 0,
        "the sweeper still owns the original reservation"
    );
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.usage_records WHERE request_id=$1")
            .bind(&request)
            .fetch_one(world.db.pool())
            .await
            .unwrap();
    assert_eq!(rows, 0);
    let state:String=sqlx::query_scalar("SELECT state FROM oxsum.request_operations WHERE key_id=$1 ORDER BY created_at DESC LIMIT 1").bind(Uuid::parse_str(&world.key_id).unwrap()).fetch_one(world.db.pool()).await.unwrap();
    assert!(!["succeeded", "failed", "cancelled", "timed_out"].contains(&state.as_str()));
    let watched: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.open_holds WHERE hold_key=$1)")
            .bind(&hold)
            .fetch_one(world.db.pool())
            .await
            .unwrap();
    assert!(watched);
    assert_eq!(world.seen.lock().unwrap().len(), 1);
    // A different request after removing the fault retains both halves.
    let response = chat(&world, "go", "reason-row-recovered").await;
    assert_eq!(response.status(), StatusCode::OK);
    let recovered = request_id(&response);
    response.into_body().collect().await.unwrap();
    settled(&world, &recovered).await;
    assert_eq!(
        retained_reasons(&world, &recovered).await["guardrails"][0]["reason"],
        full
    );
    check_proof(&world, &recovered, false).await;
}

/// No observed provider counts must not turn a customer refund into a false
/// zero provider-cost claim, even when an upstream price is configured.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refunded_answer_without_provider_usage_keeps_cost_untracked() {
    let world = world_or_skip!();
    keyword_policy(&world, "missing-report", "4111-1111-1111-1111", "block").await;
    let response = chat(&world, "missing-usage-block", "missing-report").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let request = request_id(&response);
    assert_eq!(settled(&world, &request).await["charged"], 0);
    let (cost, basis): (Option<i64>, String) = sqlx::query_as(
        "SELECT upstream_cost_minor,cost_basis FROM oxsum.usage_records WHERE request_id=$1",
    )
    .bind(&request)
    .fetch_one(world.db.pool())
    .await
    .unwrap();
    assert_eq!(cost, None);
    assert_eq!(basis, "missing");
}

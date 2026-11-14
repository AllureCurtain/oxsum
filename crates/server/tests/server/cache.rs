//! Semantic response cache end-to-end tests (issue #497): a real wallet, a
//! scripted upstream, and the cache toggled through the admin API the way an
//! operator writes it.
//!
//! The assertions read what the bill says, not just what the response looked
//! like: a hit answers the stored body with `x-oxsum-cache: hit`, the upstream
//! is never called a second time, and the settlement carries the v10 `cache`
//! block — the proof that the charge is a replay of the entry it names, not a
//! fresh turn the caller is asked to believe.
//!
//! Needs DATABASE_URL; skips without it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::BodyExt;
use oxsum_core::{Db, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// The operator token the admin app in these tests is configured with.
const OPERATOR_TOKEN: &str = "operator-token-0123456789";

// ── the scripted upstream ────────────────────────────────────────────────────

type SemanticHeader = (String, Option<String>);

/// The upstream's state: the answer it gives every model, and what it has been
/// asked — the count is how a test proves a hit never went upstream.
#[derive(Clone, Default)]
struct Script {
    seen: Arc<Mutex<Vec<Value>>>,
    semantic_headers: Arc<Mutex<Vec<SemanticHeader>>>,
}

impl Script {
    fn seen(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

/// Starts the scripted upstream and returns its base URL, as a deployment
/// would configure it. Every completion carries the same usage report so the
/// arithmetic below reads plainly: 10 input, 2 output — and a cached-read
/// rate a tenth of the input's makes the hit's bill visibly cheaper.
async fn start_upstream(script: Script) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .route("/v1/responses", post(responses_upstream))
        .route("/v1/messages", post(messages_upstream))
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

async fn upstream(State(script): State<Script>, axum::Json(body): axum::Json<Value>) -> Response {
    script.seen.lock().unwrap().push(body.clone());
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    axum::Json(json!({
        "id": "chatcmpl-cached",
        "object": "chat.completion",
        "created": 1_700_000_000,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "the cached answer"},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "total_tokens": 12
        }
    }))
    .into_response()
}

async fn responses_upstream(
    State(script): State<Script>,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    script.seen.lock().unwrap().push(body.clone());
    axum::Json(json!({
        "id": "resp-cached",
        "object": "response",
        "model": body["model"],
        "status": "completed",
        "output": [{"type": "message", "role": "assistant", "content": [
            {"type": "output_text", "text": "the responses answer"}
        ]}],
        "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}
    }))
    .into_response()
}

async fn messages_upstream(
    State(script): State<Script>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    script.seen.lock().unwrap().push(body.clone());
    script.semantic_headers.lock().unwrap().push((
        headers["anthropic-version"].to_str().unwrap().to_owned(),
        headers
            .get("anthropic-beta")
            .map(|value| value.to_str().unwrap().to_owned()),
    ));
    axum::Json(json!({
        "id": "msg-cached", "type": "message", "role": "assistant", "model": body["model"],
        "content": [{"type": "text", "text": "the cached message"}],
        "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": 2}
    }))
    .into_response()
}

// ── the world a test runs in ─────────────────────────────────────────────────

/// An app wired to the scripted upstream, one funded organization, and the
/// handles to check what a turn left behind.
struct World {
    app: Router,
    tenants: Tenants,
    tenant_id: String,
    /// The registered organization's id, for the second world's isolation check.
    organization_id: String,
    key: String,
    script: Script,
    channel: String,
    /// The scripted upstream's base URL, resent on every channel `set` — the
    /// endpoint replaces the connection wholesale, and a test that touched
    /// only `semanticCache` must not move it.
    base_url: String,
    /// What its model name is — one per world, since a model belongs to one channel.
    model: String,
    pool: PgPool,
}

impl World {
    /// The ledger of the organization this world registered.
    async fn wallet(&self) -> Arc<oxsum_core::Wallet> {
        self.tenants
            .get(&self.tenant_id)
            .await
            .expect("the organization's ledger opens")
    }

    /// What the settlement entry for a request says, once it is there.
    async fn settlement(&self, request_id: &str) -> Option<Value> {
        let hold_key = format!("req-{request_id}:hold");
        let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
        let bundle = self
            .wallet()
            .await
            .receipt_proof(entry_id, None)
            .await
            .expect("the proof is built")
            .map(|bundle| bundle.entry);
        bundle.map(|entry| {
            serde_json::from_str(entry.description().as_str()).expect("the description is JSON")
        })
    }

    /// Waits for a settlement that happens off the response path.
    async fn settlement_within(&self, request_id: &str) -> Value {
        let deadline = oxsum_testkit::settle_deadline();
        while std::time::Instant::now() < deadline {
            if let Some(record) = self.settlement(request_id).await {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the turn never settled");
    }

    /// Writes this world's channel `semanticCache` through the admin API — the
    /// same call an operator makes, so the test reads what the gateway reads.
    /// The connection fields are resent unchanged: `set` re-credentials and
    /// re-addresses the channel wholesale, and this helper is a settings write,
    /// not a reconfiguration.
    async fn semantic_cache(&self, policy: Value) {
        let app = admin_app(self);
        let (status, body) = call(
            &app,
            "POST",
            "/api/v1/admin/channels",
            Some(json!({
                "name": self.channel,
                "baseUrl": self.base_url,
                "semanticCache": policy,
            })),
            Some(OPERATOR_TOKEN),
        )
        .await;
        assert!(status.is_success(), "the policy write failed: {body}");
    }
}

/// An app over the same database with the operator token configured: the admin
/// surface, as gateway.rs's own copy of the pattern puts it.
fn admin_app(world: &World) -> Router {
    let config = Config::new(Signup::Open, None)
        .with_secret(oxsum_core::SecretKey::from_bytes([7; 32]))
        .with_admin_token(OPERATOR_TOKEN);
    crate::support::app(Db::from_pool(world.pool.clone()), config)
}

macro_rules! world {
    () => {
        match oxsum_testkit::suite_url!() {
            Some(url) => build_world(&url).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

async fn build_world(url: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;

    let suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("cache-{suffix}");
    let model = format!("cached-{suffix}");
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let script = Script::default();
    let base_url = start_upstream(script.clone()).await;
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
    // 1 credit a token fresh, a tenth of that on the cached-read dimension —
    // the hit's bill reads plainly against the miss's.
    let book = oxsum_core::PriceBook::from_json(
        &channel,
        &json!({
            model.clone(): {
                "inputPricePerMillion": 1_000_000,
                "outputPricePerMillion": 1_000_000,
                "maxOutputTokens": 1000,
                "cacheReadPricePerMillion": 100_000,
            }
        })
        .to_string(),
    )
    .expect("the test's own prices parse");
    for (model, price) in book.models() {
        db.append_price(&channel, model, price.clone(), 100)
            .await
            .expect("the price is written");
    }
    let config = Config::new(Signup::Open, None)
        .with_secret(secret)
        .with_admin_token(OPERATOR_TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = crate::support::app(db, config);
    let tenants = Tenants::new(pool.clone());

    let (registration, key) = register(&app).await;
    let tenant_id = registration["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();
    let organization_id = registration["organization"]["id"]
        .as_str()
        .expect("registration names the organization")
        .to_owned();
    let (status, body) = call(
        &app,
        "POST",
        "/__test/funding",
        Some(json!({"idempotencyKey": "top-1", "amountMinor": 100_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the top-up failed: {body}");
    World {
        app,
        tenants,
        tenant_id,
        organization_id,
        key,
        script,
        channel,
        base_url,
        model,
        pool,
    }
}

/// Registers an organization and returns its document and its first API key.
async fn register(app: &Router) -> (Value, String) {
    let email = format!(
        "cache_{}@example.com",
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

/// One JSON call against the API.
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
        .expect("collects the body")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// A chat request, answered with its headers and status — the cache marker
/// rides the head, not the body.
async fn chat(app: &Router, key: &str, body: Value) -> (StatusCode, HeaderMap, Value) {
    gateway(app, key, "/v1/chat/completions", body).await
}

async fn gateway(
    app: &Router,
    key: &str,
    path: &str,
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    gateway_with_headers(app, key, path, body, &[]).await
}

async fn gateway_with_headers(
    app: &Router,
    key: &str,
    path: &str,
    body: Value,
    semantic_headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"));
    for (name, value) in semantic_headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The same ask the tests repeat: one user turn of text.
fn ask(model: &str, text: &str) -> Value {
    json!({"model": model, "messages": [{"role": "user", "content": text}]})
}

/// Waits for the cache row to land: the store write rides a `tokio::spawn`
/// after the settle, so a fast test would otherwise miss on a row still in
/// flight.
async fn row_within(pool: &PgPool, organization_id: &str) -> Value {
    let deadline = oxsum_testkit::settle_deadline();
    while std::time::Instant::now() < deadline {
        let row = sqlx::query(
            "SELECT hit_count, ttl_seconds FROM oxsum.cached_responses \
             WHERE organization_id = $1",
        )
        .bind(uuid::Uuid::parse_str(organization_id).unwrap())
        .fetch_optional(pool)
        .await
        .expect("the row reads");
        if let Some(row) = row {
            use sqlx::Row;
            return json!({
                "hitCount": row.try_get::<i64, _>("hit_count").unwrap(),
                "ttlSeconds": row.try_get::<Option<i64>, _>("ttl_seconds").unwrap(),
            });
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the answer was never cached");
}

/// Wait for the exact source receipt, not an earlier row from this customer.
async fn cached_request(world: &World, path: &str, body: Value) -> Value {
    let (status, headers, response) = gateway(&world.app, &world.key, path, body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert!(headers.get("x-oxsum-cache").is_none(), "expected a miss");
    let request_id = headers["x-oxsum-request-id"].to_str().unwrap();
    cached_source_within(world, request_id).await;
    response
}

async fn cached_source_within(world: &World, request_id: &str) {
    let source = *oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!(
        "req-{request_id}:hold"
    )))
    .as_uuid();
    let deadline = oxsum_testkit::settle_deadline();
    while std::time::Instant::now() < deadline {
        let stored: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.cached_responses WHERE source_entry_id=$1)",
        )
        .bind(source)
        .fetch_one(&world.pool)
        .await
        .unwrap();
        if stored {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("this response was never cached");
}

#[tokio::test]
async fn anthropic_version_and_beta_are_part_of_the_forwarded_cache_context() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let db = Db::from_pool(world.pool.clone());
    db.set_channel_pool_policy(
        &world.channel,
        &world.base_url,
        Some(&["anthropic-upstream-secret".to_owned()]),
        "anthropic",
        &oxsum_core::SecretKey::from_bytes([7; 32]),
        Some(&oxsum_core::ProviderDestinationPolicy {
            network: oxsum_core::ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }),
    )
    .await
    .unwrap();
    let body = json!({"model": world.model, "messages": [{"role": "user", "content": "hello"}], "max_tokens": 8});
    cached_request(&world, "/v1/messages", body.clone()).await;
    let (status, headers, response) = gateway_with_headers(
        &world.app,
        &world.key,
        "/v1/messages",
        body.clone(),
        &[("anthropic-version", "2023-06-01")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(headers["x-oxsum-cache"], "hit");
    let variants = [
        vec![("anthropic-beta", "feature-a")],
        vec![("anthropic-beta", "feature-b")],
        vec![
            ("anthropic-version", "2024-01-01"),
            ("anthropic-beta", "feature-b"),
        ],
    ];
    for semantic_headers in variants {
        let (status, headers, response) = gateway_with_headers(
            &world.app,
            &world.key,
            "/v1/messages",
            body.clone(),
            &semantic_headers,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert!(headers.get("x-oxsum-cache").is_none());
        cached_source_within(&world, headers["x-oxsum-request-id"].to_str().unwrap()).await;
        let (status, headers, response) = gateway_with_headers(
            &world.app,
            &world.key,
            "/v1/messages",
            body.clone(),
            &semantic_headers,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(headers["x-oxsum-cache"], "hit");
    }
    assert_eq!(world.script.seen().len(), 4);
    assert_eq!(
        *world.script.semantic_headers.lock().unwrap(),
        vec![
            ("2023-06-01".into(), None),
            ("2023-06-01".into(), Some("feature-a".into())),
            ("2023-06-01".into(), Some("feature-b".into())),
            ("2024-01-01".into(), Some("feature-b".into())),
        ]
    );
}

#[tokio::test]
async fn current_guardrails_recheck_cached_requests_and_answers_without_charging_blocks() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let body = ask(&world.model, "hello");
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    let before = world.wallet().await.available().await.unwrap();
    for pattern in ["the cached answer", "hello"] {
        let policy = format!("guard-{}", world.channel);
        let (status, response) = call(
            &world.app,
            "PUT",
            &format!("/api/v1/admin/guardrail-policies/{policy}"),
            Some(json!({"kind": "keyword", "config": {"patterns": [{"re": pattern, "action": "block"}]}})),
            Some(OPERATOR_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let (status, response) = call(
            &world.app,
            "PUT",
            &format!("/api/v1/admin/guardrail-bindings/channel/{}", world.channel),
            Some(json!({"policies": [policy]})),
            Some(OPERATOR_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let (status, _, response) = chat(&world.app, &world.key, body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
        assert_eq!(world.script.seen().len(), 1);
        assert_eq!(world.wallet().await.available().await.unwrap(), before);
    }
}

// ── the tests ────────────────────────────────────────────────────────────────

/// The whole feature in one pass: a channel opted in re-serves a repeat ask —
/// the second call answers the stored body with the hit marker, upstream is
/// asked once, and the hit's settlement is a v10 record whose `cache` block
/// names the miss's own entry. The bill proves the reuse it charges for.
#[tokio::test]
async fn a_repeat_ask_is_served_from_the_cache_and_billed_as_a_hit() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;

    let (status, miss_headers, miss_body) =
        chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
    assert_eq!(status, StatusCode::OK, "the miss failed: {miss_body}");
    let miss_request = miss_headers
        .get("x-oxsum-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("the miss names its request")
        .to_owned();
    let _ = row_within(&world.pool, &world.organization_id).await;

    // The same exact question: the complete request body is the key, so the
    // second ask repeats it byte for byte.
    let (status, headers, hit_body) =
        chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
    assert_eq!(status, StatusCode::OK, "the hit failed: {hit_body}");
    assert_eq!(
        headers.get("x-oxsum-cache").and_then(|v| v.to_str().ok()),
        Some("hit"),
        "a repeat ask answers with the hit marker"
    );
    assert_eq!(
        hit_body["choices"][0]["message"]["content"], "the cached answer",
        "the stored body is what answers"
    );
    assert_eq!(
        world.script.seen().len(),
        1,
        "upstream was asked once — the hit never went"
    );

    let hit_request = headers
        .get("x-oxsum-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("the hit names its request")
        .to_owned();
    let hit = world.settlement_within(&hit_request).await;
    assert_eq!(hit["v"], 15, "the hit settles under the current version");
    assert_eq!(hit["kind"], "usage");
    let cache = &hit["cache"];
    assert_eq!(
        cache["similarity"], 1.0,
        "an exact-digest hit claims the honest score"
    );
    assert_eq!(
        hit["upstreamAttempts"], 0,
        "the bill says nothing went upstream"
    );
    // The usage is the original turn's, repriced at the cache-read rate: the
    // input lines split 0 fresh + 10 cached at a tenth of the input price —
    // 10 × 0.1 + 2 × 1 = 3 minor against the miss's 12.
    assert_eq!(hit["charged"], 3, "the hit bills the cache-read rate");

    // And `sourceEntryId` names the miss's own settlement entry — derived
    // from the miss's hold key, which its request id spells.
    let miss_entry = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!(
        "req-{miss_request}:hold"
    )));
    assert_eq!(
        cache["sourceEntryId"].as_str().unwrap(),
        miss_entry.as_uuid().to_string(),
        "the hit names the miss's settlement entry"
    );

    // The usage row carries the same provenance in its details: a reconciler
    // sees the re-served answer without re-parsing the description.
    let details: Value =
        sqlx::query_scalar("SELECT usage_details FROM oxsum.usage_records WHERE request_id = $1")
            .bind(&hit_request)
            .fetch_one(&world.pool)
            .await
            .expect("the hit left its usage row");
    assert_eq!(
        details["cache"],
        json!({
            "sourceEntryId": miss_entry.as_uuid().to_string(),
            "similarity": 1.0,
            "cachedAnswer": true,
        }),
        "the usage row names the turn its answer was re-served from"
    );
}

/// Quota provenance and cache provenance must coexist in the same immutable
/// receipt, even though together they exceed the engine's former 512-char cap.
#[tokio::test]
async fn a_quota_funded_cache_hit_commits_complete_evidence() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let db = Db::from_pool(world.pool.clone());
    let organization = world.organization_id.parse().unwrap();
    let plan = db
        .set_subscription_plan(
            &format!("cache-quota-{}", uuid::Uuid::new_v4()),
            Some(1_000_000),
            oxsum_core::ResetPeriod::Monthly,
            false,
            None,
            None,
            true,
            0,
        )
        .await
        .unwrap();
    let subscription = db
        .bind_subscription(organization, plan.plan_id, "admin", None)
        .await
        .unwrap();
    let wallet = world.wallet().await;
    let before = wallet.available().await.unwrap();
    let request = ask(&world.model, "the quota-funded answer");
    let (status, headers, _) = chat(&world.app, &world.key, request.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let source_request = headers["x-oxsum-request-id"].to_str().unwrap();
    world.settlement_within(source_request).await;
    cached_source_within(&world, source_request).await;
    let source = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&format!(
        "req-{source_request}:hold"
    )));
    let (status, headers, _) = chat(&world.app, &world.key, request).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-oxsum-cache"], "hit");
    let request_id = headers["x-oxsum-request-id"].to_str().unwrap();
    let record = world.settlement_within(request_id).await;
    assert_eq!(record["charged"], 3);
    assert_eq!(record["subscription"]["chargedMinor"], 3);
    assert_eq!(
        record["subscription"]["subscriptionId"],
        subscription.subscription_id.to_string()
    );
    assert_eq!(
        record["cache"]["sourceEntryId"],
        source.as_uuid().to_string()
    );
    assert_eq!(record["upstreamAttempts"], 0);
    let hold_key = format!("req-{request_id}:hold");
    let proof = wallet
        .receipt_proof(
            oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key)),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let description = proof.entry.description().as_str();
    assert!(description.chars().count() > 512);
    assert!(oxsum_core::SettlementRecord::parse(description).is_some());
    assert!(
        oxsum_core::verify_bundle(
            &serde_json::to_string(&proof).unwrap(),
            &proof.entry.content_hash()
        )
        .unwrap()
    );
    assert_eq!(
        oxsum_verify::verify_charge(description),
        oxsum_verify::ChargeCheck::Recomputed
    );
    assert_eq!(world.script.seen().len(), 1);
    assert_eq!(wallet.available().await.unwrap(), before);
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(
        db.active_subscription(organization)
            .await
            .unwrap()
            .unwrap()
            .consumed_minor,
        15
    );
    let usage: (i64, String) = sqlx::query_as(
        "SELECT charged_minor,cost_basis FROM oxsum.usage_records WHERE request_id=$1",
    )
    .bind(request_id)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert_eq!(usage, (3, "cache_reuse".into()));
    let watches: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.open_holds WHERE tenant_id=$1")
            .bind(&world.tenant_id)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert_eq!(watches, 0);
}

/// The v2 digest binds the complete request body: case, whitespace, tools and
/// generation parameters are all part of the question.
#[tokio::test]
async fn the_complete_request_body_is_the_cache_key() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let ask_with = |text: &str, temperature: i64, tools: bool| {
        let mut body = json!({
            "model": world.model,
            "temperature": temperature,
            "messages": [{"role": "user", "content": text}],
        });
        if tools {
            body["tools"] = json!([{
                "type": "function",
                "function": {"name": "lookup", "parameters": {"type": "object"}},
            }]);
        }
        body
    };
    chat(&world.app, &world.key, ask_with("hello?", 0, false)).await;
    let _ = row_within(&world.pool, &world.organization_id).await;

    let (_, headers, _) = chat(&world.app, &world.key, ask_with("  HELLO?  ", 0, false)).await;
    assert!(
        headers.get("x-oxsum-cache").is_none(),
        "text is exact in v2"
    );

    let (_, headers, _) = chat(&world.app, &world.key, ask_with("hello?", 1, false)).await;
    assert!(
        headers.get("x-oxsum-cache").is_none(),
        "a different generation parameter is a different question"
    );

    cached_request(&world, "/v1/chat/completions", ask_with("hello?", 0, true)).await;

    assert_eq!(world.script.seen().len(), 4);
    // An exact repeat of the last body hits the row that body wrote.
    let (_, headers, _) = chat(&world.app, &world.key, ask_with("hello?", 0, true)).await;
    assert_eq!(
        headers.get("x-oxsum-cache").and_then(|v| v.to_str().ok()),
        Some("hit")
    );
    assert_eq!(world.script.seen().len(), 4);
}

#[tokio::test]
async fn chat_and_responses_cannot_share_even_identical_forwarded_bodies() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let body = json!({
        "model": world.model,
        "messages": [{"role": "user", "content": "hello"}],
        "input": "hello",
        "max_tokens": 8,
        "max_output_tokens": 8,
    });
    let chat_body = cached_request(&world, "/v1/chat/completions", body.clone()).await;
    assert_eq!(chat_body["object"], "chat.completion");
    let response_body = cached_request(&world, "/v1/responses", body.clone()).await;
    assert_eq!(response_body["object"], "response");
    let seen = world.script.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], seen[1], "the endpoint is the distinguishing input");
    for (path, expected) in [
        ("/v1/chat/completions", "chat.completion"),
        ("/v1/responses", "response"),
    ] {
        let (status, headers, answer) = gateway(&world.app, &world.key, path, body.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["x-oxsum-cache"], "hit");
        assert_eq!(answer["object"], expected);
    }
    assert_eq!(world.script.seen().len(), 2);
}

#[tokio::test]
async fn credential_changes_invalidate_cache_and_disabling_the_pool_stops_hits() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let db = Db::from_pool(world.pool.clone());
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let original = db
        .channel_credentials(&world.channel)
        .await
        .unwrap()
        .unwrap()[0]
        .credential_id;
    let body = ask(&world.model, "credential context");
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    let added = db
        .add_credential(&world.channel, "replacement-upstream-secret", &secret)
        .await
        .unwrap();
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    db.set_credential_status(&world.channel, original, "disabled")
        .await
        .unwrap();
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    db.drop_credential(&world.channel, original).await.unwrap();
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    assert_eq!(world.script.seen().len(), 4);
    let balance = world.wallet().await.available().await.unwrap();
    db.set_credential_status(&world.channel, added.credential_id, "disabled")
        .await
        .unwrap();
    let (status, headers, _) = chat(&world.app, &world.key, body).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(headers.get("x-oxsum-cache").is_none());
    assert_eq!(world.script.seen().len(), 4);
    assert_eq!(world.wallet().await.available().await.unwrap(), balance);
}

#[tokio::test]
async fn fallback_target_and_route_changes_invalidate_a_primary_answer() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let db = Db::from_pool(world.pool.clone());
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let fallback = format!("{}-fallback", world.channel);
    db.set_channel_pool_policy(
        &fallback,
        &world.base_url,
        Some(&["fallback-secret".to_owned()]),
        "openai",
        &secret,
        Some(&oxsum_core::ProviderDestinationPolicy {
            network: oxsum_core::ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }),
    )
    .await
    .unwrap();
    let targets = [format!("{}-a", world.model), format!("{}-b", world.model)];
    let alias = format!("{}-alias", world.model);
    let price: oxsum_core::Price = serde_json::from_value(json!({
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 1_000_000,
        "maxOutputTokens": 1000,
    }))
    .unwrap();
    for target in &targets {
        db.append_price(&fallback, target, price.clone(), 100)
            .await
            .unwrap();
    }
    db.create_alias(&fallback, &alias, &targets[0])
        .await
        .unwrap();
    db.set_fallbacks(&world.model, std::slice::from_ref(&alias))
        .await
        .unwrap();
    let body = ask(&world.model, "fallback config");
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    db.delete_alias(&fallback, &alias).await.unwrap();
    db.create_alias(&fallback, &alias, &targets[1])
        .await
        .unwrap();
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    db.append_price(&fallback, &targets[1], price, 101)
        .await
        .unwrap();
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    let (status, headers, _) = chat(&world.app, &world.key, body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-oxsum-cache"], "hit");
    assert_eq!(world.script.seen().len(), 3);
}

#[tokio::test]
async fn a_changed_permission_snapshot_misses_and_revocation_refuses_a_cached_ask() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let db = Db::from_pool(world.pool.clone());
    let (org, key) = db.authenticate(&world.key).await.unwrap().unwrap();
    let body = ask(&world.model, "permission snapshot");
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    db.update_key_constraints(
        org.id,
        key.key_id,
        oxsum_core::KeyScope::All,
        oxsum_core::KeyConstraints {
            model_allowlist: Some(vec![world.model.clone()]),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    cached_request(&world, "/v1/chat/completions", body.clone()).await;
    db.update_key_constraints(
        org.id,
        key.key_id,
        oxsum_core::KeyScope::All,
        oxsum_core::KeyConstraints {
            model_allowlist: Some(vec!["excluded-model".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let balance = world.wallet().await.available().await.unwrap();
    let (status, headers, _) = chat(&world.app, &world.key, body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(headers.get("x-oxsum-cache").is_none());
    assert_eq!(world.script.seen().len(), 2);
    assert_eq!(world.wallet().await.available().await.unwrap(), balance);
}

/// A channel that never opted in caches nothing: two identical asks both go
/// upstream, no marker rides the answer, and no row is written.
#[tokio::test]
async fn a_channel_without_the_policy_never_caches() {
    let world = world!();
    for _ in 0..2 {
        let (status, headers, body) =
            chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
        assert_eq!(status, StatusCode::OK, "the call failed: {body}");
        assert!(headers.get("x-oxsum-cache").is_none());
    }
    assert_eq!(world.script.seen().len(), 2, "every ask went upstream");
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.cached_responses WHERE organization_id = $1",
    )
    .bind(uuid::Uuid::parse_str(&world.organization_id).unwrap())
    .fetch_one(&world.pool)
    .await
    .expect("the count reads");
    assert_eq!(rows, 0, "nothing was cached");
}

/// A channel that disables the policy stops hitting: the row stays — an admin
/// flipping the switch off is not a purge — but the second ask goes upstream
/// like the first did.
#[tokio::test]
async fn disabling_the_policy_stops_the_hits() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
    let _ = row_within(&world.pool, &world.organization_id).await;

    world.semantic_cache(json!({"enabled": false})).await;
    let (status, headers, body) = chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
    assert_eq!(status, StatusCode::OK, "the call failed: {body}");
    assert!(headers.get("x-oxsum-cache").is_none());
    assert_eq!(world.script.seen().len(), 2, "a disabled cache is a miss");
}

/// A TTL-bound row serves hits inside its window and is gone past it: the
/// third ask, after the row's second has elapsed, reaches upstream again.
#[tokio::test]
async fn a_row_older_than_its_ttl_is_a_miss() {
    let world = world!();
    world
        .semantic_cache(json!({"enabled": true, "ttlSeconds": 1}))
        .await;
    chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
    let _ = row_within(&world.pool, &world.organization_id).await;

    // Inside the window the repeat hits; past it, the row is deleted on the
    // way out and the ask goes upstream fresh.
    let (_, headers, _) = chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
    assert_eq!(
        headers.get("x-oxsum-cache").and_then(|v| v.to_str().ok()),
        Some("hit")
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (_, headers, _) = chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
    assert!(
        headers.get("x-oxsum-cache").is_none(),
        "an expired row cannot serve"
    );
    assert_eq!(world.script.seen().len(), 2);
}

/// `excludeSystem` is accepted for compatibility but ignored: the complete
/// request is the key, so the system preamble always participates.
#[tokio::test]
async fn exclude_system_is_accepted_but_ignored() {
    let world = world!();
    world
        .semantic_cache(json!({"enabled": true, "excludeSystem": true}))
        .await;
    chat(
        &world.app,
        &world.key,
        json!({"model": world.model, "messages": [
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "hello?"},
        ]}),
    )
    .await;
    let _ = row_within(&world.pool, &world.organization_id).await;

    let (_, headers, _) = chat(
        &world.app,
        &world.key,
        json!({"model": world.model, "messages": [
            {"role": "system", "content": "be verbose instead"},
            {"role": "user", "content": "hello?"},
        ]}),
    )
    .await;
    assert!(
        headers.get("x-oxsum-cache").is_none(),
        "a different preamble is a different question even under excludeSystem"
    );
    assert_eq!(world.script.seen().len(), 2);
    let _ = row_within(&world.pool, &world.organization_id).await;

    // The exact original body still hits the row it wrote.
    let (_, headers, _) = chat(
        &world.app,
        &world.key,
        json!({"model": world.model, "messages": [
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "hello?"},
        ]}),
    )
    .await;
    assert_eq!(
        headers.get("x-oxsum-cache").and_then(|v| v.to_str().ok()),
        Some("hit"),
        "an exact repeat hits"
    );
    assert_eq!(world.script.seen().len(), 2);
}

/// The org boundary is the privacy rule: a second organization's identical
/// ask on the same channel is a miss, never the first's answer.
#[tokio::test]
async fn a_hit_never_crosses_organizations() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    chat(&world.app, &world.key, ask(&world.model, "hello?")).await;
    let _ = row_within(&world.pool, &world.organization_id).await;

    // A second organization on the same deployment, funded and keyed.
    let (registration, other_key) = register(&world.app).await;
    let (status, body) = call(
        &world.app,
        "POST",
        "/__test/funding",
        Some(json!({"idempotencyKey": "top-other", "amountMinor": 100_000_000})),
        Some(&other_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the other top-up failed: {body}");
    let other_org = registration["organization"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, headers, body) = chat(&world.app, &other_key, ask(&world.model, "hello?")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the other org's call failed: {body}"
    );
    assert!(
        headers.get("x-oxsum-cache").is_none(),
        "one organization's answer never serves another's"
    );
    assert_eq!(world.script.seen().len(), 2, "the second org went upstream");
    // And its answer was cached under its own org, so the same ask from it hits.
    let _ = row_within(&world.pool, &other_org).await;
    let (_, headers, _) = chat(&world.app, &other_key, ask(&world.model, "hello?")).await;
    assert_eq!(
        headers.get("x-oxsum-cache").and_then(|v| v.to_str().ok()),
        Some("hit"),
        "the other org's own repeat hits its own row"
    );
}

/// A streamed ask never reads or writes the store: the cache holds whole
/// bodies, and an SSE client cannot be handed one.
#[tokio::test]
async fn a_stream_is_never_cached() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    for _ in 0..2 {
        let (status, _, body) = chat(
            &world.app,
            &world.key,
            json!({"model": world.model, "stream": true,
                   "messages": [{"role": "user", "content": "hello?"}]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the stream failed: {body}");
    }
    assert_eq!(world.script.seen().len(), 2, "every stream went upstream");
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.cached_responses WHERE organization_id = $1",
    )
    .bind(uuid::Uuid::parse_str(&world.organization_id).unwrap())
    .fetch_one(&world.pool)
    .await
    .expect("the count reads");
    assert_eq!(rows, 0, "a streamed answer is not stored");
}

#[tokio::test]
async fn cached_response_does_not_cross_sibling_keys_or_legacy_digests() {
    let world = world!();
    world.semantic_cache(json!({"enabled": true})).await;
    let (status, _, _) = chat(&world.app, &world.key, ask(&world.model, "private answer")).await;
    assert_eq!(status, StatusCode::OK);
    let _ = row_within(&world.pool, &world.organization_id).await;
    let db = Db::from_pool(world.pool.clone());
    let sibling = db
        .create_key(
            world.organization_id.parse().unwrap(),
            None,
            None,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let (status, headers, _) = chat(
        &world.app,
        &sibling.secret,
        ask(&world.model, "private answer"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("x-oxsum-cache").is_none());
    assert_eq!(world.script.seen().len(), 2);
    let (_, headers, _) = chat(&world.app, &world.key, ask(&world.model, "private answer")).await;
    assert_eq!(headers["x-oxsum-cache"], "hit");
    // A pre-upgrade row was keyed by an old normalized-prompt digest. The
    // upgrade deletes those rows; even an old-format value inserted by hand
    // can never match a v2 keyed digest.
    let legacy = "0".repeat(64);
    sqlx::query("UPDATE oxsum.cached_responses SET prompt_digest=$2 WHERE cache_id=(SELECT cache_id FROM oxsum.cached_responses WHERE organization_id=$1 LIMIT 1)")
        .bind(world.organization_id.parse::<uuid::Uuid>().unwrap()).bind(legacy).execute(&world.pool).await.unwrap();
    let (status, headers, _) =
        chat(&world.app, &world.key, ask(&world.model, "legacy answer")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("x-oxsum-cache").is_none());
    assert_eq!(world.script.seen().len(), 3);
}

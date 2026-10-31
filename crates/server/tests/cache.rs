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

#![allow(clippy::unwrap_used, clippy::expect_used)]

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

/// The upstream's state: the answer it gives every model, and what it has been
/// asked — the count is how a test proves a hit never went upstream.
#[derive(Clone, Default)]
struct Script {
    seen: Arc<Mutex<Vec<Value>>>,
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
        for _ in 0..250 {
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
    oxsum_server::app(Db::from_pool(world.pool.clone()), config)
}

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("cache")
}

macro_rules! world {
    () => {
        match url() {
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

    let suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("cache-{suffix}");
    let model = format!("cached-{suffix}");
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let script = Script::default();
    let base_url = start_upstream(script.clone()).await;
    db.set_channel(&channel, &base_url, "upstream-secret", "openai", &secret)
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
    let app = oxsum_server::app(db, config);
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
        "/api/v1/topups",
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
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
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
    for _ in 0..250 {
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

    // The same words, differently cased and spaced — the digest normalizes
    // both away, so the second ask is the same question.
    let (status, headers, hit_body) =
        chat(&world.app, &world.key, ask(&world.model, "  HELLO?  ")).await;
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
    assert_eq!(
        hit["v"], 10,
        "the hit settles under the cache-aware version"
    );
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

/// `excludeSystem` folds the preamble out of the question: two asks that
/// differ only in `system` text still digest to the same row.
#[tokio::test]
async fn exclude_system_drops_the_preamble_from_the_digest() {
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
    assert_eq!(
        headers.get("x-oxsum-cache").and_then(|v| v.to_str().ok()),
        Some("hit"),
        "a different preamble still hits under excludeSystem"
    );
    assert_eq!(world.script.seen().len(), 1);
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
        "/api/v1/topups",
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

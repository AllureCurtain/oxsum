//! Coordinated admission and settlement (issue #603, roadmap T12): the
//! customer/key/member budget coordination, the operation row's crash
//! recovery, the day-close watermark, the typed zero-cost receipt a fully
//! entitlement-funded turn settles with, and a sale-priced turn's committed
//! digest.
//!
//! The assertions are the invariants the coordination exists to keep —
//! balance, ledger and operation state — not only response statuses: a member
//! budget's committed spend never crosses its integer cap under racing keys,
//! a cancelled or crashed turn releases its whole reservation, a day-close
//! waits for the operation watermark, and an empty wallet under a full
//! subscription draw settles a receipt rather than a fabricated hold.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
mod support;

#[path = "support/funded_admission.rs"]
mod funded_admission;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

/// The operator token these tests configure the admin surface with.
const TOKEN: &str = "operator-token-0123456789";
const PASSWORD: &str = "correct horse battery";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("coordinated")
}

macro_rules! world_or_skip {
    ($top_up:expr) => {
        match url() {
            Some(u) => world(&u, $top_up).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

// ── the scripted upstream ────────────────────────────────────────────────────

/// What upstream answers for one model.
#[derive(Clone)]
enum Answer {
    /// A whole completion with a fixed usage report.
    Completion { input: i64, output: i64 },
    /// A stream that emits one partial chunk and then stays open — the shape
    /// a mid-stream cancellation needs.
    Stall,
}

/// The upstream's state: what to answer, and what it saw.
#[derive(Clone, Default)]
struct Script {
    answers: Arc<Mutex<HashMap<String, Answer>>>,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl Script {
    fn answers(&self, model: &str, answer: Answer) {
        self.answers
            .lock()
            .unwrap()
            .insert(model.to_owned(), answer);
    }
    fn seen(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

async fn upstream(State(script): State<Script>, axum::Json(body): axum::Json<Value>) -> Response {
    script.seen.lock().unwrap().push(body.clone());
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    let answer = script.answers.lock().unwrap().get(&model).cloned();
    match answer {
        Some(Answer::Completion { input, output }) => Json(json!({
            "id": "chatcmpl-coord",
            "object": "chat.completion",
            "created": 1,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": input, "completion_tokens": output, "total_tokens": input + output},
        }))
        .into_response(),
        Some(Answer::Stall) => (
            StatusCode::OK,
            [("content-type", "text/event-stream")],
            Body::from_stream(async_stream::stream! {
                yield Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from_static(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}],\"usage\":null}\n\n",
                ));
                // Never finishes: the client stays connected until it gives up.
                std::future::pending::<()>().await;
            }),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {"message": "no script for this model"}})),
        )
            .into_response(),
    }
}

async fn start_upstream(script: Script) -> String {
    let app = Router::new()
        .route("/v1/chat/completions", post(upstream))
        .with_state(script);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("the upstream binds");
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    address
}

// ── the world ────────────────────────────────────────────────────────────────

struct World {
    app: Router,
    pool: PgPool,
    db: Db,
    tenants: Tenants,
    organization_id: String,
    tenant_id: String,
    /// The registering member's user id — `PATCH …/members/{user}` budgets.
    user_id: String,
    key: String,
    /// The registering member's session — keys minted under it carry
    /// `created_by` this member, which the member budget denominates.
    session: String,
    model: String,
    /// A second model priced by the token, for the over-bound settle.
    over_model: String,
    script: Script,
}

/// An app over a real database: one registered organization topped up with
/// `top_up` minor, one channel whose model answers a scripted completion.
/// The price is flat 40 minor per request — the freeze and the charge are the
/// same figure, which is what the budget-invariant arithmetic wants.
async fn world(database_url: &str, top_up: i64) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(12)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;

    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("coord-{suffix}");
    let model = format!("turn-{suffix}");
    let secret = SecretKey::from_bytes([7; 32]);
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
    db.append_price(
        &channel,
        &model,
        oxsum_core::Price {
            input_price_per_million: 0,
            output_price_per_million: 0,
            max_output_tokens: 1,
            cache_read_price_per_million: None,
            cache_write_5m_price_per_million: None,
            cache_write_1h_price_per_million: None,
            reasoning_price_per_million: None,
            cost_per_request: Some(40),
            upstream: None,
            mode: oxsum_core::BillingMode::Chat,
            rules: vec![],
        },
        100,
    )
    .await
    .expect("the price is written");
    // A second model priced by the token: its freeze admits up to the output
    // ceiling, so a report over the bound exercises the `capped` settle.
    let over_model = format!("over-{suffix}");
    db.append_price(
        &channel,
        &over_model,
        oxsum_core::Price {
            input_price_per_million: 0,
            output_price_per_million: 1_000_000,
            max_output_tokens: 5,
            cache_read_price_per_million: None,
            cache_write_5m_price_per_million: None,
            cache_write_1h_price_per_million: None,
            reasoning_price_per_million: None,
            cost_per_request: Some(40),
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
    let app = support::app(db.clone(), config);

    let email = format!(
        "coord_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let session = support::login(&app, &email, PASSWORD).await;
    let data = &body["data"];
    let key = data["apiKey"]["secret"].as_str().unwrap().to_owned();
    let organization_id = data["organization"]["id"].as_str().unwrap().to_owned();
    let tenant_id = data["organization"]["tenantId"]
        .as_str()
        .unwrap()
        .to_owned();
    let user_id = data["user"]["id"].as_str().unwrap().to_owned();
    if top_up > 0 {
        let (status, body) = call(
            &app,
            "POST",
            "/__test/funding",
            Some(json!({"idempotencyKey": format!("top-{suffix}"), "amountMinor": top_up})),
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "top-up failed: {body}");
    }

    World {
        app,
        pool: pool.clone(),
        db,
        tenants: Tenants::new(pool),
        organization_id,
        tenant_id,
        user_id,
        key,
        session,
        model,
        over_model,
        script,
    }
}

impl World {
    /// The organization's ledger.
    async fn wallet(&self) -> Arc<oxsum_core::Wallet> {
        self.tenants
            .get(&self.tenant_id)
            .await
            .expect("the organization's ledger opens")
    }

    /// The settlement entry's description for a request, parsed — the bill's
    /// own record, once it landed.
    async fn settlement(&self, request_id: &str) -> Option<Value> {
        let hold_key = format!("req-{request_id}:hold");
        let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
        let text = self
            .wallet()
            .await
            .receipt_proof(entry_id, None)
            .await
            .expect("the proof is built")
            .map(|bundle| bundle.entry.description().as_str().to_owned());
        text.map(|t| serde_json::from_str(&t).expect("the description is JSON"))
    }

    /// Waits for a settlement that happens off the request path.
    async fn settlement_within(&self, request_id: &str) -> Value {
        for _ in 0..250 {
            if let Some(record) = self.settlement(request_id).await {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the turn never settled");
    }

    /// The operation row for a request id — `operation_id` is the request id.
    async fn operation(&self, request_id: &str) -> Option<sqlx::postgres::PgRow> {
        sqlx::query(
            "SELECT state, charged_minor, outcome_kind, hold_key FROM oxsum.request_operations \
             WHERE operation_id = $1",
        )
        .bind(Uuid::parse_str(request_id).unwrap())
        .fetch_optional(&self.pool)
        .await
        .expect("the operation row reads")
    }
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
        request = support::credential(request, key);
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

/// A chat request against the gateway, credentialed by `key`. Returns the raw
/// response so a test can stream or drop the body.
async fn chat(app: &Router, key: &str, model: &str, extra: Value) -> Response {
    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}],
    });
    if let Some(map) = extra.as_object() {
        for (name, value) in map {
            body[name] = value.clone();
        }
    }
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the gateway answers")
}

fn request_id(response: &Response) -> String {
    response
        .headers()
        .get("x-oxsum-request-id")
        .expect("the answer names its request")
        .to_str()
        .unwrap()
        .to_owned()
}

/// Mint a consumption key under the member's own session — `created_by` is
/// what the member budget denominates, so the racing keys have to be theirs.
async fn mint_key(app: &Router, session: &str, name: &str) -> String {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": name})),
        Some(session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "key mint failed: {body}");
    body["data"]["secret"].as_str().unwrap().to_owned()
}

/// A plan through the admin surface.
async fn set_plan(app: &Router, name: &str, body: Value) -> (StatusCode, Value) {
    admin(
        app,
        "PUT",
        &format!("/api/v1/admin/subscription-plans/{name}"),
        Some(body),
    )
    .await
}

/// Bind a plan to this world's organization.
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

// ── A11: the member budget's integer cap under concurrent keys ───────────────

/// Five of one member's keys race the same flat-priced turn under a 100-minor
/// member budget: each hold commits 40 (flat fee, freeze == charge), so at
/// most two may be in flight or settled at once. The ledger, not the
/// response, is the assertion — the member's committed spend across every key
/// they minted never crosses the cap, whatever order the holds landed in.
#[tokio::test]
async fn a_member_budget_caps_concurrent_keys_at_the_integer_bound() {
    let world = world_or_skip!(100_000);

    // The member budget: 100 minor across every key the member minted.
    let (status, body) = call(
        &world.app,
        "PATCH",
        &format!("/api/v1/org/members/{}", world.user_id),
        Some(json!({"budgetLimitMinor": 100})),
        Some(&world.session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 1,
            output: 1,
        },
    );

    // Five keys of the same member, one racing turn each: 5 × 40 = 200 asked
    // against a 100 cap — the integer bound admits floor(100/40) = 2.
    let keys: Vec<String> = {
        let mut keys = vec![];
        for n in 0..5 {
            keys.push(mint_key(&world.app, &world.session, &format!("race-{n}")).await);
        }
        keys
    };
    let mut turns = Vec::new();
    for key in &keys {
        let app = world.app.clone();
        let key = key.clone();
        let model = world.model.clone();
        turns.push(tokio::spawn(async move {
            let response = chat(&app, &key, &model, json!({})).await;
            let status = response.status();
            let id = response
                .headers()
                .get("x-oxsum-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let _ = response.into_body().collect().await;
            (status, id)
        }));
    }
    let mut admitted = 0;
    let mut refused = 0;
    let mut ids = Vec::new();
    for turn in turns {
        let (status, id) = turn.await.unwrap();
        match status {
            StatusCode::OK => {
                admitted += 1;
                ids.push(id.unwrap());
            }
            StatusCode::TOO_MANY_REQUESTS | StatusCode::PAYMENT_REQUIRED => refused += 1,
            other => panic!("an admission answered {other}"),
        }
    }
    assert_eq!(admitted, 2, "the cap admits two 40-minor holds of 100");
    assert_eq!(refused, 3);

    // The ledger's own figure for the member: settled charges plus open
    // holds across every key they minted, never over the cap — the integer
    // invariant the per-member row lock exists to keep. `member_committed`
    // is the same ledger-side sum the hold's cap check runs.
    for id in &ids {
        world.settlement_within(id).await;
    }
    let organization_id: Uuid = world.organization_id.parse().unwrap();
    let member: Uuid = world.user_id.parse().unwrap();
    let committed = world
        .wallet()
        .await
        .member_committed(&organization_id, &member)
        .await
        .expect("the member's committed spend reads");
    assert!(
        (0..=100).contains(&committed),
        "the member's committed spend stayed inside the cap: {committed}"
    );
    assert_eq!(committed, 80, "two settled flat-fee turns: {committed}");

    // Every operation row agrees: two settled, none left dangling.
    let states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM oxsum.request_operations WHERE organization_id = $1")
            .bind(organization_id)
            .fetch_all(&world.pool)
            .await
            .unwrap();
    assert_eq!(
        states.iter().filter(|s| *s == "succeeded").count(),
        2,
        "{states:?}"
    );
}

// ── A12: cancellation, excessive usage, missing usage, crash recovery ───────

/// A client that goes away mid-stream: the turn settles `client_cancelled`,
/// the whole reservation releases, and the operation goes terminal
/// `cancelled` — the ledger and the row agree on what was delivered.
#[tokio::test]
async fn a_midstream_cancellation_settles_and_releases_the_reservation() {
    let world = world_or_skip!(1_000_000);
    world.script.answers(&world.model, Answer::Stall);

    let response = chat(
        &world.app,
        &world.key,
        &world.model,
        json!({"stream": true}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let mut body = response.into_body();
    let frame = body
        .frame()
        .await
        .expect("upstream sent a frame")
        .expect("the frame is readable");
    assert!(
        String::from_utf8_lossy(&frame.into_data().expect("the frame is data")).contains("partial")
    );
    // The client goes away: hyper drops the body, the turn settles from what
    // was forwarded.
    drop(body);

    let record = world.settlement_within(&id).await;
    assert_eq!(record["kind"], "client_cancelled", "{record}");
    let charged = record["charged"].as_i64().expect("a charge");
    assert!(
        charged > 0 && charged <= record["freeze"].as_i64().unwrap(),
        "{record}"
    );

    let wallet = world.wallet().await;
    assert_eq!(
        wallet.reserved().await.unwrap(),
        0,
        "the reservation released"
    );
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000 - charged);

    let operation = world.operation(&id).await.expect("the operation exists");
    assert_eq!(operation.get::<String, _>("state"), "cancelled");
    assert_eq!(operation.get::<i64, _>("charged_minor"), charged);
}

/// A usage report that prices above the freeze settles `capped` at the bound:
/// the caller never pays more than the reservation — the integer cap
/// invariant's other edge.
#[tokio::test]
async fn an_excessive_usage_report_caps_at_the_freeze() {
    let world = world_or_skip!(1_000_000);
    // The token-priced model freezes at 40 + 5 = 45 (flat fee plus the
    // output ceiling); upstream's report prices 40 + 41 = 81 against it.
    world.script.answers(
        &world.over_model,
        Answer::Completion {
            input: 0,
            output: 41,
        },
    );

    let response = chat(&world.app, &world.key, &world.over_model, json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    let record = world.settlement_within(&id).await;
    assert_eq!(record["kind"], "capped", "{record}");
    assert_eq!(record["charged"], record["freeze"], "{record}");
    assert_eq!(record["freeze"], 45, "{record}");
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000 - 45);

    let operation = world.operation(&id).await.expect("the operation exists");
    assert_eq!(operation.get::<String, _>("state"), "succeeded");
    assert_eq!(operation.get::<String, _>("outcome_kind"), "capped");
}

/// A worker that dies mid-turn: the operation's lease lapses, the sweeper
/// claims it, the bound hold releases through the same `sweep_one` path a
/// stale watch row takes — charge zero, kind `timed_out`, and no blind
/// retransmit upstream.
#[tokio::test]
async fn a_lapsed_operation_sweeps_terminal_without_a_double_charge() {
    let world = world_or_skip!(1_000_000);

    // The crash shape, written the way admission left it: a real ledger hold
    // plus its watch row (the fixture route takes both), and a durable
    // operation already bound to that hold key, leased to a worker that is
    // now gone — `leased_until` in the past.
    let hold_key = format!("req-{}:hold", Uuid::new_v4());
    let (status, body) = call(
        &world.app,
        "POST",
        "/__test/holds",
        Some(json!({"idempotencyKey": hold_key, "amountMinor": 40})),
        Some(&world.key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let organization_id: Uuid = world.organization_id.parse().unwrap();
    let operation_id = Uuid::new_v4();
    let (operation, fresh) = world
        .db
        .insert_operation(&oxsum_core::NewOperation {
            operation_id,
            organization_id,
            key_id: None,
            surface: "chat".into(),
            fingerprint: format!("crash-{operation_id}"),
            bound_minor: 40,
            model: world.model.clone(),
            channel: None,
            customer_id: None,
            terms_version: None,
        })
        .await
        .expect("the operation writes");
    assert!(fresh);
    assert_eq!(operation.state, oxsum_core::OperationState::Created);
    assert!(
        world
            .db
            .bind_hold(operation_id, &hold_key, "dead-worker")
            .await
            .expect("the hold binds")
    );
    // The lease is already gone: the worker died before its first heartbeat.
    sqlx::query(
        "UPDATE oxsum.request_operations SET leased_until = now() - interval '1 second' \
                 WHERE operation_id = $1",
    )
    .bind(operation_id)
    .execute(&world.pool)
    .await
    .unwrap();

    // The sweeper's own pass: stale watch rows first (none qualifies — this
    // hold is fresh), then the lapsed-operation claim.
    let report = oxsum_core::sweep_stale_holds(
        &world.db,
        &world.tenants,
        OffsetDateTime::now_utc() - Duration::from_secs(3600),
        OffsetDateTime::now_utc().date(),
    )
    .await
    .expect("the sweep runs");

    // The hold released at zero: the wallet never charged, the reservation
    // is gone, and the operation went `timed_out` — recovered by inspection,
    // never retransmitted.
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000);
    let row = sqlx::query(
        "SELECT state, charged_minor, outcome_kind FROM oxsum.request_operations \
         WHERE operation_id = $1",
    )
    .bind(operation_id)
    .fetch_one(&world.pool)
    .await
    .expect("the operation row reads");
    assert_eq!(row.get::<String, _>("state"), "timed_out");
    assert_eq!(row.get::<i64, _>("charged_minor"), 0);
    assert_eq!(row.get::<String, _>("outcome_kind"), "swept");
    assert!(
        world.script.seen().is_empty(),
        "a swept turn never retransmits upstream"
    );
    let _ = report;
}

// ── A14: the day-close watermark ─────────────────────────────────────────────

/// An open operation created before the day's end defers the organization's
/// anchor for that day: the map a sealed day commits must be final, and an
/// operation still `created`/`leased`/`running` can still settle into it.
/// Once the row goes terminal the same pass commits the anchor.
#[tokio::test]
async fn an_open_operation_defers_the_day_anchor_until_terminal() {
    let world = world_or_skip!(1_000_000);
    let organization_id: Uuid = world.organization_id.parse().unwrap();
    let yesterday = OffsetDateTime::now_utc().date() - time::Duration::days(1);

    // A day leaf for yesterday — the anchor pass anchors days already closed,
    // and a leaf is what gives it a map worth sealing.
    sqlx::query(
        "INSERT INTO oxsum.usage_day_log \
             (tenant_id, end_user, day, entry_count, rows_root, leaf_hash) \
         VALUES ($1, 'anchor-check', $2, 1, $3, $4)",
    )
    .bind(&world.tenant_id)
    .bind(yesterday)
    .bind(vec![7u8; 32])
    .bind(vec![9u8; 32])
    .execute(&world.pool)
    .await
    .expect("the day leaf writes");

    // An operation still open whose admission predates the day's end — a
    // worker mid-stream when midnight passed.
    let operation_id = Uuid::new_v4();
    world
        .db
        .insert_operation(&oxsum_core::NewOperation {
            operation_id,
            organization_id,
            key_id: None,
            surface: "chat".into(),
            fingerprint: format!("anchor-{operation_id}"),
            bound_minor: 40,
            model: world.model.clone(),
            channel: None,
            customer_id: None,
            terms_version: None,
        })
        .await
        .expect("the operation writes");
    sqlx::query(
        "UPDATE oxsum.request_operations \
         SET created_at = $2 WHERE operation_id = $1",
    )
    .bind(operation_id)
    .bind(yesterday.midnight().assume_utc() + time::Duration::hours(12))
    .execute(&world.pool)
    .await
    .unwrap();

    // The pass defers the organization rather than sealing a map the open
    // operation can still change.
    let report = world
        .db
        .anchor_usage_days(yesterday)
        .await
        .expect("the pass runs");
    assert_eq!(report.deferred, 1, "{report:?}");
    assert_eq!(report.anchors, 0, "{report:?}");
    assert!(
        world
            .db
            .usage_anchor(organization_id, yesterday)
            .await
            .expect("the anchor reads")
            .is_none(),
        "no anchor committed under an open operation"
    );

    // The turn finishes: the operation goes terminal, and the next pass
    // commits the anchor the day was owed.
    assert_eq!(
        world
            .db
            .settle_operation(
                operation_id,
                "dead-worker",
                oxsum_core::OperationState::Succeeded,
                40,
                "usage",
            )
            .await
            .expect("the terminal write runs"),
        oxsum_core::OperationSettle::Settled
    );
    let report = world
        .db
        .anchor_usage_days(yesterday)
        .await
        .expect("the pass reruns");
    assert_eq!(report.deferred, 0, "{report:?}");
    assert_eq!(report.anchors, 1, "{report:?}");
    assert!(
        world
            .db
            .usage_anchor(organization_id, yesterday)
            .await
            .expect("the anchor reads")
            .is_some(),
        "the anchor committed once the operation settled"
    );
}

// ── A13: the typed zero-cost receipt ─────────────────────────────────────────

/// A turn the subscription draw covers whole, on a wallet that never topped
/// up: admitted, served, settled — and the settlement is the typed zero-cost
/// receipt (`charged: 0` on the wallet side, the `subscription` block naming
/// what paid), never a fabricated hold. The empty wallet stays untouched.
#[tokio::test]
async fn a_fully_covered_turn_settles_the_zero_cost_receipt_on_an_empty_wallet() {
    // No top-up at all: the wallet's available is 0, so a fabricated hold
    // would refuse admission outright — the receipt is what makes the
    // entitlement-funded turn possible.
    let world = world_or_skip!(0);
    let name = format!("cover-{}", &Uuid::new_v4().simple().to_string()[..8]);
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

    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 2,
            output: 3,
        },
    );
    let response = chat(&world.app, &world.key, &world.model, json!({})).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a fully-covered turn admits on an empty wallet"
    );
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    let record = world.settlement_within(&id).await;
    assert_eq!(record["v"], 14, "{record}");
    assert_eq!(record["kind"], "usage", "{record}");
    assert_eq!(
        record["charged"], 40,
        "the bill's full figure is the flat fee: {record}"
    );
    // The subscription block names what paid — the whole charge, so the
    // wallet's own share of the receipt is zero.
    let subscription = &record["subscription"];
    assert_eq!(subscription["chargedMinor"], 40, "{record}");
    assert!(
        subscription["drawnMinor"].as_i64().unwrap() >= 40,
        "{record}"
    );

    // The empty wallet never moved: nothing reserved, nothing settled —
    // the receipt is metadata, not money.
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 0);
    assert_eq!(wallet.available().await.unwrap(), 0);

    // The draw settled against the period, not the wallet.
    let consumed: i64 = sqlx::query_scalar(
        "SELECT consumed_minor FROM oxsum.org_subscriptions \
         WHERE organization_id = $1 AND state = 'active'",
    )
    .bind(Uuid::parse_str(&world.organization_id).unwrap())
    .fetch_one(&world.pool)
    .await
    .expect("the subscription reads");
    assert_eq!(consumed, 40);

    let operation = world.operation(&id).await.expect("the operation exists");
    assert_eq!(operation.get::<String, _>("state"), "succeeded");
    assert_eq!(operation.get::<i64, _>("charged_minor"), 40);

    // No ledger hold ever stood under the hold key: the entry id a hold
    // would have taken is absent — the receipt, not a hold, is the record.
    let hold_entry = oxsum_core::entry_id_for(&format!("req-{id}:hold"));
    assert!(
        world
            .wallet()
            .await
            .receipt_proof(hold_entry, None)
            .await
            .expect("the proof reads")
            .is_none(),
        "no wallet hold entry exists for the fully-covered turn"
    );
}

// ── the sale-priced turn ─────────────────────────────────────────────────────

/// A business-owner session in Business context, for `change_sale_terms`.
///
/// The suite's `set_registration_fixture` already initialized the singleton
/// business (open mode, for the customer signups), so the consent flow's
/// accept path is closed — this grants owner staff directly, the same
/// fixture `heads.rs` uses when it finds the business initialized.
async fn sale_owner(db: &Db) -> String {
    use oxsum_core::{SessionContext, SessionContextRequest};
    let password = "sale fixture owner password";
    let user = db
        .register(oxsum_core::NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: password.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let session = db
        .login(&user.user.email, password)
        .await
        .unwrap()
        .into_session();
    let business = db.business_ownership().await.unwrap().business;
    sqlx::query(
        "INSERT INTO oxsum.business_staff (business_id,user_id,role) VALUES ($1,$2,'owner')",
    )
    .bind(business.id)
    .bind(user.user.id)
    .execute(db.pool())
    .await
    .unwrap();
    db.switch_session_context(
        &session.token,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    session.token
}

/// A customer sale schedule's resolved snapshot prices the turn end to end:
/// the freeze reserves at the sale rates, the charge itemizes them, and the
/// v14 settlement commits the snapshot's digest — the figure
/// `sale_snapshot_digests` recorded at admission.
#[tokio::test]
async fn a_sale_priced_turn_commits_the_snapshot_digest() {
    let world = world_or_skip!(1_000_000);
    let organization_id: Uuid = world.organization_id.parse().unwrap();
    let owner = sale_owner(&world.db).await;

    // A sale book pricing this world's model at 2 minor a token with a 50%
    // discount — a deliberate counterpoint to the wholesale flat fee, so the
    // settlement's figure could only have come from the snapshot.
    let sale = json!({"price": {"inputPricePerMillion": 2_000_000, "outputPricePerMillion": 2_000_000, "maxOutputTokens": 1000}, "discountPercent": 50});
    let mutate = |change: Value| {
        let db = world.db.clone();
        let owner = owner.clone();
        async move {
            db.change_sale_terms(
                &owner,
                &serde_json::from_value(json!({
                    "idempotencyKey": Uuid::new_v4().to_string(),
                    "reason": "coordinated test fixture",
                    "change": change,
                }))
                .unwrap(),
            )
            .await
            .expect("the sale change lands")
        }
    };
    let book = mutate(json!({"operation": "createBook", "name": "coordinated book"}))
        .await
        .resource_id;
    mutate(json!({
        "operation": "saveDraft",
        "bookId": book,
        "expectedRevision": 0,
        "models": {world.model.clone(): sale},
    }))
    .await;
    mutate(json!({"operation": "publishDraft", "bookId": book, "draftRevision": 1})).await;
    // `effectiveFrom` absent starts the window at the write — sale windows
    // cannot begin in the past, and the turn's admission instant is later.
    mutate(json!({
        "operation": "assignBook",
        "organizationId": organization_id,
        "bookId": book,
        "version": 1,
    }))
    .await;

    // The resolved snapshot's digest, the way the settlement will name it.
    let snapshot = world
        .db
        .resolve_sale_price(
            sqlx::query_scalar("SELECT business_id FROM oxsum.businesses WHERE singleton")
                .fetch_one(&world.pool)
                .await
                .unwrap(),
            organization_id,
            &world.model,
            OffsetDateTime::now_utc(),
        )
        .await
        .expect("the sale resolves");
    let canonical = oxsum_core::canonical_sale_snapshot(&snapshot).unwrap();
    let digest = oxsum_core::sale_snapshot_hash(&canonical).to_hex();

    world.script.answers(
        &world.model,
        Answer::Completion {
            input: 10,
            output: 2,
        },
    );
    let response = chat(&world.app, &world.key, &world.model, json!({})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    let record = world.settlement_within(&id).await;
    assert_eq!(record["v"], 14, "{record}");
    // 12 tokens × 2 minor, halved by the book's discount = 12 — the figure
    // the wholesale catalog could never have produced (its rate is 0).
    assert_eq!(record["charged"], 12, "{record}");
    assert_eq!(record["discountPercent"], 50, "{record}");
    assert_eq!(
        record["sale"]["digest"], digest,
        "the committed digest is the resolved snapshot's"
    );
    let wallet = world.wallet().await;
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 1_000_000 - 12);
}

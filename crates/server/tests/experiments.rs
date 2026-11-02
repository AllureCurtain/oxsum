//! Gateway end-to-end tests for experiments (issue #509): a request carrying
//! `X-Oxsum-Experiment: <name>` serves under a variant the open experiment's
//! weighted pick lands on, and the settlement's v13 `experiment` block and the
//! usage row's `usage_details.experiment` record which ran.
//!
//! The CRUD surface is `/api/v1/org/experiments` — a member reads the list and
//! the grouped results, an owner or an admin session writes the variant set
//! and ends the experiment, and an API key is refused both writes the way
//! membership management refuses it. The upstream is a scripted axum server on
//! a free port, so which model answered is deterministic.
//!
//! The tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

// ── the scripted upstream ────────────────────────────────────────────────────

/// What upstream answers for one model.
#[derive(Clone)]
enum Answer {
    /// A whole completion with a usage report.
    Completion { usage: Option<(i64, i64)> },
}

/// The upstream's state: what to answer, and what it has been asked.
#[derive(Clone, Default)]
struct Script {
    answers: Arc<Mutex<HashMap<String, Answer>>>,
    seen: Arc<Mutex<Vec<String>>>,
}
impl Script {
    /// Teaches upstream how to answer for one model.
    fn answers(&self, model: &str, answer: Answer) {
        self.answers
            .lock()
            .unwrap()
            .insert(model.to_owned(), answer);
    }

    /// The model each received request named, in order.
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

/// Starts the scripted upstream and returns its base URL, as a deployment
/// would configure it. The `/v1` tail is what a provider publishes; the
/// gateway appends the protocol's own path to it.
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

/// The `/v1/chat/completions` side of the scripted upstream: records the model
/// the body named, then answers what the script said for it.
async fn upstream(State(script): State<Script>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let model = body["model"].as_str().unwrap_or_default().to_owned();
    script.seen.lock().unwrap().push(model.clone());
    match script.answers.lock().unwrap().get(&model).cloned() {
        Some(Answer::Completion { usage }) => axum::Json(completion(model, usage)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": {"message": "no scripted answer"}})),
        )
            .into_response(),
    }
}

/// A whole chat completion in OpenAI's shape, with whatever usage upstream
/// reports.
fn completion(model: String, usage: Option<(i64, i64)>) -> Value {
    let usage = usage.map(|(prompt_tokens, completion_tokens)| {
        json!({
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        })
    });
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop",
        }],
        "usage": usage,
    })
}

// ── the world a test runs in ─────────────────────────────────────────────────

/// An app wired to a scripted upstream, one funded organization, and the
/// handles to check it.
struct World {
    app: Router,
    tenants: Tenants,
    tenant_id: String,
    /// The owner's API key — the gateway credential, refused the CRUD writes.
    key: String,
    /// The owner's session cookie — the role the writes need.
    cookie: String,
    organization_id: Uuid,
    script: Script,
    /// What its model names end in.
    suffix: String,
    /// The world's own pool — helpers read the schema the API writes.
    pool: PgPool,
}

impl World {
    /// One of this world's models, as the gateway and its upstream see it.
    fn model(&self, name: &str) -> String {
        format!("{name}-{}", self.suffix)
    }

    /// Teaches this world's upstream how to answer for one of its models.
    fn answers(&self, model: &str) {
        self.script.answers(
            &self.model(model),
            Answer::Completion {
                usage: Some((5, 3)),
            },
        );
    }

    /// The settlement entry's description for a request, verbatim.
    async fn settlement_description(&self, request_id: &str) -> Option<String> {
        let hold_key = format!("req-{request_id}:hold");
        let entry_id = oxsum_core::entry_id_for(&oxsum_core::settlement_key_for(&hold_key));
        let wallet = self
            .tenants
            .get(&self.tenant_id)
            .await
            .expect("the organization's ledger opens");
        let bundle = wallet
            .receipt_proof(entry_id, None)
            .await
            .expect("the proof is built")
            .map(|bundle| bundle.entry);
        bundle.map(|entry| entry.description().as_str().to_owned())
    }

    /// What the settlement entry for a request says, once it is there.
    async fn settlement(&self, request_id: &str) -> Option<Value> {
        self.settlement_description(request_id)
            .await
            .map(|text| serde_json::from_str(&text).expect("the description is JSON"))
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
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("experiments")
}

/// A world with `top_up` minor units of credit, or an early return when there
/// is no database.
macro_rules! world {
    ($top_up:expr) => {
        match url() {
            Some(url) => world_for(&url, $top_up).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// With input and output both 1_000_000 per million, a token costs exactly one
/// minor unit. The model names carry the world's suffix because the database is
/// shared between tests: a model belongs to exactly one channel.
fn price_list(suffix: &str) -> String {
    let price = json!({
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 1_000_000,
        "maxOutputTokens": 1000,
    });
    let mut models = serde_json::Map::new();
    for model in ["plain", "var-a", "var-b"] {
        models.insert(format!("{model}-{suffix}"), price.clone());
    }
    Value::Object(models).to_string()
}

async fn world_for(url: &str, top_up: i64) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");

    let suffix = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("mock-{suffix}");
    let secret = oxsum_core::SecretKey::from_bytes([7; 32]);
    let script = Script::default();
    let base_url = start_upstream(script.clone()).await;
    let book = oxsum_core::PriceBook::from_json(&channel, &price_list(&suffix))
        .expect("the test's own prices parse");
    db.set_channel(&channel, &base_url, "upstream-secret", "openai", &secret)
        .await
        .expect("the channel is written");
    for (model, price) in book.models() {
        db.append_price(&channel, model, price.clone(), 100)
            .await
            .expect("the price is written");
    }
    let config = Config::new(Signup::Open, None).with_secret(secret);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let (app, _billing, _metrics) = oxsum_server::app_with_billing(db, config);
    let tenants = Tenants::new(pool.clone());

    let (registration, key, cookie) = register(&app).await;
    let tenant_id = registration["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();
    let organization_id: Uuid = registration["organization"]["id"]
        .as_str()
        .expect("registration names the organization")
        .parse()
        .expect("the organization id is a uuid");
    if top_up > 0 {
        call(
            &app,
            "POST",
            "/api/v1/topups",
            Some(json!({"idempotencyKey": "top-1", "amountMinor": top_up})),
            Some(&key),
            None,
            None,
        )
        .await;
    }
    World {
        app,
        tenants,
        tenant_id,
        key,
        cookie,
        organization_id,
        script,
        suffix,
        pool,
    }
}

/// Registers an organization and returns its document, its first API key and
/// the session cookie the CRUD writes need.
async fn register(app: &Router) -> (Value, String, String) {
    let email = format!(
        "exp_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let res = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "registration failed: {}",
        res.body
    );
    let key = res.body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    // Login to get a session cookie — the owner role the writes need.
    let login_res = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        login_res.status,
        StatusCode::OK,
        "login failed: {}",
        login_res.body
    );
    let cookie = login_res
        .set_cookie
        .as_deref()
        .and_then(|c| c.split(';').next())
        .and_then(|c| c.strip_prefix("oxsum_session="))
        .expect("login sets the session cookie")
        .to_owned();
    (res.body["data"].clone(), key, cookie)
}

struct Res {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
}

/// One JSON call against the API; the optional credentials are the API key and
/// the session cookie, and `experiment` rides the `x-oxsum-experiment` header.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    cookie: Option<&str>,
    experiment: Option<&str>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    if let Some(name) = experiment {
        req = req.header("x-oxsum-experiment", name);
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        body,
        set_cookie,
    }
}

/// A chat request that keeps the response headers the request id lives in.
async fn chat_keeping_headers(
    app: &Router,
    key: &str,
    experiment: Option<&str>,
    body: Value,
) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {key}"));
    if let Some(name) = experiment {
        request = request.header("x-oxsum-experiment", name);
    }
    let request = request
        .body(Body::from(body.to_string()))
        .expect("the test's own request");
    app.clone()
        .oneshot(request)
        .await
        .expect("the router answers")
}

/// A chat request with a message that costs a known little.
fn simple(model: &str) -> Value {
    json!({"model": model, "messages": [{"role": "user", "content": "hi"}]})
}

/// The request id of a response, which is how its bill is found.
fn request_id(response: &Response) -> String {
    response
        .headers()
        .get("x-oxsum-request-id")
        .expect("every gateway response carries a request id")
        .to_str()
        .expect("the request id is ASCII")
        .to_owned()
}

/// Creates or replaces an experiment's variant set through the session surface.
async fn put_experiment(world: &World, name: &str, variants: Value) -> Res {
    call(
        &world.app,
        "PUT",
        &format!("/api/v1/org/experiments/{name}"),
        Some(json!({ "variants": variants })),
        None,
        Some(&world.cookie),
        None,
    )
    .await
}

// ── the cases ────────────────────────────────────────────────────────────────

/// A request under the experiment header serves a variant's model, not the
/// requested one, and the settlement's v13 `experiment` block records the
/// binding — `{id, variant}` — while `servedModel`/`model` name the served
/// model as always.
#[tokio::test]
async fn the_header_serves_a_variant_and_records_the_binding() {
    let world = world!(10_000_000);
    let var_a = world.model("var-a");
    let res = put_experiment(&world, "ab", json!([{ "model": var_a, "weight": 1 }])).await;
    assert_eq!(res.status, StatusCode::OK, "put failed: {}", res.body);
    let experiment_id = res.body["data"]["id"]
        .as_str()
        .expect("the experiment answers its id")
        .to_owned();
    world.answers("var-a");

    // The request asks for `plain`; the experiment's only variant is `var-a`.
    let response = chat_keeping_headers(
        &world.app,
        &world.key,
        Some("ab"),
        simple(&world.model("plain")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();

    assert_eq!(
        world.script.seen(),
        vec![var_a.clone()],
        "the variant's model answered, not the requested one"
    );
    let record = world.settlement_within(&id).await;
    assert_eq!(record["v"], 13, "the description is v13: {record}");
    assert_eq!(record["servedModel"], var_a);
    assert_eq!(record["model"], var_a, "model stays the served name");
    assert_eq!(
        record["experiment"]["id"].as_str(),
        Some(experiment_id.as_str()),
        "the experiment block names the experiment: {record}"
    );
    assert_eq!(
        record["experiment"]["variant"].as_str(),
        Some(var_a.as_str()),
        "the experiment block names the served variant: {record}"
    );
}

/// A request naming no experiment — or an experiment that is not open —
/// resolves the plain model untouched, and the settlement carries no
/// `experiment` block: an experiment is a routing hint, never an admission
/// gate.
#[tokio::test]
async fn a_missing_or_ended_name_leaves_the_model_untouched() {
    let world = world!(10_000_000);
    world.answers("plain");
    let var_a = world.model("var-a");
    let res = put_experiment(&world, "ab", json!([{ "model": var_a, "weight": 1 }])).await;
    assert_eq!(res.status, StatusCode::OK);
    // End it: the header now names a closed experiment.
    let res = call(
        &world.app,
        "POST",
        "/api/v1/org/experiments/ab/end",
        None,
        None,
        Some(&world.cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "end failed: {}", res.body);

    for experiment in [None, Some("ab"), Some("no-such-experiment")] {
        let response = chat_keeping_headers(
            &world.app,
            &world.key,
            experiment,
            simple(&world.model("plain")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let id = request_id(&response);
        let _ = response.into_body().collect().await.unwrap().to_bytes();
        let record = world.settlement_within(&id).await;
        assert_eq!(record["servedModel"], world.model("plain"));
        assert!(
            record.get("experiment").is_none(),
            "no experiment block on a plain serve: {record}"
        );
    }
    assert_eq!(
        world.script.seen(),
        vec![
            world.model("plain"),
            world.model("plain"),
            world.model("plain")
        ],
        "every request served the plain model"
    );
}

/// The results read groups the experiment's recorded usage by variant: turns,
/// token and charge sums, the errored count. An ended experiment still
/// answers — its recorded spend still groups.
#[tokio::test]
async fn the_results_read_groups_spend_by_variant() {
    let world = world!(10_000_000);
    let var_a = world.model("var-a");
    let res = put_experiment(&world, "ab", json!([{ "model": var_a, "weight": 1 }])).await;
    let experiment_id = res.body["data"]["id"]
        .as_str()
        .expect("the experiment answers its id")
        .to_owned();
    world.answers("var-a");

    let response = chat_keeping_headers(
        &world.app,
        &world.key,
        Some("ab"),
        simple(&world.model("plain")),
    )
    .await;
    let id = request_id(&response);
    let _ = response.into_body().collect().await.unwrap().to_bytes();
    let _ = world.settlement_within(&id).await;

    // The usage row carries the same experiment pair the settlement does.
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT usage_details -> 'experiment' ->> 'variant' \
         FROM oxsum.usage_records \
         WHERE tenant_id = $1 AND usage_details -> 'experiment' ->> 'id' = $2::text",
    )
    .bind(&world.tenant_id)
    .bind(&experiment_id)
    .fetch_optional(&world.pool)
    .await
    .expect("the usage row reads");
    assert_eq!(
        row.as_ref().map(|r| r.0.as_str()),
        Some(var_a.as_str()),
        "the usage row records the variant"
    );

    let res = call(
        &world.app,
        "GET",
        &format!("/api/v1/org/experiments/{experiment_id}/results"),
        None,
        Some(&world.key),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "results failed: {}", res.body);
    let groups = res.body["data"].as_array().expect("the groups list");
    assert_eq!(groups.len(), 1, "one variant served: {groups:?}");
    assert_eq!(groups[0]["variant"].as_str(), Some(var_a.as_str()));
    assert_eq!(groups[0]["turns"].as_i64(), Some(1));
    assert_eq!(groups[0]["inputTokens"].as_i64(), Some(5));
    assert_eq!(groups[0]["outputTokens"].as_i64(), Some(3));
    assert!(groups[0]["chargedMinor"].as_i64().unwrap() > 0);
    assert_eq!(groups[0]["errored"].as_i64(), Some(0));

    // A foreign experiment id is a 404, not an empty set that reads as
    // "served nothing".
    let res = call(
        &world.app,
        "GET",
        &format!("/api/v1/org/experiments/{}/results", Uuid::new_v4()),
        None,
        Some(&world.key),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

/// The role gate the writes keep: a member's session and an API key are both
/// refused `PUT` and `end`; the reads stay open to any member — and to the
/// organization's key, which acts with the organization's authority.
#[tokio::test]
async fn the_writes_are_owner_admin_only() {
    let world = world!(0);
    let var_a = world.model("var-a");
    let variants = json!([{ "model": var_a, "weight": 1 }]);

    // An API key names no role: the writes refuse it before the rule runs.
    let res = call(
        &world.app,
        "PUT",
        "/api/v1/org/experiments/ab",
        Some(json!({ "variants": variants })),
        Some(&world.key),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "a key may not write");

    // The owner's session writes the experiment; an ending it serves is the
    // same gate — both writes pass under the owner cookie.
    let res = put_experiment(&world, "ab", variants.clone()).await;
    assert_eq!(res.status, StatusCode::OK, "the owner writes");

    // A member's session reads but cannot write: seed a second person into the
    // owner's organization as a member.
    let member_email = format!(
        "exp_member_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let res = call(
        &world.app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": member_email, "password": PASSWORD})),
        None,
        None,
        None,
    )
    .await;
    let member_id: Uuid = res.body["data"]["user"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role, created_at) \
         VALUES ($1, $2, 'member', now() - interval '1 hour')",
    )
    .bind(world.organization_id)
    .bind(member_id)
    .execute(&world.pool)
    .await
    .expect("seeds the membership");
    let res = call(
        &world.app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": member_email, "password": PASSWORD})),
        None,
        None,
        None,
    )
    .await;
    let member_cookie = res
        .set_cookie
        .as_deref()
        .and_then(|c| c.split(';').next())
        .and_then(|c| c.strip_prefix("oxsum_session="))
        .expect("the member's session cookie")
        .to_owned();

    // The member reads the list.
    let res = call(
        &world.app,
        "GET",
        "/api/v1/org/experiments",
        None,
        None,
        Some(&member_cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "a member reads the list");
    assert_eq!(res.body["data"].as_array().unwrap().len(), 1);

    // The member cannot write or end.
    let res = call(
        &world.app,
        "PUT",
        "/api/v1/org/experiments/ab",
        Some(json!({ "variants": variants })),
        None,
        Some(&member_cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "a member may not write");
    let res = call(
        &world.app,
        "POST",
        "/api/v1/org/experiments/ab/end",
        None,
        None,
        Some(&member_cookie),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "a member may not end");
}

/// The write-time validation: a variant naming an unserved model is a 400 — it
/// would dead-end every request it landed on — and a weight below 1 is the
/// same.
#[tokio::test]
async fn an_unserved_variant_or_a_bad_weight_is_a_400() {
    let world = world!(0);
    let res = put_experiment(
        &world,
        "ab",
        json!([{ "model": "no-such-model", "weight": 1 }]),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::BAD_REQUEST,
        "an unserved model is a 400"
    );
    let res = put_experiment(
        &world,
        "ab",
        json!([{ "model": world.model("var-a"), "weight": 0 }]),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::BAD_REQUEST,
        "a zero weight is a 400"
    );
    let res = put_experiment(&world, "ab", json!([])).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "an empty set is a 400");
}

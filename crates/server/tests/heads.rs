//! HTTP tests: signed tree heads and consistency proofs.
//!
//! The offline tests cover the unconfigured-key 503 and the public key endpoint; the online
//! ones run the user flow from the issue: hold an old head, grow the log, then verify the
//! new head was appended onto the old one — signature first, consistency proof second.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use doubleentry::witness::signing::VerifyingKey;
use doubleentry::witness::{KeyName, SignedTreeHead};
use doubleentry::{ConsistencyProof, Hash, TreeHead};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

const SEED: [u8; 32] = [42; 32];

/// A pool that connects to nothing: rejection happens before the database is reached, so the
/// pool only has to exist. `connect_lazy` defers the first connection to first use.
fn unused_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL")
}

/// An app over a pool that never connects, for requests answered without the database.
fn offline_app(seed: Option<[u8; 32]>) -> Router {
    let mut config = Config::new(Signup::Open, None);
    if let Some(seed) = seed {
        config = config.with_head_signing_seed(seed);
    }
    oxsum_server::app(Db::from_pool(unused_pool()), config)
}

/// An app over a real database with oxsum's tables migrated.
async fn online_app(url: &str, seed: Option<[u8; 32]>) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let mut config = Config::new(Signup::Open, None);
    if let Some(seed) = seed {
        config = config.with_head_signing_seed(seed);
    }
    (oxsum_server::app(db, config), pool)
}

/// The DATABASE_URL tests need, or None to skip.
fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("heads")
}

macro_rules! app_or_skip {
    ($seed:expr) => {
        match url() {
            Some(u) => online_app(&u, $seed).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
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
        req = req.header("authorization", format!("Bearer {key}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn register(app: &Router, name: &str) -> (Value, String) {
    let email = format!(
        "{name}_{}@example.com",
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
    let secret = body["data"]["apiKey"]["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    (body["data"].clone(), secret)
}

async fn top_up(app: &Router, key: &str, minor: i64) {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": uuid::Uuid::new_v4().to_string(), "amountMinor": minor})),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "top-up failed: {body}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The verifying key the response publishes, as doubleentry's type.
fn published_key(body: &Value) -> VerifyingKey {
    let name = KeyName::new(body["data"]["keyName"].as_str().unwrap()).unwrap();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(body["data"]["publicKey"].as_str().unwrap())
        .unwrap();
    let bytes: [u8; 32] = raw.try_into().unwrap();
    VerifyingKey::from_bytes(name, bytes).unwrap()
}

/// Checks the signed-note half of a `/log/head` or `/log/consistency` answer: the note
/// parses, its body matches the structured head, and the operator's signature line
/// verifies against the published key.
fn check_signature(body: &Value, size: u64, root: &str) {
    let note = SignedTreeHead::parse(body["data"]["note"].as_str().unwrap()).unwrap();
    assert_eq!(note.head.size, size, "note body and structured size agree");
    assert_eq!(
        note.head.root.to_hex(),
        root,
        "note body and structured root agree"
    );
    assert!(note.extensions.is_empty());

    let key = published_key(body);
    assert_eq!(
        body["data"]["keyHash"].as_str().unwrap(),
        hex(&key.note_key_hash()),
        "the selector names this key's signature line"
    );
    let signature = note
        .signatures
        .iter()
        .find(|s| s.key_hash == key.note_key_hash())
        .expect("the operator's signature line is on the note");
    assert!(key.verify(&note.body(), signature));
    assert_eq!(
        body["data"]["keyName"].as_str().unwrap(),
        "oxsum/tree-heads"
    );
}

fn tree_head(value: &Value) -> TreeHead {
    TreeHead {
        size: value["size"].as_u64().unwrap(),
        root: Hash::parse_hex(value["root"].as_str().unwrap()).unwrap(),
    }
}

#[tokio::test]
async fn the_key_endpoint_is_public_and_names_the_key() {
    let app = offline_app(Some(SEED));
    let (status, body) = call(&app, "GET", "/api/v1/log/key", None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["keyName"].as_str().unwrap(),
        "oxsum/tree-heads"
    );
    let raw = base64::engine::general_purpose::STANDARD
        .decode(body["data"]["publicKey"].as_str().unwrap())
        .unwrap();
    assert_eq!(raw.len(), 32);
    assert_eq!(body["data"]["keyHash"].as_str().unwrap().len(), 8);
}

#[tokio::test]
async fn without_a_seed_the_log_surface_answers_503() {
    let app = offline_app(None);
    let (status, body) = call(&app, "GET", "/api/v1/log/key", None, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        body["error"]["code"].as_str().unwrap(),
        "SERVICE_UNAVAILABLE"
    );
}

#[tokio::test]
async fn the_head_endpoints_need_a_credential() {
    let app = offline_app(Some(SEED));
    let (status, _) = call(&app, "GET", "/api/v1/log/head", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&app, "GET", "/api/v1/log/consistency?from=1", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_old_head_verifies_the_new_one_was_appended_onto_it() {
    let (app, _pool) = app_or_skip!(Some(SEED));
    let (_registration, key) = register(&app, "heads").await;
    let tenant_id = _registration["organization"]["tenantId"]
        .as_str()
        .unwrap()
        .to_owned();

    top_up(&app, &key, 1_000_000).await;
    let (status, first) = call(&app, "GET", "/api/v1/log/head", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let old = tree_head(&first["data"]);
    assert_eq!(old.size, 1);
    assert_eq!(
        first["data"]["origin"].as_str().unwrap(),
        format!("oxsum/ledgers/{tenant_id}")
    );
    check_signature(&first, old.size, &old.root.to_hex());

    // The log grows; the user still holds the old head.
    top_up(&app, &key, 2_000_000).await;

    let (status, answer) = call(
        &app,
        "GET",
        "/api/v1/log/consistency?from=1",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");

    // The new head, as the response structures it, matches the one a fresh read serves.
    let (status, second) = call(&app, "GET", "/api/v1/log/head", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let new = tree_head(&answer["data"]["head"]);
    assert_eq!(new.size, 2);
    assert_eq!(new.root, tree_head(&second["data"]).root);
    check_signature(&answer, new.size, &new.root.to_hex());

    // The proof binds the head the user holds to the signed new one.
    let old_from_server = tree_head(&answer["data"]["oldHead"]);
    assert_eq!(
        old_from_server, old,
        "the server recomputed the user's head"
    );
    let proof = ConsistencyProof {
        old_size: answer["data"]["proof"]["oldSize"].as_u64().unwrap(),
        new_size: answer["data"]["proof"]["newSize"].as_u64().unwrap(),
        path: answer["data"]["proof"]["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| Hash::parse_hex(h.as_str().unwrap()).unwrap())
            .collect(),
    };
    assert_eq!(proof.old_size, 1);
    assert_eq!(proof.new_size, new.size);
    assert!(
        proof.verify(&old, &new),
        "the new head was appended onto the held one"
    );
}

#[tokio::test]
async fn consistency_refuses_the_cases_that_would_prove_nothing() {
    let (app, _pool) = app_or_skip!(Some(SEED));
    let (_registration, key) = register(&app, "consistency-edges").await;
    top_up(&app, &key, 1_000_000).await;

    // From the empty tree every log extends every log: refused, not answered.
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/log/consistency?from=0",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"].as_str().unwrap(), "VALIDATION_ERROR");

    // Beyond the log: 400, not a proof of nothing.
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/log/consistency?from=999",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Not a number at all: the serde rejection answers the same envelope, not
    // axum's bare text a client parsing the error format would not read (issue #181).
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/log/consistency?from=abc",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"].as_str().unwrap(), "VALIDATION_ERROR");

    // From the current size: the trivial proof, empty path.
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/log/consistency?from=1",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["proof"]["path"].as_array().unwrap().is_empty());
    assert_eq!(
        body["data"]["oldHead"]["root"].as_str().unwrap(),
        body["data"]["head"]["root"].as_str().unwrap()
    );
}

#[tokio::test]
async fn each_organization_gets_its_own_origin() {
    let (app, _pool) = app_or_skip!(Some(SEED));
    let (reg_a, key_a) = register(&app, "heads-a").await;
    let (_reg_b, key_b) = register(&app, "heads-b").await;
    top_up(&app, &key_a, 1_000_000).await;
    top_up(&app, &key_b, 1_000_000).await;

    let (_, head_a) = call(&app, "GET", "/api/v1/log/head", None, Some(&key_a)).await;
    let (_, head_b) = call(&app, "GET", "/api/v1/log/head", None, Some(&key_b)).await;
    let origin_a = head_a["data"]["origin"].as_str().unwrap();
    let origin_b = head_b["data"]["origin"].as_str().unwrap();
    assert_ne!(origin_a, origin_b);
    assert_eq!(
        origin_a,
        format!(
            "oxsum/ledgers/{}",
            reg_a["organization"]["tenantId"].as_str().unwrap()
        )
    );
    // Same operator key signs both ledgers.
    assert_eq!(
        head_a["data"]["publicKey"].as_str().unwrap(),
        head_b["data"]["publicKey"].as_str().unwrap()
    );
}

#[tokio::test]
async fn the_head_endpoints_answer_503_without_a_seed() {
    let (app, _pool) = app_or_skip!(None);
    let (_registration, key) = register(&app, "heads-noseed").await;
    for path in ["/api/v1/log/head", "/api/v1/log/consistency?from=1"] {
        let (status, body) = call(&app, "GET", path, None, Some(&key)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}: {body}");
        assert_eq!(
            body["error"]["code"].as_str().unwrap(),
            "SERVICE_UNAVAILABLE"
        );
    }
}

/// Logs in and returns the session cookie the page server functions authenticate by.
async fn session_cookie(app: &Router, email: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"email": email, "password": "correct horse battery"}).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    res.headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned()
}

/// Registers a fresh organization and returns its first API key plus a session cookie.
async fn key_and_session(app: &Router, name: &str) -> (String, String) {
    let email = format!(
        "{name}_{}@example.com",
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
        .unwrap()
        .to_owned();
    (key, session_cookie(app, &email).await)
}

/// The bills page's HTML with the session cookie. The Topcoat dashboard renders
/// server-side — there is no `get_log_head` server function anymore; the signed
/// head is a section of the page.
async fn bills_html(app: &Router, cookie: Option<&str>) -> (StatusCode, String) {
    let mut req = Request::builder().method("GET").uri("/dashboard/bills");
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("the page is UTF-8"),
    )
}

/// One form post against the app, with the session cookie.
async fn post_form(
    app: &Router,
    path: &str,
    body: &str,
    cookie: Option<&str>,
) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        String::from_utf8(bytes.to_vec()).expect("the page is UTF-8"),
    )
}

#[tokio::test]
async fn the_bills_page_serves_the_head_and_the_consistency_check() {
    let (app, _pool) = app_or_skip!(Some(SEED));
    let (key, cookie) = key_and_session(&app, "heads-pages").await;
    top_up(&app, &key, 1_000_000).await;

    // The bills page renders the signed head section, session-scoped.
    let (status, html) = bills_html(&app, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "the bills page renders");
    assert!(html.contains("Ledger head"), "the head section renders");
    assert!(
        html.contains("oxsum/tree-heads"),
        "the section names the signing key"
    );
    assert!(html.contains("Size 1"), "the head's size renders");

    // The log grows; the consistency form proves the archived head is still
    // inside it — the check that ran in the browser in the Leptos dashboard.
    top_up(&app, &key, 2_000_000).await;
    let (status, html) = post_form(
        &app,
        "/dashboard/bills/consistency",
        "from=1",
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the consistency check answers");
    assert!(
        html.contains("the log still contains the archived head"),
        "the verdict renders"
    );
    assert!(
        html.contains("From size 1 to size 2"),
        "the proof's sizes render"
    );
}

#[tokio::test]
async fn without_a_seed_the_head_section_degrades() {
    let (app, _pool) = app_or_skip!(None);
    let (key, cookie) = key_and_session(&app, "heads-nopage").await;
    top_up(&app, &key, 1_000_000).await;

    // The page degrades instead of failing: the section reports itself
    // unconfigured, where the API answers 503.
    let (status, html) = bills_html(&app, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "the bills page renders");
    assert!(
        html.contains("This deployment signs no heads"),
        "the section degrades: {html}"
    );

    // The consistency check has nothing to check against: an internal error.
    let (status, _) = post_form(
        &app,
        "/dashboard/bills/consistency",
        "from=1",
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "no seed, no proof"
    );
}

#[tokio::test]
async fn the_head_surface_needs_a_session() {
    let (app, _pool) = app_or_skip!(Some(SEED));
    // Without a session the page sends the visitor to /login, and so does the
    // consistency form — the dashboard's surface does not answer anonymously.
    let (status, _) = bills_html(&app, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "the page needs a session");
    let (status, _) = post_form(&app, "/dashboard/bills/consistency", "from=1", None).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "the form needs a session");
}

// ── signed price heads (issue #204) ─────────────────────────────────────────
//
// A bill names its `(channel, model, priceVersion)`; these tests pin the
// endpoint that attests the named price is the catalog's own: a signed
// checkpoint per priced row under the same operator key that signs tree heads.

/// A channel name the tests own, so tests never share one.
fn fresh_channel(name: &str) -> String {
    format!("{name}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// Writes one channel with one priced model straight to the catalog, the way
/// the admin API's own store calls do. Returns the version `append_price` wrote.
async fn price_one(db: &Db, channel: &str, model: &str, input: i64) -> i64 {
    db.set_channel(
        channel,
        "https://upstream.example/v1",
        "sk-secret-1234",
        "openai",
        &oxsum_core::SecretKey::from_bytes([7; 32]),
    )
    .await
    .expect("the channel registers");
    db.append_price(
        channel,
        model,
        oxsum_core::Price {
            input_price_per_million: input,
            output_price_per_million: 2_000_000,
            max_output_tokens: 8_000,
            cache_read_price_per_million: None,
            cache_write_5m_price_per_million: None,
            cache_write_1h_price_per_million: None,
            reasoning_price_per_million: None,
            cost_per_request: None,
            upstream: None,
            mode: Default::default(),
            rules: Vec::new(),
        },
        100,
    )
    .await
    .expect("the price appends")
}

/// Checks a price-head answer the way `check_signature` checks a tree head:
/// the note parses, its body is the checkpoint the structured fields describe
/// — the catalog origin, the version as `size`, the row hash as `root` — and
/// the operator's signature verifies under the key `/log/key` publishes.
fn check_price_note(body: &Value, channel: &str, version: i64) {
    let data = &body["data"];
    assert_eq!(data["channel"].as_str().unwrap(), channel);
    assert_eq!(data["version"].as_i64().unwrap(), version);
    assert_eq!(
        data["origin"].as_str().unwrap(),
        format!("oxsum/price-catalog/{channel}")
    );

    let note = SignedTreeHead::parse(data["note"].as_str().unwrap()).unwrap();
    assert_eq!(note.head.size, version as u64);
    assert_eq!(
        note.head.root.to_hex(),
        data["rowHash"].as_str().unwrap(),
        "note body and structured rowHash agree"
    );
    assert!(note.extensions.is_empty());

    let key = published_key(body);
    let signature = note
        .signatures
        .iter()
        .find(|s| s.key_hash == key.note_key_hash())
        .expect("the operator's signature line is on the note");
    assert!(key.verify(&note.body(), signature));
}

#[tokio::test]
async fn a_price_checkpoint_binds_the_named_version_to_the_signed_row() {
    let (app, _pool) = app_or_skip!(Some(SEED));
    let db = Db::from_pool(_pool.clone());
    let (_registration, key) = register(&app, "price-heads").await;
    let channel = fresh_channel("priced");
    let model = fresh_channel("m");
    let version = price_one(&db, &channel, &model, 1_000_000).await;
    assert_eq!(version, 1);

    // No version asked: the latest is served.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/log/price-head?channel={channel}&model={model}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    check_price_note(&body, &channel, 1);

    // The served root commits to the canonical row of the stored v1 — the
    // same bytes `receipt_proof` puts in a bundle's `price.row`.
    let row = oxsum_core::price_row(&_pool, &channel, &model, Some(1))
        .await
        .unwrap()
        .expect("v1 exists");
    let canonical = oxsum_core::canonical_price_row(&channel, &row).unwrap();
    assert_eq!(
        body["data"]["rowHash"].as_str().unwrap(),
        oxsum_core::price_row_hash(&canonical).to_hex()
    );

    // The catalog moves on: v2 lands, latest answers v2…
    price_one(&db, &channel, &model, 9_000_000).await;
    let (status, latest) = call(
        &app,
        "GET",
        &format!("/api/v1/log/price-head?channel={channel}&model={model}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{latest}");
    check_price_note(&latest, &channel, 2);
    assert_ne!(
        latest["data"]["rowHash"].as_str().unwrap(),
        body["data"]["rowHash"].as_str().unwrap(),
        "v2 commits to a different row"
    );

    // …and the bill under v1 still attests the identical checkpoint: signing
    // is deterministic, so the note a bundle carried stays the note served.
    let (status, pinned) = call(
        &app,
        "GET",
        &format!("/api/v1/log/price-head?channel={channel}&model={model}&version=1"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pinned}");
    check_price_note(&pinned, &channel, 1);
    assert_eq!(
        pinned["data"]["note"].as_str().unwrap(),
        body["data"]["note"].as_str().unwrap(),
        "the v1 note is re-rendered identically"
    );
}

#[tokio::test]
async fn the_price_head_endpoint_refuses_the_cases_that_attest_nothing() {
    let (app, _pool) = app_or_skip!(Some(SEED));
    let (_registration, key) = register(&app, "price-heads-edges").await;
    let channel = fresh_channel("priced-edges");
    let model = fresh_channel("m-edges");
    price_one(&Db::from_pool(_pool.clone()), &channel, &model, 1_000_000).await;

    // An unwritten version: 404, not the latest in disguise.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/log/price-head?channel={channel}&model={model}&version=99"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // An unpriced model and an unknown channel: the same absence.
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/log/price-head?channel={channel}&model=never-priced"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/log/price-head?channel=never-a-channel&model=m",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // `model` is not optional: a request naming only a channel is a 400.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/log/price-head?channel={channel}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"].as_str().unwrap(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn the_price_head_endpoint_needs_a_seed_and_a_credential() {
    // No seed: the log surface degrades to 503, identically to /log/head.
    let (app, _pool) = app_or_skip!(None);
    let (_registration, key) = register(&app, "price-heads-noseed").await;
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/log/price-head?channel=anything&model=m",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        body["error"]["code"].as_str().unwrap(),
        "SERVICE_UNAVAILABLE"
    );

    // No credential: a checkpoint is served only to a principal.
    let app = offline_app(Some(SEED));
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/log/price-head?channel=anything&model=m",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// `/log/checkpoint` answers the last witness-anchored head with its
/// cosignatures — `data: null` before the first anchor (issue #231).
#[tokio::test]
async fn log_checkpoint() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let (app, pool) = online_app(&url, Some(SEED)).await;
    let (registration, key) = register(&app, "checkpoint").await;
    top_up(&app, &key, 10_000).await;
    let org_id = registration["organization"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Nothing anchored yet.
    let (status, body) = call(&app, "GET", "/api/v1/log/checkpoint", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"].is_null(), "{body}");

    // Simulate a finished witness pass: the stored note is the operator-signed
    // head this endpoint itself serves, plus one witness's cosigned note.
    let (_, head_body) = call(&app, "GET", "/api/v1/log/head", None, Some(&key)).await;
    let note = head_body["data"]["note"].as_str().unwrap().to_owned();
    let size = head_body["data"]["head"]["size"].as_u64().unwrap() as i64;
    sqlx::query(
        "INSERT INTO oxsum.witness_checkpoints (organization_id, head_size, note) \
         VALUES ($1::uuid, $2, $3)",
    )
    .bind(&org_id)
    .bind(size)
    .bind(&note)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.witness_cosignatures \
         (organization_id, witness_url, head_size, note) \
         VALUES ($1::uuid, 'https://witness.example', $2, $3)",
    )
    .bind(&org_id)
    .bind(size)
    .bind(format!("{note}\n\n— witness.example+abcd c2ln"))
    .execute(&pool)
    .await
    .unwrap();

    let (status, body) = call(&app, "GET", "/api/v1/log/checkpoint", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["headSize"], size, "{body}");
    assert_eq!(body["data"]["note"], note, "{body}");
    let cosigs = body["data"]["cosignatures"].as_array().unwrap();
    assert_eq!(cosigs.len(), 1, "{body}");
    assert_eq!(cosigs[0]["witnessUrl"], "https://witness.example");
    assert!(
        cosigs[0]["note"]
            .as_str()
            .unwrap()
            .contains("witness.example"),
        "{cosigs:?}"
    );
}

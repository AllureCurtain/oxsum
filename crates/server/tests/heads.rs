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
    std::env::var("DATABASE_URL").ok()
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

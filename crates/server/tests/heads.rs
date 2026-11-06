//! HTTP tests: signed tree heads, consistency proofs, signed price checkpoints and the
//! witness-anchored log checkpoint — the `/api/v1/log/*` surface the verifier reads.
//!
//! The offline tests cover the unconfigured-key 503 and the public key endpoint; the online
//! ones run the user flow from the issue: hold an old head, grow the log, then verify the
//! new head was appended onto the old one — signature first, consistency proof second.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

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
    support::app(Db::from_pool(unused_pool()), config)
}

/// An app over a real database with oxsum's tables migrated.
async fn online_app(url: &str, seed: Option<[u8; 32]>) -> (Router, PgPool) {
    online_app_with(url, seed, Vec::new()).await
}

/// `online_app` plus a pinned witness keyset — what `OXSUM_WITNESS_KEYS` would
/// have configured (issue #259).
async fn online_app_with(
    url: &str,
    seed: Option<[u8; 32]>,
    witness_keys: Vec<oxsum_server::WitnessKey>,
) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let mut config = Config::new(Signup::Open, None).with_witness_keys(witness_keys);
    if let Some(seed) = seed {
        config = config.with_head_signing_seed(seed);
    }
    (support::app(db, config), pool)
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
        "/__test/funding",
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

// ── signed sale heads (issue #600) ──────────────────────────────────────────
//
// The sale side of the price binding: a customer settlement's `sale.digest`
// commits a resolved `SalePriceSnapshot`, and `/api/v1/log/sale-head` serves
// its operator-signed checkpoint — origin `oxsum/sale-catalog/<business>`,
// the book version as `size`, the snapshot digest as `root`. These tests pin
// the endpoint's contract: the customer's own credential scopes the read,
// the digest path serves the stored canonical row verbatim, and the cases
// that would attest nothing answer 404 or 400 — never a borrowed identity.

/// Registers a customer, returns its org id and API key.
async fn sale_customer(app: &Router, name: &str) -> (String, String) {
    let (registration, key) = register(app, name).await;
    let org = registration["organization"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (org, key)
}

/// Gives `db` a business owner session and publishes a one-model book
/// assigned to `org` — the live terms `resolve_sale_price` then pins.
/// Returns the business id the sale-catalog origin names.
async fn assign_sale_book(db: &Db, owner: &str, org: &str, model: &str) -> uuid::Uuid {
    use oxsum_core::{SaleChange, SaleChangeRequest, SaleMutationResult, SaleTerms};
    use std::collections::BTreeMap;

    let org: uuid::Uuid = org.parse().unwrap();
    let business = db.business_ownership().await.unwrap().business.id;
    let terms: SaleTerms = serde_json::from_value(json!({
        "price": {
            "inputPricePerMillion": 1_000_000,
            "outputPricePerMillion": 2_000_000,
            "maxOutputTokens": 100
        },
        "discountPercent": 0
    }))
    .unwrap();
    let change = |change: SaleChange| {
        let db = db.clone();
        let owner = owner.to_owned();
        async move {
            db.change_sale_terms(
                &owner,
                &SaleChangeRequest {
                    idempotency_key: uuid::Uuid::new_v4().to_string(),
                    reason: "sale head fixture".into(),
                    change,
                },
            )
            .await
            .unwrap()
        }
    };
    let book: SaleMutationResult = change(SaleChange::CreateBook {
        name: uuid::Uuid::new_v4().to_string(),
    })
    .await;
    change(SaleChange::SaveDraft {
        book_id: book.resource_id,
        expected_revision: 0,
        models: BTreeMap::from([(model.to_owned(), terms)]),
    })
    .await;
    change(SaleChange::PublishDraft {
        book_id: book.resource_id,
        draft_revision: 1,
    })
    .await;
    change(SaleChange::AssignBook {
        organization_id: org,
        book_id: book.resource_id,
        version: 1,
        effective_from: None,
        effective_until: None,
        replaces: None,
    })
    .await;
    business
}

/// A business-owner session in Business context. The business is a singleton
/// and this binary's tests share one scratch database, so only the first test
/// to run initializes it through the production consent flow; a test that
/// finds it already initialized registers its own user and grants owner staff
/// directly — the consent path's coverage lives in `customer_lifecycle.rs`,
/// and this fixture only needs the role `change_sale_terms` checks.
async fn sale_owner(db: &Db) -> String {
    use oxsum_core::{
        BusinessSetupRequest, BusinessSetupTokenRequest, NewUser, SessionContext,
        SessionContextRequest, SetupAuthority,
    };
    let password = "sale head owner fixture password";
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", uuid::Uuid::new_v4()),
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
    if business.initialized_at.is_none() {
        let setup = db
            .mint_business_setup(
                SetupAuthority::Deployment,
                &BusinessSetupTokenRequest {
                    idempotency_key: uuid::Uuid::new_v4().to_string(),
                    owner_email: user.user.email.clone(),
                },
            )
            .await
            .unwrap();
        db.accept_business_setup(
            &session.token,
            &BusinessSetupRequest {
                token: setup.token.unwrap(),
                business_name: "Sale head fixture".into(),
            },
        )
        .await
        .unwrap();
    } else {
        sqlx::query(
            "INSERT INTO oxsum.business_staff (business_id,user_id,role) VALUES ($1,$2,'owner')",
        )
        .bind(business.id)
        .bind(user.user.id)
        .execute(db.pool())
        .await
        .unwrap();
    }
    db.switch_session_context(
        &session.token,
        &SessionContextRequest {
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    session.token
}

/// Checks a sale-head answer the way `check_price_note` checks a price head:
/// the note parses, the origin is the business's sale catalog, `size` is the
/// book version, `root` is the snapshot digest, and the operator's signature
/// verifies under the key `/log/key` publishes.
fn check_sale_note(body: &Value, business: &str, org: &str, model: &str) {
    let data = &body["data"];
    assert_eq!(data["businessId"].as_str().unwrap(), business);
    assert_eq!(data["organizationId"].as_str().unwrap(), org);
    assert_eq!(data["model"].as_str().unwrap(), model);
    assert_eq!(
        data["origin"].as_str().unwrap(),
        format!("oxsum/sale-catalog/{business}")
    );

    let note = SignedTreeHead::parse(data["note"].as_str().unwrap()).unwrap();
    assert_eq!(note.head.size, data["version"].as_u64().unwrap());
    assert_eq!(
        note.head.root.to_hex(),
        data["digest"].as_str().unwrap(),
        "the checkpoint root is the snapshot digest"
    );
    assert_eq!(
        data["rowHash"].as_str().unwrap(),
        data["digest"].as_str().unwrap(),
        "the row hash is the digest the settlement commits"
    );

    let key = published_key(body);
    let signature = note
        .signatures
        .iter()
        .find(|s| s.key_hash == key.note_key_hash())
        .expect("the operator's signature line is on the note");
    assert!(key.verify(&note.body(), signature));
}

#[tokio::test]
async fn a_sale_checkpoint_binds_the_customers_terms_to_the_signed_row() {
    let (app, pool) = app_or_skip!(Some(SEED));
    let db = Db::from_pool(pool.clone());
    let owner = sale_owner(&db).await;
    let (org, key) = sale_customer(&app, "sale-heads").await;
    let model = fresh_channel("sale-m");
    let business = assign_sale_book(&db, &owner, &org, &model).await;

    // By terms: the live resolution signs under the customer's own credential.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/log/sale-head?organizationId={org}&model={model}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    check_sale_note(&body, &business.to_string(), &org, &model);

    // By digest: the stored canonical row re-signs identically — the same
    // note, byte for byte.
    let digest = body["data"]["digest"].as_str().unwrap().to_owned();
    let (status, again) = call(
        &app,
        "GET",
        &format!("/api/v1/log/sale-head?digest={digest}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["data"]["note"].as_str().unwrap(),
        body["data"]["note"].as_str().unwrap(),
        "the committed digest re-signs deterministically"
    );
    check_sale_note(&again, &business.to_string(), &org, &model);
}

#[tokio::test]
async fn the_sale_head_endpoint_refuses_the_cases_that_attest_nothing() {
    let (app, pool) = app_or_skip!(Some(SEED));
    let db = Db::from_pool(pool.clone());
    let owner = sale_owner(&db).await;
    let (org, key) = sale_customer(&app, "sale-heads-edges").await;
    let (other_org, other_key) = sale_customer(&app, "sale-heads-other").await;
    let model = fresh_channel("sale-m-edges");
    assign_sale_book(&db, &owner, &org, &model).await;

    // A customer may name only its own organization: another customer's id is
    // 404, never a cross-customer read of their terms.
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/log/sale-head?organizationId={other_org}&model={model}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The digest that commits org A's terms answers not_found to org B's
    // credential — the signed head names its customer, and the endpoint holds
    // it to the caller.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/log/sale-head?organizationId={org}&model={model}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let digest = body["data"]["digest"].as_str().unwrap();
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/log/sale-head?digest={digest}"),
        None,
        Some(&other_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // No live terms for the model: absence is 404, not a checkpoint of nothing.
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/log/sale-head?organizationId={org}&model=never-priced"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An unrecorded digest is the same absence.
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/log/sale-head?digest={}", "0".repeat(64)),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Neither a digest nor the terms pair: 400, a request that cannot select
    // a snapshot at all.
    let (status, body) = call(&app, "GET", "/api/v1/log/sale-head", None, Some(&key)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"].as_str().unwrap(), "VALIDATION_ERROR");
}

#[tokio::test]
async fn the_sale_head_endpoint_needs_a_seed_and_a_credential() {
    // No seed: the log surface degrades to 503, identically to /log/head.
    let (app, _pool) = app_or_skip!(None);
    let (_org, key) = sale_customer(&app, "sale-heads-noseed").await;
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/log/sale-head?digest=anything",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(
        body["error"]["code"].as_str().unwrap(),
        "SERVICE_UNAVAILABLE"
    );

    // No credential: a customer's sale terms are served only to a principal.
    let app = offline_app(Some(SEED));
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/log/sale-head?digest=anything",
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
    // The signed-head response flattens the head beside `note`: `data.size`,
    // `data.root` — there is no `data.head` object.
    let size = head_body["data"]["size"].as_u64().unwrap() as i64;
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
    // No `OXSUM_WITNESS_KEYS` configured: the endpoint serves the cosigned note
    // but an empty pinset — a verifier holding its own pins can still check.
    assert_eq!(
        body["data"]["witnessKeys"].as_array().unwrap().len(),
        0,
        "{body}"
    );
}

/// `/log/checkpoint` serves the deployment's pinned witness keyset as
/// `witnessKeys` (issue #259), and the stored cosigned note verifies against
/// the pinned key end-to-end.
#[tokio::test]
async fn log_checkpoint_serves_the_pinned_witness_keys() {
    use doubleentry::witness::Origin;
    use doubleentry::witness::signing::SigningKey;

    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };

    // A real witness key pair: the pin is what `OXSUM_WITNESS_KEYS` publishes —
    // name, hex public key, hex cosignature selector — and the cosigned note it
    // produces must verify against exactly that pin.
    let witness_url = "https://witness.example.com";
    let signing = SigningKey::from_seed(KeyName::new("w1").unwrap(), [7; 32]);
    let verifying = signing.verifying_key();
    let pin = oxsum_server::WitnessKey {
        witness_url: witness_url.to_owned(),
        key_name: verifying.name().as_str().to_owned(),
        public_key: hex(&verifying.to_bytes()),
        key_hash: hex(&verifying.cosignature_key_hash()),
    };

    let (app, pool) = online_app_with(&url, Some(SEED), vec![pin]).await;
    let (registration, key) = register(&app, "checkpoint-keys").await;
    top_up(&app, &key, 10_000).await;
    let org_id = registration["organization"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Anchor a checkpoint the witness then cosigns: the operator-signed note
    // plus the witness's own `cosignature/v1` line over the same body.
    let (_, head_body) = call(&app, "GET", "/api/v1/log/head", None, Some(&key)).await;
    let note = head_body["data"]["note"].as_str().unwrap().to_owned();
    let size = head_body["data"]["size"].as_u64().unwrap() as i64;
    let parsed = SignedTreeHead::parse(&note).unwrap();
    let body_text = parsed.body();
    let mut cosigned =
        SignedTreeHead::new(Origin::new(parsed.origin.as_str()).unwrap(), parsed.head);
    cosigned.add_signature(signing.cosign_body(&body_text, 1_762_000_000));
    let cosigned_note = cosigned.to_note();

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
         VALUES ($1::uuid, $2, $3, $4)",
    )
    .bind(&org_id)
    .bind(witness_url)
    .bind(size)
    .bind(&cosigned_note)
    .execute(&pool)
    .await
    .unwrap();

    let (status, body) = call(&app, "GET", "/api/v1/log/checkpoint", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let keys = body["data"]["witnessKeys"].as_array().unwrap();
    assert_eq!(keys.len(), 1, "{body}");
    assert_eq!(keys[0]["witnessUrl"], witness_url, "{body}");
    assert_eq!(keys[0]["keyName"], "w1", "{body}");
    assert_eq!(
        keys[0]["keyHash"],
        hex(&verifying.cosignature_key_hash()),
        "{body}"
    );

    // The served pin verifies the served cosignature under the checkpoint's own
    // origin and head — the check oxsum_verify::verify_witness runs.
    let verified = oxsum_verify::verify_witness(
        &oxsum_verify::WitnessNote {
            witness_url,
            note: &cosigned_note,
        },
        &[oxsum_verify::WitnessPin {
            witness_url,
            key_name: keys[0]["keyName"].as_str().unwrap(),
            public_key: keys[0]["publicKey"].as_str().unwrap(),
            key_hash: keys[0]["keyHash"].as_str().unwrap(),
        }],
        parsed.origin.as_str(),
        &parsed.head,
    );
    assert!(
        matches!(
            verified,
            oxsum_verify::WitnessVerdict::Verified { at: 1_762_000_000 }
        ),
        "{verified:?}"
    );
}

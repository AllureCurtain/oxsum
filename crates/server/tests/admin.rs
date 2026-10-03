//! The platform admin surface: channels, their prices, and the operator token that opens it.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of failing, so
//! a bare `cargo test` still passes.
//!
//! The database is shared between tests, so every channel these tests create has a name of its own.
//! What the price *history* means for a bill is tested where the bill is written: the gateway tests
//! change a price while a streamed turn is in flight (crates/server/tests/gateway.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// The operator token these tests configure. Long enough to be worth configuring, per config.rs.
const TOKEN: &str = "operator-token-0123456789";

/// The key that seals channel credentials in these tests.
fn secret() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => admin_app(&u, Some(TOKEN)).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// An app over a real database, with oxsum's tables migrated and the admin surface configured.
async fn admin_app(database_url: &str, token: Option<&str>) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let mut config = Config::new(Signup::Open, None).with_secret(secret());
    if let Some(token) = token {
        config = config.with_admin_token(token);
    }
    // A deployment never reaches the listener without this, and a test should not either: it is what
    // proves the configured key can open every channel in the database.
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    (oxsum_server::app(db, config), pool)
}

/// A channel name of this test's own, so tests sharing the database never collide.
fn fresh(name: &str) -> String {
    format!("{name}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// One call against the app, with the operator token unless another credential is given.
async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    call_with(app, method, path, body, Some(TOKEN)).await
}

/// One call with a chosen credential, or none at all.
async fn call_with(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .header("content-type", "application/json")
        .body(match body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        })
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_channel_is_created_priced_and_listed() {
    let (app, _pool) = app_or_skip!();
    let channel = fresh("admin");
    let model = fresh("model");

    // Nothing there yet.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // Creating it: the credential comes back as its last four characters and nothing more.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/channels",
        Some(json!({
            "name": channel,
            "baseUrl": "https://upstream.example/v1/",
            "apiKey": "sk-upstream-abcd1234",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["name"], channel.as_str());
    assert_eq!(body["data"]["baseUrl"], "https://upstream.example/v1");
    assert_eq!(body["data"]["apiKeyLast4"], "1234");
    assert_eq!(body["data"]["models"].as_array().expect("models").len(), 0);
    assert!(!body.to_string().contains("sk-upstream-abcd1234"), "{body}");

    // Prices: the first write is version 1, the second appends version 2.
    let price = |input: i64, output: i64| {
        json!({
            "model": model,
            "inputPricePerMillion": input,
            "outputPricePerMillion": output,
            "maxOutputTokens": 1000,
        })
    };
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        Some(price(1_000_000, 2_000_000)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], 1);
    assert_eq!(body["data"]["model"], model.as_str());

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        Some(price(3_000_000, 4_000_000)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], 2);

    // The list shows the current version only…
    let (status, body) = call(&app, "GET", "/api/v1/admin/channels", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body["data"]
        .as_array()
        .expect("a list of channels")
        .iter()
        .find(|entry| entry["name"] == channel.as_str())
        .expect("this test's channel is listed");
    assert_eq!(listed["models"].as_array().expect("models").len(), 1);
    assert_eq!(listed["models"][0]["version"], 2);
    assert_eq!(listed["models"][0]["inputPricePerMillion"], 3_000_000);

    // …and the history still holds the first one, which is what an old bill was priced by.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let versions = body["data"].as_array().expect("a list of versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["version"], 2);
    assert_eq!(versions[0]["outputPricePerMillion"], 4_000_000);
    assert_eq!(versions[1]["version"], 1);
    assert_eq!(versions[1]["inputPricePerMillion"], 1_000_000);
    assert_eq!(versions[1]["outputPricePerMillion"], 2_000_000);
}

#[tokio::test]
async fn changing_the_connection_keeps_the_prices() {
    let (app, _pool) = app_or_skip!();
    let channel = fresh("repointed");
    let model = fresh("model");
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/channels",
        Some(json!({
            "name": channel,
            "baseUrl": "https://first.example/v1",
            "apiKey": "sk-first",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        Some(json!({
            "model": model,
            "inputPricePerMillion": 1_000_000,
            "outputPricePerMillion": 1_000_000,
            "maxOutputTokens": 100,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A second POST of the same name is the same channel with a new connection and its own key.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/channels",
        Some(json!({
            "name": channel,
            "baseUrl": "https://second.example/v1",
            "apiKey": "sk-second",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["baseUrl"], "https://second.example/v1");
    assert_eq!(body["data"]["apiKeyLast4"], "cond");
    // The version it already had is still the version in force.
    assert_eq!(body["data"]["models"][0]["version"], 1);
    assert_eq!(body["data"]["models"][0]["model"], model.as_str());
}

#[tokio::test]
async fn only_the_operator_token_opens_the_surface() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let (app, _pool) = admin_app(&url, Some(TOKEN)).await;
    let channels = "/api/v1/admin/channels";

    // No credential, a wrong one, and a key-shaped one that belongs to no organization: all 401.
    for token in [None, Some("not-the-token"), Some("oxs-0123456789abcdef")] {
        let (status, body) = call_with(&app, "GET", channels, None, token).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}: {body}");
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }

    // A deployment with no token configured has no admin surface at all, not an open one.
    let (unconfigured, _pool) = admin_app(&url, None).await;
    let (status, body) = call_with(&unconfigured, "GET", channels, None, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn what_could_not_be_used_is_refused_in_oxsums_shape() {
    let (app, _pool) = app_or_skip!();
    let channel = fresh("refusals");
    let model = fresh("model");

    // A channel that could not be reached or authenticated is not stored.
    for bad in [
        json!({"name": "bad name", "baseUrl": "https://x.example/v1", "apiKey": "sk"}),
        json!({"name": fresh("nourl"), "baseUrl": "not a url", "apiKey": "sk"}),
        json!({"name": fresh("noscheme"), "baseUrl": "ftp://x.example", "apiKey": "sk"}),
        json!({"name": fresh("nokey"), "baseUrl": "https://x.example", "apiKey": "  "}),
    ] {
        let (status, body) = call(&app, "POST", "/api/v1/admin/channels", Some(bad.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    }

    // An unknown channel is not found rather than a bad request.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/channels/{}/prices", fresh("absent")),
        Some(json!({
            "model": model,
            "inputPricePerMillion": 1,
            "outputPricePerMillion": 1,
            "maxOutputTokens": 10,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "NOT_FOUND");

    // A price that could not be charged by is refused, and so is an unknown field.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/channels",
        Some(json!({
            "name": channel,
            "baseUrl": "https://x.example/v1",
            "apiKey": "sk-1234",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for bad in [
        json!({"model": model, "inputPricePerMillion": -1, "outputPricePerMillion": 0, "maxOutputTokens": 10}),
        json!({"model": model, "inputPricePerMillion": 0, "outputPricePerMillion": 0, "maxOutputTokens": 0}),
        json!({"model": " ", "inputPricePerMillion": 0, "outputPricePerMillion": 0, "maxOutputTokens": 10}),
    ] {
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/v1/admin/channels/{channel}/prices"),
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }

    // A model another channel already serves is a conflict: one model belongs to one channel.
    let owner = fresh("owner");
    let shared = fresh("shared");
    for name in [&owner, &channel] {
        let (status, _) = call(
            &app,
            "POST",
            "/api/v1/admin/channels",
            Some(json!({
                "name": name,
                "baseUrl": "https://x.example/v1",
                "apiKey": "sk-1234",
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let price = json!({
        "model": shared,
        "inputPricePerMillion": 1,
        "outputPricePerMillion": 1,
        "maxOutputTokens": 10,
    });
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/channels/{owner}/prices"),
        Some(price.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        Some(price),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "CONFLICT");
}

/// One GET whose body is kept as bytes: the server-rendered pages are HTML, and the
/// error envelope's `serde_json` parse would drop what this test asserts on.
async fn get_raw(app: &Router, path: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The platform-admin pages are served by the same binary: the token gate is what SSR
/// renders (the credential is never the server's to hold), and it asks for the operator
/// token rather than a login — no session enters this surface.
#[tokio::test]
async fn the_admin_pages_render_the_token_gate() {
    let (app, _pool) = app_or_skip!();
    let (status, html) = get_raw(&app, "/admin/channels").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET /admin/channels answered {status}"
    );
    assert!(
        html.contains("Platform admin"),
        "GET /admin/channels renders the gate: {html}"
    );
    assert!(
        html.contains("Operator token"),
        "GET /admin/channels asks for the operator token: {html}"
    );

    // `/admin` lands on the channels page — by a redirect or by the rendered gate,
    // whichever the router emits; a 404 is the wrong answer either way.
    let (status, _) = get_raw(&app, "/admin").await;
    assert!(
        status.is_success() || status.is_redirection(),
        "GET /admin answered {status}"
    );
}

/// Registers an account so a personal organization exists to list; answers its id and
/// the API key the signup minted, which is a credential of the organization — never of
/// the platform.
async fn register(app: &Router, name: &str) -> (String, String) {
    let (status, body) = call_with(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({
            "email": format!("{name}_{}@example.com", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            "password": "correct horse battery",
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["data"]["organization"]["id"]
            .as_str()
            .expect("an organization id")
            .to_owned(),
        body["data"]["apiKey"]["secret"]
            .as_str()
            .expect("the minted key")
            .to_owned(),
    )
}

/// `GET /api/v1/admin/organizations` lists every organization with its balance, under
/// the operator token only: an organization's own key — a credential with all of the
/// organization's authority — is not the platform's.
#[tokio::test]
async fn the_organizations_list_shows_every_organization_with_its_balance() {
    let (app, _pool) = app_or_skip!();
    let (id, key) = register(&app, "orgview").await;

    // The organization tops itself up through its own credential, the way the chat
    // page does — the admin surface has no top-up yet (#60).
    let (status, body) = call_with(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "topup-orgview-1", "amountMinor": 7_500_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(&app, "GET", "/api/v1/admin/organizations", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let organizations = body["data"].as_array().expect("a list of organizations");
    let org = organizations
        .iter()
        .find(|org| org["id"] == id)
        .expect("the registered organization is listed");
    assert_eq!(org["kind"], "personal");
    assert_eq!(org["members"], 1);
    assert_eq!(
        org["availableMinor"], 7_500_000,
        "the top-up settled: {org}"
    );
    assert_eq!(org["reservedMinor"], 0);

    // Neither a wrong token nor an organization's own credential opens the list.
    let (status, _) = call_with(
        &app,
        "GET",
        "/api/v1/admin/organizations",
        None,
        Some("not-the-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call_with(&app, "GET", "/api/v1/admin/organizations", None, Some(&key)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

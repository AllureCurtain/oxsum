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
    assert_eq!(body["data"]["protocol"], "openai");
    assert_eq!(body["data"]["apiKeyLast4"], "1234");
    assert_eq!(body["data"]["models"].as_array().expect("models").len(), 0);
    assert!(!body.to_string().contains("sk-upstream-abcd1234"), "{body}");

    // A protocol the adapter registry does not know is refused at write time: the
    // channel could never normalize its usage reports, so the row must not exist.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/channels",
        Some(json!({
            "name": fresh("admin"),
            "baseUrl": "https://upstream.example/v1",
            "apiKey": "sk-upstream-abcd1234",
            "protocol": "anthropic",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");

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

/// An itemized price — cache, reasoning, flat fee, upstream costs and a
/// conditional set — writes and reads back whole, while a book whose rules
/// could price the same request at the same specificity is refused (issue #108).
#[tokio::test]
async fn an_itemized_price_writes_lists_and_refuses_ambiguity() {
    let (app, _pool) = app_or_skip!();
    let channel = fresh("priced");
    let model = fresh("model");
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/channels",
        Some(json!({
            "name": channel,
            "baseUrl": "https://upstream.example/v1",
            "apiKey": "sk-upstream-abcd1234",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let price = json!({
        "model": model,
        "inputPricePerMillion": 1_000_000,
        "outputPricePerMillion": 2_000_000,
        "maxOutputTokens": 1000,
        "cacheReadPricePerMillion": 100_000,
        "cacheWrite5mPricePerMillion": 1_250_000,
        "cacheWrite1hPricePerMillion": 2_500_000,
        "reasoningPricePerMillion": 4_000_000,
        "costPerRequest": 500,
        "mode": "chat",
        "upstream": {
            "inputPricePerMillion": 400_000,
            "outputPricePerMillion": 800_000,
        },
        "rules": [{
            "match": {"serviceTier": "priority"},
            "price": {
                "inputPricePerMillion": 5_000_000,
                "outputPricePerMillion": 6_000_000,
                "maxOutputTokens": 2000,
            },
        }],
    });
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        Some(price),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["version"], 1);

    // The history reads every dimension back, rules and upstream included.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = &body["data"][0];
    assert_eq!(listed["cacheReadPricePerMillion"], 100_000);
    assert_eq!(listed["cacheWrite5mPricePerMillion"], 1_250_000);
    assert_eq!(listed["cacheWrite1hPricePerMillion"], 2_500_000);
    assert_eq!(listed["reasoningPricePerMillion"], 4_000_000);
    assert_eq!(listed["costPerRequest"], 500);
    assert_eq!(listed["mode"], "chat");
    assert_eq!(listed["upstream"]["inputPricePerMillion"], 400_000);
    assert_eq!(listed["rules"][0]["match"]["serviceTier"], "priority");
    assert_eq!(
        listed["rules"][0]["price"]["inputPricePerMillion"],
        5_000_000
    );

    // Two windows at the same specificity that overlap are ambiguous, and the
    // write is refused rather than silently ordered.
    let ambiguous = json!({
        "model": model,
        "inputPricePerMillion": 1,
        "outputPricePerMillion": 1,
        "maxOutputTokens": 10,
        "rules": [
            {"match": {"minInputTokens": 100}, "price": {"inputPricePerMillion": 1, "outputPricePerMillion": 1, "maxOutputTokens": 10}},
            {"match": {"maxInputTokens": 1000}, "price": {"inputPricePerMillion": 1, "outputPricePerMillion": 1, "maxOutputTokens": 10}},
        ],
    });
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/channels/{channel}/prices"),
        Some(ambiguous),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
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
    // page does — an admin's way into another organization's money is an adjustment.
    let (status, body) = call_with(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "topup-orgview-1", "amountMinor": 7_500_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The list is oldest-first pages: a brand-new organization is the last row, so
    // the read walks the cursor to it (issue #93).
    let mut org = None;
    let mut cursor: Option<String> = None;
    while org.is_none() {
        let path = match &cursor {
            Some(c) => format!("/api/v1/admin/organizations?cursor={c}"),
            None => "/api/v1/admin/organizations".to_owned(),
        };
        let (status, body) = call(&app, "GET", &path, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let organizations = body["data"]["organizations"]
            .as_array()
            .expect("a list of organizations");
        org = organizations.iter().find(|org| org["id"] == id).cloned();
        match body["data"]["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    let org = org.expect("the registered organization is listed");
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

/// `GET /api/v1/admin/organizations` paginates (issue #93): `limit` caps the page,
/// `nextCursor` resumes where it ended — echoed verbatim — and a cursor that was
/// never issued is a validation error, not a server error.
#[tokio::test]
async fn the_organizations_list_paginates_oldest_first() {
    let (app, _pool) = app_or_skip!();
    register(&app, "orgpage").await;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    // Walk three pages deep at most: the shared database's tail is unbounded, and
    // ordering and resume are what the walk asserts.
    for _ in 0..3 {
        let path = match &cursor {
            Some(c) => format!("/api/v1/admin/organizations?limit=1&cursor={c}"),
            None => "/api/v1/admin/organizations?limit=1".to_owned(),
        };
        let (status, body) = call(&app, "GET", &path, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let organizations = body["data"]["organizations"]
            .as_array()
            .expect("a page of organizations");
        assert_eq!(organizations.len(), 1, "the page honors its limit: {body}");
        seen.push(
            organizations[0]["id"]
                .as_str()
                .expect("an organization id")
                .to_owned(),
        );
        match body["data"]["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert!(seen.len() >= 2, "at least two organizations were paged");
    assert_eq!(
        seen.iter().collect::<std::collections::HashSet<_>>().len(),
        seen.len(),
        "no organization twice: {seen:?}"
    );

    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/admin/organizations?cursor=not-a-cursor",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

/// `GET /api/v1/admin/holds` lists every unsettled hold globally, named by the
/// organization whose ledger reserves it — the in-flight page's source.
#[tokio::test]
async fn the_holds_list_shows_what_the_platform_is_reserving() {
    let (app, pool) = app_or_skip!();
    let (id, _key) = register(&app, "inflight").await;

    // The tenant id the ledger knows the organization by, and a hold watched for it —
    // the gateway's own write path, done by hand.
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(uuid::Uuid::parse_str(&id).unwrap())
            .fetch_one(&pool)
            .await
            .expect("the organization's tenant id");
    let request = fresh("req");
    let hold_key = format!("{request}:hold");
    sqlx::query(
        "INSERT INTO oxsum.open_holds \
         (hold_key, tenant_id, request_id, model, channel, price_version, \
          input_price, output_price, freeze_minor) \
         VALUES ($1, $2, $3, $4, $5, 1, 0, 0, 1234)",
    )
    .bind(&hold_key)
    .bind(&tenant_id)
    .bind(&request)
    .bind("model-x")
    .bind("chan-y")
    .execute(&pool)
    .await
    .expect("a watched hold");

    let (status, body) = call(&app, "GET", "/api/v1/admin/holds", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let holds = body["data"].as_array().expect("a list of holds");
    let hold = holds
        .iter()
        .find(|hold| hold["requestId"] == request)
        .expect("the hold is listed");
    assert_eq!(hold["model"], "model-x");
    assert_eq!(hold["channel"], "chan-y");
    assert_eq!(hold["priceVersion"], 1);
    assert_eq!(hold["freezeMinor"], 1234);
    assert!(
        hold["openedAt"].is_string(),
        "when the hold opened is listed: {hold}"
    );
    assert_ne!(
        hold["organization"], tenant_id,
        "the organization is named, not keyed: {hold}"
    );

    sqlx::query("DELETE FROM oxsum.open_holds WHERE hold_key = $1")
        .bind(&hold_key)
        .execute(&pool)
        .await
        .expect("the test's hold is cleaned up");
}

/// `GET /api/v1/admin/anomalies` lists the settled turns that did not price cleanly —
/// `capped`, `estimated`, `client_cancelled`, `swept`, `unpriced` — read back out of each
/// organization's own ledger, so a normal usage settlement and an upstream error do not
/// appear, and the rows carry what an operator needs to find where money leaks
/// upstream.
#[tokio::test]
async fn the_anomalies_list_shows_the_turns_that_did_not_price_cleanly() {
    let (app, pool) = app_or_skip!();
    let (id, key) = register(&app, "anomaly").await;
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(uuid::Uuid::parse_str(&id).expect("a uuid"))
            .fetch_one(&pool)
            .await
            .expect("the registered organization has a tenant");
    let wallet = oxsum_core::Wallet::open(pool.clone(), &tenant_id)
        .await
        .expect("the organization's wallet opens");
    let today = time::OffsetDateTime::now_utc().date();
    let topup = fresh("topup-anomaly");
    wallet
        .top_up(&topup, 10_000_000, today)
        .await
        .expect("a top-up to settle against");

    // One settlement of each kind. The requests written anomalously are the ones the
    // endpoint must list; `usage` and `upstream_error` are the clean turns it must not.
    let kinds = [
        ("usage", "usage-ok"),
        ("upstream_error", "upstream-err"),
        ("capped", "anomaly-capped"),
        ("estimated", "anomaly-estimated"),
        ("client_cancelled", "anomaly-cancelled"),
        ("swept", "anomaly-swept"),
        ("unpriced", "anomaly-unpriced"),
    ];
    for (kind, request) in kinds {
        let hold_key = fresh(request);
        wallet
            .hold(&hold_key, "", 900_000, today)
            .await
            .expect("a hold settles");
        let usage = oxsum_core::UsageRecord::tokens(10, 20).expect("the seed counts");
        let lines = [
            oxsum_core::BillLine {
                item: "input".to_owned(),
                units: usage.input_tokens,
                price_per_m: 1_000,
            },
            oxsum_core::BillLine {
                item: "output".to_owned(),
                units: usage.output_tokens,
                price_per_m: 2_000,
            },
        ];
        let description = oxsum_core::Settlement {
            request,
            channel: "chan-y",
            model: "model-x",
            price_version: 2,
            kind: serde_json::from_str::<oxsum_core::SettlementKind>(&format!("\"{kind}\""))
                .expect("a settlement kind"),
            usage: &usage,
            lines: &lines,
            matched_rule: None,
            charged: 60,
            freeze: 900_000,
        }
        .description()
        .expect("the settlement record serializes");
        wallet
            .settle(&hold_key, &description, 60, today)
            .await
            .expect("the turn settles");
    }

    let (status, body) = call(&app, "GET", "/api/v1/admin/anomalies", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let anomalies = body["data"].as_array().expect("a list of anomalies");

    for request in [
        "anomaly-capped",
        "anomaly-estimated",
        "anomaly-cancelled",
        "anomaly-swept",
        "anomaly-unpriced",
    ] {
        let row = anomalies
            .iter()
            .find(|row| row["requestId"] == request)
            .unwrap_or_else(|| panic!("{request} is listed: {anomalies:?}"));
        assert_eq!(row["channel"], "chan-y");
        assert_eq!(row["model"], "model-x");
        assert_eq!(row["priceVersion"], 2);
        assert_eq!(row["chargedMinor"], 60);
        assert_eq!(row["freezeMinor"], 900_000);
        assert!(row["bookedOn"].is_string(), "the booking date: {row}");
        assert_ne!(
            row["organization"], tenant_id,
            "the organization is named, not keyed: {row}"
        );
    }
    assert_eq!(
        anomalies
            .iter()
            .find(|row| row["requestId"] == "anomaly-capped")
            .unwrap()["kind"],
        "capped",
    );

    // Clean turns never appear: no `usage`, no upstream error.
    for request in ["usage-ok", "upstream-err"] {
        assert!(
            !anomalies.iter().any(|row| row["requestId"] == request),
            "{request} is not an anomaly"
        );
    }

    // A wrong token and an organization's own credential stay out.
    let (status, _) = call_with(
        &app,
        "GET",
        "/api/v1/admin/anomalies",
        None,
        Some("not-the-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call_with(&app, "GET", "/api/v1/admin/anomalies", None, Some(&key)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// `GET /api/v1/admin/margin` sums what the platform charged against what
/// upstream cost it, per channel and model — and separates the turns no
/// `upstream` block tracked from the ones it priced at zero (issue #112).
#[tokio::test]
async fn the_margin_view_sums_charges_against_upstream_costs() {
    let (app, pool) = app_or_skip!();
    let db = Db::from_pool(pool.clone());
    let channel = fresh("margin-chan");
    let model = fresh("margin-model");

    // Three turns on the pair: two the price tracked upstream for (charged 12
    // at a cost of 6, charged 8 at a cost of 4) and one untracked — a swept
    // row, or a price with no `upstream` block.
    for (i, (charged, upstream)) in [(12, Some(6)), (8, Some(4)), (5, None)].iter().enumerate() {
        db.record_usage(&oxsum_core::UsageRow {
            request_id: format!("margin-{}-{i}", &channel[channel.len() - 8..]),
            tenant_id: "margin-test".to_owned(),
            key_id: None,
            model: model.clone(),
            channel: channel.clone(),
            price_version: 1,
            kind: oxsum_core::SettlementKind::Usage,
            entry_id: uuid::Uuid::new_v4(),
            usage: oxsum_core::UsageRecord::tokens(10, 2).expect("the seed counts"),
            charged_minor: *charged,
            freeze_minor: 100,
            upstream_cost_minor: *upstream,
        })
        .await
        .expect("the row is written");
    }

    let (status, body) = call(&app, "GET", "/api/v1/admin/margin", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let margin = body["data"].as_array().expect("a list of margin rows");
    let row = margin
        .iter()
        .find(|row| row["channel"] == channel)
        .unwrap_or_else(|| panic!("{channel} is listed: {margin:?}"));
    assert_eq!(row["model"], model);
    assert_eq!(row["turns"], 3);
    assert_eq!(row["chargedMinor"], 25);
    // Upstream tracked 10 of the 25 charged; the margin is the difference.
    assert_eq!(row["upstreamCostMinor"], 10);
    assert_eq!(row["marginMinor"], 15);
    // The untracked turn is counted on its own — NULL is not zero, so a
    // coverage gap shows up here instead of inflating the margin.
    assert_eq!(row["untrackedTurns"], 1);

    // The operator's only: an organization's key stays out, like every admin view.
    let (status, _) = call_with(
        &app,
        "GET",
        "/api/v1/admin/margin",
        None,
        Some("not-the-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// `GET`/`POST /api/v1/admin/closings`: closing a month seals it in every
/// organization's ledger — the seal is the closing record — and a sealed month
/// accepts no new entries. Only a month that has fully ended can close, and
/// closing one twice answers the same record.
#[tokio::test]
async fn closing_a_month_seals_it_everywhere() {
    let (app, pool) = app_or_skip!();
    let (id, key) = register(&app, "closing").await;

    // The organization has something in its ledger to close over.
    let (status, body) = call_with(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": fresh("topup-closing"), "amountMinor": 5_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The month before today's always exists and has always fully ended.
    let today = time::OffsetDateTime::now_utc().date();
    let previous = today.replace_day(1).unwrap() - time::Duration::days(1);
    let month = format!("{:04}-{:02}", previous.year(), u8::from(previous.month()));

    // The month still running, and a malformed one, are refused.
    let current = format!("{:04}-{:02}", today.year(), u8::from(today.month()));
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/admin/closings",
        Some(json!({"month": current})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the running month cannot close"
    );
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/admin/closings",
        Some(json!({"month": "last month"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a malformed month is refused"
    );

    // Close last month: every organization answers its closing record.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/closings",
        Some(json!({"month": &month})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let records = body["data"].as_array().expect("the closing records");
    let record = records
        .iter()
        .find(|row| row["organization"].is_string())
        .expect("each organization sealed the month");
    assert_eq!(record["period"], month);
    assert!(record["treeRoot"].is_string());
    assert!(record["trialBalanceRoot"].is_string());
    assert!(record["sealHash"].is_string());

    // GET lists what the ledgers hold — the same record, newest first.
    let (status, body) = call(&app, "GET", "/api/v1/admin/closings", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let closings = body["data"].as_array().expect("a list of closings");
    assert!(
        closings.iter().any(|row| row["period"] == month),
        "the closed month is listed: {closings:?}"
    );

    // Closing it again answers the record again, not an error.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/closings",
        Some(json!({"month": &month})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // And the month really is closed: an entry dated into it is refused by the
    // ledger itself — the sealed watermark, not a route-layer convention.
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(uuid::Uuid::parse_str(&id).expect("a uuid"))
            .fetch_one(&pool)
            .await
            .expect("the registered organization has a tenant");
    let wallet = oxsum_core::Wallet::open(pool.clone(), &tenant_id)
        .await
        .expect("the organization's wallet opens");
    let inside = previous;
    let result = wallet.hold(&fresh("closed-month"), "", 1000, inside).await;
    assert!(
        result.is_err(),
        "an entry dated into a sealed month is refused"
    );
    // The current month still takes entries.
    let (status, body) = call_with(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": fresh("topup-after-close"), "amountMinor": 1_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the open month still writes: {body}"
    );

    // A wrong token and an organization's own credential stay out.
    for body in [None, Some(json!({"month": &month}))] {
        let (status, _) = call_with(
            &app,
            if body.is_some() { "POST" } else { "GET" },
            "/api/v1/admin/closings",
            body.clone(),
            Some("not-the-token"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = call_with(
            &app,
            if body.is_none() { "GET" } else { "POST" },
            "/api/v1/admin/closings",
            body.clone(),
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

/// `POST /api/v1/admin/organizations/{id}/adjustments` books a signed amount with a
/// required reason: a grant raises the balance, a deduction lowers it, and a
/// deduction beyond what the wallet holds is the ledger's own no-overdraft refusal —
/// 402, the same one a hold gets.
#[tokio::test]
async fn an_adjustment_grants_and_deducts_with_a_reason() {
    let (app, _pool) = app_or_skip!();
    let (org, key) = register(&app, "adjust").await;
    let path = format!("/api/v1/admin/organizations/{org}/adjustments");

    // Grant.
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({
            "amountMinor": 5_000_000,
            "reason": "goodwill credit",
            "idempotencyKey": "adj-grant-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["amountMinor"], 5_000_000);
    assert_eq!(body["data"]["reason"], "goodwill credit");
    assert_eq!(body["data"]["availableMinor"], 5_000_000);
    assert_eq!(body["data"]["organizationId"].as_str(), Some(org.as_str()));
    assert!(
        body["data"]["entryId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "the booked entry's id"
    );

    // Deduct.
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({
            "amountMinor": -2_000_000,
            "reason": "billing correction",
            "idempotencyKey": "adj-deduct-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["availableMinor"], 3_000_000);

    // A deduction deeper than the balance is refused by the wallet's limit, and the
    // reason lands in the organization's own log — the entry the grant wrote.
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({
            "amountMinor": -4_000_000,
            "reason": "too deep",
            "idempotencyKey": "adj-deep-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["error"]["code"], "INSUFFICIENT_FUNDS");

    // The organization's own read agrees.
    let (status, body) = call_with(&app, "GET", "/api/v1/balance", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["availableMinor"], 3_000_000);
}

/// Zero is not an adjustment, an empty reason is not a reason, and an unknown
/// organization is not found — the envelope codes say which.
#[tokio::test]
async fn an_adjustment_needs_a_reason_a_nonzero_amount_and_a_real_organization() {
    let (app, _pool) = app_or_skip!();
    let (org, _key) = register(&app, "adjustbad").await;
    let path = format!("/api/v1/admin/organizations/{org}/adjustments");

    for body in [
        json!({"amountMinor": 0, "reason": "nothing", "idempotencyKey": "adj-bad-0"}),
        json!({"amountMinor": 1_000_000, "reason": "   ", "idempotencyKey": "adj-bad-1"}),
        json!({"amountMinor": 1_000_000, "reason": "no key"}),
    ] {
        let (status, body) = call(&app, "POST", &path, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    }

    let (status, body) = call(
        &app,
        "POST",
        &format!(
            "/api/v1/admin/organizations/{}/adjustments",
            uuid::Uuid::new_v4()
        ),
        Some(json!({"amountMinor": 1, "reason": "nobody", "idempotencyKey": "adj-bad-2"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// A retried adjustment is the same entry — the idempotency key, not a new write —
/// and only the operator token reaches the endpoint at all.
#[tokio::test]
async fn an_adjustment_replays_and_refuses_every_credential_but_the_operators() {
    let (app, _pool) = app_or_skip!();
    let (org, key) = register(&app, "adjustidem").await;
    let path = format!("/api/v1/admin/organizations/{org}/adjustments");
    let request = json!({
        "amountMinor": 1_000_000,
        "reason": "launch credit",
        "idempotencyKey": "adj-idem-1",
    });

    let (status, first) = call(&app, "POST", &path, Some(request.clone())).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second) = call(&app, "POST", &path, Some(request)).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(first["data"]["entryId"], second["data"]["entryId"]);
    assert_eq!(
        second["data"]["availableMinor"], 1_000_000,
        "no second write"
    );

    for token in [None, Some("not-the-token"), Some(key.as_str())] {
        let (status, _) = call_with(&app, "POST", &path, None, token).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}");
    }
}

/// `OXSUM_SIGNUP_BONUS_MINOR` credits a new organization's wallet at registration;
/// at zero — the default — registration writes no ledger entry, so the balance is 0.
#[tokio::test]
async fn the_signup_bonus_credits_a_new_organization_at_registration() {
    let Some(u) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None)
        .with_secret(secret())
        .with_admin_token(TOKEN)
        .with_signup_bonus(2_000_000);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = oxsum_server::app(db, config);

    let (_org, key) = register(&app, "bonus").await;
    let (status, body) = call_with(&app, "GET", "/api/v1/balance", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["availableMinor"], 2_000_000,
        "the bonus landed"
    );
}

/// `POST /api/v1/admin/users/{userId}/password-reset` sets a new password and
/// revokes every session the user held — the only recovery path v1 has, since
/// no email is sent. The organization's own credential cannot reach it.
#[tokio::test]
async fn a_password_reset_changes_the_password_and_kills_the_sessions() {
    let (app, pool) = app_or_skip!();

    let (status, body) = call_with(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({
            "email": format!("pwreset_{}@example.com", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            "password": "correct horse battery",
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let user_id = body["data"]["user"]["id"].as_str().unwrap().to_owned();
    let email = body["data"]["user"]["email"].as_str().unwrap().to_owned();

    // One live session for the reset to kill.
    let (status, body) = call_with(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // An unknown user is 404 — the operator token already authorizes the call.
    let (status, body) = call(
        &app,
        "POST",
        &format!(
            "/api/v1/admin/users/{}/password-reset",
            uuid::Uuid::new_v4()
        ),
        Some(json!({"newPassword": "a fresh passphrase"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // The organization's own key — a credential with all of the organization's
    // authority — is not the platform's.
    let (_, key) = register(&app, "pwreset-nope").await;
    let (status, _) = call_with(
        &app,
        "POST",
        &format!("/api/v1/admin/users/{user_id}/password-reset"),
        Some(json!({"newPassword": "a fresh passphrase"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The reset: password changed, the live session revoked.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/users/{user_id}/password-reset"),
        Some(json!({"newPassword": "a fresh passphrase"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["sessionsRevoked"], 1,
        "the one live session died with it"
    );

    let (status, _) = call_with(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the old password is dead");

    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.sessions WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(uuid::Uuid::parse_str(&user_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, 0, "no session survived the reset");

    let (status, body) = call_with(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": "a fresh passphrase"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The signup rule still gates what the endpoint accepts.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/users/{user_id}/password-reset"),
        Some(json!({"newPassword": "short"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

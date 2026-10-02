//! HTTP tests: routing, key auth and error formats.
//!
//! The tests that only exercise routing and rejection run without a database; the ones that
//! register or move money need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// A pool that connects to nothing: rejection happens before the database is reached, so the
/// pool only has to exist. `connect_lazy` defers the first connection to first use.
fn unused_pool() -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL")
}

/// An app over a pool that never connects, for requests answered without the database.
fn offline_app(signup: Signup) -> Router {
    // No gateway channel: these tests cover the wallet API, which does not depend on one.
    oxsum_server::app(Db::from_pool(unused_pool()), Config::new(signup, None))
}

/// An app over a real database with oxsum's tables migrated.
async fn online_app(url: &str, signup: Signup) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (oxsum_server::app(db, Config::new(signup, None)), pool)
}

/// The DATABASE_URL tests need, or None to skip.
fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! app_or_skip {
    ($signup:expr) => {
        match url() {
            Some(u) => online_app(&u, $signup).await,
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
    // A body means JSON; no body means no content type either, which is what a client that
    // sends nothing sends, and what makes an optional body optional rather than empty JSON.
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
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// Registers a fresh organization and returns its first key's secret.
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

#[tokio::test]
async fn health_needs_no_key() {
    let app = offline_app(Signup::Open);
    let res = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_ledger_and_the_organization_are_behind_a_key() {
    let app = offline_app(Signup::Open);
    for path in ["/api/v1/balance", "/api/v1/org", "/api/v1/org/keys"] {
        let (status, body) = call(&app, "GET", path, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(body["error"]["code"], "UNAUTHORIZED", "{path}");
    }
    // A tenant in the path is not a thing any more: the credential names the organization.
    let (status, _) = call(&app, "GET", "/api/v1/tenants/acme/balance", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn registration_is_closed_unless_signup_is_open() {
    let app = offline_app(Signup::Invite);
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": "someone@example.com", "password": "correct horse battery"})),
        None,
    )
    .await;
    // 403, not 404: the endpoint is there, this deployment does not take registrations.
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "FORBIDDEN");
}

#[tokio::test]
async fn an_unknown_key_is_unauthorized() {
    let (app, _pool) = app_or_skip!(Signup::Open);
    let (status, body) = call(&app, "GET", "/api/v1/balance", None, Some("oxs-deadbeef")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    // The message does not say whether the key format, existence or state was the problem.
    assert!(!body["error"]["message"].as_str().unwrap().contains("oxs-"));
}

#[tokio::test]
async fn registering_rejects_bad_input() {
    let (app, _pool) = app_or_skip!(Signup::Open);
    for body in [
        json!({"email": "not-an-address", "password": "correct horse battery"}),
        json!({"email": "a@b.example", "password": "short"}),
    ] {
        let (status, body) = call(&app, "POST", "/api/v1/auth/register", Some(body), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    }

    // The same email twice is a conflict, not a second account.
    let email = format!(
        "dupe_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let body = json!({"email": email, "password": "correct horse battery"});
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(body.clone()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(&app, "POST", "/api/v1/auth/register", Some(body), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "CONFLICT");
}

#[tokio::test]
async fn wallet_round_trip() {
    let (app, _pool) = app_or_skip!(Signup::Open);
    let (_registration, key) = register(&app, "roundtrip").await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "t1", "amountMinor": 5_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry_id = body["data"]["entryId"].as_str().unwrap().to_owned();

    // The same idempotency key is a replay, not a second top-up.
    let (status, replay) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "t1", "amountMinor": 5_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["data"]["isNew"], false);
    assert_eq!(replay["data"]["entryId"], entry_id);

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": "h1", "amountMinor": 9_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["error"]["code"], "INSUFFICIENT_FUNDS");

    // Hold 3 credits, settle for 2: the upper bound is frozen, the actual charge is posted,
    // and the difference is available again.
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": "h2", "amountMinor": 3_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(&app, "GET", "/api/v1/balance", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["availableMinor"], 2_000_000);

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/settlements",
        Some(json!({"holdKey": "h2", "actualMinor": 2_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(&app, "GET", "/api/v1/balance", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["availableMinor"], 3_000_000);

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/entries/{entry_id}/proof"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["proof"].is_object());
}

#[tokio::test]
async fn a_settlement_naming_no_hold_is_not_found() {
    let (app, _pool) = app_or_skip!(Signup::Open);
    let (_registration, key) = register(&app, "unheld").await;

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "t1", "amountMinor": 5_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A settlement naming a hold that was never taken: there is nothing to release, even
    // though the wallet holds enough to cover it.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/settlements",
        Some(json!({"holdKey": "ghost", "actualMinor": 0})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "NOT_FOUND");

    // Take a hold, settle it, and settle it again for a different charge: the hold is
    // discharged, so the second settlement is a conflict.
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": "h1", "amountMinor": 2_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/settlements",
        Some(json!({"holdKey": "h1", "actualMinor": 1_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/settlements",
        Some(json!({"holdKey": "h1", "actualMinor": 0})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "CONFLICT");

    // Neither refusal moved anything: the wallet holds what the one settlement left.
    let (status, body) = call(&app, "GET", "/api/v1/balance", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["availableMinor"], 4_000_000);
}

#[tokio::test]
async fn a_reused_key_with_different_content_is_a_conflict() {
    let (app, _pool) = app_or_skip!(Signup::Open);
    let (_registration, key) = register(&app, "reuse").await;

    let (status, first) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "t1", "amountMinor": 5_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let entry_id = first["data"]["entryId"].as_str().unwrap().to_owned();

    // Same key, different amount: a request the caller can fix, so 409 rather than 500, and
    // nothing about the ledger's internals in the message.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "t1", "amountMinor": 6_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "CONFLICT");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("idempotency key"), "{message}");
    assert!(!message.contains("storage"), "{message}");

    // A different kind under the same key is the same conflict, and neither attempt moved
    // anything.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": "t1", "amountMinor": 1_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "CONFLICT");

    let (status, body) = call(&app, "GET", "/api/v1/balance", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["availableMinor"], 5_000_000);

    // The key still belongs to the entry that took it, so the original request still replays.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "t1", "amountMinor": 5_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["isNew"], false);
    assert_eq!(body["data"]["entryId"], entry_id);
}

#[tokio::test]
async fn the_credential_names_the_organization() {
    let (app, _pool) = app_or_skip!(Signup::Open);
    let (first, first_key) = register(&app, "orgs_a").await;
    let (second, second_key) = register(&app, "orgs_b").await;

    // Two registrations, two organizations, two ledgers.
    assert_ne!(first["organization"]["id"], second["organization"]["id"]);
    let first_tenant = first["organization"]["tenantId"].as_str().unwrap();
    assert_eq!(first_tenant.len(), 32);

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "a1", "amountMinor": 7_000_000})),
        Some(&first_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entry_id = body["data"]["entryId"].as_str().unwrap().to_owned();

    // B's key sees B's money, which is none of A's.
    let (status, body) = call(&app, "GET", "/api/v1/balance", None, Some(&second_key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["availableMinor"], 0);

    // B's key cannot prove an entry of A's ledger, or revoke A's key.
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/entries/{entry_id}/proof"),
        None,
        Some(&second_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let first_key_id = first["apiKey"]["id"].as_str().unwrap();
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/keys/{first_key_id}"),
        None,
        Some(&second_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A's key still works, so that attempt changed nothing.
    let (status, _) = call(&app, "GET", "/api/v1/balance", None, Some(&first_key)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn keys_are_minted_listed_and_revoked() {
    let (app, pool) = app_or_skip!(Signup::Open);
    let (_registration, key) = register(&app, "keys").await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "ci"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let second_id = body["data"]["id"].as_str().unwrap().to_owned();
    let second_secret = body["data"]["secret"].as_str().unwrap().to_owned();
    assert!(second_secret.starts_with("oxs-"));
    assert_ne!(second_secret, key);

    // Listing never carries a secret, and the new key names the same organization.
    let (status, body) = call(&app, "GET", "/api/v1/org/keys", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    let keys = body["data"].as_array().unwrap();
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().all(|key| key.get("secret").is_none()));
    assert_eq!(keys[0]["id"], second_id.as_str());
    assert_eq!(keys[0]["name"], "ci");

    // Any active key of an organization may spend and mint; the second key works too.
    let (status, _) = call(&app, "GET", "/api/v1/balance", None, Some(&second_secret)).await;
    assert_eq!(status, StatusCode::OK);

    // Revoking takes effect immediately, and only for that key.
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/keys/{second_id}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["revokedAt"].is_string());
    let (status, _) = call(&app, "GET", "/api/v1/balance", None, Some(&second_secret)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&app, "GET", "/api/v1/balance", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK);

    // A key id that does not exist is not found.
    let missing = uuid::Uuid::new_v4();
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/keys/{missing}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Expiry ends a key just as revocation does.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "short", "expiresAt": "2030-01-01T00:00:00Z"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let expiring_id = body["data"]["id"].as_str().unwrap();
    let expiring_secret = body["data"]["secret"].as_str().unwrap();
    let (status, _) = call(&app, "GET", "/api/v1/balance", None, Some(expiring_secret)).await;
    assert_eq!(status, StatusCode::OK);
    // Move the expiry into the past rather than waiting for it.
    sqlx::query(
        "UPDATE oxsum.api_keys SET expires_at = now() - interval '1 second' WHERE key_id = $1",
    )
    .bind(uuid::Uuid::parse_str(expiring_id).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let (status, _) = call(&app, "GET", "/api/v1/balance", None, Some(expiring_secret)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The body is optional: a key without a name and without an expiry is a valid request.
    // (`{}` would do the same; a content type with an empty body is a malformed request.)
    let (status, body) = call(&app, "POST", "/api/v1/org/keys", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["name"].is_null());
    assert!(body["data"]["expiresAt"].is_null());
    assert!(body["data"]["secret"].as_str().unwrap().starts_with("oxs-"));
}

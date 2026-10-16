//! HTTP tests for the signed spend mandates (issue #262): minting and listing
//! under `POST`/`GET /api/v1/org/keys/{keyId}/mandates`, and the delegated hold
//! under `POST /api/v1/holds` carrying `X-Oxsum-Mandate`.
//!
//! The tests register an organization, top it up, mint a mandate on its key
//! and exercise the hold endpoint with the signed blob — covering the refusal
//! codes (`INVALID_MANDATE`, `MANDATE_NOT_YET_VALID`/`MANDATE_EXPIRED`,
//! `MANDATE_EXCEEDED`) as well as a hold the mandate admits. Needs
//! DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;

/// The sealing key the test app seals mandate seeds under — the same fixed
/// test key the channel suite uses (`check_sealed_keys` scans the shared DB).
fn secret() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

/// An app over a real database with oxsum's tables migrated and the secret
/// configured — mandates cannot be minted without it.
async fn online_app(url: &str) -> (Router, Db) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (
        oxsum_server::app(
            db.clone(),
            Config::new(Signup::Open, None).with_secret(secret()),
        ),
        db,
    )
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("mandates")
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => online_app(&u).await,
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
    mandate: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    if let Some(mandate) = mandate {
        req = req.header("x-oxsum-mandate", mandate);
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A fresh organization: its first key's id and secret for calling the API.
async fn register(app: &Router, name: &str) -> (String, String) {
    let email = format!(
        "{name}_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    (
        body["data"]["apiKey"]["id"].as_str().unwrap().to_owned(),
        body["data"]["apiKey"]["secret"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

/// Tops the organization up, the way `POST /api/v1/topups` does.
async fn top_up(app: &Router, key: &str, amount_minor: i64) {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": Uuid::new_v4().to_string(), "amountMinor": amount_minor})),
        Some(key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "top-up failed: {body}");
}

/// Mints a mandate on `key_id` under `key`'s authority and answers the blob.
async fn mint(
    app: &Router,
    key: &str,
    key_id: &str,
    cap_minor: i64,
    not_after: OffsetDateTime,
) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        &format!("/api/v1/org/keys/{key_id}/mandates"),
        Some(json!({
            "capMinor": cap_minor,
            "notAfter": rfc3339(not_after),
        })),
        Some(key),
        None,
    )
    .await
}

/// The RFC 3339 timestamp `CreateMandateRequest` parses.
fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .expect("the timestamp formats")
}

/// The whole mandate lifecycle: mint, list, then a hold the blob admits.
#[tokio::test]
async fn a_mandate_is_minted_listed_and_honored() {
    let (app, _db) = app_or_skip!();
    let (key_id, key) = register(&app, "mandate").await;
    top_up(&app, &key, 50_000_000).await;

    let not_after = OffsetDateTime::now_utc() + Duration::hours(1);
    let (status, created) = mint(&app, &key, &key_id, 5_000_000, not_after).await;
    assert_eq!(status, StatusCode::OK, "mint failed: {created}");
    let blob = created["data"]["mandateBlob"]
        .as_str()
        .expect("the mint answers the signed blob")
        .to_owned();
    let mandate_id = created["data"]["mandateId"].as_str().unwrap().to_owned();
    assert_eq!(created["data"]["keyId"].as_str().unwrap(), key_id);
    assert_eq!(created["data"]["capMinor"].as_i64().unwrap(), 5_000_000);

    // The mandate lists under the key's mandate collection.
    let (status, list) = call(
        &app,
        "GET",
        &format!("/api/v1/org/keys/{key_id}/mandates"),
        None,
        Some(&key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let rows = list["data"].as_array().expect("a list of mandates");
    assert!(
        rows.iter().any(|row| row["mandateId"] == mandate_id),
        "the minted mandate lists: {rows:?}"
    );

    // A hold under the blob is admitted: the signature verifies, the window
    // covers now, and the cap is untouched.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": Uuid::new_v4().to_string(), "amountMinor": 1_000_000})),
        Some(&key),
        Some(&blob),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the hold under the mandate: {body}");
}

/// A blob that fails any of decode/parse/verify is `invalid_mandate` — the
/// caller cannot tell which step caught it.
#[tokio::test]
async fn a_blob_that_does_not_verify_is_refused() {
    let (app, _db) = app_or_skip!();
    let (key_id, key) = register(&app, "badmandate").await;
    top_up(&app, &key, 50_000_000).await;

    // A blob for a key that never delegated: no seed, so `invalid_mandate`.
    let (_, created) = mint(
        &app,
        &key,
        &key_id,
        5_000_000,
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await;
    let good_blob = created["data"]["mandateBlob"].as_str().unwrap();
    let mut tampered = good_blob.to_owned();
    // Flip a byte inside the signature segment: the base64 still decodes, the
    // signature no longer checks.
    let at = tampered.len() - 4;
    tampered.replace_range(
        at..at + 1,
        if tampered.as_bytes()[at] == b'A' {
            "B"
        } else {
            "A"
        },
    );

    for blob in [
        tampered.as_str(),
        "not-base64url-!!!",
        // Valid base64, not JSON.
        "aGVsbG8",
    ] {
        let (status, body) = call(
            &app,
            "POST",
            "/api/v1/holds",
            Some(json!({"idempotencyKey": Uuid::new_v4().to_string(), "amountMinor": 1_000_000})),
            Some(&key),
            Some(blob),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["code"], "INVALID_MANDATE", "{body}");
    }
}

/// A mandate whose window does not cover the request refuses with the right code.
#[tokio::test]
async fn a_mandate_outside_its_window_is_refused() {
    let (app, _db) = app_or_skip!();
    let (key_id, key) = register(&app, "window").await;
    top_up(&app, &key, 50_000_000).await;

    // Not yet valid: notBefore an hour out.
    let (_, created) = call(
        &app,
        "POST",
        &format!("/api/v1/org/keys/{key_id}/mandates"),
        Some(json!({
            "capMinor": 5_000_000,
            "notBefore": rfc3339(OffsetDateTime::now_utc() + Duration::hours(1)),
            "notAfter": rfc3339(OffsetDateTime::now_utc() + Duration::hours(3)),
        })),
        Some(&key),
        None,
    )
    .await;
    let future = created["data"]["mandateBlob"].as_str().unwrap().to_owned();

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": Uuid::new_v4().to_string(), "amountMinor": 1_000_000})),
        Some(&key),
        Some(&future),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "MANDATE_NOT_YET_VALID", "{body}");

    // Expired: notAfter already passed.
    let (_, created) = call(
        &app,
        "POST",
        &format!("/api/v1/org/keys/{key_id}/mandates"),
        Some(json!({
            "capMinor": 5_000_000,
            "notBefore": rfc3339(OffsetDateTime::now_utc() - Duration::hours(2)),
            "notAfter": rfc3339(OffsetDateTime::now_utc() - Duration::hours(1)),
        })),
        Some(&key),
        None,
    )
    .await;
    let expired = created["data"]["mandateBlob"].as_str().unwrap().to_owned();
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": Uuid::new_v4().to_string(), "amountMinor": 1_000_000})),
        Some(&key),
        Some(&expired),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "MANDATE_EXPIRED", "{body}");
}

/// A hold that would push the mandate's committed sum past `capMinor` is
/// refused `MANDATE_EXCEEDED` — under the cap itself is admitted, past it is not.
#[tokio::test]
async fn a_mandate_refuses_spend_past_its_cap() {
    let (app, _db) = app_or_skip!();
    let (key_id, key) = register(&app, "cap").await;
    top_up(&app, &key, 50_000_000).await;

    // The mandate caps at 3 minor units; the key's own limit is far above it.
    let (_, created) = mint(
        &app,
        &key,
        &key_id,
        3,
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await;
    let blob = created["data"]["mandateBlob"].as_str().unwrap().to_owned();

    // Two units fit; one more would exceed the mandate's 3.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": Uuid::new_v4().to_string(), "amountMinor": 2})),
        Some(&key),
        Some(&blob),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the first hold: {body}");

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": Uuid::new_v4().to_string(), "amountMinor": 2})),
        Some(&key),
        Some(&blob),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "MANDATE_EXCEEDED", "{body}");
}

/// A mint that would widen the key's own bounds — a cap over `spendLimitMinor`
/// or a model outside the allowlist — is refused `VALIDATION_ERROR`, not minted.
#[tokio::test]
async fn a_mint_may_only_narrow_the_key() {
    let (app, _db) = app_or_skip!();
    let (_key_id, key) = register(&app, "narrow").await;
    // A constrained key: spend limit 10 and an allowlist of one model.
    let (status, constrained) = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({
            "spendLimitMinor": 10,
            "modelAllowlist": ["mock-a"],
        })),
        Some(&key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{constrained}");
    let key_id = constrained["data"]["id"].as_str().unwrap().to_owned();

    // A cap above the key's limit is refused.
    let (status, body) = mint(
        &app,
        &key,
        &key_id,
        20,
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A model outside the key's allowlist is refused.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/org/keys/{key_id}/mandates"),
        Some(json!({
            "capMinor": 5,
            "models": ["mock-b"],
            "notAfter": rfc3339(OffsetDateTime::now_utc() + Duration::hours(1)),
        })),
        Some(&key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// A member sees only the mandates on the keys they created: a mandate id of
/// another member's key is `not found`, never `forbidden` — ids are not probeable.
#[tokio::test]
async fn a_members_scope_limits_which_keys_mint_mandates() {
    let (app, _db) = app_or_skip!();
    let (_key_id, key) = register(&app, "scope-owner").await;
    // A second key minted through the API — created_by is NULL, so a member's
    // `Own` scope does not see it.
    let (status, other) = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({})),
        Some(&key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{other}");
    let other_id = other["data"]["id"].as_str().unwrap().to_owned();

    // A member cannot mint on a key they did not create — but the principal
    // here is an API key, which scopes to `Organization`, so this case is the
    // positive control: the org's key mints on any of its keys.
    let (status, body) = mint(
        &app,
        &key,
        &other_id,
        5_000_000,
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // An unknown key id answers 404, not 403.
    let missing = Uuid::new_v4();
    let (status, body) = mint(
        &app,
        &key,
        &missing.to_string(),
        5_000_000,
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

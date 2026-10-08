//! The admin audit log endpoint (issue #160): every mutating `/api/v1/admin`
//! call leaves one row, readable newest-first through `GET /admin/audit`.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing. The database is shared between tests, so every tier
//! name, discount key and organization these tests create is one of its own.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The operator token these tests configure.
const TOKEN: &str = "operator-token-0123456789";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => app_for(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// An app over a real database, migrated, with the admin surface configured.
async fn app_for(database_url: &str) -> (Router, Db, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None)
        .with_secret(SecretKey::from_bytes([7; 32]))
        .with_admin_token(TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    (oxsum_server::app(db.clone(), config), db, pool)
}

/// A name of this test's own, so tests sharing the database never collide.
fn fresh(name: &str) -> String {
    format!("{name}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// An organization of this test's own, registered the way a user's is.
async fn register(db: &Db) -> Uuid {
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", fresh("audit")),
            password: "correct horse battery".into(),
            organization_name: None,
        })
        .await
        .expect("registration succeeds");
    registration.organization.id
}

/// One call against the app. `token` says whether the operator token rides
/// along; the log's own read is part of what the token guards.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: bool,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if token {
        request = request.header("authorization", format!("Bearer {TOKEN}"));
    }
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
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

/// Every audit row under one action, walked page by page.
async fn audit_rows(app: &Router, action: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut path = format!("/api/v1/admin/audit?action={action}&limit=100");
    loop {
        let (status, body) = call(app, "GET", &path, None, true).await;
        assert_eq!(status, StatusCode::OK);
        rows.extend(body["data"]["entries"].as_array().unwrap().iter().cloned());
        match body["data"]["nextCursor"].as_str() {
            Some(cursor) => {
                path = format!("/api/v1/admin/audit?action={action}&limit=100&cursor={cursor}")
            }
            None => break,
        }
    }
    rows
}

#[tokio::test]
async fn an_admin_write_is_audited_with_its_detail() {
    let (app, _db, _pool) = app_or_skip!();
    let tier = fresh("tier");

    let (status, _) = call(
        &app,
        "PUT",
        &format!("/api/v1/admin/tiers/{tier}"),
        Some(json!({"requestsPerMinute": 42, "modelAllowlist": ["model-a"]})),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let rows = audit_rows(&app, "tier.set").await;
    let row = rows
        .iter()
        .find(|row| row["target"] == tier)
        .expect("the tier write is logged");
    assert_eq!(row["actor"], "operator");
    assert_eq!(row["detail"]["requestsPerMinute"], 42);
    assert_eq!(row["detail"]["modelAllowlist"], json!(["model-a"]));
}

#[tokio::test]
async fn the_log_pages_newest_first_through_a_cursor() {
    let (app, _db, _pool) = app_or_skip!();

    // Three writes of this test's own, in order — the walk checks they answer
    // newest first, one page at a time.
    let mut names = Vec::new();
    for _ in 0..3 {
        let tier = fresh("paged");
        let (status, _) = call(
            &app,
            "PUT",
            &format!("/api/v1/admin/tiers/{tier}"),
            Some(json!({})),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        names.push(tier);
    }

    // Walk only this test's action, two rows a page: the first row is the last
    // write, the cursor resumes where the page ended, and the log's end has
    // no nextCursor.
    let mut seen = Vec::new();
    let mut path = "/api/v1/admin/audit?action=tier.set&limit=2".to_string();
    loop {
        let (status, body) = call(&app, "GET", &path, None, true).await;
        assert_eq!(status, StatusCode::OK);
        for entry in body["data"]["entries"].as_array().unwrap() {
            seen.push(entry["target"].as_str().unwrap().to_string());
        }
        match body["data"]["nextCursor"].as_str() {
            Some(cursor) => {
                path = format!("/api/v1/admin/audit?action=tier.set&limit=2&cursor={cursor}")
            }
            None => break,
        }
    }
    let mine: Vec<_> = seen
        .iter()
        .filter(|name| names.contains(name))
        .cloned()
        .collect();
    let mut expected = names;
    expected.reverse();
    assert_eq!(mine, expected, "the test's writes answer newest first");
}

#[tokio::test]
async fn a_malformed_cursor_is_a_validation_error() {
    let (app, _db, _pool) = app_or_skip!();
    let (status, body) = call(
        &app,
        "GET",
        "/api/v1/admin/audit?cursor=not-a-cursor",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn an_unknown_action_is_a_validation_error() {
    let (app, _db, _pool) = app_or_skip!();
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/admin/audit?action=no.such.action",
        None,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_keyed_replay_audits_once() {
    let (app, db, _pool) = app_or_skip!();
    let key = fresh("key");
    // Scoped to this test's own organization and model — a global discount
    // would keep discounting every shared-database test after this one ends.
    let organization = register(&db).await;
    let model = fresh("model");

    for _ in 0..2 {
        let (status, _) = call(
            &app,
            "POST",
            "/api/v1/admin/discounts",
            Some(json!({
                "percent": 15,
                "organizationId": organization,
                "model": model,
                "idempotencyKey": key,
            })),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let rows = audit_rows(&app, "discount.create").await;
    let matching: Vec<_> = rows
        .iter()
        .filter(|row| row["idempotencyKey"] == key)
        .collect();
    assert_eq!(matching.len(), 1, "a retried create is one audit row");
}

#[tokio::test]
async fn the_log_needs_the_operator_token() {
    let (app, _db, _pool) = app_or_skip!();
    let (status, _) = call(&app, "GET", "/api/v1/admin/audit", None, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_channel_set_keeps_its_key_out_of_the_log() {
    let (app, _db, _pool) = app_or_skip!();
    let channel = fresh("channel");
    let secret = format!("sk-audit-secret-{}", Uuid::new_v4());

    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/admin/channels",
        Some(json!({
            "name": channel,
            "baseUrl": "https://upstream.example.com",
            "apiKey": secret,
        })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let rows = audit_rows(&app, "channel.set").await;
    let row = rows
        .iter()
        .find(|row| row["target"] == channel)
        .expect("the channel write is logged");
    let serialized = row.to_string();
    assert!(
        !serialized.contains(&secret[..20]),
        "the upstream credential never enters the log"
    );
}

#[tokio::test]
async fn a_password_reset_audits_the_target_not_the_password() {
    let (app, db, _pool) = app_or_skip!();
    let organization = register(&db).await;
    let user = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM oxsum.memberships WHERE organization_id = $1 LIMIT 1",
    )
    .bind(organization)
    .fetch_one(db.pool())
    .await
    .expect("the member reads");
    let password = format!("replacement-{}", Uuid::new_v4());

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/users/{user}/password-reset"),
        Some(json!({ "newPassword": password })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let rows = audit_rows(&app, "user.password_reset").await;
    let row = rows
        .iter()
        .find(|row| row["target"] == user.to_string())
        .expect("the reset is logged");
    assert!(!row.to_string().contains(&password[..12]));
    assert_eq!(row["detail"]["sessionsRevoked"], 0);
}

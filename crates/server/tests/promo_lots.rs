//! Promotional lots over HTTP (issue #609): the operator grants and cancels a
//! lot on an organization, the organization reads its own lots, a replayed
//! grant answers the standing row, and a lot of another organization is a
//! not-found rather than a cross-org write.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

/// The operator token these tests configure.
const TOKEN: &str = "operator-token-0123456789";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("promo_lots")
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => admin_app(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// An app over a real database, with oxsum's tables migrated and the admin
/// surface configured.
async fn admin_app(database_url: &str) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let config = Config::new(Signup::Open, None)
        .with_secret(SecretKey::from_bytes([9; 32]))
        .with_admin_token(TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    (support::app(db, config), pool)
}

/// One call against the app, with the operator token unless another credential
/// is given.
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
        request = support::credential(request, token);
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

/// Registers an account so a personal organization exists; answers the
/// organization id and the member session.
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
        support::login(
            app,
            body["data"]["user"]["email"].as_str().unwrap(),
            "correct horse battery",
        )
        .await,
    )
}

/// `POST /api/v1/admin/organizations/{id}/promo-lots` grants a lot the
/// organization can read back at `GET /api/v1/org/promo-lots`; a replay under
/// the same idempotency key answers the standing lot instead of granting twice.
#[tokio::test]
async fn a_grant_lists_for_the_organization_and_replays() {
    let (app, _pool) = app_or_skip!();
    let (org, session) = register(&app, "promo").await;
    let path = format!("/api/v1/admin/organizations/{org}/promo-lots");

    // Grant with an expiry.
    let expires = time::OffsetDateTime::now_utc() + time::Duration::days(14);
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({
            "amountMinor": 4_000_000,
            "expiresAt": expires,
            "reason": "launch credit",
            "idempotencyKey": "launch-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lot = &body["data"];
    assert_eq!(lot["amountMinor"], 4_000_000);
    assert_eq!(lot["remainingMinor"], 4_000_000);
    assert_eq!(lot["source"], "admin");
    assert_eq!(lot["sourceRef"], "launch-1");
    assert_eq!(lot["organizationId"].as_str(), Some(org.as_str()));
    let lot_id = lot["lotId"].as_str().expect("the granted lot's id");

    // A never-expiring second grant.
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({
            "amountMinor": 2_000_000,
            "idempotencyKey": "forever-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["expiresAt"], Value::Null);

    // Replay the first grant: the same key answers the standing lot.
    let (status, body) = call(
        &app,
        "POST",
        &path,
        Some(json!({
            "amountMinor": 4_000_000,
            "reason": "launch credit",
            "idempotencyKey": "launch-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["lotId"].as_str(), Some(lot_id));

    // The organization reads its own lots, soonest-expiring first: the
    // 14-day lot precedes the never-expiring one.
    let (status, body) =
        call_with(&app, "GET", "/api/v1/org/promo-lots", None, Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["data"].as_array().expect("a list of lots");
    assert_eq!(rows.len(), 2, "{body}");
    assert_eq!(rows[0]["lotId"].as_str(), Some(lot_id));
    assert!(rows[0]["expiresAt"].as_str().is_some());
    assert_eq!(rows[1]["expiresAt"], Value::Null);

    // The admin list answers the same two lots.
    let (status, body) = call(&app, "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"].as_array().unwrap().len(), 2, "{body}");
}

/// `POST …/promo-lots/{lotId}/cancel` burns the unspent remainder: the lot
/// closes `cancelled`, drops out of the open list, and rides the list again
/// under `includeClosed`. A lot of another organization is a 404, not a write.
#[tokio::test]
async fn a_cancel_burns_the_remainder_and_scopes_to_the_org() {
    let (app, _pool) = app_or_skip!();
    let (org, session) = register(&app, "cancel").await;
    let (other_org, _other_session) = register(&app, "other").await;
    let grants = format!("/api/v1/admin/organizations/{org}/promo-lots");

    let (status, body) = call(
        &app,
        "POST",
        &grants,
        Some(json!({
            "amountMinor": 6_000_000,
            "idempotencyKey": "cancel-me-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lot_id = body["data"]["lotId"].as_str().unwrap().to_owned();

    // A cancel addressed to a different organization is a not-found.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/organizations/{other_org}/promo-lots/{lot_id}/cancel"),
        Some(json!({"idempotencyKey": "cancel-x-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // The real cancel burns the whole unspent remainder.
    let (status, body) = call(
        &app,
        "POST",
        &format!("{grants}/{lot_id}/cancel"),
        Some(json!({"reason": "bundle refunded", "idempotencyKey": "cancel-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["closeReason"], "cancelled");
    assert_eq!(body["data"]["remainingMinor"], 0);
    assert!(body["data"]["closedAt"].as_str().is_some());

    // Open list no longer carries it; `includeClosed` brings it back.
    let (status, body) =
        call_with(&app, "GET", "/api/v1/org/promo-lots", None, Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"].as_array().unwrap().len(), 0, "{body}");
    let (status, body) = call_with(
        &app,
        "GET",
        "/api/v1/org/promo-lots?includeClosed=true",
        None,
        Some(&session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["closeReason"], "cancelled");

    // The granted money is gone from spendable balance too — it burned, it did
    // not refund.
    let (status, body) = call_with(&app, "GET", "/api/v1/balance", None, Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["availableMinor"], 0);
}

/// An expiry that already passed closes on the next pass: the admin grants a
/// lot whose `expiresAt` has lapsed, the expire-lots job burns its remainder,
/// and the lot answers `expired` under `includeClosed`.
#[tokio::test]
async fn an_expired_lot_burns_on_the_pass() {
    let (app, pool) = app_or_skip!();
    let (org, session) = register(&app, "expire").await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/organizations/{org}/promo-lots"),
        Some(json!({
            "amountMinor": 3_000_000,
            "expiresAt": time::OffsetDateTime::now_utc() - time::Duration::seconds(1),
            "idempotencyKey": "lapsed-1",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lot_id = body["data"]["lotId"].as_str().unwrap();

    // The expiry pass, driven the way the periodic job drives it.
    let db = Db::from_pool(pool.clone());
    let tenants = oxsum_core::Tenants::new(pool.clone());
    let closed = db
        .expire_due_promo_lots(&tenants, 100, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    assert!(closed >= 1);

    // The lot closed `expired` with nothing left.
    let (status, body) = call_with(
        &app,
        "GET",
        "/api/v1/org/promo-lots?includeClosed=true",
        None,
        Some(&session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body["data"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["lotId"].as_str() == Some(lot_id))
        .expect("the lot rides the closed list");
    assert_eq!(row["closeReason"], "expired");
    assert_eq!(row["remainingMinor"], 0);
}

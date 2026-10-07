//! The admin surface for organization terms (issue #158): tier profiles, their
//! assignment to organizations, and pricing discounts.
//!
//! A tier is a capability package — the gateway enforces it at admission — and
//! a discount is a percent off the priced sum that a settlement snapshots.
//! What a discounted turn actually charges is proven end to end where the turn
//! runs: the gateway tests in gateway.rs.
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
            email: format!("{}@example.com", fresh("terms")),
            password: "correct horse battery".into(),
            organization_name: None,
        })
        .await
        .expect("registration succeeds");
    registration.organization.id
}

/// One call against the app, credentialed with the operator token.
async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {TOKEN}"));
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

/// The tier the gateway would enforce for the organization, read from core —
/// the admin organization list paged through is what an operator opens, but
/// its walk opens every ledger on the way, so a test reads the tier the way
/// the gateway does.
async fn org_tier(db: &Db, organization_id: Uuid) -> Option<String> {
    db.tier_of(organization_id)
        .await
        .expect("the tier reads")
        .map(|profile| profile.name)
}

#[tokio::test]
async fn a_tier_is_written_listed_replaced_and_retired() {
    let (app, _db, _pool) = app_or_skip!();
    let tier = fresh("tier");

    // Nothing there yet: a delete of a name that was never a tier is a 404.
    let (status, _) = call(&app, "DELETE", &format!("/api/v1/admin/tiers/{tier}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Written: the profile answers what was put.
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/v1/admin/tiers/{tier}"),
        Some(json!({"requestsPerMinute": 60, "modelAllowlist": ["model-a", "model-b"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["name"], tier);
    assert_eq!(body["data"]["requestsPerMinute"], 60);
    assert_eq!(
        body["data"]["modelAllowlist"],
        json!(["model-a", "model-b"])
    );
    assert_eq!(body["data"]["organizations"], 0);

    // Listed among the profiles.
    let (status, body) = call(&app, "GET", "/api/v1/admin/tiers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["data"]
            .as_array()
            .expect("the list is an array")
            .iter()
            .any(|t| t["name"] == tier),
        "{body}"
    );

    // A PUT by name replaces the whole package: absent fields clear, and a
    // replayed write is the same profile — the endpoint carries no key.
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/v1/admin/tiers/{tier}"),
        Some(json!({"requestsPerMinute": 5})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["requestsPerMinute"], 5);
    assert_eq!(body["data"]["modelAllowlist"], Value::Null);

    // Retired, and a second retire is a 404.
    let (status, _) = call(&app, "DELETE", &format!("/api/v1/admin/tiers/{tier}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "DELETE", &format!("/api/v1/admin/tiers/{tier}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_tier_write_validates_its_package() {
    let (app, _db, _pool) = app_or_skip!();

    // Names are slugs.
    for name in ["Bad", "-x", "x-", "a--b", "UPPER"] {
        let (status, body) = call(
            &app,
            "PUT",
            &format!("/api/v1/admin/tiers/{name}"),
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
    }

    // A non-positive allowance and a malformed allowlist are refused.
    let tier = fresh("tier");
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/v1/admin/tiers/{tier}"),
        Some(json!({"requestsPerMinute": 0})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/v1/admin/tiers/{tier}"),
        Some(json!({"modelAllowlist": ["", "x"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // An unknown field is refused outright, not silently ignored.
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/v1/admin/tiers/{tier}"),
        Some(json!({"requestsPerMinute": 10, "surprise": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn an_organization_carries_and_drops_its_tier() {
    let (app, db, _pool) = app_or_skip!();
    let organization_id = register(&db).await;
    let tier = fresh("tier");
    let (status, body) = call(
        &app,
        "PUT",
        &format!("/api/v1/admin/tiers/{tier}"),
        Some(json!({"requestsPerMinute": 30})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A tier that does not exist cannot be assigned.
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/v1/admin/organizations/{organization_id}"),
        Some(json!({"idempotencyKey": fresh("patch"), "tier": fresh("none")})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Assigned: the organization's admin row answers the tier, and the
    // profile's member count sees it.
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/v1/admin/organizations/{organization_id}"),
        Some(json!({"idempotencyKey": fresh("patch"), "tier": tier})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["tier"], tier);
    assert_eq!(
        org_tier(&db, organization_id).await.as_deref(),
        Some(tier.as_str())
    );
    let (status, body) = call(&app, "GET", "/api/v1/admin/tiers", None).await;
    assert_eq!(status, StatusCode::OK);
    let profile = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == tier)
        .expect("the profile is listed");
    assert_eq!(profile["organizations"], 1);

    // An assigned tier refuses deletion: retiring it would silently uncap its
    // members. Clear the assignment first.
    let (status, body) = call(&app, "DELETE", &format!("/api/v1/admin/tiers/{tier}"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/v1/admin/organizations/{organization_id}"),
        Some(json!({"idempotencyKey": fresh("patch"), "tier": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["tier"], Value::Null);
    assert_eq!(org_tier(&db, organization_id).await, None);
    let (status, _) = call(&app, "DELETE", &format!("/api/v1/admin/tiers/{tier}"), None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_discount_is_created_replayed_listed_and_ended() {
    let (app, db, _pool) = app_or_skip!();
    let organization_id = register(&db).await;
    let key = fresh("disc");

    // Created scoped to the organization, with a label and an open window.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/discounts",
        Some(json!({
            "idempotencyKey": key,
            "organizationId": organization_id,
            "percent": 25,
            "label": "launch partner",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let discount_id = body["data"]["discountId"]
        .as_str()
        .expect("the row's id")
        .to_owned();
    assert_eq!(body["data"]["organizationId"], organization_id.to_string());
    assert_eq!(body["data"]["percent"], 25);
    assert_eq!(body["data"]["validUntil"], Value::Null);

    // Replayed with the same key and fields, the created row is the answer —
    // the retry is the same write. With different fields the key is a
    // conflict.
    let (status, replay) = call(
        &app,
        "POST",
        "/api/v1/admin/discounts",
        Some(json!({
            "idempotencyKey": key,
            "organizationId": organization_id,
            "percent": 25,
            "label": "launch partner",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["data"]["discountId"], discount_id);
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/discounts",
        Some(json!({
            "idempotencyKey": key,
            "organizationId": organization_id,
            "percent": 30,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Listed, ended or not: a settled bill's discountPercent points back at
    // the row.
    let (status, body) = call(&app, "GET", "/api/v1/admin/discounts", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["discountId"] == discount_id),
        "{body}"
    );

    // Ended: the window closes now, and the row stays in the list. A retried
    // delete answers the same row — an already-ended discount ends no further.
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/v1/admin/discounts/{discount_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["data"]["validUntil"], Value::Null);
    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/v1/admin/discounts/{discount_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // An id no row carries is a 404.
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/v1/admin/discounts/{}", Uuid::new_v4()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_discount_write_validates_its_terms() {
    let (app, db, _pool) = app_or_skip!();

    // A percent is 1..=100 — a hundred settles at zero, zero and above are not
    // discounts.
    for percent in [0, 101, -5] {
        let (status, body) = call(
            &app,
            "POST",
            "/api/v1/admin/discounts",
            Some(json!({"idempotencyKey": fresh("d"), "percent": percent})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{percent}: {body}");
    }

    // A window that ends before it starts.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/discounts",
        Some(json!({
            "idempotencyKey": fresh("d"),
            "percent": 10,
            "validFrom": "2026-01-02T00:00:00Z",
            "validUntil": "2026-01-01T00:00:00Z",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A scope naming no organization is refused, not silently global.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/discounts",
        Some(json!({
            "idempotencyKey": fresh("d"),
            "percent": 10,
            "organizationId": Uuid::new_v4(),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Every write carries its key: without one the body does not deserialize.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/discounts",
        Some(json!({"percent": 10})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A model-scoped, windowed discount round-trips its fields.
    let organization_id = register(&db).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/discounts",
        Some(json!({
            "idempotencyKey": fresh("d"),
            "percent": 100,
            "organizationId": organization_id,
            "model": "model-x",
            "validFrom": "2026-06-01T00:00:00Z",
            "validUntil": "2026-07-01T00:00:00Z",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["model"], "model-x");
    assert_eq!(body["data"]["percent"], 100);
    assert_eq!(body["data"]["validFrom"], "2026-06-01T00:00:00Z");
    assert_eq!(body["data"]["validUntil"], "2026-07-01T00:00:00Z");
}

#[tokio::test]
async fn admin_endpoints_refuse_other_credentials() {
    let (app, _db, _pool) = app_or_skip!();
    for (method, path) in [
        ("GET", "/api/v1/admin/tiers"),
        ("PUT", "/api/v1/admin/tiers/x"),
        ("GET", "/api/v1/admin/discounts"),
        ("POST", "/api/v1/admin/discounts"),
        ("DELETE", "/api/v1/admin/discounts/x"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("the test's own request");
        let status = app
            .clone()
            .oneshot(request)
            .await
            .expect("the router answers")
            .status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
}

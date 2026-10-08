//! Organization suspension and member budgets over HTTP (issue #162): the admin
//! PATCH that flips `suspended_at`, the 403 a suspended organization's holds get —
//! key-authenticated and session-authenticated alike — the `budgetLimitMinor` a
//! member PATCH sets, and `org.suspended` as a subscribable webhook event.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::SecretKey;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const TOKEN: &str = "operator-token-0123456789";
const PASSWORD: &str = "correct horse battery";

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

/// An app over a real database, migrated, with the admin surface and the secret
/// webhooks sign with configured.
async fn app_for(database_url: &str) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = oxsum_core::Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None)
        .with_secret(SecretKey::from_bytes([7; 32]))
        .with_admin_token(TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    (oxsum_server::app(db, config), pool)
}

struct Res {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
}

/// One HTTP call; `auth` is the bearer credential, `cookie` a session cookie.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    auth: Option<&str>,
    cookie: Option<&str>,
) -> Res {
    let mut request = Request::builder().method(method).uri(path);
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    if let Some(auth) = auth {
        request = request.header("authorization", format!("Bearer {auth}"));
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("oxsum_session={cookie}"));
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                .expect("the test's own request"),
        )
        .await
        .expect("the router answers");
    let status = response.status();
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    Res {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        set_cookie,
    }
}

/// A registered account: its organization id, its first key's secret, and a session
/// cookie for the member surface.
struct Account {
    organization_id: String,
    user_id: String,
    key: String,
    cookie: String,
}

async fn register(app: &Router, name: &str) -> Account {
    let email = format!(
        "{name}_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let res = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "registration failed: {}",
        res.body
    );
    let cookie = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await
    .set_cookie
    .expect("a session cookie was set")
    .split(';')
    .next()
    .unwrap()
    .strip_prefix("oxsum_session=")
    .expect("the session cookie")
    .to_owned();
    Account {
        organization_id: res.body["data"]["organization"]["id"]
            .as_str()
            .unwrap()
            .to_owned(),
        user_id: res.body["data"]["user"]["id"].as_str().unwrap().to_owned(),
        key: res.body["data"]["apiKey"]["secret"]
            .as_str()
            .unwrap()
            .to_owned(),
        cookie,
    }
}

/// The admin PATCH for an organization's billing terms and suspension.
async fn patch_organization(app: &Router, organization_id: &str, body: Value) -> Res {
    call(
        app,
        "PATCH",
        &format!("/api/v1/admin/organizations/{organization_id}"),
        Some(body),
        Some(TOKEN),
        None,
    )
    .await
}

/// The admin list's row shape, from its first page — the serialization fields the
/// contract adds are asserted here; the values are asserted through core, because
/// walking to a fresh organization's page opens every ledger on the way.
async fn first_admin_org(app: &Router) -> Value {
    let res = call(
        app,
        "GET",
        "/api/v1/admin/organizations?limit=1",
        None,
        Some(TOKEN),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    res.body["data"]["organizations"][0].clone()
}

/// The organization's suspension stamp, read through core — the admin list's
/// `suspended`/`suspendedAt` serialize this same row.
async fn suspended_at(pool: &PgPool, organization_id: &str) -> bool {
    oxsum_core::Db::from_pool(pool.clone())
        .organization_by_id(organization_id.parse().unwrap())
        .await
        .unwrap()
        .suspended_at
        .is_some()
}

/// Funds the account's wallet through its API key.
async fn top_up(app: &Router, key: &str, idem: &str, minor: i64) {
    let res = call(
        app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": idem, "amountMinor": minor})),
        Some(key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "top-up failed: {}", res.body);
}

/// One hold attempt, answered `(status, body)`.
async fn hold(
    app: &Router,
    auth: Option<&str>,
    cookie: Option<&str>,
    idem: &str,
    minor: i64,
) -> Res {
    call(
        app,
        "POST",
        "/api/v1/holds",
        Some(json!({"idempotencyKey": idem, "amountMinor": minor})),
        auth,
        cookie,
    )
    .await
}

/// Suspending flips `suspendedAt`, surfaces on the admin list, and reinstating
/// clears both — all through the same PATCH the other billing terms use.
#[tokio::test]
async fn suspension_round_trips_through_the_admin_patch() {
    let (app, pool) = app_or_skip!();
    let account = register(&app, "susp").await;
    let org = account.organization_id;

    let res = patch_organization(
        &app,
        &org,
        json!({"idempotencyKey": "s-1", "suspended": true}),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["suspended"], true);
    assert!(suspended_at(&pool, &org).await);

    // The list serializes both fields the contract names on every row.
    let listed = first_admin_org(&app).await;
    assert!(listed.get("suspended").is_some_and(|v| v.is_boolean()));
    assert!(listed.get("suspendedAt").is_some(), "{listed}");

    // A replayed flag still answers the state it stands in.
    let res = patch_organization(
        &app,
        &org,
        json!({"idempotencyKey": "s-2", "suspended": true}),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["suspended"], true);

    let res = patch_organization(
        &app,
        &org,
        json!({"idempotencyKey": "s-3", "suspended": false}),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["suspended"], false);
    assert!(!suspended_at(&pool, &org).await);
}

/// A suspended organization's new holds refuse, by key and by session; reinstating
/// admits them again. The refusal is FORBIDDEN, the code the contract names.
#[tokio::test]
async fn a_suspended_organization_reserves_nothing() {
    let (app, _) = app_or_skip!();
    let account = register(&app, "gate").await;
    top_up(&app, &account.key, "top-1", 10_000_000).await;
    assert_eq!(
        hold(&app, Some(&account.key), None, "h-ok", 1_000_000)
            .await
            .status,
        StatusCode::OK
    );

    patch_organization(
        &app,
        &account.organization_id,
        json!({"idempotencyKey": "s-1", "suspended": true}),
    )
    .await;

    let res = hold(&app, Some(&account.key), None, "h-key", 1_000_000).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "FORBIDDEN");
    let res = hold(&app, None, Some(&account.cookie), "h-session", 1_000_000).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    patch_organization(
        &app,
        &account.organization_id,
        json!({"idempotencyKey": "s-2", "suspended": false}),
    )
    .await;
    assert_eq!(
        hold(&app, Some(&account.key), None, "h-back", 1_000_000)
            .await
            .status,
        StatusCode::OK
    );
}

/// A member's `budgetLimitMinor` is set and cleared through the same member PATCH
/// a role change uses, and caps committed spend across the member's keys.
#[tokio::test]
async fn a_member_budget_spans_minted_keys() {
    let (app, _) = app_or_skip!();
    let account = register(&app, "mbud").await;
    top_up(&app, &account.key, "top-1", 100_000_000).await;

    // Two keys minted in the session, so both attribute to the member.
    let mut keys = Vec::new();
    for name in ["k1", "k2"] {
        let res = call(
            &app,
            "POST",
            "/api/v1/org/keys",
            Some(json!({"name": name})),
            None,
            Some(&account.cookie),
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.body);
        keys.push(res.body["data"]["secret"].as_str().unwrap().to_owned());
    }

    let member_path = format!("/api/v1/org/members/{}", account.user_id);
    // An empty PATCH is a validation error — the call named nothing to change.
    let res = call(
        &app,
        "PATCH",
        &member_path,
        Some(json!({})),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);

    let res = call(
        &app,
        "PATCH",
        &member_path,
        Some(json!({"budgetLimitMinor": 10_000_000})),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["budgetLimitMinor"], 10_000_000);

    // 6 + 6 over a 10 cap: each key alone would fit; together they may not.
    assert_eq!(
        hold(&app, Some(&keys[0]), None, "mb-1", 6_000_000)
            .await
            .status,
        StatusCode::OK
    );
    let res = hold(&app, Some(&keys[1]), None, "mb-2", 6_000_000).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "KEY_LIMIT_EXCEEDED");

    // Clearing the cap with an explicit null restores the member's spend.
    let res = call(
        &app,
        "PATCH",
        &member_path,
        Some(json!({"budgetLimitMinor": null})),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body["data"]["budgetLimitMinor"].is_null());
    assert_eq!(
        hold(&app, Some(&keys[1]), None, "mb-3", 6_000_000)
            .await
            .status,
        StatusCode::OK
    );
}

/// `org.suspended` is a subscribable event: the endpoint accepts it, and a
/// suspension through the admin PATCH queues a delivery for it.
#[tokio::test]
async fn org_suspended_is_a_subscribable_event() {
    let (app, _) = app_or_skip!();
    let account = register(&app, "hook").await;

    let res = call(
        &app,
        "POST",
        "/api/v1/webhooks",
        Some(json!({"url": "https://receiver.example/x", "events": ["org.suspended"]})),
        Some(&account.key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let endpoint_id = res.body["data"]["id"].as_str().unwrap();

    patch_organization(
        &app,
        &account.organization_id,
        json!({"idempotencyKey": "s-1", "suspended": true}),
    )
    .await;

    let res = call(
        &app,
        "GET",
        &format!("/api/v1/webhooks/{endpoint_id}/deliveries"),
        None,
        Some(&account.key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let deliveries = res.body["data"].as_array().unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0]["eventType"], "org.suspended");
}

//! HTTP tests for web login: the session cookie, login/logout, and the role rules on
//! the organization and key endpoints.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

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

const PASSWORD: &str = "correct horse battery";

/// An app over a real database with oxsum's tables migrated.
async fn online_app(url: &str, secure_cookie: bool) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None).with_session_cookie_secure(secure_cookie);
    (oxsum_server::app(db, config), pool)
}

/// The DATABASE_URL tests need, or None to skip.
fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! app_or_skip {
    ($secure:expr) => {
        match url() {
            Some(u) => online_app(&u, $secure).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

struct Res {
    status: StatusCode,
    body: Value,
    set_cookie: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    cookie: Option<&str>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    // A body means JSON; no body means no content type either, which is what a client that
    // sends nothing sends.
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        body,
        set_cookie,
    }
}

/// Registers a fresh organization; returns the email, the registration body and the first
/// key's secret.
async fn register(app: &Router, name: &str) -> (String, Value, String) {
    let email = format!(
        "{name}_{}@example.com",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
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
    let secret = res.body["data"]["apiKey"]["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    (email, res.body["data"].clone(), secret)
}

async fn login(app: &Router, email: &str, password: &str) -> Res {
    call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": password})),
        None,
        None,
    )
    .await
}

/// The cookie value out of a login's `Set-Cookie` header.
fn session_cookie(res: &Res) -> String {
    let set_cookie = res.set_cookie.as_deref().expect("a cookie was set");
    set_cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned()
}

#[tokio::test]
async fn login_sets_an_httponly_samesite_lax_cookie_and_names_user_org_and_role() {
    let (app, _pool) = app_or_skip!(false);
    let (email, registration, _secret) = register(&app, "cookie").await;

    let res = login(&app, &email, PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let set_cookie = res.set_cookie.as_deref().expect("Set-Cookie is set");
    assert!(
        set_cookie.starts_with("oxsum_session=oxsess-"),
        "the cookie carries the session token: {set_cookie}"
    );
    for attribute in ["Path=/", "HttpOnly", "SameSite=Lax"] {
        assert!(
            set_cookie.contains(attribute),
            "the cookie is {attribute}: {set_cookie}"
        );
    }
    assert!(
        !set_cookie.contains("Secure"),
        "no Secure attribute by default, so local http works: {set_cookie}"
    );

    assert_eq!(res.body["data"]["user"]["email"], email);
    assert_eq!(
        res.body["data"]["organization"]["id"],
        registration["organization"]["id"]
    );
    assert_eq!(res.body["data"]["role"], "owner");
    assert!(res.body["data"]["session"]["id"].is_string());
}

#[tokio::test]
async fn the_secure_flag_reaches_the_cookie() {
    let (app, _pool) = app_or_skip!(true);
    let (email, _registration, _secret) = register(&app, "secure").await;

    let res = login(&app, &email, PASSWORD).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let set_cookie = res.set_cookie.as_deref().expect("Set-Cookie is set");
    assert!(
        set_cookie.contains("; Secure"),
        "the deployment asked for Secure: {set_cookie}"
    );
}

#[tokio::test]
async fn a_wrong_password_and_an_unknown_email_are_the_same_401() {
    let (app, _pool) = app_or_skip!(false);
    let (email, _registration, _secret) = register(&app, "loginfail").await;

    let wrong = login(&app, &email, "wrong password 123").await;
    let unknown = login(&app, "nobody-has-this@example.com", "wrong password 123").await;
    for res in [&wrong, &unknown] {
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
        assert_eq!(res.body["error"]["code"], "UNAUTHORIZED");
        assert_eq!(res.body["error"]["message"], "invalid email or password");
        assert!(res.set_cookie.is_none(), "no cookie on a failed login");
    }
    assert_eq!(
        wrong.body["error"]["message"], unknown.body["error"]["message"],
        "the two failures are indistinguishable"
    );
}

#[tokio::test]
async fn the_cookie_authenticates_the_session_and_the_ledger_endpoints() {
    let (app, _pool) = app_or_skip!(false);
    let (email, registration, key) = register(&app, "sessionauth").await;
    let cookie = session_cookie(&login(&app, &email, PASSWORD).await);

    // GET /api/v1/session describes the session.
    let res = call(&app, "GET", "/api/v1/session", None, None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["user"]["email"], email);
    assert_eq!(
        res.body["data"]["organization"]["id"],
        registration["organization"]["id"]
    );
    assert_eq!(res.body["data"]["role"], "owner");

    // The ledger endpoints work exactly as they do with a key.
    let res = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "s1", "amountMinor": 5_000_000})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(&app, "GET", "/api/v1/balance", None, None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["availableMinor"], 5_000_000);

    // Without any credential, the session and the ledger both answer 401.
    for path in ["/api/v1/session", "/api/v1/balance", "/api/v1/org/keys"] {
        let res = call(&app, "GET", path, None, None, None).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(res.body["error"]["code"], "UNAUTHORIZED", "{path}");
    }

    // The key still works everywhere, unchanged.
    let res = call(&app, "GET", "/api/v1/session", None, Some(&key), None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "NOT_FOUND");
}

#[tokio::test]
async fn logout_revokes_the_session_clears_the_cookie_and_is_idempotent() {
    let (app, _pool) = app_or_skip!(false);
    let (email, _registration, _secret) = register(&app, "logout").await;
    let cookie = session_cookie(&login(&app, &email, PASSWORD).await);

    let res = call(
        &app,
        "POST",
        "/api/v1/auth/logout",
        None,
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let set_cookie = res
        .set_cookie
        .as_deref()
        .expect("Set-Cookie clears the cookie");
    assert!(
        set_cookie.starts_with("oxsum_session=;"),
        "the cookie is cleared: {set_cookie}"
    );
    assert!(
        set_cookie.contains("Max-Age=0"),
        "the clearing expires immediately: {set_cookie}"
    );

    // The same cookie stops working.
    let res = call(&app, "GET", "/api/v1/session", None, None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);

    // Logging out twice is not an error — with the dead cookie, or with none at all.
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/logout",
        None,
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(&app, "POST", "/api/v1/auth/logout", None, None, None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

#[tokio::test]
async fn an_expired_session_is_unauthorized() {
    let (app, pool) = app_or_skip!(false);
    let (email, _registration, _secret) = register(&app, "expiry").await;
    let res = login(&app, &email, PASSWORD).await;
    let cookie = session_cookie(&res);
    let session_id = res.body["data"]["session"]["id"].as_str().unwrap();

    sqlx::query(
        "UPDATE oxsum.sessions SET expires_at = now() - interval '1 minute' \
         WHERE session_id = $1::uuid",
    )
    .bind(session_id)
    .execute(&pool)
    .await
    .unwrap();

    let res = call(&app, "GET", "/api/v1/session", None, None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn a_session_dies_with_its_membership() {
    let (app, pool) = app_or_skip!(false);
    let (email, registration, _secret) = register(&app, "membership").await;
    let cookie = session_cookie(&login(&app, &email, PASSWORD).await);
    let org_id = registration["organization"]["id"].as_str().unwrap();
    let user_id = registration["user"]["id"].as_str().unwrap();

    sqlx::query(
        "DELETE FROM oxsum.memberships WHERE organization_id = $1::uuid AND user_id = $2::uuid",
    )
    .bind(org_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    let res = call(&app, "GET", "/api/v1/session", None, None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

#[tokio::test]
async fn an_explicit_bearer_token_never_falls_through_to_the_cookie() {
    let (app, _pool) = app_or_skip!(false);
    let (email, _registration, _secret) = register(&app, "precedence").await;
    let cookie = session_cookie(&login(&app, &email, PASSWORD).await);

    // A wrong bearer token plus a valid cookie is still 401: the explicit credential is
    // resolved as a key and does not get a second chance as a session.
    let res = call(
        &app,
        "GET",
        "/api/v1/session",
        None,
        Some("oxs-deadbeef"),
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
}

#[tokio::test]
async fn the_cookie_is_never_accepted_on_the_gateway() {
    let (app, _pool) = app_or_skip!(false);
    let (email, _registration, key) = register(&app, "gateway").await;
    let cookie = session_cookie(&login(&app, &email, PASSWORD).await);

    // The key opens the gateway, as before.
    let res = call(&app, "GET", "/v1/models", None, Some(&key), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // The cookie does not, and the refusal is the gateway's own error shape.
    let res = call(&app, "GET", "/v1/models", None, None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);
    assert_eq!(res.body["error"]["type"], "authentication_error");
}

#[tokio::test]
async fn members_manage_only_their_own_keys() {
    let (app, pool) = app_or_skip!(false);
    let (owner_email, owner_data, owner_signup_key) = register(&app, "boss").await;
    let (member_email, member_data, _member_signup_key) = register(&app, "teammate").await;
    let org_a = owner_data["organization"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let owner_uid = owner_data["user"]["id"].as_str().unwrap().to_owned();
    let member_uid = member_data["user"]["id"].as_str().unwrap().to_owned();

    // The member's oldest membership is in the owner's organization, so their session acts
    // as it. Invitation flows are a later item; the row goes in directly.
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role, created_at) \
         VALUES ($1::uuid, $2::uuid, 'member', now() - interval '1 hour')",
    )
    .bind(&org_a)
    .bind(&member_uid)
    .execute(&pool)
    .await
    .unwrap();

    let owner_login = login(&app, &owner_email, PASSWORD).await;
    let owner_cookie = session_cookie(&owner_login);
    let member_login = login(&app, &member_email, PASSWORD).await;
    let member_cookie = session_cookie(&member_login);
    assert_eq!(member_login.body["data"]["role"], "member");
    assert_eq!(member_login.body["data"]["organization"]["id"], org_a);

    // Each mints a key through their session; the key records who minted it.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "owner-key"})),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["createdBy"], owner_uid);
    let owner_key = res.body["data"]["id"].as_str().unwrap().to_owned();
    let owner_secret = res.body["data"]["secret"].as_str().unwrap().to_owned();

    let res = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "member-key"})),
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["createdBy"], member_uid);
    let member_key = res.body["data"]["id"].as_str().unwrap().to_owned();

    // A key minted with an API key records no creator.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "machine-key"})),
        Some(&owner_signup_key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body["data"]["createdBy"].is_null());

    // The member lists only the key they created.
    let res = call(
        &app,
        "GET",
        "/api/v1/org/keys",
        None,
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let listed: Vec<&str> = res.body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, [member_key.as_str()]);

    // The owner sees every key of the organization.
    let res = call(
        &app,
        "GET",
        "/api/v1/org/keys",
        None,
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"].as_array().unwrap().len(), 4);

    // A member revoking a key they did not create gets 404, and the key stays live.
    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/keys/{owner_key}"),
        None,
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "NOT_FOUND");
    let res = call(&app, "GET", "/api/v1/org", None, Some(&owner_secret), None).await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the key still works: {}",
        res.body
    );

    // The owner revoking the member's key works.
    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/keys/{member_key}"),
        None,
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert!(res.body["data"]["revokedAt"].is_string());
}

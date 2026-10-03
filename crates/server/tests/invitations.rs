//! HTTP tests for invitation links (issue #59): an owner or admin mints a link, the
//! person holding it registers into the organization — the only registration an
//! `invite`-mode deployment allows. The link is valid seven days and usable once:
//! spent, expired and unknown tokens are all the same "not valid".
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
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

/// An app over a real database with oxsum's tables migrated, in the given signup mode.
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

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => online_app(&u, Signup::Open).await,
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

/// One plain HTTP call against the app.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
    cookie: Option<&str>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
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
    Res {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        set_cookie,
    }
}

/// A registered account: the personal organization signup made, and the first key.
struct Account {
    organization_id: Uuid,
    email: String,
    key: String,
}

/// Registers a fresh account with a personal organization.
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
    let data = &res.body["data"];
    Account {
        organization_id: data["organization"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
        email,
        key: data["apiKey"]["secret"].as_str().unwrap().to_owned(),
    }
}

/// Logs in and returns the session cookie.
async fn login(app: &Router, email: &str) -> String {
    let res = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "login failed: {}", res.body);
    res.set_cookie
        .as_deref()
        .expect("a cookie was set")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned()
}

/// Mints an invitation with the session and returns the token.
async fn invite(app: &Router, cookie: &str) -> String {
    let res = call(
        app,
        "POST",
        "/api/v1/org/invitations",
        None,
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "minting failed: {}", res.body);
    res.body["data"]["token"]
        .as_str()
        .expect("a token")
        .to_owned()
}

/// Registers through an invitation token.
async fn redeem(app: &Router, token: &str, name: &str) -> Res {
    let email = format!(
        "{name}_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    call(
        app,
        "POST",
        "/api/v1/invitations/redeem",
        Some(json!({"token": token, "email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await
}

/// The organizations a session belongs to, as ids.
async fn org_ids(app: &Router, cookie: &str) -> Vec<String> {
    let res = call(app, "GET", "/api/v1/orgs", None, None, Some(cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res.body["data"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|entry| {
            entry["organization"]["id"]
                .as_str()
                .expect("an id")
                .to_owned()
        })
        .collect()
}

/// An owner mints a link; the person holding it registers, lands in the inviting
/// organization as a member with a first API key, and no personal organization of
/// their own — the inviting one is the only membership.
#[tokio::test]
async fn an_owner_invites_and_the_link_registers_a_member() {
    let (app, _pool) = app_or_skip!();
    let owner = register(&app, "inv_owner").await;
    let cookie = login(&app, &owner.email).await;

    let res = call(
        &app,
        "POST",
        "/api/v1/org/invitations",
        None,
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let data = &res.body["data"];
    let token = data["token"].as_str().expect("a token").to_owned();
    assert!(token.starts_with("oxi-"), "the token carries its mark");
    // Seven days of life, minted server-side.
    let expires_at = OffsetDateTime::parse(
        data["expiresAt"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let life = expires_at - OffsetDateTime::now_utc();
    assert!(
        life > time::Duration::days(6) && life <= time::Duration::days(7),
        "seven days: {life}"
    );

    let res = redeem(&app, &token, "inv_new").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let data = &res.body["data"];
    assert_eq!(
        data["organization"]["id"].as_str(),
        Some(owner.organization_id.to_string().as_str()),
        "joined the inviting organization"
    );
    assert!(
        data["apiKey"]["secret"]
            .as_str()
            .unwrap()
            .starts_with("oxs-"),
        "a first API key for that organization"
    );
    let email = data["user"]["email"].as_str().unwrap().to_owned();

    // One membership — the invitation's organization, role member — and the session
    // acts as it.
    let cookie = login(&app, &email).await;
    assert_eq!(
        org_ids(&app, &cookie).await,
        vec![owner.organization_id.to_string()],
        "no personal organization was created"
    );
    let res = call(&app, "GET", "/api/v1/session", None, None, Some(&cookie)).await;
    assert_eq!(res.body["data"]["role"], "member");
}

/// An admin mints links too; a member and an API key are refused — minting is the
/// same owner-or-admin person rule as every membership write.
#[tokio::test]
async fn only_an_owner_or_admin_session_mints_a_link() {
    let (app, _pool) = app_or_skip!();
    let owner = register(&app, "inv_owner2").await;
    let owner_cookie = login(&app, &owner.email).await;

    // A member joins through a link, then tries to mint one.
    let member_token = invite(&app, &owner_cookie).await;
    let res = redeem(&app, &member_token, "inv_member").await;
    let member_email = res.body["data"]["user"]["email"]
        .as_str()
        .unwrap()
        .to_owned();
    let member_cookie = login(&app, &member_email).await;
    let res = call(
        &app,
        "POST",
        "/api/v1/org/invitations",
        None,
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // Promote the member to admin: now minting works.
    let member_id =
        sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM oxsum.users WHERE email = $1")
            .bind(&member_email)
            .fetch_one(&_pool)
            .await
            .unwrap();
    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{member_id}"),
        Some(json!({"role": "admin"})),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(
        &app,
        "POST",
        "/api/v1/org/invitations",
        None,
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // An API key is not a person: refused at the same door membership writes use.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/invitations",
        None,
        Some(&owner.key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
}

/// A link works once: the second redemption of the same token is "not valid", and
/// so is a token that never existed — the response does not say which.
#[tokio::test]
async fn a_link_works_once_and_an_unknown_token_is_the_same_not_valid() {
    let (app, _pool) = app_or_skip!();
    let owner = register(&app, "inv_owner3").await;
    let cookie = login(&app, &owner.email).await;
    let token = invite(&app, &cookie).await;

    let res = redeem(&app, &token, "inv_once").await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    for token in [token, format!("oxi-{}", "0".repeat(64))] {
        let res = redeem(&app, &token, "inv_again").await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{token}");
        assert_eq!(res.body["error"]["code"], "NOT_FOUND");
    }
}

/// An expired link is "not valid" — the row stays, but its seven days are over.
#[tokio::test]
async fn an_expired_link_is_not_valid() {
    let (app, pool) = app_or_skip!();
    let owner = register(&app, "inv_owner4").await;
    let cookie = login(&app, &owner.email).await;
    let res = call(
        &app,
        "POST",
        "/api/v1/org/invitations",
        None,
        None,
        Some(&cookie),
    )
    .await;
    let data = &res.body["data"];
    let id = data["id"].as_str().unwrap().to_owned();
    let token = data["token"].as_str().unwrap().to_owned();

    sqlx::query(
        "UPDATE oxsum.invitations SET expires_at = now() - interval '1 hour'          WHERE invitation_id = $1",
    )
    .bind(id.parse::<Uuid>().unwrap())
    .execute(&pool)
    .await
    .expect("ages the link");

    let res = redeem(&app, &token, "inv_late").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "NOT_FOUND");
}

/// Two redemptions of one link at once: exactly one wins — the row lock spends the
/// link before the second attempt can see it as live.
#[tokio::test]
async fn two_redemptions_at_once_have_exactly_one_winner() {
    let (app, _pool) = app_or_skip!();
    let owner = register(&app, "inv_owner5").await;
    let cookie = login(&app, &owner.email).await;
    let token = invite(&app, &cookie).await;

    let (first, second) = (
        redeem(&app, &token, "inv_race_a"),
        redeem(&app, &token, "inv_race_b"),
    );
    let (first, second) = tokio::join!(first, second);
    let mut statuses = [first.status, second.status];
    statuses.sort_unstable();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::NOT_FOUND],
        "one won, one lost: {first:?} {second:?}",
        first = first.body,
        second = second.body
    );
}

/// An email that is taken is a conflict — and the link survives it: it was never
/// spent, so it can still register someone else.
#[tokio::test]
async fn a_taken_email_is_a_conflict_and_the_link_survives() {
    let (app, _pool) = app_or_skip!();
    let owner = register(&app, "inv_owner6").await;
    let cookie = login(&app, &owner.email).await;
    let token = invite(&app, &cookie).await;

    let res = call(
        &app,
        "POST",
        "/api/v1/invitations/redeem",
        Some(json!({"token": token, "email": owner.email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "CONFLICT");

    let res = redeem(&app, &token, "inv_after_conflict").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the link still works: {}",
        res.body
    );
}

/// A bad email or a short password is a validation error — checked before the link
/// is spent, so the link survives those too.
#[tokio::test]
async fn a_bad_email_or_password_is_refused_without_spending_the_link() {
    let (app, _pool) = app_or_skip!();
    let owner = register(&app, "inv_owner7").await;
    let cookie = login(&app, &owner.email).await;
    let token = invite(&app, &cookie).await;

    for body in [
        json!({"token": token, "email": "not-an-email", "password": PASSWORD}),
        json!({"token": token, "email": "fine@example.com", "password": "short"}),
    ] {
        let res = call(
            &app,
            "POST",
            "/api/v1/invitations/redeem",
            Some(body),
            None,
            None,
        )
        .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
        assert_eq!(res.body["error"]["code"], "VALIDATION_ERROR");
    }

    let res = redeem(&app, &token, "inv_after_bad").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the link still works: {}",
        res.body
    );
}

/// In `invite` mode self-registration stays refused — and the link is the way in.
#[tokio::test]
async fn invite_mode_refuses_self_registration_but_accepts_the_link() {
    let Some(u) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let (app, _pool) = online_app(&u, Signup::Invite).await;

    let res = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": "self@example.com", "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // Seed an inviter the only way left — straight through the core, the gate only
    // guards HTTP — then let its link in.
    let seed_email = format!(
        "seeded_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let db = Db::from_pool(_pool.clone());
    db.register(oxsum_core::NewUser {
        email: seed_email.clone(),
        password: PASSWORD.to_owned(),
        organization_name: None,
    })
    .await
    .expect("seeds an owner");

    let cookie = login(&app, &seed_email).await;
    let token = invite(&app, &cookie).await;
    let res = redeem(&app, &token, "inv_only_way").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the link registers in invite mode: {}",
        res.body
    );
}

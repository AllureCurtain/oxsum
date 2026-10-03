//! HTTP tests for team organizations (issue #58): a user belongs to several
//! organizations, lists them, creates a `team` one as its owner, and switches which
//! one the session acts as — after which every organization-scoped read, the
//! dashboard included, answers for the chosen one. The refusals too: an
//! organization the user is not a member of is not found, and an API key cannot
//! list, create or switch — a key belongs to exactly one organization.
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
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

/// An app over a real database with oxsum's tables migrated.
async fn online_app(url: &str) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (oxsum_server::app(db, Config::new(Signup::Open, None)), pool)
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
            Some(u) => online_app(&u).await,
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

/// The organizations the session may act as, asserted to have loaded.
async fn orgs(app: &Router, cookie: &str) -> Vec<Value> {
    let res = call(app, "GET", "/api/v1/orgs", None, None, Some(cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res.body["data"].as_array().expect("a list").clone()
}

/// The role the list reports for an organization id.
fn role_of(list: &[Value], id: &str) -> Option<String> {
    list.iter()
        .find(|entry| entry["organization"]["id"].as_str() == Some(id))
        .map(|entry| entry["role"].as_str().expect("a role").to_owned())
}

/// The session's acting organization id.
async fn acting(app: &Router, cookie: &str) -> String {
    let res = call(app, "GET", "/api/v1/session", None, None, Some(cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    res.body["data"]["organization"]["id"]
        .as_str()
        .expect("an organization id")
        .to_owned()
}

/// One server function of the pages, called the way its own code calls it.
async fn page_call(app: &Router, name: &str, body: &str, cookie: &str) -> (StatusCode, Value) {
    let (path, method) = leptos::server_fn::axum::server_fn_paths()
        .find(|(path, _)| path.starts_with(&format!("/_pages/{name}")))
        .unwrap_or_else(|| panic!("the {name} server function is registered"));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", format!("oxsum_session={cookie}"))
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A user creates a team organization and becomes its owner; the list shows both
/// memberships, and the session keeps acting as the organization it had.
#[tokio::test]
async fn a_user_creates_a_team_organization_and_becomes_its_owner() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "create").await;
    let cookie = login(&app, &account.email).await;

    let res = call(
        &app,
        "POST",
        "/api/v1/orgs",
        Some(json!({"name": "  The Team  "})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["name"], "The Team", "the name is trimmed");
    assert_eq!(res.body["data"]["kind"], "team");
    let team_id = res.body["data"]["id"].as_str().unwrap().to_owned();

    let list = orgs(&app, &cookie).await;
    assert_eq!(list.len(), 2, "two memberships now: {list:?}");
    assert_eq!(
        role_of(&list, &account.organization_id.to_string()).as_deref(),
        Some("owner")
    );
    assert_eq!(role_of(&list, &team_id).as_deref(), Some("owner"));

    // Creating does not switch: the session still acts as the organization it had.
    assert_eq!(
        acting(&app, &cookie).await,
        account.organization_id.to_string()
    );
}

/// Switching carries the session: the session row, the session read, the
/// organization read and the dashboard all answer for the chosen organization —
/// and switching back works the same way.
#[tokio::test]
async fn switching_the_organization_carries_the_session() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "switch").await;
    let cookie = login(&app, &account.email).await;
    let personal = account.organization_id.to_string();

    let created = call(
        &app,
        "POST",
        "/api/v1/orgs",
        Some(json!({"name": "Switch To Me"})),
        None,
        Some(&cookie),
    )
    .await;
    let team_id = created.body["data"]["id"].as_str().unwrap().to_owned();

    let res = call(
        &app,
        "POST",
        "/api/v1/session/organization",
        Some(json!({"organizationId": team_id})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        res.body["data"]["organization"]["id"].as_str(),
        Some(team_id.as_str())
    );
    assert_eq!(res.body["data"]["role"], "owner");

    // The same cookie now resolves to the team: session, organization and the
    // dashboard's own read all agree.
    assert_eq!(acting(&app, &cookie).await, team_id);
    let res = call(&app, "GET", "/api/v1/org", None, None, Some(&cookie)).await;
    assert_eq!(res.body["data"]["id"].as_str(), Some(team_id.as_str()));
    let (status, dashboard) = page_call(&app, "get_dashboard", "", &cookie).await;
    assert_eq!(status, StatusCode::OK, "{dashboard}");
    assert_eq!(dashboard["orgName"], "Switch To Me");

    // And back.
    let res = call(
        &app,
        "POST",
        "/api/v1/session/organization",
        Some(json!({"organizationId": personal})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(
        res.body["data"]["organization"]["id"].as_str(),
        Some(personal.as_str())
    );
    assert_eq!(acting(&app, &cookie).await, personal);
}

/// An organization the user is not a member of is not found — whether it exists
/// or not is not said.
#[tokio::test]
async fn a_user_cannot_switch_to_an_organization_they_do_not_belong_to() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "outsider").await;
    let other = register(&app, "outsider_other").await;
    let cookie = login(&app, &account.email).await;

    for organization_id in [
        other.organization_id.to_string(),
        Uuid::new_v4().to_string(),
    ] {
        let res = call(
            &app,
            "POST",
            "/api/v1/session/organization",
            Some(json!({"organizationId": organization_id})),
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{organization_id}");
        assert_eq!(res.body["error"]["code"], "NOT_FOUND");
    }
    assert_eq!(
        acting(&app, &cookie).await,
        account.organization_id.to_string()
    );
}

/// A name that trims to nothing is refused, and so is one that is too long.
#[tokio::test]
async fn an_empty_or_overlong_name_is_refused() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "names").await;
    let cookie = login(&app, &account.email).await;

    for name in ["", "   ", &"x".repeat(81)] {
        let res = call(
            &app,
            "POST",
            "/api/v1/orgs",
            Some(json!({"name": name})),
            None,
            Some(&cookie),
        )
        .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{name:?}");
        assert_eq!(res.body["error"]["code"], "VALIDATION_ERROR");
    }
    assert_eq!(orgs(&app, &cookie).await.len(), 1, "nothing was created");
}

/// An API key cannot list, create or switch organizations: a key belongs to
/// exactly one, and these are a person's actions.
#[tokio::test]
async fn an_api_key_cannot_list_create_or_switch_organizations() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "keyed").await;

    let res = call(&app, "GET", "/api/v1/orgs", None, Some(&account.key), None).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    let res = call(
        &app,
        "POST",
        "/api/v1/orgs",
        Some(json!({"name": "Keyed Team"})),
        Some(&account.key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    let res = call(
        &app,
        "POST",
        "/api/v1/session/organization",
        Some(json!({"organizationId": account.organization_id})),
        Some(&account.key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
}

/// The list shows the organizations in the order the memberships were created,
/// oldest first — the same order a session resolves without a choice.
#[tokio::test]
async fn the_list_is_oldest_membership_first() {
    let (app, pool) = app_or_skip!();
    let account = register(&app, "ordered").await;
    let cookie = login(&app, &account.email).await;

    // Two more organizations, created a while ago in a known order.
    for (name, hours) in [("Second", 2), ("Third", 1)] {
        sqlx::query(
            "WITH org AS ( \
                 INSERT INTO oxsum.organizations (organization_id, name, tenant_id, kind) \
                 VALUES ($1, $2, $3, 'team') RETURNING organization_id \
             ) \
             INSERT INTO oxsum.memberships (organization_id, user_id, role, created_at) \
             SELECT organization_id, $4, 'member', now() - ($5::text || ' hours')::interval FROM org",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(Uuid::new_v4().simple().to_string())
        .bind(account_user_id(&pool, &account.email).await)
        .bind(hours.to_string())
        .execute(&pool)
        .await
        .expect("seeds an organization");
    }

    let list = orgs(&app, &cookie).await;
    let names: Vec<&str> = list
        .iter()
        .map(|entry| entry["organization"]["name"].as_str().unwrap())
        .collect();
    // The seeded memberships predate signup's, so they come first, oldest first.
    assert_eq!(names[..2], ["Second", "Third"], "oldest first: {names:?}");
    assert_eq!(names.len(), 3, "{names:?}");
}

/// A user's id, by email — for seeding memberships the endpoints cannot create.
async fn account_user_id(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar("SELECT user_id FROM oxsum.users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("the user exists")
}

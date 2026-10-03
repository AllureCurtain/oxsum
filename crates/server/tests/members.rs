//! HTTP tests for membership management (issue #56): the authorization matrix, the four
//! actions — add an existing account by email, remove a member, change a role, transfer
//! ownership — and every refusal the rules promise: a member cannot manage, an admin cannot
//! touch an owner, the last owner cannot be removed or demoted, an unknown email is refused.
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
    /// The body, parsed as JSON; `Value::Null` for a body that is not JSON (an HTML page).
    body: Value,
    /// The body as it arrived, for the pages.
    text: String,
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
        text: String::from_utf8_lossy(&bytes).into_owned(),
        set_cookie,
    }
}

/// A registered account: the user signup created, the personal organization it gave them,
/// and the first key.
struct Account {
    user_id: Uuid,
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
        user_id: data["user"]["id"].as_str().unwrap().parse().unwrap(),
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
///
/// A session is created for the user's *oldest* membership, so this has to run after
/// [`seed_membership`] to act as a seeded organization.
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

/// Gives `account` a membership in `organization_id` in `role`, an hour before the personal
/// membership signup created.
///
/// The management endpoints cannot create an admin, so seeding through them would be
/// circular; and the row has to predate signup's own membership, because a session acts as
/// the user's oldest one.
async fn seed_membership(pool: &PgPool, organization_id: Uuid, account: &Account, role: &str) {
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role, created_at) \
         VALUES ($1, $2, $3, now() - interval '1 hour')",
    )
    .bind(organization_id)
    .bind(account.user_id)
    .bind(role)
    .execute(pool)
    .await
    .expect("seeds a membership");
}

/// One server function of the pages, called the way its own code calls it: the URL-encoded
/// body of the function's arguments, with the session cookie.
///
/// The path is read from the same registry the router registers its server-function routes
/// from, because leptos suffixes it with a hash of the crate and module it was declared in.
async fn page_call(
    app: &Router,
    name: &str,
    body: &str,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let (path, method) = leptos::server_fn::axum::server_fn_paths()
        .find(|(path, _)| path.starts_with(&format!("/_pages/{name}")))
        .unwrap_or_else(|| panic!("the {name} server function is registered"));
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// The members page's own read: `/_pages/get_members`, answered as a `MembersView`.
async fn members_page(app: &Router, cookie: Option<&str>) -> (StatusCode, Value) {
    page_call(app, "get_members", "", cookie).await
}

/// The members page's data, asserted to have loaded.
async fn members(app: &Router, cookie: &str) -> Value {
    let (status, body) = members_page(app, Some(cookie)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the members list did not load: {body}"
    );
    body
}

/// The roles the list shows, by email.
fn roles_of(list: &Value) -> Vec<(String, String)> {
    list["members"]
        .as_array()
        .expect("a member list")
        .iter()
        .map(|member| {
            (
                member["email"].as_str().unwrap().to_owned(),
                member["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The role one email holds in the list, or None.
fn role_of(list: &Value, email: &str) -> Option<String> {
    roles_of(list)
        .into_iter()
        .find(|(member, _)| member == email)
        .map(|(_, role)| role)
}

/// An organization with an owner, an admin and a member, all able to log in.
struct Fixture {
    owner: Account,
    owner_cookie: String,
    admin: Account,
    admin_cookie: String,
    member: Account,
    member_cookie: String,
}

/// Registers the three people, seeds the admin and member roles in the owner's
/// organization, and logs all of them in.
async fn fixture(app: &Router, pool: &PgPool, name: &str) -> Fixture {
    let owner = register(app, &format!("{name}_owner")).await;
    let admin = register(app, &format!("{name}_admin")).await;
    let member = register(app, &format!("{name}_member")).await;
    seed_membership(pool, owner.organization_id, &admin, "admin").await;
    seed_membership(pool, owner.organization_id, &member, "member").await;
    Fixture {
        owner_cookie: login(app, &owner.email).await,
        admin_cookie: login(app, &admin.email).await,
        member_cookie: login(app, &member.email).await,
        owner,
        admin,
        member,
    }
}

/// An owner adds an existing account by email, and the personal organization becomes a team.
#[tokio::test]
async fn an_owner_adds_an_existing_account_and_the_organization_becomes_a_team() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "add").await;
    let new_member = register(&app, "add_new").await;

    // A personal organization to begin with: signup made it one person's.
    let before = call(
        &app,
        "GET",
        "/api/v1/org",
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(before.body["data"]["kind"], "personal", "{}", before.body);

    let res = call(
        &app,
        "POST",
        "/api/v1/org/members",
        Some(json!({"email": new_member.email})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["email"], new_member.email.as_str());
    assert_eq!(res.body["data"]["role"], "member");
    assert_eq!(
        res.body["data"]["userId"],
        new_member.user_id.to_string().as_str()
    );

    // The second person makes it a team; the ledger does not move, so the organization
    // still answers.
    let after = call(
        &app,
        "GET",
        "/api/v1/org",
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(after.body["data"]["kind"], "team", "{}", after.body);

    let list = members(&app, &f.owner_cookie).await;
    assert_eq!(role_of(&list, &new_member.email).as_deref(), Some("member"));
}

/// An unknown email is refused and nothing is stored: pending invitations are issue #59.
#[tokio::test]
async fn an_unknown_email_is_not_found_and_a_second_add_is_a_conflict() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "unknown").await;
    let before = members(&app, &f.owner_cookie).await;

    let res = call(
        &app,
        "POST",
        "/api/v1/org/members",
        Some(json!({"email": "nobody@example.com"})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "NOT_FOUND");
    assert!(
        res.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nobody@example.com"),
        "the refusal names the address: {}",
        res.body
    );

    // Nothing was added, and a malformed address is a validation failure, not a 404.
    let after = members(&app, &f.owner_cookie).await;
    assert_eq!(before["members"], after["members"]);
    let res = call(
        &app,
        "POST",
        "/api/v1/org/members",
        Some(json!({"email": "not-an-address"})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "VALIDATION_ERROR");
}

/// An owner changes a member's role and removes the membership; the removed person's session
/// stops authenticating, and a second removal is not a silent success.
#[tokio::test]
async fn an_owner_changes_a_role_and_removes_a_member() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "roles").await;

    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{}", f.member.user_id),
        Some(json!({"role": "admin"})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["role"], "admin");

    // The role is read fresh from the membership on every request, so the same session is an
    // admin now — and admins may add members.
    let extra = register(&app, "roles_extra").await;
    let res = call(
        &app,
        "POST",
        "/api/v1/org/members",
        Some(json!({"email": extra.email})),
        None,
        Some(&f.member_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // Setting the role a member already has is a no-op, not a conflict.
    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{}", f.member.user_id),
        Some(json!({"role": "admin"})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["role"], "admin");

    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", f.member.user_id),
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["email"], f.member.email.as_str());
    assert!(role_of(&members(&app, &f.owner_cookie).await, &f.member.email).is_none());

    // The session row is still there, but the membership it names is gone: it authenticates
    // nothing.
    let res = call(
        &app,
        "GET",
        "/api/v1/org",
        None,
        None,
        Some(&f.member_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{}", res.body);

    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", f.member.user_id),
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
}

/// An owner transfers ownership: the target owns the organization, the previous owner is an
/// admin, and there is exactly one owner.
#[tokio::test]
async fn an_owner_transfers_ownership_and_becomes_an_admin() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "transfer").await;

    let res = call(
        &app,
        "POST",
        "/api/v1/org/ownership",
        Some(json!({"userId": f.admin.user_id})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    assert_eq!(res.body["data"]["owner"]["email"], f.admin.email.as_str());
    assert_eq!(res.body["data"]["owner"]["role"], "owner");
    assert_eq!(
        res.body["data"]["previousOwner"]["email"],
        f.owner.email.as_str()
    );
    assert_eq!(res.body["data"]["previousOwner"]["role"], "admin");

    let list = members(&app, &f.admin_cookie).await;
    assert_eq!(role_of(&list, &f.admin.email).as_deref(), Some("owner"));
    assert_eq!(role_of(&list, &f.owner.email).as_deref(), Some("admin"));
    assert_eq!(
        roles_of(&list)
            .iter()
            .filter(|(_, role)| role == "owner")
            .count(),
        1,
        "exactly one owner: {list}"
    );

    // The previous owner still manages members as an admin, but may not transfer ownership.
    let extra = register(&app, "transfer_extra").await;
    let res = call(
        &app,
        "POST",
        "/api/v1/org/members",
        Some(json!({"email": extra.email})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(
        &app,
        "POST",
        "/api/v1/org/ownership",
        Some(json!({"userId": extra.user_id})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "FORBIDDEN");

    // Transferring to the current owner names an owner, not a member.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/ownership",
        Some(json!({"userId": f.admin.user_id})),
        None,
        Some(&f.admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "CONFLICT");
}

/// An admin manages members, and is refused on the owner's row and on ownership.
#[tokio::test]
async fn an_admin_manages_members_but_may_not_touch_the_owner() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "admin").await;

    // Add, change a role, remove: all allowed for an admin.
    let extra = register(&app, "admin_extra").await;
    let res = call(
        &app,
        "POST",
        "/api/v1/org/members",
        Some(json!({"email": extra.email})),
        None,
        Some(&f.admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{}", extra.user_id),
        Some(json!({"role": "admin"})),
        None,
        Some(&f.admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{}", extra.user_id),
        Some(json!({"role": "member"})),
        None,
        Some(&f.admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", extra.user_id),
        None,
        None,
        Some(&f.admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);

    // The owner's row: neither removal nor a role change.
    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", f.owner.user_id),
        None,
        None,
        Some(&f.admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "FORBIDDEN");
    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{}", f.owner.user_id),
        Some(json!({"role": "member"})),
        None,
        Some(&f.admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    let res = call(
        &app,
        "POST",
        "/api/v1/org/ownership",
        Some(json!({"userId": f.admin.user_id})),
        None,
        Some(&f.admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // Promoting anyone to owner is not a request this endpoint can express, for any role:
    // the contract's role is admin or member.
    for cookie in [&f.admin_cookie, &f.owner_cookie] {
        let res = call(
            &app,
            "PATCH",
            &format!("/api/v1/org/members/{}", f.member.user_id),
            Some(json!({"role": "owner"})),
            None,
            Some(cookie),
        )
        .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.body);
        assert_eq!(res.body["error"]["code"], "VALIDATION_ERROR");
    }
    let list = members(&app, &f.owner_cookie).await;
    assert_eq!(role_of(&list, &f.owner.email).as_deref(), Some("owner"));
    assert_eq!(role_of(&list, &f.member.email).as_deref(), Some("member"));
}

/// A member reaches no management endpoint, on any of the four actions — over REST and
/// through the page's own server functions.
#[tokio::test]
async fn a_member_cannot_manage_memberships() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "member").await;

    let extra = register(&app, "member_extra").await;
    let attempts: Vec<(&str, String, Option<Value>)> = vec![
        (
            "POST",
            "/api/v1/org/members".to_owned(),
            Some(json!({"email": extra.email})),
        ),
        (
            "DELETE",
            format!("/api/v1/org/members/{}", f.admin.user_id),
            None,
        ),
        (
            "PATCH",
            format!("/api/v1/org/members/{}", f.admin.user_id),
            Some(json!({"role": "member"})),
        ),
        (
            "POST",
            "/api/v1/org/ownership".to_owned(),
            Some(json!({"userId": f.member.user_id})),
        ),
    ];
    for (method, path, body) in attempts {
        let res = call(&app, method, &path, body, None, Some(&f.member_cookie)).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{method} {path} was not refused: {}",
            res.body
        );
        assert_eq!(res.body["error"]["code"], "FORBIDDEN");
    }

    // Nothing happened: the extra account is not a member, the admin still is.
    let list = members(&app, &f.member_cookie).await;
    assert!(role_of(&list, &extra.email).is_none());
    assert_eq!(role_of(&list, &f.admin.email).as_deref(), Some("admin"));

    // The page's own function refuses the same call, with the same rule.
    let (status, _) = page_call(
        &app,
        "add_member",
        &format!("email={}", extra.email),
        Some(&f.member_cookie),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "the page let a member add one");
    let list = members(&app, &f.member_cookie).await;
    assert!(role_of(&list, &extra.email).is_none());
}

/// Another organization's people manage only their own: no endpoint takes a tenant from the
/// caller, so org A can be reached by nothing but a session of org A.
#[tokio::test]
async fn another_organization_manages_only_its_own_memberships() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "cross").await;

    // A second organization, with an admin of its own and a plain member.
    let other_owner = register(&app, "cross_org").await;
    let other_admin = register(&app, "cross_admin").await;
    let other_member = register(&app, "cross_member").await;
    seed_membership(&pool, other_owner.organization_id, &other_admin, "admin").await;
    seed_membership(&pool, other_owner.organization_id, &other_member, "member").await;
    let owner_cookie = login(&app, &other_owner.email).await;
    let admin_cookie = login(&app, &other_admin.email).await;
    let member_cookie = login(&app, &other_member.email).await;

    // A plain member there reaches nothing either.
    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", other_admin.user_id),
        None,
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);

    // Its admin manages that organization: an org A user id is not a member of it.
    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", f.member.user_id),
        None,
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{}", f.member.user_id),
        Some(json!({"role": "admin"})),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    // Ownership is the owner's alone: an admin is refused before the target is even looked
    // at, and the org A user id an owner names is not a member here.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/ownership",
        Some(json!({"userId": f.member.user_id})),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.body);
    let res = call(
        &app,
        "POST",
        "/api/v1/org/ownership",
        Some(json!({"userId": f.member.user_id})),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);

    // Adding an org A member to this organization is legitimate — one account may be in many
    // organizations — and it changes nothing in org A.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/members",
        Some(json!({"email": f.member.email})),
        None,
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let owned = members(&app, &f.owner_cookie).await;
    assert_eq!(role_of(&owned, &f.owner.email).as_deref(), Some("owner"));
    assert_eq!(role_of(&owned, &f.member.email).as_deref(), Some("member"));
    assert_eq!(role_of(&owned, &other_admin.email), None);

    // The list a caller reads is its own organization's.
    let theirs = members(&app, &admin_cookie).await;
    assert!(role_of(&theirs, &f.owner.email).is_none());
    assert_eq!(
        role_of(&theirs, &other_admin.email).as_deref(),
        Some("admin")
    );
    assert_eq!(role_of(&theirs, &f.member.email).as_deref(), Some("member"));
}

/// The organization's last owner cannot be removed or demoted: the owner seat is never left
/// empty, so ownership has to be transferred first.
#[tokio::test]
async fn the_last_owner_cannot_be_removed_or_demoted() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "last").await;

    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", f.owner.user_id),
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "CONFLICT");
    assert!(
        res.body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("last owner"),
        "{}",
        res.body
    );

    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{}", f.owner.user_id),
        Some(json!({"role": "admin"})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "CONFLICT");

    // Still the owner, and still able to manage.
    let list = members(&app, &f.owner_cookie).await;
    assert_eq!(role_of(&list, &f.owner.email).as_deref(), Some("owner"));
    let res = call(
        &app,
        "GET",
        "/api/v1/org",
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
}

/// An API key is not a person and names no role: it manages no memberships, on any action.
#[tokio::test]
async fn an_api_key_cannot_manage_memberships() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "key").await;

    let extra = register(&app, "key_extra").await;
    let attempts: Vec<(&str, String, Option<Value>)> = vec![
        (
            "POST",
            "/api/v1/org/members".to_owned(),
            Some(json!({"email": extra.email})),
        ),
        (
            "DELETE",
            format!("/api/v1/org/members/{}", f.member.user_id),
            None,
        ),
        (
            "PATCH",
            format!("/api/v1/org/members/{}", f.member.user_id),
            Some(json!({"role": "admin"})),
        ),
        (
            "POST",
            "/api/v1/org/ownership".to_owned(),
            Some(json!({"userId": f.member.user_id})),
        ),
    ];
    for (method, path, body) in attempts {
        // The key authenticates — this is an authorization refusal, not a missing
        // credential — and it is the owner's own organization's key.
        let res = call(&app, method, &path, body, Some(&f.owner.key), None).await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{method} {path} was not refused: {}",
            res.body
        );
        assert_eq!(res.body["error"]["code"], "FORBIDDEN");
    }

    let list = members(&app, &f.owner_cookie).await;
    assert!(role_of(&list, &extra.email).is_none());
    assert_eq!(role_of(&list, &f.member.email).as_deref(), Some("member"));
}

/// Naming a user who is not a member is not found, on all three actions that name one.
#[tokio::test]
async fn a_user_who_is_not_a_member_is_not_found() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "absent").await;
    let stranger = register(&app, "absent_stranger").await;

    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", stranger.user_id),
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    assert_eq!(res.body["error"]["code"], "NOT_FOUND");
    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/org/members/{}", stranger.user_id),
        Some(json!({"role": "admin"})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
    let res = call(
        &app,
        "POST",
        "/api/v1/org/ownership",
        Some(json!({"userId": stranger.user_id})),
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.body);
}

/// Any member may read the member list; a caller with no session reads nothing.
#[tokio::test]
async fn any_member_may_read_the_member_list() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "read").await;

    for (cookie, role) in [
        (&f.owner_cookie, "owner"),
        (&f.admin_cookie, "admin"),
        (&f.member_cookie, "member"),
    ] {
        let list = members(&app, cookie).await;
        assert_eq!(list["role"], role);
        assert_eq!(role_of(&list, &f.owner.email).as_deref(), Some("owner"));
        assert_eq!(role_of(&list, &f.admin.email).as_deref(), Some("admin"));
        assert_eq!(role_of(&list, &f.member.email).as_deref(), Some("member"));
    }

    // Without a session, the page's own read answers an error rather than a list.
    let (status, _) = members_page(&app, None).await;
    assert_ne!(status, StatusCode::OK, "the list was readable anonymously");

    // A membership in the organization is what makes the list readable at all: resolution
    // joins `memberships`, so the session of someone who has been removed reads nothing.
    let res = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/members/{}", f.member.user_id),
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.body);
    let (status, _) = members_page(&app, Some(&f.member_cookie)).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a former member still read the list"
    );
    // And the organization's people still read it.
    let list = members(&app, &f.owner_cookie).await;
    assert!(role_of(&list, &f.member.email).is_none());
}

/// The members page renders the management controls only for the roles that may use them.
#[tokio::test]
async fn the_members_page_renders_controls_only_for_managers() {
    let (app, pool) = app_or_skip!();
    let f = fixture(&app, &pool, "html").await;

    let owner_page = call(
        &app,
        "GET",
        "/dashboard/members",
        None,
        None,
        Some(&f.owner_cookie),
    )
    .await;
    assert_eq!(owner_page.status, StatusCode::OK, "{}", owner_page.body);
    assert!(
        owner_page.text.contains("Add member"),
        "an owner is offered the add form"
    );
    assert!(
        owner_page.text.contains("Make owner"),
        "an owner is offered the ownership transfer"
    );

    let member_page = call(
        &app,
        "GET",
        "/dashboard/members",
        None,
        None,
        Some(&f.member_cookie),
    )
    .await;
    assert_eq!(member_page.status, StatusCode::OK, "{}", member_page.body);
    // The list is HTML: the members are there, the controls are not.
    assert!(
        member_page.text.contains(&f.owner.email),
        "a member still reads the list"
    );
    assert!(
        !member_page.text.contains("Add member"),
        "a member is offered no form"
    );
    assert!(
        !member_page.text.contains("Make owner"),
        "a member is offered no ownership transfer"
    );
}

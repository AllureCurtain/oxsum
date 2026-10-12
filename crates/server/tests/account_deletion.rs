//! HTTP tests for account deletion (issue #202): `DELETE /api/v1/users/me` —
//! the password gate, the last-owner wall, the membership cleanup, the revoked
//! sessions and keys, and the pseudonymized usage attribution. The ledger is
//! untouched, and a deleted user's settled bill still verifies.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    ActingKey, BillLine, Db, SecretKey, Settlement, SettlementKind, Tenants, UsageRecord, UsageRow,
    entry_id_for, hold_description, settlement_key_for,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::Row;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

/// The server's posting date, UTC.
fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}

/// An app over a real database with oxsum's tables migrated.
async fn online_app(url: &str) -> (Router, Db) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None).with_secret(SecretKey::from_bytes([9; 32]));
    (oxsum_server::app(db.clone(), config), db)
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
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
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res {
        status,
        body,
        set_cookie,
    }
}

/// A registered account: the user signup created, the personal organization it
/// gave them, and the first key.
struct Account {
    user_id: Uuid,
    organization_id: Uuid,
    email: String,
    key: String,
    key_id: Uuid,
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
        key_id: data["apiKey"]["id"].as_str().unwrap().parse().unwrap(),
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

/// The user's tombstone row, read straight from the table.
async fn user_row(db: &Db, user_id: Uuid) -> (String, String) {
    let row =
        sqlx::query("SELECT email_normalized, password_hash FROM oxsum.users WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(db.pool())
            .await
            .expect("the user row exists");
    (
        row.try_get("email_normalized").unwrap(),
        row.try_get("password_hash").unwrap(),
    )
}

/// A settled turn written the way the gateway writes it: hold, settlement and
/// the usage row beside it, attributed to `key_id` with caller attribution.
async fn settle_request(
    db: &Db,
    tenants: &Tenants,
    tenant_id: &str,
    key_id: Uuid,
    request: &str,
    end_user: &str,
) {
    let wallet = tenants.get(tenant_id).await.expect("the wallet opens");
    let hold_key = format!("req-{request}:hold");
    let freeze = 500;
    wallet
        .hold_for_key(
            &ActingKey {
                key_id,
                spend_limit_minor: None,
                requests_per_minute: None,
                tokens_per_minute: None,
            },
            Some("m"),
            &hold_key,
            &hold_description(request, "m", freeze).expect("the hold serializes"),
            freeze,
            today(),
        )
        .await
        .expect("the hold is taken");
    let usage = UsageRecord::tokens(1_000, 500).expect("the seed counts");
    let lines = [
        BillLine {
            item: "input".to_owned(),
            units: usage.input_tokens,
            price_per_m: 1_000,
        },
        BillLine {
            item: "output".to_owned(),
            units: usage.output_tokens,
            price_per_m: 2_000,
        },
    ];
    let record = Settlement {
        request,
        channel: "mock",
        model: "m",
        price_version: 1,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged: 200,
        freeze,
        upstream_attempts: 1,
    };
    wallet
        .settle(
            &hold_key,
            &record.description().expect("the record serializes"),
            200,
            today(),
        )
        .await
        .expect("the settlement lands");
    let mut usage = usage;
    usage.end_user = Some(end_user.to_owned());
    usage.tags.insert("env".to_owned(), "prod".to_owned());
    db.record_usage(&UsageRow {
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: tenant_id.to_owned(),
        key_id: Some(key_id),
        model: "m".to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
        usage,
        charged_minor: 200,
        freeze_minor: freeze,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    })
    .await
    .expect("the usage row is written");
}

/// The organization row of `account`'s personal org.
async fn org_row(db: &Db, organization_id: Uuid) -> (String, bool) {
    let row = sqlx::query(
        "SELECT name, (suspended_at IS NOT NULL) AS suspended \
         FROM oxsum.organizations WHERE organization_id = $1",
    )
    .bind(organization_id)
    .fetch_one(db.pool())
    .await
    .expect("the organization row exists");
    (
        row.try_get("name").unwrap(),
        row.try_get("suspended").unwrap(),
    )
}

// ── the gate ────────────────────────────────────────────────────────────────

/// Deleting needs the session cookie and the right password: a key is refused
/// 403, a wrong password 401, a missing body 400.
#[tokio::test]
async fn the_delete_is_session_only_and_password_confirmed() {
    let (app, _db) = app_or_skip!();
    let account = register(&app, "gate").await;
    let cookie = login(&app, &account.email).await;

    // An API key is not a person.
    let res = call(
        &app,
        "DELETE",
        "/api/v1/users/me",
        Some(json!({"password": PASSWORD})),
        Some(&account.key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{:#}", res.body);

    // A wrong password is the same 401 a bad login gives.
    let res = call(
        &app,
        "DELETE",
        "/api/v1/users/me",
        Some(json!({"password": "not the password"})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{:#}", res.body);

    // No credential at all: the auth middleware's own 401.
    let res = call(
        &app,
        "DELETE",
        "/api/v1/users/me",
        Some(json!({"password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{:#}", res.body);
}

/// The last-owner wall: a sole owner cannot delete out from under an
/// organization — transfer the seat first, and a second owner lets the delete
/// proceed.
#[tokio::test]
async fn the_last_owner_is_refused_until_the_seat_moves() {
    let (app, db) = app_or_skip!();
    let owner = register(&app, "last_owner").await;
    let other = register(&app, "last_owner_other").await;
    let cookie = login(&app, &owner.email).await;

    // Seed a second membership for `other` in the owner's personal org, but
    // the owner is still the only owner.
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role) \
         VALUES ($1, $2, 'member')",
    )
    .bind(owner.organization_id)
    .bind(other.user_id)
    .execute(db.pool())
    .await
    .expect("the membership is seeded");

    let res = call(
        &app,
        "DELETE",
        "/api/v1/users/me",
        Some(json!({"password": PASSWORD})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::CONFLICT, "{:#}", res.body);

    // Transfer ownership to `other`, then the delete goes through.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/ownership",
        Some(json!({"userId": other.user_id})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "transfer failed: {:#}",
        res.body
    );

    let res = call(
        &app,
        "DELETE",
        "/api/v1/users/me",
        Some(json!({"password": PASSWORD})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{:#}", res.body);
}

// ── the walk ────────────────────────────────────────────────────────────────

/// The full walk: the user row is tombstoned, sessions and keys die,
/// memberships go, the personal organization is tombstoned and suspended, and
/// the account can never log in or act again.
#[tokio::test]
async fn a_delete_tombstones_the_account_and_kills_every_credential() {
    let (app, db) = app_or_skip!();
    let account = register(&app, "walk").await;
    let cookie = login(&app, &account.email).await;
    // A second key minted through the session — `created_by` records the
    // user only when a person mints; a key minted by a key belongs to the
    // organization, not the account.
    let res = call(
        &app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": "second"})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{:#}", res.body);
    let second_key = res.body["data"]["secret"].as_str().unwrap().to_owned();

    let res = call(
        &app,
        "DELETE",
        "/api/v1/users/me",
        Some(json!({"password": PASSWORD})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{:#}", res.body);
    let data = &res.body["data"];
    assert!(data["sessionsRevoked"].as_i64().unwrap() >= 1);
    assert!(data["keysRevoked"].as_i64().unwrap() >= 2);
    assert!(data["membershipsRemoved"].as_i64().unwrap() >= 1);

    // The tombstone row.
    let (email_normalized, password_hash) = user_row(&db, account.user_id).await;
    assert_eq!(
        email_normalized,
        format!("deleted-{}@deleted.invalid", account.user_id)
    );
    assert_eq!(password_hash, "!deleted");

    // The session that asked is dead: it can no longer act.
    let res = call(&app, "GET", "/api/v1/session", None, None, Some(&cookie)).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{:#}", res.body);
    // Both keys are dead.
    for key in [&account.key, &second_key] {
        let res = call(&app, "GET", "/api/v1/balance", None, Some(key), None).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{:#}", res.body);
    }
    // The account cannot log back in: same 401 a wrong password gives.
    let res = call(
        &app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": account.email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{:#}", res.body);

    // The personal organization is tombstoned and suspended.
    let (name, suspended) = org_row(&db, account.organization_id).await;
    assert_eq!(name, format!("deleted-{}", account.organization_id));
    assert!(suspended);

    // Its memberships are gone.
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.memberships WHERE user_id = $1")
            .bind(account.user_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// A member of someone else's team organization deletes: their membership in
/// that org goes, the org itself is untouched, and their own personal org is
/// tombstoned.
#[tokio::test]
async fn a_member_leaves_the_team_org_untouched() {
    let (app, db) = app_or_skip!();
    let owner = register(&app, "team_owner").await;
    let member = register(&app, "team_member").await;
    // The member joins the owner's org.
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role) \
         VALUES ($1, $2, 'member')",
    )
    .bind(owner.organization_id)
    .bind(member.user_id)
    .execute(db.pool())
    .await
    .expect("the membership is seeded");
    let cookie = login(&app, &member.email).await;

    let res = call(
        &app,
        "DELETE",
        "/api/v1/users/me",
        Some(json!({"password": PASSWORD})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{:#}", res.body);
    assert_eq!(
        res.body["data"]["membershipsRemoved"].as_i64().unwrap(),
        2,
        "the personal and the team memberships"
    );

    // The team org is untouched: name unchanged, not suspended, still has its owner.
    let (name, suspended) = org_row(&db, owner.organization_id).await;
    assert_eq!(name, owner.email.split('@').next().unwrap());
    assert!(!suspended);
    let owner_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.memberships WHERE organization_id = $1 AND role = 'owner'",
    )
    .bind(owner.organization_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(owner_count, 1);

    // The member's own personal org is tombstoned.
    let (name, suspended) = org_row(&db, member.organization_id).await;
    assert_eq!(name, format!("deleted-{}", member.organization_id));
    assert!(suspended);
}

/// The attribution the account's keys wrote is pseudonymized — settled usage
/// rows and in-flight watch rows alike — while the settlement entry itself is
/// untouched: the bill still verifies through the ledger's own proof.
#[tokio::test]
async fn the_usage_attribution_is_pseudonymized_and_the_bill_still_verifies() {
    let (app, db) = app_or_skip!();
    let account = register(&app, "attr").await;
    let cookie = login(&app, &account.email).await;

    // The org's tenant id and a funded wallet.
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(account.organization_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let tenants = Tenants::new(db.pool().clone());
    let wallet = tenants.get(&tenant_id).await.expect("the wallet opens");
    wallet
        .top_up("seed-topup", 100_000, today())
        .await
        .expect("the top-up lands");

    // One settled turn under the account's key, attributed to a caller id.
    settle_request(
        &db,
        &tenants,
        &tenant_id,
        account.key_id,
        "req-attr",
        "alice",
    )
    .await;

    // The attribution is there before the delete.
    let (end_user, tags) = sqlx::query_as::<_, (Option<String>, serde_json::Value)>(
        "SELECT end_user, tags FROM oxsum.usage_records WHERE key_id = $1",
    )
    .bind(account.key_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(end_user.as_deref(), Some("alice"));
    assert_eq!(tags["env"], "prod");

    let res = call(
        &app,
        "DELETE",
        "/api/v1/users/me",
        Some(json!({"password": PASSWORD})),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{:#}", res.body);

    // The attribution is pseudonymized; the row itself stays.
    let row = sqlx::query("SELECT end_user, tags FROM oxsum.usage_records WHERE key_id = $1")
        .bind(account.key_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let end_user: Option<String> = row.try_get("end_user").unwrap();
    let tags: serde_json::Value = row.try_get("tags").unwrap();
    assert!(end_user.is_none());
    assert_eq!(tags, serde_json::json!({}));

    // The bill still verifies: the ledger was never touched, so the entry's
    // proof bundle — fetched through the wallet, which suspension does not
    // gate — recomputes and links exactly as it did before. The entry id is
    // derived from the request's hold key the way the gateway's own links do
    // (`settle_request` builds the hold key as `req-<request>:hold`).
    let entry_id = entry_id_for(&settlement_key_for("req-req-attr:hold"));
    let wallet = tenants.get(&tenant_id).await.expect("the ledger opens");
    let bundle = wallet
        .receipt_proof(entry_id, None)
        .await
        .expect("the proof is built")
        .expect("the settlement entry is still there");
    let bundle_json = serde_json::to_string(&bundle).expect("the bundle serializes");
    let content_hash = bundle.entry.content_hash();
    assert!(
        oxsum_core::verify_bundle(&bundle_json, &content_hash).expect("the bundle parses"),
        "the bill still verifies after the account is gone"
    );
}

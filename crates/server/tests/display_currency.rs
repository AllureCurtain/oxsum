//! HTTP tests for the display currency (issue #257): the organization's own
//! `PATCH /api/v1/org`, the operator's admin PATCH carrying the same field,
//! the bills export's converted column — and that none of it reaches a bill's
//! proof, because the conversion is presentational only.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const TOKEN: &str = "operator-token-0123456789";
const PASSWORD: &str = "correct horse battery";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("display_currency")
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

async fn app_for(database_url: &str) -> (Router, PgPool) {
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
    (oxsum_server::app(db, config), pool)
}

/// One response, with the body kept as bytes — the exports are files, not JSON
/// envelopes.
struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    set_cookie: Option<String>,
}

impl Res {
    fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("the body is text")
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }
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
    let headers = response.headers().clone();
    let set_cookie = headers
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
        headers,
        body: bytes.to_vec(),
        set_cookie,
    }
}

/// A registered account: its organization id, its first key's secret, and a
/// session cookie for the member surface.
struct Account {
    organization_id: String,
    email: String,
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
        res.text()
    );
    let body = res.json();
    let key = body["data"]["apiKey"]["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    // Registration returns the account but no session — the cookie comes from
    // the login that follows it.
    let login = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
        None,
    )
    .await;
    assert_eq!(
        login.status,
        StatusCode::OK,
        "login failed: {}",
        login.text()
    );
    let cookie = login
        .set_cookie
        .expect("a login sets the session cookie")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .unwrap()
        .to_owned();
    Account {
        organization_id: body["data"]["organization"]["id"]
            .as_str()
            .unwrap()
            .to_owned(),
        email,
        key,
        cookie,
    }
}

/// The organization's own PATCH: an owner sets the display unit, the org read
/// answers it, and an explicit null clears it.
#[tokio::test]
async fn the_display_currency_round_trips_through_the_org_patch() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "display").await;

    // Unset, the organization renders credits alone.
    let res = call(&app, "GET", "/api/v1/org", None, Some(&account.key), None).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(res.json()["data"]["displayCurrency"].is_null());

    let res = call(
        &app,
        "PATCH",
        "/api/v1/org",
        Some(json!({
            "displayCurrency": {"code": "USD", "ratePerCredit": "0.0014", "decimals": 2}
        })),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["data"]["displayCurrency"]["code"], "USD");
    assert_eq!(
        res.json()["data"]["displayCurrency"]["ratePerCredit"],
        "0.0014"
    );
    assert_eq!(res.json()["data"]["displayCurrency"]["decimals"], 2);

    // The stored row carries it — the org read answers what was written, not
    // what the request claimed.
    let res = call(&app, "GET", "/api/v1/org", None, Some(&account.key), None).await;
    assert_eq!(res.json()["data"]["displayCurrency"]["code"], "USD");

    // An explicit null clears the pair.
    let res = call(
        &app,
        "PATCH",
        "/api/v1/org",
        Some(json!({"displayCurrency": null})),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(res.json()["data"]["displayCurrency"].is_null());
}

/// A member's session and an API key may not set the display: the write is an
/// owner-or-admin action, the same door membership management stands behind.
#[tokio::test]
async fn members_and_keys_cannot_set_the_display() {
    let (app, _pool) = app_or_skip!();
    let owner = register(&app, "owner").await;
    let member = register(&app, "member").await;

    // Add the member account to the owner's organization so its session acts
    // there.
    let add = call(
        &app,
        "POST",
        "/api/v1/org/members",
        Some(json!({"email": member.email})),
        None,
        Some(&owner.cookie),
    )
    .await;
    assert_eq!(add.status, StatusCode::OK, "{}", add.text());

    // Switch the member's session to the shared organization, then PATCH.
    let switch = call(
        &app,
        "POST",
        "/api/v1/session/organization",
        Some(json!({"organizationId": owner.organization_id})),
        None,
        Some(&member.cookie),
    )
    .await;
    assert_eq!(switch.status, StatusCode::OK, "{}", switch.text());
    let res = call(
        &app,
        "PATCH",
        "/api/v1/org",
        Some(json!({"displayCurrency": {"code": "USD", "ratePerCredit": "1", "decimals": 2}})),
        None,
        Some(&member.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());

    // An API key is not a person and may not set it either.
    let res = call(
        &app,
        "PATCH",
        "/api/v1/org",
        Some(json!({"displayCurrency": {"code": "USD", "ratePerCredit": "1", "decimals": 2}})),
        Some(&owner.key),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN, "{}", res.text());
}

/// The operator's admin PATCH takes the same field — billing terms are the
/// admin's page, and the display is one of them.
#[tokio::test]
async fn the_admin_patch_carries_the_display() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "admin-display").await;
    let org = account.organization_id;

    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/admin/organizations/{org}"),
        Some(json!({
            "idempotencyKey": "d-1",
            "displayCurrency": {"code": "EUR", "ratePerCredit": "0.0012", "decimals": 2}
        })),
        Some(TOKEN),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["data"]["displayCurrency"]["code"], "EUR");

    // The org read sees it too — the two surfaces answer the same row.
    let res = call(&app, "GET", "/api/v1/org", None, Some(&account.key), None).await;
    assert_eq!(res.json()["data"]["displayCurrency"]["code"], "EUR");

    // A malformed pair is a validation error, not a stored half-config.
    let res = call(
        &app,
        "PATCH",
        &format!("/api/v1/admin/organizations/{org}"),
        Some(json!({
            "idempotencyKey": "d-2",
            "displayCurrency": {"code": "usd", "ratePerCredit": "x", "decimals": 2}
        })),
        Some(TOKEN),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST, "{}", res.text());
}

/// The exports render the converted figure beside the ledger's own — and a
/// display change never reaches the charge the ledger proves.
#[tokio::test]
async fn the_exports_render_the_display() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "export-display").await;

    // Top up so the export has a row to render.
    let topup = call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"amountMinor": 5_000_000i64, "idempotencyKey": "t-1"})),
        Some(&account.key),
        None,
    )
    .await;
    assert_eq!(topup.status, StatusCode::OK, "{}", topup.text());

    // Set the display through the org PATCH.
    let res = call(
        &app,
        "PATCH",
        "/api/v1/org",
        Some(json!({
            "displayCurrency": {"code": "USD", "ratePerCredit": "0.0014", "decimals": 2}
        })),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    // The CSV export's last two columns are the conversion and its unit:
    // 5 credits at $0.0014/credit renders $0.01.
    let res = call(
        &app,
        "GET",
        "/dashboard/bills/export.csv",
        None,
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.header("content-type"), "text/csv; charset=utf-8");
    let body = res.text();
    assert_eq!(
        body.lines().next().unwrap(),
        "bookedOn,entryId,kind,amountMinor,description,contentHash,displayAmount,displayCurrency"
    );
    let row = body.lines().nth(1).expect("one transaction row");
    assert!(
        row.contains(",0.01,USD"),
        "the row carries the conversion: {row}"
    );
    assert!(
        row.contains(",5000000,"),
        "the ledger figure is unchanged: {row}"
    );

    // The JSON export names the same fields on the row.
    let res = call(
        &app,
        "GET",
        "/dashboard/bills/export.json",
        None,
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = res.json();
    let row = &rows[0];
    assert_eq!(row["amountMinor"], 5_000_000i64);
    assert_eq!(row["displayAmount"], "0.01");
    assert_eq!(row["displayCurrency"], "USD");
}

/// Unset, the exports write the columns empty — a credits-only organization
/// still gets the field list, never a half-filled conversion.
#[tokio::test]
async fn the_exports_carry_empty_display_columns_when_unset() {
    let (app, _pool) = app_or_skip!();
    let account = register(&app, "no-display").await;
    call(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"amountMinor": 1_000_000i64, "idempotencyKey": "t-1"})),
        Some(&account.key),
        None,
    )
    .await;

    let res = call(
        &app,
        "GET",
        "/dashboard/bills/export.json",
        None,
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let rows = res.json();
    assert!(rows[0]["displayAmount"].is_null());
    assert!(rows[0]["displayCurrency"].is_null());
}

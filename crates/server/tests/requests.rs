//! HTTP tests for the requests list (issue #55).
//!
//! What the API promises is one row per settled gateway request — its status, the tokens
//! it used and what it charged (an integer in minor units in the payload), with the date,
//! the request id, the key and the model that name it. These tests seed organizations,
//! write the requests the gateway itself writes (a hold under a key, then the settlement
//! whose record carries the model, the token counts and the charge), and read
//! `GET /api/v1/billing-records` the way the React requests page does.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    ActingKey, BillLine, Db, Settlement, SettlementKind, Tenants, UsageRecord, hold_description,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

/// An app over a real database with oxsum's tables migrated.
async fn online_app(url: &str) -> (Router, Db) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (
        oxsum_server::app(db.clone(), Config::new(Signup::Open, None)),
        db,
    )
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("requests")
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

/// One response, with the body kept as bytes.
struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// One plain HTTP call against the app.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    cookie: Option<&str>,
) -> Res {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    Res {
        status: res.status(),
        headers: res.headers().clone(),
        body: res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

/// A registered organization: the tenant its ledger lives in, the session that reads the
/// list, and the key the organization signed up with.
struct Account {
    tenant_id: String,
    cookie: String,
    key_id: Uuid,
}

/// Registers a fresh organization and logs in; the signup key is the first one it spends
/// with.
async fn account(app: &Router, name: &str) -> Account {
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
    )
    .await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "registration failed: {}",
        res.json()
    );
    let registered = res.json();
    let tenant_id = registered["data"]["organization"]["tenantId"]
        .as_str()
        .expect("registration names a tenant")
        .to_owned();
    let key_id = registered["data"]["apiKey"]["id"]
        .as_str()
        .expect("registration mints a key")
        .parse()
        .expect("the key id is a uuid");
    let res = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "login failed: {}", res.json());
    let cookie = res
        .headers
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .expect("a cookie was set")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned();
    Account {
        tenant_id,
        cookie,
        key_id,
    }
}

/// Mints one more key for the organization: a session mints it, so it belongs to the same
/// organization and may spend.
async fn mint_key(app: &Router, cookie: &str, name: &str) -> (Uuid, String) {
    let res = call(
        app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": name})),
        Some(cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "minting failed: {}", res.json());
    let minted = res.json();
    let id = minted["data"]["id"]
        .as_str()
        .expect("the key has an id")
        .parse()
        .expect("the key id is a uuid");
    let prefix = minted["data"]["prefix"]
        .as_str()
        .expect("the key has a display prefix")
        .to_owned();
    (id, prefix)
}

/// Tops the organization up through its own session: holds need a balance behind them.
async fn top_up(app: &Router, cookie: &str, amount_minor: i64) {
    let res = call(
        app,
        "POST",
        "/api/v1/topups",
        Some(json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "amountMinor": amount_minor,
        })),
        Some(cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "top-up failed: {}", res.json());
}

/// The server's date: the ledger records a booking date and accepts no client-supplied one.
fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}

/// One settled gateway request, written the way the gateway writes it: the hold under the
/// key that pays, then the settlement whose record carries the model, the token counts and
/// the charge. The status is the settlement kind the record names.
#[allow(clippy::too_many_arguments)]
async fn seed_request(
    tenants: &Tenants,
    tenant_id: &str,
    key_id: Uuid,
    request: &str,
    model: &str,
    status: SettlementKind,
    input_tokens: i64,
    output_tokens: i64,
    cost_minor: i64,
    freeze_minor: i64,
) {
    let wallet = tenants
        .get(tenant_id)
        .await
        .expect("the organization's wallet opens");
    let hold_key = format!("req-{request}:hold");
    wallet
        .hold_for_key(
            &ActingKey {
                key_id,
                spend_limit_minor: None,
                requests_per_minute: None,
                tokens_per_minute: None,
                tokens_per_minute_scopes: None,
            },
            Some(model),
            &hold_key,
            &hold_description(request, model, freeze_minor).expect("the hold record serializes"),
            freeze_minor,
            today(),
            None,
        )
        .await
        .expect("the hold is taken");
    let usage = UsageRecord::tokens(input_tokens, output_tokens).expect("the seed counts");
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
        model,
        price_version: 1,
        kind: status,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged: cost_minor,
        freeze: freeze_minor,
        upstream_attempts: 1,
        mandate: None,
        reason: None,
    };
    wallet
        .settle(
            &hold_key,
            &record
                .description()
                .expect("the settlement record serializes"),
            cost_minor,
            today(),
        )
        .await
        .expect("the settlement lands");
}

/// The session's settled-turn rows, the way the React requests page asks for them:
/// `GET /api/v1/billing-records` — then narrowed by key id and model the way the
/// app's own filters narrow the list it already holds.
async fn requests(
    app: &Router,
    cookie: &str,
    key_id: Option<Uuid>,
    model: Option<&str>,
) -> Vec<Value> {
    let res = call(
        app,
        "GET",
        "/api/v1/billing-records?limit=200",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "the requests list answers");
    res.json()["data"]["rows"]
        .as_array()
        .expect("the list shape")
        .iter()
        .filter(|row| {
            key_id.is_none_or(|id| row["keyId"] == id.as_simple().to_string().as_str())
                && model.is_none_or(|m| row["model"] == m)
        })
        .cloned()
        .collect()
}

/// Each request is one row with its status and its cost in minor units, beside the
/// date, the request id, the key and the model that name it, newest first.
#[tokio::test]
async fn the_requests_list_carries_each_requests_status_usage_and_cost() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "requests-rows").await;
    top_up(&app, &account.cookie, 5_000_000).await;
    let (minted_id, _minted_prefix) = mint_key(&app, &account.cookie, "second").await;
    let tenants = Tenants::new(db.pool().clone());
    seed_request(
        &tenants,
        &account.tenant_id,
        account.key_id,
        "req-usage",
        "mock-a",
        SettlementKind::Usage,
        116,
        100,
        316,
        400,
    )
    .await;
    seed_request(
        &tenants,
        &account.tenant_id,
        minted_id,
        "req-swept",
        "mock-b",
        SettlementKind::Swept,
        0,
        0,
        0,
        500,
    )
    .await;

    let rows = requests(&app, &account.cookie, None, None).await;
    assert_eq!(rows.len(), 2, "{rows:?}");

    // Newest first: the swept request was written last.
    let swept = &rows[0];
    assert_eq!(swept["requestId"], "req-swept");
    assert_eq!(swept["bookedOn"], serde_json::to_value(today()).unwrap());
    assert_eq!(swept["model"], "mock-b");
    assert_eq!(swept["keyId"], minted_id.as_simple().to_string().as_str());
    assert_eq!(swept["kind"], "swept", "the record's own word");
    assert_eq!(swept["chargedMinor"], 0, "a swept turn charged nothing");

    let billed = &rows[1];
    assert_eq!(billed["requestId"], "req-usage");
    assert_eq!(billed["bookedOn"], serde_json::to_value(today()).unwrap());
    assert_eq!(billed["model"], "mock-a");
    assert_eq!(
        billed["keyId"],
        account.key_id.as_simple().to_string().as_str()
    );
    assert_eq!(billed["kind"], "usage");
    assert_eq!(
        billed["chargedMinor"], 316,
        "what the settlement charged, in minor units"
    );
}

/// The rows carry the key and the model that name each turn, so the app's filters
/// narrow by either — including the combination that matches nothing, which is an
/// empty list and not an error.
#[tokio::test]
async fn the_request_rows_narrow_by_key_and_by_model() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "requests-filters").await;
    top_up(&app, &account.cookie, 5_000_000).await;
    let (minted_id, _minted_prefix) = mint_key(&app, &account.cookie, "second").await;
    let tenants = Tenants::new(db.pool().clone());
    seed_request(
        &tenants,
        &account.tenant_id,
        account.key_id,
        "req-a",
        "mock-a",
        SettlementKind::Usage,
        10,
        20,
        40,
        100,
    )
    .await;
    seed_request(
        &tenants,
        &account.tenant_id,
        minted_id,
        "req-b",
        "mock-b",
        SettlementKind::Estimated,
        30,
        40,
        80,
        200,
    )
    .await;

    // No filter: both requests.
    assert_eq!(requests(&app, &account.cookie, None, None).await.len(), 2);

    // By key: only the turns that key paid.
    let by_key = requests(&app, &account.cookie, Some(account.key_id), None).await;
    assert_eq!(by_key.len(), 1, "{by_key:?}");
    assert_eq!(by_key[0]["requestId"], "req-a");
    let by_other_key = requests(&app, &account.cookie, Some(minted_id), None).await;
    assert_eq!(by_other_key.len(), 1, "{by_other_key:?}");
    assert_eq!(by_other_key[0]["requestId"], "req-b");

    // By model: only the turns that model priced.
    let by_model = requests(&app, &account.cookie, None, Some("mock-b")).await;
    assert_eq!(by_model.len(), 1, "{by_model:?}");
    assert_eq!(by_model[0]["requestId"], "req-b");
    assert_eq!(by_model[0]["kind"], "estimated");

    // Both together: the one turn that is both.
    let both = requests(&app, &account.cookie, Some(account.key_id), Some("mock-a")).await;
    assert_eq!(both.len(), 1, "{both:?}");
    assert_eq!(both[0]["requestId"], "req-a");

    // A combination that matches nothing: the key pays for one model, not the other.
    let nothing = requests(&app, &account.cookie, Some(account.key_id), Some("mock-b")).await;
    assert!(nothing.is_empty(), "no rows, and no error: {nothing:?}");
}

/// The list needs a session like every other organization read; without one it
/// answers a refusal, not a shell.
#[tokio::test]
async fn the_requests_list_needs_a_session() {
    let (app, _db) = app_or_skip!();
    let res = call(&app, "GET", "/api/v1/billing-records", None, None).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED, "the list refuses");
}

/// One organization never reads another's requests: the rows come out of the organization's
/// own ledger, and the keys that name them are its own.
#[tokio::test]
async fn the_requests_list_shows_only_the_sessions_organization() {
    let (app, db) = app_or_skip!();
    let first = account(&app, "requests-first").await;
    let second = account(&app, "requests-second").await;
    for who in [&first, &second] {
        top_up(&app, &who.cookie, 5_000_000).await;
    }
    let tenants = Tenants::new(db.pool().clone());
    seed_request(
        &tenants,
        &first.tenant_id,
        first.key_id,
        "req-first",
        "mock-a",
        SettlementKind::Usage,
        1,
        2,
        3,
        100,
    )
    .await;
    seed_request(
        &tenants,
        &second.tenant_id,
        second.key_id,
        "req-second",
        "mock-a",
        SettlementKind::Usage,
        4,
        5,
        6,
        100,
    )
    .await;

    let mine = requests(&app, &first.cookie, None, None).await;
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0]["requestId"], "req-first");
    assert_eq!(
        mine[0]["keyId"],
        first.key_id.as_simple().to_string().as_str()
    );
    let theirs = requests(&app, &second.cookie, None, None).await;
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    assert_eq!(theirs[0]["requestId"], "req-second");
}

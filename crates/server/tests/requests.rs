//! HTTP tests for the requests page (issue #55).
//!
//! What the page promises is one row per settled gateway request — its status, the tokens
//! it used and what it charged (an integer in minor units in the payload), with the date, the request id, the key and
//! the model that name it — filterable by key and by model out of the URL, and an empty
//! table with a message when nothing matches. These tests seed organizations, write the
//! requests the gateway itself writes (a hold under a key, then the settlement whose
//! record carries the model, the token counts and the charge), and read the payload the
//! page consumes through its own server function, plus the page as it is rendered.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{ActingKey, Db, Settlement, SettlementKind, Tenants, hold_description};
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

/// One response, with the body kept as bytes: the page is HTML, the server functions JSON.
struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn text(&self) -> String {
        String::from_utf8(self.body.clone()).expect("the body is text")
    }

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
/// page, and the key the organization signed up with.
struct Account {
    tenant_id: String,
    cookie: String,
    key_id: Uuid,
    prefix: String,
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
    let prefix = registered["data"]["apiKey"]["prefix"]
        .as_str()
        .expect("the key has a display prefix")
        .to_owned();
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
        prefix,
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
            },
            &hold_key,
            &hold_description(request, model, freeze_minor).expect("the hold record serializes"),
            freeze_minor,
            today(),
        )
        .await
        .expect("the hold is taken");
    let record = Settlement {
        request,
        channel: "mock",
        model,
        price_version: 1,
        kind: status,
        input_tokens,
        output_tokens,
        input_price: 1_000,
        output_price: 2_000,
        charged: cost_minor,
        freeze: freeze_minor,
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

/// The page's payload, the way the page's own code asks for it: the
/// `/_pages/get_requests` server function with the session cookie, its two filters sent in
/// the URL-encoded body its codec uses. Leptos suffixes the path with a hash of the crate
/// it was declared in, so it is read from the same registry the router registers its
/// server-function routes from.
async fn requests(
    app: &Router,
    cookie: &str,
    key: Option<&str>,
    model: Option<&str>,
) -> Vec<Value> {
    let (path, method) = leptos::server_fn::axum::server_fn_paths()
        .find(|(path, _)| path.starts_with("/_pages/get_requests"))
        .expect("the requests page's server function is registered");
    let mut body = String::new();
    for (name, value) in [("key", key), ("model", model)] {
        let Some(value) = value else { continue };
        if !body.is_empty() {
            body.push('&');
        }
        body.push_str(&format!("{name}={}", encode(value)));
    }
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", format!("oxsum_session={cookie}"))
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "the server function answers");
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<Value>(&bytes).expect("the requests answer JSON")["rows"]
        .as_array()
        .expect("the payload is a page of rows")
        .clone()
}

/// One value of the URL-encoded body the page's codec sends. The prefixes and model names
/// these tests use need no encoding, but a value with a reserved character in it must not
/// split the body it sits in.
fn encode(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('+', "%2B")
        .replace(' ', "+")
}

/// The fields one row carries, pinned: what the page shows, under the names the payload
/// uses. A field added or renamed on either side fails here.
const ROW_FIELDS: [&str; 8] = [
    "bookedOn",
    "costMinor",
    "inputTokens",
    "keyPrefix",
    "model",
    "outputTokens",
    "requestId",
    "status",
];

/// The fields one row carries, sorted, as the tests compare them.
fn fields_of(row: &Value) -> Vec<String> {
    let mut fields: Vec<String> = row
        .as_object()
        .expect("a row is an object")
        .keys()
        .cloned()
        .collect();
    fields.sort_unstable();
    fields
}

/// Each request is one row with its status, its usage and its cost — in minor units, as an
/// integer — beside the date, the request id, the key and the model that name it, newest
/// first.
#[tokio::test]
async fn the_requests_page_lists_each_requests_status_usage_and_cost() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "requests-rows").await;
    top_up(&app, &account.cookie, 5_000_000).await;
    let (minted_id, minted_prefix) = mint_key(&app, &account.cookie, "second").await;
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
    assert_eq!(fields_of(swept), ROW_FIELDS, "{swept}");
    assert_eq!(swept["requestId"], "req-swept");
    assert_eq!(swept["bookedOn"], today().to_string());
    assert_eq!(swept["model"], "mock-b");
    assert_eq!(swept["keyPrefix"], minted_prefix.as_str());
    assert_eq!(swept["status"], "swept", "the record's own word");
    assert_eq!(swept["inputTokens"], 0);
    assert_eq!(swept["outputTokens"], 0);
    assert!(
        swept["costMinor"].is_i64(),
        "money is an integer in minor units, never a formatted amount: {swept}"
    );
    assert_eq!(swept["costMinor"], 0, "a swept turn charged nothing");

    let billed = &rows[1];
    assert_eq!(fields_of(billed), ROW_FIELDS, "{billed}");
    assert_eq!(billed["requestId"], "req-usage");
    assert_eq!(billed["bookedOn"], today().to_string());
    assert_eq!(billed["model"], "mock-a");
    assert_eq!(billed["keyPrefix"], account.prefix.as_str());
    assert_eq!(billed["status"], "usage");
    assert_eq!(billed["inputTokens"], 116, "the tokens the settlement used");
    assert_eq!(billed["outputTokens"], 100);
    assert!(
        billed["costMinor"].is_i64(),
        "money is an integer in minor units: {billed}"
    );
    assert_eq!(billed["costMinor"], 316, "what the settlement charged");
}

/// The filters are the page's URL: one by key, one by model, and both together — including
/// the combination that matches nothing, which is an empty list and not an error.
#[tokio::test]
async fn the_request_filters_narrow_by_key_and_by_model() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "requests-filters").await;
    top_up(&app, &account.cookie, 5_000_000).await;
    let (minted_id, minted_prefix) = mint_key(&app, &account.cookie, "second").await;
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
    let by_key = requests(&app, &account.cookie, Some(&account.prefix), None).await;
    assert_eq!(by_key.len(), 1, "{by_key:?}");
    assert_eq!(by_key[0]["requestId"], "req-a");
    let by_other_key = requests(&app, &account.cookie, Some(&minted_prefix), None).await;
    assert_eq!(by_other_key.len(), 1, "{by_other_key:?}");
    assert_eq!(by_other_key[0]["requestId"], "req-b");

    // By model: only the turns that model priced.
    let by_model = requests(&app, &account.cookie, None, Some("mock-b")).await;
    assert_eq!(by_model.len(), 1, "{by_model:?}");
    assert_eq!(by_model[0]["requestId"], "req-b");
    assert_eq!(by_model[0]["status"], "estimated");

    // Both together: the one turn that is both.
    let both = requests(&app, &account.cookie, Some(&account.prefix), Some("mock-a")).await;
    assert_eq!(both.len(), 1, "{both:?}");
    assert_eq!(both[0]["requestId"], "req-a");

    // A combination that matches nothing: the key pays for one model, not the other.
    let nothing = requests(&app, &account.cookie, Some(&account.prefix), Some("mock-b")).await;
    assert!(nothing.is_empty(), "no rows, and no error: {nothing:?}");
}

/// An empty result is an empty table with a message, not an error: the page renders its
/// filters out of the query string, and says why it has nothing to show.
#[tokio::test]
async fn the_requests_page_renders_its_filters_and_an_empty_table() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "requests-page").await;
    top_up(&app, &account.cookie, 5_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    seed_request(
        &tenants,
        &account.tenant_id,
        account.key_id,
        "req-page",
        "mock-a",
        SettlementKind::Usage,
        116,
        100,
        316,
        400,
    )
    .await;

    // The filter the URL carries is the filter the page reads: the row's cost is rendered
    // as credits, like every other amount in the dashboard, and the key filter fills the
    // form's field.
    let filtered = call(
        &app,
        "GET",
        &format!("/dashboard/requests?key={}", account.prefix),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(filtered.status, StatusCode::OK);
    let html = filtered.text();
    assert!(html.contains("req-page"), "the row is rendered: {html}");
    assert!(
        html.contains(">0.000316<"),
        "the cost is formatted as credits, like every other amount: {html}"
    );
    assert!(
        !html.contains(">316<"),
        "the page never shows the bare integer: {html}"
    );
    assert!(
        html.contains(&format!("value=\"{}\"", account.prefix)),
        "the filter is filled from the URL: {html}"
    );

    // A filter that matches nothing: 200, the table's header, and a message.
    let empty = call(
        &app,
        "GET",
        "/dashboard/requests?key=oxs-nothing&model=no-such-model",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(empty.status, StatusCode::OK);
    let html = empty.text();
    assert!(
        html.contains("No requests match these filters."),
        "an empty result says so: {html}"
    );
    assert!(html.contains("<th"), "the empty table keeps its header");
    assert!(!html.contains("req-page"), "and lists nothing: {html}");

    // The page needs a session like every other dashboard page; without one it renders the
    // shell and carries no records.
    let anonymous = call(&app, "GET", "/dashboard/requests", None, None).await;
    assert_eq!(anonymous.status, StatusCode::OK, "the shell renders");
    assert!(
        !anonymous.text().contains("req-page"),
        "no records without a session"
    );
}

/// One organization never reads another's requests: the rows come out of the organization's
/// own ledger, and the keys that name them are its own.
#[tokio::test]
async fn the_requests_page_shows_only_the_sessions_organization() {
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
    assert_eq!(mine[0]["keyPrefix"], first.prefix.as_str());
    let theirs = requests(&app, &second.cookie, None, None).await;
    assert_eq!(theirs.len(), 1, "{theirs:?}");
    assert_eq!(theirs[0]["requestId"], "req-second");
}

//! HTTP tests for the usage rollup (roadmap P3-3, issue #126).
//!
//! What the API promises is the daily rollup of the organization's settled usage —
//! the `usage_daily` rows `record_usage` maintains beside every usage write — summed
//! per day per `(channel, model, key)` row, scoped by the session's keys the way the
//! bills list scopes its rows. These tests seed organizations, settle requests the
//! way the gateway does (hold, settle, usage row), and read `GET /api/v1/usage`.
//!
//! Needs DATABASE_URL and skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    ActingKey, BillLine, Db, Settlement, SettlementKind, Tenants, UsageRecord, UsageRow,
    entry_id_for, hold_description, settlement_key_for,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
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
    oxsum_testkit::suite_db_url("usage")
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

/// A registered organization: the ids the tests scope with, the session that reads
/// the page, and the key the organization signed up with.
struct Account {
    user_id: Uuid,
    organization_id: Uuid,
    tenant_id: String,
    email: String,
    cookie: String,
    key_id: Uuid,
}

/// Registers a fresh organization and logs in.
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
    Account {
        user_id: registered["data"]["user"]["id"]
            .as_str()
            .expect("registration names a user")
            .parse()
            .expect("the user id is a uuid"),
        organization_id: registered["data"]["organization"]["id"]
            .as_str()
            .expect("registration names an organization")
            .parse()
            .expect("the organization id is a uuid"),
        tenant_id: registered["data"]["organization"]["tenantId"]
            .as_str()
            .expect("registration names a tenant")
            .to_owned(),
        cookie: login(app, &email).await,
        key_id: registered["data"]["apiKey"]["id"]
            .as_str()
            .expect("registration mints a key")
            .parse()
            .expect("the key id is a uuid"),
        email,
    }
}

/// Logs in and returns the session cookie.
///
/// A session is created for the user's *oldest* membership, so for a seeded member
/// this has to run after [`membership`] to act as the seeded organization.
async fn login(app: &Router, email: &str) -> String {
    let res = call(
        app,
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": email, "password": PASSWORD})),
        None,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "login failed: {}", res.json());
    res.header("set-cookie")
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .expect("the session cookie")
        .to_owned()
}

impl Res {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }
}

/// Gives `account` a `member` membership in `organization_id`, an hour before the
/// personal membership signup created, so the account's next login acts as this
/// organization — the members suite's seeding, for the page's scope tests.
async fn membership(pool: &PgPool, organization_id: Uuid, user_id: Uuid) {
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role, created_at) \
         VALUES ($1, $2, 'member', now() - interval '1 hour')",
    )
    .bind(organization_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("seeds a membership");
}

/// Registers a member of `owner`'s organization: a fresh account seeded as a member
/// an hour back, logged in again so its session acts as the organization rather than
/// the personal one signup gave it.
async fn member_of(app: &Router, db: &Db, owner: &Account, name: &str) -> (String, Account) {
    let mut member = account(app, name).await;
    membership(db.pool(), owner.organization_id, member.user_id).await;
    member.cookie = login(app, &member.email).await;
    (member.cookie.clone(), member)
}

/// Mints one more key for the session's organization; a session mint records who
/// minted it, which is what scopes a member's rows to their own keys.
async fn mint_key(app: &Router, cookie: &str, name: &str) -> Uuid {
    let res = call(
        app,
        "POST",
        "/api/v1/org/keys",
        Some(json!({"name": name})),
        Some(cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "minting failed: {}", res.json());
    res.json()["data"]["id"]
        .as_str()
        .expect("the key has an id")
        .parse()
        .expect("the key id is a uuid")
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

/// One settled gateway request written the way the gateway writes it — the hold under
/// the key that pays (or `None` for the shared unattributed row), the settlement, and
/// the usage row `record_usage` rolls into the daily table.
#[allow(clippy::too_many_arguments)]
async fn seed_request(
    db: &Db,
    tenants: &Tenants,
    tenant_id: &str,
    key_id: Option<Uuid>,
    request: &str,
    model: &str,
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
    match key_id {
        Some(key_id) => {
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
                    &hold_description(request, model, freeze_minor)
                        .expect("the hold record serializes"),
                    freeze_minor,
                    today(),
                    None,
                )
                .await
                .expect("the hold is taken");
        }
        None => {
            wallet
                .hold(&hold_key, "seed hold", freeze_minor, today())
                .await
                .expect("the hold is taken");
        }
    }
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
        upstream_model: None,
        price_version: 1,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged: cost_minor,
        freeze: freeze_minor,
        upstream_attempts: 1,
        mandate: None,
        subscription: None,
        cache: None,
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
    db.record_usage(&UsageRow {
        // The usage table deduplicates on request_id globally, so a fixed id a
        // previous run wrote would silently skip this insert.
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: tenant_id.to_owned(),
        key_id,
        mandate_id: None,
        model: model.to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
        status: "settled".to_owned(),
        failure_reason: None,
        usage: usage.clone(),
        counts: oxsum_core::CountsProvenance::Provider {
            input: usage.input_tokens,
            output: usage.output_tokens,
        },
        charged_minor: cost_minor,
        freeze_minor,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    })
    .await
    .expect("the usage row is written");
}

/// The session's usage rows, the way the React usage page asks for them:
/// `GET /api/v1/usage`.
async fn usage(app: &Router, cookie: &str) -> Vec<Value> {
    let res = call(app, "GET", "/api/v1/usage", None, Some(cookie)).await;
    assert_eq!(res.status, StatusCode::OK, "the usage rollup answers");
    res.json()["data"]["days"]
        .as_array()
        .expect("the rollup shape")
        .clone()
}

/// One owner's rollup answers the window, scoped to the organization.
#[tokio::test]
async fn the_usage_page_sums_the_window() {
    let (app, db) = app_or_skip!();
    let owner = account(&app, "usage-rows").await;
    top_up(&app, &owner.cookie, 50_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    seed_request(
        &db,
        &tenants,
        &owner.tenant_id,
        Some(owner.key_id),
        "usage-a",
        "mock-a",
        116,
        100,
        316,
        400,
    )
    .await;
    seed_request(
        &db,
        &tenants,
        &owner.tenant_id,
        Some(owner.key_id),
        "usage-b",
        "mock-a",
        200,
        50,
        300,
        400,
    )
    .await;
    // An unattributed turn — the shared row every credential of the organization
    // reads.
    seed_request(
        &db,
        &tenants,
        &owner.tenant_id,
        None,
        "usage-shared",
        "mock-b",
        10,
        5,
        100,
        400,
    )
    .await;

    // Another organization's usage never shows in this one's window.
    let other = account(&app, "usage-other").await;
    top_up(&app, &other.cookie, 50_000_000).await;
    seed_request(
        &db,
        &tenants,
        &other.tenant_id,
        Some(other.key_id),
        "usage-other",
        "mock-z",
        999,
        999,
        9_999,
        10_000,
    )
    .await;

    let days = usage(&app, &owner.cookie).await;
    // mock-a's two turns roll into one day row: 116 + 200 input tokens,
    // 100 + 50 output tokens, 616 minor charged.
    let mock_a = days
        .iter()
        .find(|row| row["model"] == "mock-a")
        .expect("the mock-a rollup row");
    assert_eq!(
        mock_a["day"],
        serde_json::to_value(today()).unwrap(),
        "today's rollup row"
    );
    assert_eq!(mock_a["inputTokens"], 316, "the input tokens sum");
    assert_eq!(mock_a["outputTokens"], 150, "the output tokens sum");
    assert_eq!(mock_a["chargedMinor"], 616, "the charged total");
    // The unattributed shared turn rolls into its own row too.
    let mock_b = days
        .iter()
        .find(|row| row["model"] == "mock-b")
        .expect("the shared rollup row");
    assert_eq!(mock_b["inputTokens"], 10, "the shared row's input tokens");
    assert!(
        mock_b["keyId"].is_null(),
        "the shared row attributes no key"
    );
    // Another organization's usage never shows in this one's window.
    assert!(
        !days.iter().any(|row| row["model"] == "mock-z"),
        "another organization's usage stays out"
    );
}

/// A member's page is the bills page's rule applied to the rollup: their own keys'
/// usage plus the unattributed shared rows — and never another member's spend.
#[tokio::test]
async fn a_member_reads_own_usage_and_the_shared_rows() {
    let (app, db) = app_or_skip!();
    let owner = account(&app, "usage-owner").await;
    top_up(&app, &owner.cookie, 50_000_000).await;
    let (member_cookie, _member) = member_of(&app, &db, &owner, "usage-member").await;
    // The member's key is minted inside the owner's organization, so a turn it pays
    // is the member's own spend; the owner's key pays for a turn of its own.
    let member_key = mint_key(&app, &member_cookie, "member").await;
    let tenants = Tenants::new(db.pool().clone());
    seed_request(
        &db,
        &tenants,
        &owner.tenant_id,
        Some(member_key),
        "usage-member",
        "mock-member",
        100,
        50,
        200,
        400,
    )
    .await;
    seed_request(
        &db,
        &tenants,
        &owner.tenant_id,
        Some(owner.key_id),
        "usage-owner",
        "mock-owner",
        900,
        800,
        9_000,
        10_000,
    )
    .await;
    seed_request(
        &db,
        &tenants,
        &owner.tenant_id,
        None,
        "usage-shared",
        "mock-shared",
        10,
        5,
        100,
        400,
    )
    .await;

    let days = usage(&app, &member_cookie).await;
    // The member's own turn rolls up: 100 input tokens, 200 minor charged.
    let own = days
        .iter()
        .find(|row| row["model"] == "mock-member")
        .expect("the member's own usage");
    assert_eq!(own["inputTokens"], 100, "the member's own usage rolls up");
    assert_eq!(own["chargedMinor"], 200, "the member's own charge");
    // The shared unattributed turn rolls up too.
    let shared = days
        .iter()
        .find(|row| row["model"] == "mock-shared")
        .expect("the shared unattributed usage");
    assert_eq!(shared["inputTokens"], 10, "the shared unattributed usage");
    // The owner's spend stays out of the member's rollup.
    assert!(
        !days.iter().any(|row| row["model"] == "mock-owner"),
        "the owner's spend stays out"
    );
}

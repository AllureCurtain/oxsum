//! HTTP tests: the end-user signed usage reports (issue #261).
//!
//! `POST /api/v1/org/usage-reports` mints a self-contained bundle — the
//! settlement entries attributed to one `endUser` over an ended span, the
//! marker committed once into the organization's ledger, and a signed head —
//! which `oxsum_verify::verify_usage_report` checks offline. These tests seed
//! settled usage the way the gateway writes it, mint through the API, and run
//! the real verifier on the answer: a minted report verifies, a re-issue
//! replays the standing marker, an open or empty span is refused, and a report
//! answers only for the organization that minted it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    BillLine, Db, Settlement, SettlementKind, Tenants, UsageRecord, UsageRow, entry_id_for,
    settlement_key_for,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
/// The deployment's head-signing seed, so the bundle carries the signature
/// fields the verifier's last check runs.
const HEAD_SEED: [u8; 32] = [7; 32];

/// An app over a real database with oxsum's tables migrated, signing heads.
async fn online_app(url: &str) -> (Router, Db) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (
        oxsum_server::app(
            db.clone(),
            Config::new(Signup::Open, None).with_head_signing_seed(HEAD_SEED),
        ),
        db,
    )
}

/// The DATABASE_URL the tests need, or None to skip.
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("usage_reports")
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
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
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

/// A registered organization: the session that calls and the tenant the ledger
/// is under.
struct Account {
    tenant_id: String,
    cookie: String,
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
        tenant_id: registered["data"]["organization"]["tenantId"]
            .as_str()
            .expect("registration names a tenant")
            .to_owned(),
        cookie: login(app, &email).await,
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

/// One settled gateway request attributed to `end_user`, written the way the
/// gateway writes it — the hold, the settlement, and the usage row. `on` is
/// the booking date: a report's span must end before today, so the seeded turn
/// books into the past the report then covers.
#[allow(clippy::too_many_arguments)]
async fn seed_request(
    db: &Db,
    tenants: &Tenants,
    tenant_id: &str,
    request: &str,
    end_user: &str,
    cost_minor: i64,
    freeze_minor: i64,
    on: time::Date,
) {
    let wallet = tenants
        .get(tenant_id)
        .await
        .expect("the organization's wallet opens");
    let hold_key = format!("req-{request}:hold");
    wallet
        .hold(&hold_key, "seed hold", freeze_minor, on)
        .await
        .expect("the hold is taken");
    let mut usage = UsageRecord::tokens(100, 50).expect("the seed counts");
    usage.end_user = Some(end_user.to_owned());
    let record = Settlement {
        request,
        channel: "mock",
        model: "mock-model",
        price_version: 1,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &[BillLine {
            item: "input".to_owned(),
            units: usage.input_tokens,
            price_per_m: 1_000,
        }],
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
            on,
        )
        .await
        .expect("the settlement lands");
    db.record_usage(&UsageRow {
        request_id: format!("{request}-{}", Uuid::new_v4().simple()),
        tenant_id: tenant_id.to_owned(),
        key_id: None,
        mandate_id: None,
        model: "mock-model".to_owned(),
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

/// `POST` a usage report; returns the raw response so refusals stay inspectable.
async fn mint(app: &Router, cookie: &str, end_user: &str, from: &str, to: &str) -> Res {
    call(
        app,
        "POST",
        "/api/v1/org/usage-reports",
        Some(json!({"endUser": end_user, "from": from, "to": to})),
        Some(cookie),
    )
    .await
}

/// The verifier's published key, rebuilt from the fields the bundle itself
/// serves — the same reconstruction `GET /api/v1/log/key` would answer.
fn published_key(report: &Value) -> oxsum_verify::PublishedKey<'_> {
    oxsum_verify::PublishedKey {
        key_name: report["keyName"]
            .as_str()
            .expect("a signing deployment names a key"),
        public_key: report["publicKey"]
            .as_str()
            .expect("a signing deployment serves a public key"),
        key_hash: report["keyHash"]
            .as_str()
            .expect("a signing deployment serves a key hash"),
    }
}

/// The report's origin: the tenant's ledger origin the head note names.
fn origin_of(account: &Account) -> String {
    format!("oxsum/ledgers/{}", account.tenant_id)
}

/// Two requests, one `end_user`, settle over yesterday — a span that has fully
/// ended — and the minted report verifies and replays.
#[tokio::test]
async fn a_minted_usage_report_verifies_and_replays() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "report").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let end_user = "customer-1";
    // The span must be fully ended — `to` is yesterday — so the turns book into
    // the day the report then covers.
    let to = today() - time::Duration::days(1);
    for request in ["req-a", "req-b"] {
        seed_request(
            &db,
            &tenants,
            &account.tenant_id,
            request,
            end_user,
            300,
            400,
            to,
        )
        .await;
    }

    let span = (to.to_string(), to.to_string());
    let res = mint(&app, &account.cookie, end_user, &span.0, &span.1).await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the mint failed: {}",
        res.json()
    );
    let report = res.json()["data"].clone();

    // The descriptor: two settlements for `customer-1`, the folded subset digest.
    assert_eq!(report["endUser"].as_str(), Some(end_user));
    assert_eq!(report["entryCount"].as_i64(), Some(2));
    assert_eq!(report["chargedTotalMinor"].as_i64(), Some(600));
    assert_eq!(report["entries"].as_array().map(Vec::len), Some(2));
    assert!(report["marker"].is_object(), "the marker is served");
    assert!(
        report["note"].is_string(),
        "a signing deployment signs the head"
    );

    // The whole bundle verifies offline: membership, the committed digest, the
    // marker's binding, and the head's signature under the served key.
    let json = serde_json::to_string(&report).unwrap();
    oxsum_verify::verify_usage_report(
        &json,
        &origin_of(&account),
        end_user,
        Some(&published_key(&report)),
    )
    .expect("the minted report verifies");

    // Re-issuing the same span replays the standing marker — the once-per-span
    // rule the completeness claim rests on.
    let replay = mint(&app, &account.cookie, end_user, &span.0, &span.1).await;
    assert_eq!(replay.status, StatusCode::OK);
    let replayed = replay.json();
    assert_eq!(
        replayed["data"]["reportId"].as_str(),
        report["reportId"].as_str(),
        "a re-issue replays the standing marker"
    );

    // The frozen bundle re-serves identically: the report cannot grow to absorb
    // usage the org omitted the first time.
    let id = report["reportId"].as_str().unwrap();
    let fetched = call(
        &app,
        "GET",
        &format!("/api/v1/org/usage-reports/{id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(fetched.status, StatusCode::OK);
    let bundle = fetched.json()["data"].clone();
    oxsum_verify::verify_usage_report(
        &serde_json::to_string(&bundle).unwrap(),
        &origin_of(&account),
        end_user,
        Some(&published_key(&bundle)),
    )
    .expect("the re-fetched report verifies");
    assert_eq!(
        bundle["subsetHash"].as_str(),
        report["subsetHash"].as_str(),
        "the frozen subset digest is stable"
    );
}

/// A span whose `to` has not fully ended is refused — the marker attests a
/// complete subset, and an open day can still book usage the report would omit.
#[tokio::test]
async fn an_open_span_is_refused() {
    let (app, _db) = app_or_skip!();
    let account = account(&app, "open").await;
    let today = today().to_string();
    let res = mint(&app, &account.cookie, "customer-1", "2026-01-01", &today).await;
    assert_eq!(
        res.status,
        StatusCode::BAD_REQUEST,
        "an open `to` is refused: {}",
        res.json()
    );
}

/// A report over nothing is refused — an empty subset commits nothing.
#[tokio::test]
async fn an_empty_subset_is_refused() {
    let (app, _db) = app_or_skip!();
    let account = account(&app, "empty").await;
    let to = (today() - time::Duration::days(1)).to_string();
    let res = mint(&app, &account.cookie, "never-heard-of-them", &to, &to).await;
    assert_eq!(
        res.status,
        StatusCode::BAD_REQUEST,
        "an endUser with no usage is refused: {}",
        res.json()
    );
}

/// A report answers only for the organization that minted it — another org's
/// id probes nothing.
#[tokio::test]
async fn a_report_answers_only_for_its_organization() {
    let (app, db) = app_or_skip!();
    let owner = account(&app, "owner").await;
    let other = account(&app, "other").await;
    top_up(&app, &owner.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let end_user = "customer-1";
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &owner.tenant_id,
        "req-a",
        end_user,
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let res = mint(&app, &owner.cookie, end_user, &to, &to).await;
    assert_eq!(res.status, StatusCode::OK);
    let minted = res.json();
    let id = minted["data"]["reportId"].as_str().unwrap();

    let probe = call(
        &app,
        "GET",
        &format!("/api/v1/org/usage-reports/{id}"),
        None,
        Some(&other.cookie),
    )
    .await;
    assert_eq!(
        probe.status,
        StatusCode::NOT_FOUND,
        "a report answers only for its organization"
    );
}

/// The list endpoint answers the minted reports' descriptors, newest first.
#[tokio::test]
async fn the_report_list_names_minted_descriptors() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "list").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "req-a",
        "customer-1",
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let minted = mint(&app, &account.cookie, "customer-1", &to, &to).await;
    let id = minted.json()["data"]["reportId"]
        .as_str()
        .unwrap()
        .to_owned();

    let res = call(
        &app,
        "GET",
        "/api/v1/org/usage-reports",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let list = res.json();
    let rows = list["data"]["rows"].as_array().expect("rows is a list");
    assert!(
        rows.iter().any(|row| row["reportId"].as_str() == Some(&id)),
        "the list names the minted report: {list}"
    );
    // The list carries the descriptor alone — no rows or proofs.
    let row = rows
        .iter()
        .find(|row| row["reportId"].as_str() == Some(&id))
        .unwrap();
    assert_eq!(row["endUser"].as_str(), Some("customer-1"));
    assert_eq!(row["entryCount"].as_i64(), Some(1));
}

/// A bundle tampered after mint does not verify — the subset the report shows
/// must fold to the digest the marker committed.
#[tokio::test]
async fn a_tampered_bundle_does_not_verify() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "tamper").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let end_user = "customer-1";
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "req-a",
        end_user,
        300,
        400,
        to,
    )
    .await;

    let to = to.to_string();
    let res = mint(&app, &account.cookie, end_user, &to, &to).await;
    let mut report = res.json()["data"].clone();

    // Swap one shown entry's recorded hash for another row's: the fold can no
    // longer reach the committed subset digest, whatever the proofs say.
    let marker_hash = report["marker"]["contentHash"].as_str().unwrap().to_owned();
    let entries = report["entries"].as_array_mut().expect("entries is a list");
    entries[0]["contentHash"] = Value::String(marker_hash);

    let outcome = oxsum_verify::verify_usage_report(
        &serde_json::to_string(&report).unwrap(),
        &origin_of(&account),
        end_user,
        Some(&published_key(&report)),
    );
    assert!(
        outcome.is_err(),
        "a tampered bundle must not verify: {outcome:?}"
    );
}

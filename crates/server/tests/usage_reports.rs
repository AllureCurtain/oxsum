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

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    BillLine, Db, Settlement, SettlementKind, Tenants, UsageRecord, UsageRow, entry_id_for,
    settlement_key_for,
};
use oxsum_server::{Config, Mailer, Signup};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
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
            Config::new(Signup::Open, None)
                .with_head_signing_seed(HEAD_SEED)
                // The bill page embeds the pinned set and the notes for the
                // head (issue #328); one pin is enough for the tests, and its
                // public half is never used to verify here.
                .with_witness_keys(vec![oxsum_server::WitnessKey {
                    witness_url: "https://witness.test/".to_owned(),
                    key_name: "test-witness".to_owned(),
                    public_key: "11".repeat(32),
                    key_hash: "22222222".to_owned(),
                }]),
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
    organization_id: String,
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
        organization_id: registered["data"]["organization"]["id"]
            .as_str()
            .expect("registration names an organization")
            .to_owned(),
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
        requested_model: "mock-model",
        served_model: "mock-model",
        upstream_model: None,
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
        subscription: None,
        cache: None,
        guardrail: Vec::new(),
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

/// Reads an escaped `name="…"` attribute back out of the rendered page — the
/// same decoding a browser does — so the embedded bundle can be verified.
fn attribute(body: &str, name: &str) -> String {
    let needle = format!("{name}=\"");
    let start = body.find(&needle).expect("the attribute is present") + needle.len();
    let rest = &body[start..];
    let end = rest.find('"').expect("the attribute closes");
    rest[..end]
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
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

/// The anchored-day completeness proof (issue #339): a report over an anchored
/// span carries the customer's day-log head, its anchor-map inclusion and every
/// day leaf; the verifier rebuilds each day's leaf from the shown rows, so a
/// dropped day or a row assigned to the wrong day fails as `Completeness` —
/// not as an empty day.
#[tokio::test]
async fn an_anchored_report_proves_its_completeness() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "complete").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let end_user = "complete-customer";
    // A fixed past span no other test settles on: the anchor pass visits every
    // organization, so sharing a day with the tests running beside this one
    // would seal their settlements.
    let to = time::macros::date!(2026 - 09 - 17);
    let from = to - time::Duration::days(2);

    // One turn on `from`, two on `to` — two day leaves in the customer's log.
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "c1",
        end_user,
        100,
        200,
        from,
    )
    .await;
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "c2",
        end_user,
        300,
        400,
        to,
    )
    .await;
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "c3",
        end_user,
        300,
        400,
        to,
    )
    .await;

    // The daily anchor closes `to`.
    let anchored = db.anchor_usage_days(to).await.expect("the pass anchors");
    assert!(
        anchored.anchors >= 1,
        "the test organization is anchored (the pass also sees other suites' orgs)"
    );

    let span = (from.to_string(), to.to_string());
    let res = mint(&app, &account.cookie, end_user, &span.0, &span.1).await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "the mint failed: {}",
        res.json()
    );
    let report = res.json()["data"].clone();

    let completeness = report
        .get("completeness")
        .expect("the bundle carries the completeness proof");
    assert_eq!(completeness["v"].as_i64(), Some(1));
    assert_eq!(
        completeness["anchor"]["day"].as_str(),
        Some(to.to_string().as_str()),
        "the anchor is the day the span ends on"
    );
    assert_eq!(
        completeness["head"]["size"].as_i64(),
        Some(2),
        "two day leaves"
    );
    let days = completeness["days"]
        .as_array()
        .expect("the proof lists the leaves");
    assert_eq!(days.len(), 2);
    assert_eq!(days[0]["day"].as_str(), Some(from.to_string().as_str()));
    assert_eq!(days[0]["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(days[1]["rows"].as_array().map(Vec::len), Some(2));

    let json = serde_json::to_string(&report).unwrap();
    oxsum_verify::verify_usage_report(
        &json,
        &origin_of(&account),
        end_user,
        Some(&published_key(&report)),
    )
    .expect("the anchored report verifies");

    // A dropped day: the head's indices no longer cover 0..size.
    let mut dropped: Value = serde_json::from_str(&json).unwrap();
    dropped["completeness"]["days"]
        .as_array_mut()
        .unwrap()
        .pop();
    let err = oxsum_verify::verify_usage_report(
        &serde_json::to_string(&dropped).unwrap(),
        &origin_of(&account),
        end_user,
        None,
    )
    .expect_err("a dropped day must fail");
    assert!(
        matches!(err, oxsum_verify::UsageReportError::Completeness(_)),
        "{err:?}"
    );

    // A row moved to the wrong day: the entries (and the subset digest) are
    // unchanged, but the day's leaf no longer rebuilds from its rows.
    let mut moved: Value = serde_json::from_str(&json).unwrap();
    let first = moved["completeness"]["days"][0]["rows"][0].clone();
    moved["completeness"]["days"][1]["rows"]
        .as_array_mut()
        .unwrap()
        .push(first);
    moved["completeness"]["days"][0]["rows"] = json!([]);
    let err = oxsum_verify::verify_usage_report(
        &serde_json::to_string(&moved).unwrap(),
        &origin_of(&account),
        end_user,
        None,
    )
    .expect_err("a misattributed row must fail");
    assert!(
        matches!(err, oxsum_verify::UsageReportError::Completeness(_)),
        "{err:?}"
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

/// A report link is the customer's only credential: the token is answered once,
/// resolves to the report without a session, and never appears in the list.
#[tokio::test]
async fn a_report_link_resolves_and_is_listed_without_its_token() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "link").await;
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
    let report = mint(&app, &account.cookie, end_user, &to, &to).await.json()["data"].clone();
    let report_id = report["reportId"].as_str().expect("a report id").to_owned();

    let created = call(
        &app,
        "POST",
        &format!("/api/v1/org/usage-reports/{report_id}/links"),
        Some(json!({"label": "August bill"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "the mint failed: {}",
        created.json()
    );
    let data = created.json()["data"].clone();
    let token = data["token"].as_str().expect("the token is answered once");
    assert!(token.starts_with("oxb-"), "the mark identifies the token");
    assert_eq!(data["link"]["scope"].as_str(), Some("report"));
    assert_eq!(data["link"]["reportId"].as_str(), Some(report_id.as_str()));
    assert_eq!(data["link"]["endUser"].as_str(), Some(end_user));
    assert_eq!(data["link"]["label"].as_str(), Some("August bill"));
    assert_eq!(data["link"]["revokedAt"], Value::Null);

    // The token alone loads the bill: the page has no session to spend.
    let resolved = db
        .resolve_report_link(token)
        .await
        .expect("the lookup runs")
        .expect("the link resolves");
    assert_eq!(resolved.tenant_id, account.tenant_id);
    assert_eq!(resolved.end_user, end_user);
    assert_eq!(
        resolved.report_id,
        Some(Uuid::parse_str(&report_id).expect("the id parses"))
    );

    // Resolution touched `lastUsedAt`; the list serves the descriptor, not the secret.
    let listed = call(
        &app,
        "GET",
        "/api/v1/org/usage-reports/links",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK);
    let rows = listed.json()["data"]["rows"].clone();
    let row = rows
        .as_array()
        .expect("rows is a list")
        .iter()
        .find(|row| row["linkId"] == data["link"]["linkId"])
        .expect("the minted link is listed")
        .clone();
    assert!(row["lastUsedAt"].is_string(), "resolution is recorded");
    assert!(
        row.get("token").is_none(),
        "the list never echoes the token"
    );
}

/// Revocation and expiry both stop a link, and revocation is idempotent.
#[tokio::test]
async fn a_revoked_or_expired_link_resolves_to_nothing() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "revoke").await;
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
    let report = mint(&app, &account.cookie, end_user, &to, &to).await.json()["data"].clone();
    let report_id = report["reportId"].as_str().expect("a report id").to_owned();

    let created = call(
        &app,
        "POST",
        &format!("/api/v1/org/usage-reports/{report_id}/links"),
        Some(json!({})),
        Some(&account.cookie),
    )
    .await;
    let data = created.json()["data"].clone();
    let link_id = data["link"]["linkId"]
        .as_str()
        .expect("a link id")
        .to_owned();
    let token = data["token"].as_str().expect("a token").to_owned();

    let revoked = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/usage-reports/links/{link_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.json());
    assert!(revoked.json()["data"]["revokedAt"].is_string());
    assert!(
        db.resolve_report_link(&token)
            .await
            .expect("the lookup runs")
            .is_none(),
        "a revoked link resolves to nothing"
    );

    // Deleting again is the same answer, not a conflict.
    let again = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/usage-reports/links/{link_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK);

    // An expired link is indistinguishable from a revoked one.
    let second = call(
        &app,
        "POST",
        &format!("/api/v1/org/usage-reports/{report_id}/links"),
        Some(json!({})),
        Some(&account.cookie),
    )
    .await
    .json()["data"]
        .clone();
    let second_id = Uuid::parse_str(second["link"]["linkId"].as_str().expect("a link id"))
        .expect("the id parses");
    sqlx::query(
        "UPDATE oxsum.report_links SET expires_at = now() - interval '1 second' WHERE link_id = $1",
    )
    .bind(second_id)
    .execute(db.pool())
    .await
    .expect("the expiry moves into the past");
    assert!(
        db.resolve_report_link(second["token"].as_str().expect("a token"))
            .await
            .expect("the lookup runs")
            .is_none(),
        "an expired link resolves to nothing"
    );
    assert!(
        db.resolve_report_link("oxb-unknown")
            .await
            .expect("the lookup runs")
            .is_none(),
        "an unknown token resolves to nothing"
    );
}

/// The end-user scope lists a customer's reports; the bounds and the
/// cross-organization rule refuse the rest.
#[tokio::test]
async fn an_end_user_link_names_the_customer_and_validation_refuses_the_rest() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "portal").await;
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
    let report = mint(&app, &account.cookie, end_user, &to, &to).await.json()["data"].clone();
    let report_id = report["reportId"].as_str().expect("a report id").to_owned();

    let created = call(
        &app,
        "POST",
        "/api/v1/org/usage-reports/links",
        Some(json!({"endUser": end_user, "label": "customer portal"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "the mint failed: {}",
        created.json()
    );
    let data = created.json()["data"].clone();
    assert_eq!(data["link"]["scope"].as_str(), Some("end_user"));
    assert_eq!(data["link"]["reportId"], Value::Null);
    let resolved = db
        .resolve_report_link(data["token"].as_str().expect("a token"))
        .await
        .expect("the lookup runs")
        .expect("the link resolves");
    assert_eq!(resolved.scope, oxsum_core::LinkScope::EndUser);
    assert_eq!(resolved.report_id, None);

    // A customer nothing was billed to, an expiry past 90 days and an oversized
    // label are all refused as caller mistakes.
    let unknown = call(
        &app,
        "POST",
        "/api/v1/org/usage-reports/links",
        Some(json!({"endUser": "nobody"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let too_far = call(
        &app,
        "POST",
        "/api/v1/org/usage-reports/links",
        Some(json!({"endUser": end_user, "expiresAt": "2099-01-01T00:00:00Z"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(too_far.status, StatusCode::BAD_REQUEST);
    let long_label = call(
        &app,
        "POST",
        "/api/v1/org/usage-reports/links",
        Some(json!({"endUser": end_user, "label": "x".repeat(129)})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(long_label.status, StatusCode::BAD_REQUEST);

    // Another organization cannot mint or revoke a link for this report.
    let other = self::account(&app, "other").await;
    let foreign = call(
        &app,
        "POST",
        &format!("/api/v1/org/usage-reports/{report_id}/links"),
        Some(json!({})),
        Some(&other.cookie),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    let foreign_revoke = call(
        &app,
        "DELETE",
        &format!(
            "/api/v1/org/usage-reports/links/{}",
            data["link"]["linkId"].as_str().expect("a link id")
        ),
        None,
        Some(&other.cookie),
    )
    .await;
    assert_eq!(foreign_revoke.status, StatusCode::NOT_FOUND);
}

/// The public bill page: the token alone opens it, the frozen figures render,
/// and the security headers ride along. Revoking the link closes the page.
#[tokio::test]
async fn a_bill_link_renders_the_frozen_report_with_its_headers() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "bill").await;
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
    let report = mint(&app, &account.cookie, end_user, &to, &to).await.json()["data"].clone();
    let report_id = report["reportId"].as_str().expect("a report id").to_owned();

    let created = call(
        &app,
        "POST",
        &format!("/api/v1/org/usage-reports/{report_id}/links"),
        Some(json!({})),
        Some(&account.cookie),
    )
    .await;
    let token = created.json()["data"]["token"]
        .as_str()
        .expect("a token")
        .to_owned();

    // A cosignature for exactly this head, as the witness pass would have
    // stored it; the page embeds it for the browser's check (issue #328).
    sqlx::query(
        "INSERT INTO oxsum.witness_cosignatures (organization_id, witness_url, head_size, note) \
         VALUES ($1::uuid, $2, $3, $4)",
    )
    .bind(&account.organization_id)
    .bind("https://witness.test/")
    .bind(report["head"]["size"].as_i64().expect("a head size"))
    .bind("the cosigned note")
    .execute(db.pool())
    .await
    .expect("the cosignature row lands");

    let page = call(&app, "GET", &format!("/bill/{token}"), None, None).await;
    assert_eq!(page.status, StatusCode::OK, "the page did not render");
    let body = String::from_utf8(page.body.clone()).expect("the page is UTF-8");
    assert!(body.contains(end_user), "the bill names the customer");
    assert!(body.contains("1 entry"), "the frozen count renders");
    assert!(body.contains("0.000300"), "the frozen charge renders");
    assert!(
        body.contains("Operator key: ") && !body.contains("not published"),
        "a signing deployment publishes its key on the page"
    );
    assert_eq!(page.header("referrer-policy"), "no-referrer");
    assert_eq!(page.header("cache-control"), "private, no-store");
    assert_eq!(page.header("x-robots-tag"), "noindex");

    // The page ships exactly one script — its own — and embeds the bundle the
    // browser verifier runs on. That bundle must verify offline, or the button
    // would check bytes the page never showed.
    assert!(
        body.contains("src=\"/bill.js\""),
        "the page loads its own verifier script"
    );
    assert_eq!(
        body.matches("<script").count(),
        1,
        "the page carries no third-party script"
    );
    assert!(body.contains("Verify this bill"), "the button is rendered");
    let embedded = attribute(&body, "data-bundle");
    let bundle: Value = serde_json::from_str(&embedded).expect("the embedded bundle parses");
    oxsum_verify::verify_usage_report(
        &embedded,
        &origin_of(&account),
        end_user,
        Some(&published_key(&bundle)),
    )
    .expect("the embedded bundle verifies offline");

    // The bundle names the pinned witness set, the log the cosignatures must
    // cover and the note stored for this head — the three pieces bill.js feeds
    // to verifyWitnesses.
    assert_eq!(
        bundle["witnessOrigin"].as_str().expect("a witness origin"),
        origin_of(&account)
    );
    assert_eq!(
        bundle["witnessKeys"][0]["witnessUrl"]
            .as_str()
            .expect("the pinned witness"),
        "https://witness.test/"
    );
    assert_eq!(
        bundle["witnessNotes"][0]["note"]
            .as_str()
            .expect("the stored cosignature"),
        "the cosigned note"
    );

    // Unknown, revoked and expired tokens are the same 404.
    let unknown = call(&app, "GET", "/bill/oxb-unknown", None, None).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    let link_id = created.json()["data"]["link"]["linkId"]
        .as_str()
        .expect("a link id")
        .to_owned();
    let revoked = call(
        &app,
        "DELETE",
        &format!("/api/v1/org/usage-reports/links/{link_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK);
    let closed = call(&app, "GET", &format!("/bill/{token}"), None, None).await;
    assert_eq!(closed.status, StatusCode::NOT_FOUND);
}

/// The customer-scoped link lists every report issued to that customer, one
/// section per report, without any per-report token.
#[tokio::test]
async fn an_end_user_bill_link_lists_every_report() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "portal-page").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let end_user = "customer-1";
    let older = today() - time::Duration::days(2);
    let newer = today() - time::Duration::days(1);
    for (request, day, charge) in [("req-a", older, 300), ("req-b", newer, 400)] {
        seed_request(
            &db,
            &tenants,
            &account.tenant_id,
            request,
            end_user,
            charge,
            500,
            day,
        )
        .await;
        let day = day.to_string();
        let minted = mint(&app, &account.cookie, end_user, &day, &day).await;
        assert_eq!(minted.status, StatusCode::OK, "{}", minted.json());
    }

    let created = call(
        &app,
        "POST",
        "/api/v1/org/usage-reports/links",
        Some(json!({"endUser": end_user})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.json());
    let token = created.json()["data"]["token"]
        .as_str()
        .expect("a token")
        .to_owned();

    let page = call(&app, "GET", &format!("/bill/{token}"), None, None).await;
    assert_eq!(page.status, StatusCode::OK);
    let body = String::from_utf8(page.body.clone()).expect("the page is UTF-8");
    assert!(body.contains(end_user));
    assert!(body.contains("0.000300"), "the older report renders");
    assert!(body.contains("0.000400"), "the newer report renders");
    assert_eq!(
        body.matches("<section class=\"bill\">").count(),
        2,
        "one section per issued report"
    );
}

/// A minimal SMTP stub: one connection at a time, the verbs lettre sends, and
/// the DATA body handed back to the test. The bill-mail test needs to see what
/// actually went out — the database keeps only the token's hash.
struct SmtpStub {
    url: String,
    mails: mpsc::Receiver<String>,
}

async fn smtp_stub() -> SmtpStub {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds a stub SMTP");
    let port = listener.local_addr().expect("an address").port();
    let (tx, mails) = mpsc::channel(8);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                if write.write_all(b"220 stub ESMTP\r\n").await.is_err() {
                    return;
                }
                let mut reader = BufReader::new(read);
                let mut data = String::new();
                let mut in_data = false;
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if in_data {
                        if line.trim_end_matches(['\r', '\n']) == "." {
                            let _ = tx.send(std::mem::take(&mut data)).await;
                            in_data = false;
                            if write.write_all(b"250 queued\r\n").await.is_err() {
                                return;
                            }
                        } else {
                            data.push_str(&line);
                        }
                        continue;
                    }
                    let verb = line
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase();
                    let reply = match verb.as_str() {
                        "EHLO" | "HELO" => "250 stub\r\n",
                        "MAIL" | "RCPT" | "RSET" | "NOOP" => "250 ok\r\n",
                        "DATA" => {
                            in_data = true;
                            "354 go ahead\r\n"
                        }
                        "QUIT" => {
                            let _ = write.write_all(b"221 bye\r\n").await;
                            return;
                        }
                        _ => "502 not implemented\r\n",
                    };
                    if write.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    SmtpStub {
        url: format!("smtp://127.0.0.1:{port}"),
        mails,
    }
}

/// The next mail the stub captured, unwrapped from its quoted-printable
/// soft breaks (`=\r\n`) and `=3D` escapes.
async fn next_mail(stub: &mut SmtpStub) -> String {
    let raw = tokio::time::timeout(Duration::from_secs(10), stub.mails.recv())
        .await
        .expect("a mail arrives")
        .expect("the stub keeps running");
    raw.replace("=\r\n", "")
        .replace("=\n", "")
        .replace("=3D", "=")
}

/// An app over a real database with its mailer pointed at a stub.
async fn mailing_app(url: &str) -> (Router, Db, SmtpStub) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let stub = smtp_stub().await;
    let mailer = Mailer::new(
        &stub.url,
        "oxsum <noreply@oxsum.test>",
        "http://oxsum.test/",
    )
    .expect("a mailer over the stub");
    let config = Config::new(Signup::Open, None)
        .with_head_signing_seed(HEAD_SEED)
        .with_mailer(mailer);
    (oxsum_server::app(db.clone(), config), db, stub)
}

/// The next mail that carries `needle` — registration already sends a
/// verification mail through the same stub, so the bill mail is not necessarily
/// the first one.
async fn mail_with(stub: &mut SmtpStub, needle: &str) -> String {
    for _ in 0..5 {
        let mail = next_mail(stub).await;
        if mail.contains(needle) {
            return mail;
        }
    }
    panic!("no captured mail carried {needle}");
}

/// A link minted with an `email` mails the customer the `/bill/{token}` URL and
/// says `sent`; the mail carries the period and total too (issue #315).
#[tokio::test]
async fn an_emailed_bill_link_reaches_the_customer() {
    let Some(url) = url() else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let (app, db, mut stub) = mailing_app(&url).await;
    let account = account(&app, "mail-link").await;
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
    let report = mint(&app, &account.cookie, end_user, &to, &to).await.json()["data"].clone();
    let report_id = report["reportId"].as_str().expect("a report id").to_owned();

    let created = call(
        &app,
        "POST",
        &format!("/api/v1/org/usage-reports/{report_id}/links"),
        Some(json!({"email": "customer@example.com", "label": "August"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.json());
    assert_eq!(
        created.json()["data"]["delivery"].as_str(),
        Some("sent"),
        "the response names the delivery"
    );
    let token = created.json()["data"]["token"]
        .as_str()
        .expect("a token")
        .to_owned();

    let mail = mail_with(&mut stub, "/bill/").await;
    assert!(
        mail.contains(&format!("/bill/{token}")),
        "the mail carries the capability link: {mail}"
    );
    assert!(mail.contains(end_user), "the mail names the customer");
    assert!(
        mail.contains("0.000300"),
        "the mail carries the bill total: {mail}"
    );
    assert!(
        mail.contains("Verify this bill"),
        "the mail names the browser check"
    );
}

/// `email` on a deployment without a mailer is a caller mistake: the link is not
/// minted, so the operator never believes a mail went out.
#[tokio::test]
async fn email_without_a_mailer_is_refused() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "no-mailer").await;
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
    let report = mint(&app, &account.cookie, end_user, &to, &to).await.json()["data"].clone();
    let report_id = report["reportId"].as_str().expect("a report id").to_owned();

    let refused = call(
        &app,
        "POST",
        &format!("/api/v1/org/usage-reports/{report_id}/links"),
        Some(json!({"email": "customer@example.com"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        refused.json()["error"]["code"].as_str(),
        Some("VALIDATION_ERROR")
    );

    let malformed = call(
        &app,
        "POST",
        &format!("/api/v1/org/usage-reports/{report_id}/links"),
        Some(json!({"email": "not-an-address"})),
        Some(&account.cookie),
    )
    .await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);

    // Nothing was minted by either refusal.
    let links = call(
        &app,
        "GET",
        "/api/v1/org/usage-reports/links",
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(
        links.json()["data"]["rows"].as_array().map(Vec::len),
        Some(0)
    );
}

/// A report names the pinned witnesses and the log origin, and — once a
/// cosignature for its head is stored — carries that note too (issue #328).
#[tokio::test]
async fn a_report_carries_the_pinned_witnesses_and_its_cosignature() {
    let (app, db) = app_or_skip!();
    let account = account(&app, "witness-report").await;
    top_up(&app, &account.cookie, 1_000_000_000).await;
    let tenants = Tenants::new(db.pool().clone());
    let end_user = "customer-1";
    let to = today() - time::Duration::days(1);
    seed_request(
        &db,
        &tenants,
        &account.tenant_id,
        "req-w1",
        end_user,
        300,
        400,
        to,
    )
    .await;
    let to = to.to_string();
    let report = mint(&app, &account.cookie, end_user, &to, &to).await.json()["data"].clone();

    assert_eq!(
        report["witnessOrigin"]
            .as_str()
            .expect("the log origin is named"),
        origin_of(&account)
    );
    assert_eq!(
        report["witnessKeys"][0]["witnessUrl"]
            .as_str()
            .expect("the pinned witness is named"),
        "https://witness.test/"
    );
    assert!(
        report.get("witnessNotes").is_none(),
        "no cosignature is stored yet, so the field is absent rather than empty"
    );

    // A cosignature for exactly this head, as the witness pass stores one.
    sqlx::query(
        "INSERT INTO oxsum.witness_cosignatures (organization_id, witness_url, head_size, note) \
         VALUES ($1::uuid, $2, $3, $4)",
    )
    .bind(&account.organization_id)
    .bind("https://witness.test/")
    .bind(report["head"]["size"].as_i64().expect("a head size"))
    .bind("the cosigned note")
    .execute(db.pool())
    .await
    .expect("the cosignature row lands");

    let report_id = report["reportId"].as_str().expect("a report id");
    let fetched = call(
        &app,
        "GET",
        &format!("/api/v1/org/usage-reports/{report_id}"),
        None,
        Some(&account.cookie),
    )
    .await;
    assert_eq!(fetched.status, StatusCode::OK, "{}", fetched.json());
    let body = fetched.json();
    assert_eq!(
        body["data"]["witnessNotes"][0]["note"]
            .as_str()
            .expect("the stored cosignature"),
        "the cosigned note"
    );
    assert_eq!(
        body["data"]["witnessNotes"][0]["witnessUrl"]
            .as_str()
            .expect("the witness url"),
        "https://witness.test/"
    );
}

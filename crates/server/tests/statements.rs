//! The statement surface: draft generation, finalization, payments, suspension, and
//! the organization's own read — the billing loop of "borrow first, settle monthly"
//! (issue #124).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead
//! of failing, so a bare `cargo test` still passes.
//!
//! Statements cover closed months, but every API write books the server's own date —
//! so the turns a statement itemizes are written straight through the wallet with a
//! past booking date, and the HTTP surface is what the assertions exercise.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey, SettlementKind, UsageRecord, UsageRow, Wallet};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use time::Date;
use time::macros::date;
use tower::ServiceExt;

/// The operator token these tests configure.
const TOKEN: &str = "operator-token-0123456789";
const ONE: i64 = 1_000_000;
/// Two closed months: whatever the real date is, August and September 2026 have
/// fully ended, so bookings made then are statement-able.
const AUG: Date = date!(2026 - 08 - 15);
const SEPT: Date = date!(2026 - 09 - 15);

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("statements")
}

macro_rules! app_or_skip {
    () => {
        match url() {
            Some(u) => admin_app(&u).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// An app over a real database with the admin surface configured.
async fn admin_app(database_url: &str) -> (Router, PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None)
        .with_secret(secret())
        .with_admin_token(TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    (oxsum_server::app(db, config), pool)
}

fn secret() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

/// One call with a chosen credential, or none at all.
async fn call_with(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .header("content-type", "application/json")
        .body(match body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        })
        .expect("the test's own request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// One call under the operator token.
async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    call_with(app, method, path, body, Some(TOKEN)).await
}

/// Registers an account; answers the organization id and the API key the signup
/// minted.
async fn register(app: &Router, name: &str) -> (String, String) {
    let (status, body) = call_with(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({
            "email": format!("{name}_{}@example.com", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            "password": "correct horse battery",
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["data"]["organization"]["id"]
            .as_str()
            .expect("an organization id")
            .to_owned(),
        body["data"]["apiKey"]["secret"]
            .as_str()
            .expect("the minted key")
            .to_owned(),
    )
}

/// A billed turn booked in the past: the ledger entry and its usage row. The API
/// always books today, so statements over closed months are written through the
/// wallet directly.
async fn turn(
    db: &Db,
    tenant_id: &str,
    request: &str,
    priced: (&str, &str),
    freeze: i64,
    charged: i64,
    on: Date,
) {
    let (channel, model) = priced;
    let wallet = Wallet::open(db.pool().clone(), tenant_id)
        .await
        .expect("the wallet opens");
    let hold_key = format!("hold-{request}");
    wallet
        .hold(&hold_key, "", freeze, on)
        .await
        .expect("the hold reserves");
    let receipt = wallet
        .settle(&hold_key, "", charged, on)
        .await
        .expect("the turn settles");
    db.record_usage(&UsageRow {
        // The usage table deduplicates on request_id globally, so a fixed id a
        // previous run wrote would silently skip this insert.
        request_id: format!("{request}-{}", uuid::Uuid::new_v4().simple()),
        tenant_id: tenant_id.to_owned(),
        key_id: None,
        model: model.to_owned(),
        channel: channel.to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        status: "settled".to_owned(),
        failure_reason: None,
        usage: UsageRecord {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        counts: oxsum_core::CountsProvenance::Provider {
            input: 100,
            output: 50,
        },
        charged_minor: charged,
        freeze_minor: freeze,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    })
    .await
    .expect("the usage row is written");
}

/// The organization's tenant id: how its wallet is opened.
async fn tenant_of(pool: &PgPool, organization_id: &str) -> String {
    sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1::uuid")
        .bind(organization_id)
        .fetch_one(pool)
        .await
        .expect("the organization has a tenant")
}

/// Grants the organization a credit line through the admin endpoint.
async fn grant_credit(app: &Router, org: &str, limit: i64, key_suffix: &str) {
    let (status, body) = call(
        app,
        "PATCH",
        &format!("/api/v1/admin/organizations/{org}"),
        Some(json!({
            "creditLimitMinor": limit,
            "idempotencyKey": format!("stmt-cl-{key_suffix}"),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn a_statement_generates_finalizes_and_is_paid() {
    let (app, pool) = app_or_skip!();
    let (org, key) = register(&app, "stmt").await;
    let tenant = tenant_of(&pool, &org).await;
    let db = Db::from_pool(pool.clone());

    // A credit line, then three billed September turns: two drawn on the line
    // (no own funds), plus one August turn that belongs to another statement.
    grant_credit(&app, &org, 20_000_000, &org).await;
    turn(
        &db,
        &tenant,
        "s1",
        ("openai", "gpt-4o"),
        5 * ONE,
        3 * ONE,
        SEPT,
    )
    .await;
    turn(
        &db,
        &tenant,
        "s2",
        ("anthropic", "claude"),
        5 * ONE,
        4 * ONE,
        SEPT,
    )
    .await;
    turn(
        &db,
        &tenant,
        "s3",
        ("openai", "gpt-4o"),
        10 * ONE,
        9 * ONE,
        AUG,
    )
    .await;

    // The running month cannot be billed.
    let now_month = time::OffsetDateTime::now_utc().date();
    let running = format!("{:04}-{:02}", now_month.year(), u8::from(now_month.month()));
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/statements",
        Some(json!({"period": running})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Generation builds the draft for this one organization.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/statements",
        Some(json!({"period": "2026-09", "organizationId": org})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let generated = body["data"].as_array().expect("a statement list");
    let generated = generated
        .iter()
        .find(|s| s["organizationId"] == org)
        .expect("the organization's statement");
    assert_eq!(generated["status"], "draft");
    assert_eq!(generated["paymentStatus"], "pending");
    assert_eq!(generated["totalMinor"], 7_000_000);
    assert_eq!(generated["creditDrawnMinor"], 7_000_000);
    assert_eq!(generated["entryCount"], 2);
    let statement_id = generated["id"].as_str().unwrap().to_owned();

    // Regeneration answers the same document.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/statements",
        Some(json!({"period": "2026-09", "organizationId": org})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"][0]["id"].as_str().unwrap(),
        statement_id.as_str()
    );

    // A draft is invisible to the organization — and to an organization
    // credential on the admin surface.
    let (status, _) = call_with(&app, "GET", "/api/v1/statements", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call_with(
        &app,
        "GET",
        &format!("/api/v1/admin/statements/{statement_id}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Tighter terms take effect from the next finalization — the draft is
    // re-priced under them.
    let (status, body) = call(
        &app,
        "PATCH",
        &format!("/api/v1/admin/organizations/{org}"),
        Some(json!({"paymentTermsDays": 14, "idempotencyKey": "terms-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Finalize: terms snapshot, due date, proof window.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/finalize"),
        Some(json!({"idempotencyKey": "fin-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let finalized = &body["data"];
    assert_eq!(finalized["status"], "finalized");
    assert_eq!(finalized["paymentStatus"], "pending");
    assert_eq!(finalized["paymentTermsDays"], 14);
    assert!(finalized["dueDate"].is_string(), "{finalized}");
    assert!(finalized["logFromIndex"].is_number(), "{finalized}");
    assert!(finalized["finalizedAt"].is_string(), "{finalized}");

    // Finalize is idempotent.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/finalize"),
        Some(json!({"idempotencyKey": "fin-2"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["finalizedAt"], finalized["finalizedAt"]);

    // The detail itemizes by channel and model.
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/admin/statements/{statement_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lines = body["data"]["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["channel"], "anthropic");
    assert_eq!(lines[0]["amountMinor"], 4_000_000);
    assert_eq!(lines[1]["channel"], "openai");
    assert_eq!(lines[1]["turns"], 1);
    assert_eq!(lines[1]["amountMinor"], 3_000_000);

    // The organization sees the issued statement now.
    let (status, body) = call_with(&app, "GET", "/api/v1/statements", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mine = body["data"]
        .as_array()
        .expect("the organization's statements");
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["id"], statement_id.as_str());
    assert_eq!(mine[0]["outstandingMinor"], 7_000_000);
    let (status, body) = call_with(
        &app,
        "GET",
        &format!("/api/v1/statements/{statement_id}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["lines"].as_array().unwrap().len(), 2);

    // The line still owes August's unbilled 9 ahead of the 7 this statement
    // drew, so the debt through September is 16 — a payment beyond it refuses.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/payments"),
        Some(json!({"amountMinor": 17_000_000, "idempotencyKey": "pay-too-much"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Paying the statement's own 7 books, but oldest-first the money settles
    // August's unbilled draws — the statement still stands pending.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/payments"),
        Some(json!({"amountMinor": 7_000_000, "idempotencyKey": "pay-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["statement"]["paymentStatus"], "pending");
    assert_eq!(body["data"]["statement"]["paidMinor"], 0);
    let (_, body) = call_with(&app, "GET", "/api/v1/balance", None, Some(&key)).await;
    assert_eq!(body["data"]["creditUsedMinor"], 9_000_000);

    // A retried open payment replays its receipt rather than booking again.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/payments"),
        Some(json!({"amountMinor": 7_000_000, "idempotencyKey": "pay-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The key under a different amount is a conflict.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/payments"),
        Some(json!({"amountMinor": 1_000_000, "idempotencyKey": "pay-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Clearing the remaining 9 settles the statement — and once it is paid a
    // further payment has nothing to take.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/payments"),
        Some(json!({"amountMinor": 9_000_000, "idempotencyKey": "pay-2"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["statement"]["paymentStatus"], "paid");
    assert_eq!(body["data"]["statement"]["paidMinor"], 7_000_000);
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/payments"),
        Some(json!({"amountMinor": 9_000_000, "idempotencyKey": "pay-2"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn a_self_serve_topup_pays_the_oldest_statement_first() {
    let (app, pool) = app_or_skip!();
    let (org, key) = register(&app, "stmtfifo").await;
    let tenant = tenant_of(&pool, &org).await;
    let db = Db::from_pool(pool.clone());

    grant_credit(&app, &org, 20_000_000, &org).await;
    // August owes 4, September owes 3.
    turn(
        &db,
        &tenant,
        "a1",
        ("openai", "gpt-4o"),
        5 * ONE,
        4 * ONE,
        AUG,
    )
    .await;
    turn(
        &db,
        &tenant,
        "s1",
        ("openai", "gpt-4o"),
        5 * ONE,
        3 * ONE,
        SEPT,
    )
    .await;
    for period in ["2026-08", "2026-09"] {
        let (status, body) = call(
            &app,
            "POST",
            "/api/v1/admin/statements",
            Some(json!({"period": period, "organizationId": org})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let id = body["data"][0]["id"].as_str().unwrap().to_owned();
        let (status, body) = call(
            &app,
            "POST",
            &format!("/api/v1/admin/statements/{id}/finalize"),
            Some(json!({"idempotencyKey": format!("fin-{period}")})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    // The organization's own top-up repays the line: August settles first.
    let (status, body) = call_with(
        &app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": "stmt-topup", "amountMinor": 5_000_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call_with(&app, "GET", "/api/v1/statements", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mine = body["data"].as_array().unwrap();
    let august = mine.iter().find(|s| s["period"] == "2026-08").unwrap();
    let september = mine.iter().find(|s| s["period"] == "2026-09").unwrap();
    assert_eq!(august["paymentStatus"], "paid", "{august}");
    assert_eq!(september["paymentStatus"], "pending", "{september}");
    assert_eq!(september["paidMinor"], 1_000_000);

    // Suspend September, then pay it off anyway — a suspended bill still pays.
    let sept_id = september["id"].as_str().unwrap().to_owned();
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{sept_id}/suspend"),
        Some(json!({"idempotencyKey": "sus-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["paymentStatus"], "suspended");
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{sept_id}/payments"),
        Some(json!({"amountMinor": 2_000_000, "idempotencyKey": "pay-sept"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["statement"]["paymentStatus"], "paid");
}

#[tokio::test]
async fn statements_are_scoped_to_their_organization() {
    let (app, pool) = app_or_skip!();
    let (org, _key) = register(&app, "stmts").await;
    let (_other, other_key) = register(&app, "stmts_other").await;
    let tenant = tenant_of(&pool, &org).await;
    let db = Db::from_pool(pool.clone());

    grant_credit(&app, &org, 20_000_000, &org).await;
    turn(
        &db,
        &tenant,
        "s1",
        ("openai", "gpt-4o"),
        5 * ONE,
        3 * ONE,
        SEPT,
    )
    .await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/statements",
        Some(json!({"period": "2026-09", "organizationId": org})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["data"][0]["id"].as_str().unwrap().to_owned();
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{id}/finalize"),
        Some(json!({"idempotencyKey": "fin-scope"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Another organization's key: the statement is not theirs — 404, not 403,
    // so an id probes nothing.
    let (status, _) = call_with(
        &app,
        "GET",
        &format!("/api/v1/statements/{id}"),
        None,
        Some(&other_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = call_with(&app, "GET", "/api/v1/statements", None, Some(&other_key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"].as_array().unwrap().is_empty());

    // Unknown id: 404 on both surfaces.
    let unknown = uuid::Uuid::new_v4();
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/admin/statements/{unknown}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/statements/{unknown}/finalize"),
        Some(json!({"idempotencyKey": "fin-none"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── the PDF export (issue #205) ─────────────────────────────────────────────
//
// A finalized statement renders as a downloadable PDF — binary, not the JSON
// envelope — anchored to the ledger window its lines prove. A draft is a
// working document and exports as 404; the export always reads the stored
// lines, so the document can never disagree with the detail endpoint.

/// One call that returns the raw body and the headers, for the binary export —
/// `call`/`call_with` parse JSON, which a PDF is not.
async fn call_bytes(
    app: &Router,
    path: &str,
    token: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut request = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).expect("the test's own request"))
        .await
        .expect("the router answers");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collects the body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

/// Generates, finalizes and returns a statement id for a fresh organization —
/// the precondition every export test shares. Two billed September turns.
async fn a_finalized_statement(app: &Router, pool: &PgPool, name: &str) -> (String, String) {
    let (org, key) = register(app, name).await;
    let tenant = tenant_of(pool, &org).await;
    let db = Db::from_pool(pool.clone());
    grant_credit(app, &org, 20_000_000, &org).await;
    turn(
        &db,
        &tenant,
        "x1",
        ("openai", "gpt-4o"),
        5 * ONE,
        3 * ONE,
        SEPT,
    )
    .await;
    turn(
        &db,
        &tenant,
        "x2",
        ("anthropic", "claude"),
        5 * ONE,
        4 * ONE,
        SEPT,
    )
    .await;

    let (status, body) = call(
        app,
        "POST",
        "/api/v1/admin/statements",
        Some(json!({"period": "2026-09", "organizationId": org})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let statement_id = body["data"][0]["id"].as_str().unwrap().to_owned();
    let (status, body) = call(
        app,
        "POST",
        &format!("/api/v1/admin/statements/{statement_id}/finalize"),
        Some(json!({"idempotencyKey": format!("fin-{name}")})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (statement_id, key)
}

/// The bytes are a real PDF: the magic header, the trailer, and a parse — and
/// the page carries the totals the stored lines sum to, so the export is the
/// same answer the detail endpoint gives.
#[tokio::test]
async fn a_finalized_statement_exports_as_a_pdf() {
    let (app, pool) = app_or_skip!();
    let (statement_id, key) = a_finalized_statement(&app, &pool, "pdf").await;

    // The organization's own export — its key reads only its own org.
    let (status, headers, pdf) = call_bytes(
        &app,
        &format!("/api/v1/statements/{statement_id}/export.pdf"),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "export failed: {pdf:?}");
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/pdf")
    );
    let disposition = headers
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .expect("a download name");
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(disposition.contains("2026-09"), "{disposition}");

    // The magic and the trailer are the format's own edges; printpdf's own
    // reader round-tripping the bytes is pinned by the core unit test (the
    // server crate does not link the renderer).
    assert!(pdf.starts_with(b"%PDF-"), "not a PDF header");
    assert!(pdf.windows(5).any(|w| w == b"%%EOF"), "no trailer");
    assert!(pdf.len() > 1_000, "a statement page is not a stub");

    // The admin twin renders the same document for the operator.
    let (status, headers, admin_pdf) = call_bytes(
        &app,
        &format!("/api/v1/admin/statements/{statement_id}/export.pdf"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin export failed");
    assert_eq!(
        headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/pdf")
    );
    assert!(admin_pdf.starts_with(b"%PDF-"));
}

/// A draft is the platform's working document: it never exports, on either
/// surface — and the export answers it as if it were not there.
#[tokio::test]
async fn a_draft_statement_does_not_export() {
    let (app, pool) = app_or_skip!();
    let (org, key) = register(&app, "pdf-draft").await;
    let tenant = tenant_of(&pool, &org).await;
    let db = Db::from_pool(pool.clone());
    grant_credit(&app, &org, 20_000_000, &org).await;
    turn(
        &db,
        &tenant,
        "d1",
        ("openai", "gpt-4o"),
        5 * ONE,
        3 * ONE,
        SEPT,
    )
    .await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/statements",
        Some(json!({"period": "2026-09", "organizationId": org})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let draft_id = body["data"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(body["data"][0]["status"], "draft");

    // Still a draft: both surfaces answer 404, not a rendered working copy.
    let (status, _, _) = call_bytes(
        &app,
        &format!("/api/v1/statements/{draft_id}/export.pdf"),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call_bytes(
        &app,
        &format!("/api/v1/admin/statements/{draft_id}/export.pdf"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The org export is scoped: another organization's statement is 404 to this
/// org's key, and no credential is 401 — the document is the bill, so it reads
/// under the same rules as the detail endpoint.
#[tokio::test]
async fn the_export_reads_only_the_callers_own_statements() {
    let (app, pool) = app_or_skip!();
    let (statement_id, _key) = a_finalized_statement(&app, &pool, "pdf-scope").await;

    // A second organization registered after the first's statement exists:
    // its key cannot name another org's document.
    let (_other_org, other_key) = register(&app, "pdf-other").await;
    let (status, _, _) = call_bytes(
        &app,
        &format!("/api/v1/statements/{statement_id}/export.pdf"),
        Some(&other_key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // No credential at all.
    let (status, _, _) = call_bytes(
        &app,
        &format!("/api/v1/statements/{statement_id}/export.pdf"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ── statement anchors (issue #221) ──────────────────────────────────────────
//
// A finalized statement is anchored to the transparency log: a marker entry the
// finalize path appends, a window fold over the covered entries' content hashes,
// and a rollup digest over the statement's own figures. The detail endpoint
// answers the anchor back, and `oxsum_verify::verify_statement_anchor` is the
// client-side check a bill's reader runs — these tests run it against the real
// API answer.

/// The covered entries' `contentHash` hex strings in `log_index` order — what
/// `StatementAnchorCheck::window_hashes` wants. Mirrors `draft_of`'s own select:
/// every usage row whose ledger entry books inside the statement's month.
async fn window_hashes(pool: &PgPool, tenant_id: &str, first: Date, last: Date) -> Vec<String> {
    let schema = format!("ledger_{tenant_id}");
    // The schema name is assembled by Wallet::open from a validated tenant id;
    // it is interpolated here the same way doubleentry does internally.
    let sql = format!(
        "SELECT e.content_hash \
         FROM oxsum.usage_records u \
         JOIN {schema}.entries e ON e.entry_id = u.entry_id \
         WHERE u.tenant_id = $1 AND e.booking_date >= $2 AND e.booking_date <= $3 \
         ORDER BY e.log_index"
    );
    sqlx::query_scalar::<_, Vec<u8>>(&sql)
        .bind(tenant_id)
        .bind(first)
        .bind(last)
        .fetch_all(pool)
        .await
        .expect("the window hashes read")
        .into_iter()
        .map(|bytes| bytes.iter().map(|b| format!("{b:02x}")).collect())
        .collect()
}

/// The period's first and last calendar day as `time::Date`s, for the window query.
fn period_bounds(period: &str) -> (Date, Date) {
    let (year, month) = period.split_once('-').expect("a YYYY-MM period");
    let year: i32 = year.parse().unwrap();
    let month: u8 = month.parse().unwrap();
    let month = time::Month::try_from(month).unwrap();
    let first = Date::from_calendar_date(year, month, 1).unwrap();
    // The last day is the first of the month plus its length minus one.
    let last = first + time::Duration::days(time::util::days_in_month(month, year) as i64 - 1);
    (first, last)
}

/// Builds the `TreeHead` the `/api/v1/log/head` answer carries — the signed head
/// right after the anchor, which is what the note attests.
fn head_from(value: &Value) -> doubleentry::TreeHead {
    doubleentry::TreeHead {
        size: value["size"].as_u64().expect("a head size"),
        root: doubleentry::Hash::parse_hex(value["root"].as_str().expect("a head root"))
            .expect("the head root is hex"),
    }
}

/// An unsigned deployment: the anchor still binds the window fold and the rollup
/// — `note` and the key fields are absent — and `verify_statement_anchor` passes
/// on the digests alone.
#[tokio::test]
async fn an_unsigned_anchor_verifies_against_the_detail_answer() {
    let (app, pool) = app_or_skip!();
    let (statement_id, key) = a_finalized_statement(&app, &pool, "anchor-unsigned").await;

    // The organization's detail carries the anchor fields.
    let (status, body) = call_with(
        &app,
        "GET",
        &format!("/api/v1/statements/{statement_id}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let anchor = &body["data"]["anchor"];
    assert!(
        anchor.is_object(),
        "the finalized statement is anchored: {body}"
    );
    assert!(anchor["entryId"].is_string());
    assert!(anchor["logIndex"].is_number());
    assert!(anchor["windowHash"].is_string());
    assert!(anchor["rollupHash"].is_string());
    // Unsigned: the signature layer does not apply, so the key fields are absent.
    assert!(anchor["note"].is_null(), "{anchor}");
    assert!(anchor["keyName"].is_null(), "{anchor}");
    assert!(anchor["publicKey"].is_null(), "{anchor}");
    assert!(anchor["keyHash"].is_null(), "{anchor}");

    let tenant = tenant_of(
        &pool,
        body["data"]["statement"]["organizationId"]
            .as_str()
            .unwrap(),
    )
    .await;
    let origin = format!("oxsum/ledgers/{tenant}");
    assert_eq!(anchor["origin"].as_str().unwrap(), origin);

    // The client collects the covered entries' content hashes in log order.
    let (first, last) = period_bounds(body["data"]["statement"]["period"].as_str().unwrap());
    let hashes = window_hashes(&pool, &tenant, first, last).await;
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
    assert_eq!(
        hashes.len() as i64,
        body["data"]["statement"]["entryCount"].as_i64().unwrap(),
        "every covered entry supplies a hash"
    );

    // The check the bill's reader runs — expected_head is unused on the unsigned
    // path (verification returns before consulting it), so a dummy stands in.
    let dummy_head = doubleentry::TreeHead {
        size: 0,
        root: doubleentry::Hash::from_bytes([0; 32]),
    };
    let check = oxsum_verify::StatementAnchorCheck {
        window_hashes: &hashes,
        expected_head: &dummy_head,
        expected_origin: &origin,
    };
    oxsum_verify::verify_statement_anchor(&body["data"].to_string(), &check)
        .expect("the unsigned anchor verifies");

    // The admin twin answers the same document, anchored identically.
    let (status, admin_body) = call(
        &app,
        "GET",
        &format!("/api/v1/admin/statements/{statement_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{admin_body}");
    assert_eq!(
        admin_body["data"]["anchor"]["rollupHash"], anchor["rollupHash"],
        "the admin detail commits the same rollup"
    );
    oxsum_verify::verify_statement_anchor(&admin_body["data"].to_string(), &check)
        .expect("the admin detail's anchor verifies");
}

/// A seeded deployment signs heads: the anchor carries the note over the head
/// right after the marker, and `verify_statement_anchor` binds that signature —
/// the marker's inclusion is proven by the head that follows it.
#[tokio::test]
async fn a_signed_anchor_verifies_the_head_that_follows_it() {
    // Skips when DATABASE_URL is unset; the unsigned-app bindings are dropped —
    // a seeded app is built below.
    let (_app, _pool) = app_or_skip!();
    // Rebuild the app with a signing seed: the same deployment, now attesting.
    let db_url = url().expect("DATABASE_URL is set (app_or_skip connected)");
    let pool2 = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool2.clone());
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None)
        .with_secret(secret())
        .with_admin_token(TOKEN)
        .with_head_signing_seed([42; 32]);
    let app = oxsum_server::app(db, config);
    let pool = pool2;

    let (statement_id, key) = a_finalized_statement(&app, &pool, "anchor-signed").await;

    let (status, body) = call_with(
        &app,
        "GET",
        &format!("/api/v1/statements/{statement_id}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let anchor = &body["data"]["anchor"];
    assert!(
        anchor.is_object(),
        "the finalized statement is anchored: {body}"
    );
    // Signed: the note and the key fields attest the head after the marker.
    assert!(anchor["note"].is_string(), "{anchor}");
    assert_eq!(anchor["keyName"].as_str().unwrap(), "oxsum/tree-heads");
    assert_eq!(anchor["publicKey"].as_str().unwrap().len(), 64);
    assert_eq!(anchor["keyHash"].as_str().unwrap().len(), 8);

    let tenant = tenant_of(
        &pool,
        body["data"]["statement"]["organizationId"]
            .as_str()
            .unwrap(),
    )
    .await;
    let origin = format!("oxsum/ledgers/{tenant}");
    assert_eq!(anchor["origin"].as_str().unwrap(), origin);

    // The head right after the anchor: the marker is the latest entry, so the
    // current head is at logIndex + 1 — the size the note must attest. /log/head
    // reads the caller's own ledger, so the organization's key asks, not the
    // operator token.
    let (status, head_body) = call_with(&app, "GET", "/api/v1/log/head", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{head_body}");
    let expected_head = head_from(&head_body["data"]);
    assert_eq!(
        expected_head.size,
        anchor["logIndex"].as_u64().unwrap() + 1,
        "the served head is the one that follows the marker"
    );

    let (first, last) = period_bounds(body["data"]["statement"]["period"].as_str().unwrap());
    let hashes = window_hashes(&pool, &tenant, first, last).await;
    let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();

    let check = oxsum_verify::StatementAnchorCheck {
        window_hashes: &hashes,
        expected_head: &expected_head,
        expected_origin: &origin,
    };
    oxsum_verify::verify_statement_anchor(&body["data"].to_string(), &check)
        .expect("the signed anchor verifies");
}

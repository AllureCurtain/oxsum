//! HTTP tests for the programmatic read surface (roadmap P5-3, issue #146):
//! `GET /api/v1/usage`, `GET /api/v1/billing-records`, `POST /api/v1/estimate-price`
//! and `GET /api/v1/pricing`.
//!
//! The tests seed organizations, prices and settled turns the way the gateway writes
//! them, then read the four endpoints through the real router. Needs DATABASE_URL and
//! skips without it, like the other server suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    ActingKey, BillLine, Db, PriceBook, SecretKey, Settlement, SettlementKind, Tenants,
    UsageRecord, UsageRow, entry_id_for, hold_description, settlement_key_for,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

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

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A fresh organization: its tenant id for seeding and its first key's secret
/// for calling the API.
async fn register(app: &Router, name: &str) -> (String, String, Uuid) {
    let email = format!(
        "{name}_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    (
        body["data"]["organization"]["tenantId"]
            .as_str()
            .unwrap()
            .to_owned(),
        body["data"]["apiKey"]["secret"]
            .as_str()
            .unwrap()
            .to_owned(),
        body["data"]["apiKey"]["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
    )
}

/// Tops the organization up under a random key, the way `POST /api/v1/topups` does.
async fn top_up(app: &Router, key: &str, amount_minor: i64) {
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/topups",
        Some(json!({"idempotencyKey": Uuid::new_v4().to_string(), "amountMinor": amount_minor})),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "top-up failed: {body}");
}

fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}

/// A model priced through a test channel, the way an operator's rows land.
async fn price_model(db: &Db, channel: &str, model: &str, price: Value) {
    // The same test key the core channel suite seals with: `check_sealed_keys` scans
    // every channel in the shared database, so a different key would break that test.
    let secret = SecretKey::from_bytes([7; 32]);
    db.set_channel(
        channel,
        "http://unused",
        "upstream-secret",
        "openai",
        &secret,
    )
    .await
    .expect("the channel is written");
    let book = PriceBook::from_json(channel, &json!({ model: price }).to_string())
        .expect("the test's price parses");
    for (model, price) in book.models() {
        db.append_price(channel, model, price.clone())
            .await
            .expect("the price is written");
    }
}

/// One settled turn written the way the gateway writes it: hold, settlement and
/// the usage row `record_usage` rolls into the daily table.
async fn settle_request(
    db: &Db,
    tenants: &Tenants,
    tenant_id: &str,
    key_id: Option<Uuid>,
    request: &str,
    model: &str,
    charged: i64,
) {
    let wallet = tenants.get(tenant_id).await.expect("the wallet opens");
    let hold_key = format!("req-{request}:hold");
    let freeze = charged.max(1) * 2;
    match key_id {
        Some(key_id) => {
            wallet
                .hold_for_key(
                    &ActingKey {
                        key_id,
                        spend_limit_minor: None,
                        requests_per_minute: None,
                    },
                    Some(model),
                    &hold_key,
                    &hold_description(request, model, freeze).expect("the hold serializes"),
                    freeze,
                    today(),
                )
                .await
                .expect("the hold is taken");
        }
        None => {
            wallet
                .hold(&hold_key, "seed hold", freeze, today())
                .await
                .expect("the hold is taken");
        }
    }
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
        model,
        price_version: 1,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged,
        freeze,
    };
    wallet
        .settle(
            &hold_key,
            &record.description().expect("the record serializes"),
            charged,
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
        model: model.to_owned(),
        channel: "mock".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *entry_id_for(&settlement_key_for(&hold_key)).as_uuid(),
        usage,
        charged_minor: charged,
        freeze_minor: freeze,
        upstream_cost_minor: None,
    })
    .await
    .expect("the usage row is written");
}

// ── /api/v1/usage ────────────────────────────────────────────────────────────

/// The rollup the dashboard reads, answered as data — and nothing outside the
/// credential's organization.
#[tokio::test]
async fn the_usage_api_answers_the_rollup() {
    let (app, db) = app_or_skip!();
    let tenants = Tenants::new(db.pool().clone());
    let (tenant, key, key_id) = register(&app, "usage-api").await;
    let (other_tenant, other_key, other_key_id) = register(&app, "usage-api-other").await;
    top_up(&app, &key, 50_000_000).await;
    top_up(&app, &other_key, 50_000_000).await;

    settle_request(&db, &tenants, &tenant, Some(key_id), "use-a", "m1", 3_000).await;
    settle_request(&db, &tenants, &tenant, Some(key_id), "use-b", "m1", 4_000).await;
    settle_request(
        &db,
        &tenants,
        &other_tenant,
        Some(other_key_id),
        "use-x",
        "m1",
        9_000,
    )
    .await;

    let (status, body) = call(&app, "GET", "/api/v1/usage", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let days = body["data"]["days"].as_array().expect("days is an array");
    let mine: Vec<&Value> = days
        .iter()
        .filter(|row| row["channel"] == "mock" && row["model"] == "m1")
        .collect();
    // Both turns were attributed to the same key, channel and model: one bucket, two turns.
    assert_eq!(mine.len(), 1, "{days:?}");
    assert_eq!(mine[0]["turns"], 2);
    assert_eq!(mine[0]["chargedMinor"], 7_000);
    assert_eq!(mine[0]["inputTokens"], 2_000);
    assert_eq!(mine[0]["keyId"].as_str().unwrap(), key_id.to_string());
}

/// The window's bounds: malformed dates, an inverted span and a span past the
/// limit all answer VALIDATION, and no credential answers 401.
#[tokio::test]
async fn the_usage_window_is_validated() {
    let (app, _db) = app_or_skip!();
    let (_tenant, key, _key_id) = register(&app, "usage-bounds").await;

    for path in [
        "/api/v1/usage?from=not-a-date",
        "/api/v1/usage?to=2020-13-40",
        "/api/v1/usage?from=2030-01-10&to=2030-01-01",
        "/api/v1/usage?from=2020-01-01&to=2020-06-01",
    ] {
        let (status, body) = call(&app, "GET", path, None, Some(&key)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR", "{path}: {body}");
    }
    let (status, _) = call(&app, "GET", "/api/v1/usage", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ── /api/v1/billing-records ──────────────────────────────────────────────────

/// Settled turns newest-first under the `before` cursor, scoped to the
/// credential's organization.
#[tokio::test]
async fn billing_records_page_in_order() {
    let (app, db) = app_or_skip!();
    let tenants = Tenants::new(db.pool().clone());
    let (tenant, key, key_id) = register(&app, "records").await;
    let (other_tenant, other, other_key_id) = register(&app, "records-other").await;
    top_up(&app, &key, 50_000_000).await;
    top_up(&app, &other, 50_000_000).await;

    for (name, charge) in [("rec-a", 1_000), ("rec-b", 2_000), ("rec-c", 3_000)] {
        settle_request(&db, &tenants, &tenant, Some(key_id), name, "m1", charge).await;
    }
    settle_request(
        &db,
        &tenants,
        &other_tenant,
        Some(other_key_id),
        "rec-x",
        "m1",
        9_000,
    )
    .await;

    // First page of two, then the walk resumes above nextCursor.
    let (status, first) = call(
        &app,
        "GET",
        "/api/v1/billing-records?limit=2",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let rows = first["data"]["rows"].as_array().expect("rows is an array");
    assert_eq!(rows.len(), 2, "{first}");
    assert_eq!(rows[0]["chargedMinor"], 3_000);
    assert_eq!(rows[1]["chargedMinor"], 2_000);
    assert_eq!(rows[0]["kind"], "usage");
    let cursor = first["data"]["nextCursor"]
        .as_u64()
        .expect("a second page remains");

    let (status, second) = call(
        &app,
        "GET",
        &format!("/api/v1/billing-records?limit=2&before={cursor}"),
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let rows = second["data"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{second}");
    assert_eq!(rows[0]["chargedMinor"], 1_000);
    // The walk reached the log's start: no cursor is answered.
    assert!(second["data"]["nextCursor"].is_null());

    // The other organization's turn never appears in this organization's records.
    let (status, other) = call(&app, "GET", "/api/v1/billing-records", None, Some(&other)).await;
    assert_eq!(status, StatusCode::OK);
    let rows = other["data"]["rows"].as_array().unwrap();
    assert!(
        rows.iter().all(|row| row["chargedMinor"] == 9_000),
        "{other}"
    );
}

/// `limit` outside its bounds answers VALIDATION.
#[tokio::test]
async fn the_records_limit_is_bounded() {
    let (app, _db) = app_or_skip!();
    let (_tenant, key, _key_id) = register(&app, "records-limit").await;
    for path in [
        "/api/v1/billing-records?limit=0",
        "/api/v1/billing-records?limit=201",
    ] {
        let (status, body) = call(&app, "GET", path, None, Some(&key)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR", "{path}: {body}");
    }
}

// ── /api/v1/estimate-price ───────────────────────────────────────────────────

/// The estimate runs the freeze arithmetic on the declared shape: dearest
/// applicable set, output clamped to the model's ceiling, rounded up — the
/// same number the gateway would freeze.
#[tokio::test]
async fn estimate_price_is_the_freeze() {
    let (app, db) = app_or_skip!();
    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("cat-{suffix}");
    let model = format!("priced-{suffix}");
    // 1 minor per input token, 2 per output token, a 4k output ceiling — small
    // enough that the estimate is easy to check by hand.
    price_model(
        &db,
        &channel,
        &model,
        json!({"inputPricePerMillion": 1_000_000, "outputPricePerMillion": 2_000_000, "maxOutputTokens": 4_000}),
    )
    .await;
    let (_tenant, key, _key_id) = register(&app, "estimate").await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/estimate-price",
        Some(json!({"model": model, "inputTokens": 1_000, "outputTokens": 500})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 1000 input at 1 minor/token + 500 output at 2 minor/token = 2000 minor units.
    assert_eq!(body["data"]["estimateMinor"], 2_000, "{body}");
    assert_eq!(body["data"]["version"], 1);
    assert_eq!(body["data"]["model"], model);

    // Beyond the ceiling clamps to it: the freeze never overestimates past
    // what the model can produce.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/estimate-price",
        Some(json!({"model": model, "inputTokens": 0, "outputTokens": 100_000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 4000 output at 2 minor/mtok.
    assert_eq!(body["data"]["estimateMinor"], 8_000, "{body}");

    // An unknown model and a negative input both answer VALIDATION, not a price of zero.
    for body in [
        json!({"model": format!("absent-{suffix}"), "inputTokens": 1}),
        json!({"model": model, "inputTokens": -5}),
        json!({"model": model, "outputTokens": 0}),
    ] {
        let (status, res) = call(
            &app,
            "POST",
            "/api/v1/estimate-price",
            Some(body),
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{res}");
        assert_eq!(res["error"]["code"], "VALIDATION_ERROR", "{res}");
    }
}

// ── /api/v1/pricing ─────────────────────────────────────────────────────────

/// The catalog lists every model's current version with its channel and
/// protocol — and never a credential.
#[tokio::test]
async fn the_catalog_carries_no_credentials() {
    let (app, db) = app_or_skip!();
    let suffix = Uuid::new_v4().simple().to_string()[..8].to_owned();
    let channel = format!("cat-{suffix}");
    let model = format!("catalog-{suffix}");
    price_model(
        &db,
        &channel,
        &model,
        json!({"inputPricePerMillion": 5, "outputPricePerMillion": 9, "maxOutputTokens": 2_000}),
    )
    .await;
    let (_tenant, key, _key_id) = register(&app, "catalog").await;

    let (status, body) = call(&app, "GET", "/api/v1/pricing", None, Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let models = body["data"]["models"]
        .as_array()
        .expect("models is an array");
    let row = models
        .iter()
        .find(|row| row["model"] == model)
        .unwrap_or_else(|| panic!("{model} is in the catalog: {body}"));
    assert_eq!(row["channel"], channel);
    assert_eq!(row["protocol"], "openai");
    assert_eq!(row["version"], 1);
    assert_eq!(row["inputPricePerMillion"], 5);
    // No credential-shaped key anywhere in the payload — other channels' model
    // names may carry the words, so the check walks field names, not values.
    fn credential_keys(value: &Value) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack = vec![value];
        while let Some(value) = stack.pop() {
            match value {
                Value::Object(map) => {
                    for (key, inner) in map {
                        let lower = key.to_lowercase();
                        if lower.contains("apikey")
                            || lower.contains("sealed")
                            || lower.contains("credential")
                            || lower.contains("baseurl")
                        {
                            found.push(key.clone());
                        }
                        stack.push(inner);
                    }
                }
                Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
        found
    }
    assert_eq!(credential_keys(&body), Vec::<String>::new(), "{body}");

    let (status, _) = call(&app, "GET", "/api/v1/pricing", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

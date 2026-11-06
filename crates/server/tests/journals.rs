//! Plan fee journals over HTTP (issue #611, T20): the customer reads its own
//! per-period invoices (`/api/v1/org/plan/journals`), the staff lists
//! cross-organization (`/api/v1/business/journals`, `ReportsRead`) and voids
//! under `FundsWrite` — every endpoint carries one integration test, and the
//! void walks a settled journal to `void` with its reversing lines.
//! Needs DATABASE_URL; skips without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core/tests/support/integration.rs"]
mod setup;
mod support;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "journal http fixture password";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("journals_http")
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = cookie {
        req = req.header("cookie", format!("oxsum_session={token}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(app: &Router, email: &str) -> String {
    support::login(app, email, PASSWORD).await
}

/// A migrated world: the business setup exists, the owner session is in
/// business context, and a registered customer stands ready to bind.
struct World {
    app: Router,
    db: Db,
    pool: PgPool,
    owner: String,
    customer_org: Uuid,
    customer: String,
}

async fn world() -> Option<World> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let owner = setup::owner(&db).await;
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Open, None).with_secret(SecretKey::from_bytes([7; 32])),
    );

    let email = format!(
        "journal_customer_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        json!({"email": email, "password": PASSWORD}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let customer_org =
        Uuid::parse_str(body["data"]["organization"]["id"].as_str().unwrap()).unwrap();
    let customer = login(&app, &email).await;

    Some(World {
        app,
        db,
        pool,
        owner,
        customer_org,
        customer,
    })
}

/// Publishes a sold plan through the staff route and answers its version id.
async fn publish(world: &World, name: &str, price: i64) -> Uuid {
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/business/plans",
        json!({
            "name": name,
            "priceMinor": price,
            "currency": "USD",
            "quotaMinor": 10_000_000,
            "periodKind": "calendarMonth",
            "overageEnabled": false,
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Uuid::parse_str(body["data"]["planVersionId"].as_str().unwrap()).unwrap()
}

/// Binds the customer to the sold version and activates period 0 under a
/// (fixture) order id — the journal tests never drive a PSP, so the order
/// row's absence just means zero applied; the close still bills the period.
async fn bound_and_active(world: &World, version: Uuid) -> Uuid {
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/business/customers/{}/plan", world.customer_org),
        json!({"planVersionId": version}),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let customer_plan_id =
        Uuid::parse_str(body["data"]["customerPlanId"].as_str().unwrap()).unwrap();
    world
        .db
        .activate_plan_period(Uuid::new_v4(), customer_plan_id)
        .await
        .unwrap();
    customer_plan_id
}

/// Forces the customer's open period past its boundary and runs the rollover
/// — the close that writes the period's journal.
async fn close_period(world: &World) {
    sqlx::query(
        "UPDATE oxsum.org_subscriptions \
         SET current_period_end = now() - interval '60 seconds' \
         WHERE organization_id = $1 AND state = 'active'",
    )
    .bind(world.customer_org)
    .execute(&world.pool)
    .await
    .expect("the subscription window moves");
    sqlx::query(
        "UPDATE oxsum.plan_periods SET period_end = now() - interval '60 seconds' \
         WHERE state = 'active' AND customer_plan_id IN ( \
             SELECT customer_plan_id FROM oxsum.customer_plans WHERE organization_id = $1)",
    )
    .bind(world.customer_org)
    .execute(&world.pool)
    .await
    .expect("the plan period moves");
    world.db.rollover_due_subscriptions().await.unwrap();
}

/// The one closed journal the fixture produced, read through the customer
/// list — the id the detail and staff calls then exercise.
async fn the_journal(world: &World) -> Value {
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/org/plan/journals",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let journals = body["data"]["journals"].as_array().unwrap();
    assert_eq!(journals.len(), 1, "{body}");
    journals[0].clone()
}

#[tokio::test]
async fn the_customer_lists_and_reads_its_journals() {
    let Some(world) = world().await else { return };
    let version = publish(&world, "pro", 2500).await;
    bound_and_active(&world, version).await;
    close_period(&world).await;

    // The list bills the closed period: one journal, posted unpaid — the
    // orderless activation left no prepay to settle it.
    let journal = the_journal(&world).await;
    assert_eq!(journal["state"].as_str().unwrap(), "posted");
    assert_eq!(journal["periodIndex"].as_i64().unwrap(), 0);
    assert_eq!(journal["totalMinor"].as_i64().unwrap(), 2500);
    let journal_id = journal["journalId"].as_str().unwrap();

    // The detail carries the lines — the base fee the close billed.
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/org/plan/journals/{journal_id}"),
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lines = body["data"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"].as_str().unwrap(), "baseFee");
    assert_eq!(lines[0]["amountMinor"].as_i64().unwrap(), 2500);
}

#[tokio::test]
async fn the_staff_lists_and_reads_journals() {
    let Some(world) = world().await else { return };
    let version = publish(&world, "pro", 2500).await;
    let customer_plan_id = bound_and_active(&world, version).await;
    close_period(&world).await;
    let journal_id = the_journal(&world).await["journalId"]
        .as_str()
        .unwrap()
        .to_owned();

    // The staff list scopes to the state the dunning work list cares about.
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/business/journals?state=posted",
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let journals = body["data"]["journals"].as_array().unwrap();
    assert!(
        journals.iter().any(|j| j["journalId"] == journal_id),
        "{body}"
    );

    // ... and to the customer plan that owns it.
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/journals?customerPlanId={customer_plan_id}"),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["journals"][0]["journalId"].as_str().unwrap(),
        journal_id
    );

    // The staff detail is the same journal the customer reads.
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/journals/{journal_id}"),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["journalId"].as_str().unwrap(), journal_id);
    assert_eq!(body["data"]["totalMinor"].as_i64().unwrap(), 2500);
}

#[tokio::test]
async fn the_staff_void_nets_the_journal_to_zero() {
    let Some(world) = world().await else { return };
    let version = publish(&world, "pro", 2500).await;
    bound_and_active(&world, version).await;
    close_period(&world).await;
    let journal_id = the_journal(&world).await["journalId"]
        .as_str()
        .unwrap()
        .to_owned();

    // `FundsWrite`: the void appends reversing lines, the journal nets to
    // zero, and the audit keeps the reason.
    let (status, body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/business/journals/{journal_id}/void"),
        json!({"idempotencyKey": format!("void-{}", Uuid::new_v4()), "reason": "billing correction"}),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "void");
    assert_eq!(
        body["data"]["voidReason"].as_str().unwrap(),
        "billing correction"
    );
    let lines = body["data"]["lines"].as_array().unwrap();
    let sum: i64 = lines
        .iter()
        .map(|l| l["amountMinor"].as_i64().unwrap())
        .sum();
    assert_eq!(sum, 0);

    // The customer read sees the standing void — history, never edited away.
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/org/plan/journals/{journal_id}"),
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "void");
}

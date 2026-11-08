//! Financial facts, cost coverage and reconciliation over HTTP (issue #613,
//! T22): the staff read model answers the eight categories under
//! `ReportsRead`, the manual run and exception close/repair run under
//! `FundsWrite` with `finance_audit`, and the customer's
//! `/api/v1/org/usage-report` projects the same read so its total reconciles
//! to the business sum. Every endpoint carries one integration test. Needs
//! DATABASE_URL; skips without it.

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

const PASSWORD: &str = "financial facts http fixture password";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("financial_facts_http")
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
/// business context, and a registered customer stands ready.
struct World {
    app: Router,
    #[allow(dead_code)]
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
        "facts_customer_{}@example.com",
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

/// The business facts endpoint answers the eight categories under
/// `ReportsRead` — an empty world reads zeros, never a missing field, so the
/// shape the customer projection shares is fixed.
#[tokio::test]
async fn business_facts_answers_the_eight_categories() {
    let Some(world) = world().await else { return };
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/business/financial-facts",
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = &body["data"];
    for key in [
        "usageMinor",
        "ratedMinor",
        "entitlementMinor",
        "fundingMinor",
        "earnedMinor",
        "collectedMinor",
        "refundMinor",
        "receivableMinor",
    ] {
        assert!(data[key].is_i64(), "{key} missing: {body}");
    }
}

/// The customer's own projection is the same read scoped to its organization:
/// `/api/v1/org/usage-report` answers the eight categories under the caller's
/// credential, so the customer total reconciles to the business sum for that
/// organization by construction.
#[tokio::test]
async fn customer_projection_reconciles_to_business_sum() {
    let Some(world) = world().await else { return };
    // The customer's own read.
    let (status, customer_body) = call(
        &world.app,
        "GET",
        "/api/v1/org/usage-report",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{customer_body}");
    // The business read scoped to the same organization.
    let (status, business_body) = call(
        &world.app,
        "GET",
        &format!(
            "/api/v1/business/financial-facts?organizationId={}",
            world.customer_org
        ),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{business_body}");
    assert_eq!(
        customer_body["data"], business_body["data"],
        "the customer projection and the business sum must be one figure"
    );
}

/// A customer session is refused the business read — `ReportsRead` is a
/// staff capability, not a customer's.
#[tokio::test]
async fn a_customer_is_refused_the_business_read() {
    let Some(world) = world().await else { return };
    let (status, _body) = call(
        &world.app,
        "GET",
        "/api/v1/business/financial-facts",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// The cost-coverage read answers the `cost_basis` breakdown and the
/// provider/day grain under `ReportsRead`.
#[tokio::test]
async fn cost_coverage_answers_the_breakdown() {
    let Some(world) = world().await else { return };
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/business/cost-coverage",
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data = &body["data"];
    for key in [
        "realAttempt",
        "cacheReuse",
        "estimate",
        "missing",
        "providerReconciled",
        "failedAttempts",
        "byProviderDay",
    ] {
        assert!(data.get(key).is_some(), "{key} missing: {body}");
    }
}

/// A staff-requested manual run lands the run row, lists in the run history,
/// and its exceptions are readable — `FundsWrite` + finance_audit.
#[tokio::test]
async fn a_manual_run_records_and_lists() {
    let Some(world) = world().await else { return };
    let today = time::OffsetDateTime::now_utc().date();
    let day = format!("{today}");
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/business/reconciliation-runs",
        json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "periodStart": day,
            "periodEnd": day,
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let run_id = body["data"]["runId"].as_str().unwrap();
    assert_eq!(body["data"]["kind"].as_str().unwrap(), "manual");
    assert!(body["data"]["state"].is_string(), "{body}");

    let snapshot_id = body["data"]["latestSnapshotId"].as_str().unwrap();
    assert_eq!(body["data"]["coverage"]["version"], 1);
    let history = format!("/api/v1/business/reconciliation-runs/{run_id}/snapshots");
    let (status, snapshots) =
        call(&world.app, "GET", &history, Value::Null, Some(&world.owner)).await;
    assert_eq!(status, StatusCode::OK, "{snapshots}");
    assert_eq!(snapshots["data"][0]["snapshotId"], snapshot_id);
    assert!(snapshots["data"][0]["evidence"].is_null());
    let detail = format!("{history}/{snapshot_id}");
    let (status, captured) =
        call(&world.app, "GET", &detail, Value::Null, Some(&world.owner)).await;
    assert_eq!(status, StatusCode::OK, "{captured}");
    assert!(captured["data"]["evidence"].is_object());
    for route in [&history, &detail] {
        assert_eq!(
            call(&world.app, "GET", route, Value::Null, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&world.app, "GET", route, Value::Null, Some(&world.customer))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(
            &world.app,
            "GET",
            &format!(
                "/api/v1/business/reconciliation-runs/{}/snapshots/{snapshot_id}",
                Uuid::new_v4()
            ),
            Value::Null,
            Some(&world.owner)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    // The run lists and its detail reads.
    let (status, list) = call(
        &world.app,
        "GET",
        "/api/v1/business/reconciliation-runs",
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert!(
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["runId"] == run_id),
        "the run lists: {list}"
    );
    let (status, _run) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/reconciliation-runs/{run_id}"),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _exceptions) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/reconciliation-runs/{run_id}/exceptions"),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// A manual run flags a settled turn's untracked cost as `missingCost`; staff
/// waives it naming why the drift is not a defect — the close is audited.
#[tokio::test]
async fn an_exception_waives_with_a_reason() {
    let Some(world) = world().await else { return };
    // Plant a settled usage row with untracked cost under the customer's tenant.
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(world.customer_org)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.usage_records \
         (request_id, tenant_id, model, channel, price_version, kind, entry_id, \
          input_tokens, output_tokens, charged_minor, freeze_minor, status, cost_basis) \
         VALUES ('req-http-nocost', $1, 'm', 'c', 1, 'usage', $2, 1, 1, 10, 10, 'settled', 'missing')",
    )
    .bind(&tenant)
    .bind(Uuid::new_v4())
    .execute(&world.pool)
    .await
    .unwrap();

    let today = time::OffsetDateTime::now_utc().date();
    let day = format!("{today}");
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/business/reconciliation-runs",
        json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "periodStart": day,
            "periodEnd": day,
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let run_id = body["data"]["runId"].as_str().unwrap();
    let (status, exceptions) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/reconciliation-runs/{run_id}/exceptions"),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exceptions}");
    let exception = exceptions["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["sourceRef"] == "req-http-nocost")
        .expect("the untracked turn is flagged")
        .clone();
    assert_eq!(exception["kind"].as_str().unwrap(), "missing_cost");
    assert_eq!(exception["state"].as_str().unwrap(), "open");
    let exception_id = exception["exceptionId"].as_str().unwrap();

    // Waive it — the drift is not a defect (a pre-billing-row historical turn).
    let (status, body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/business/reconciliation-exceptions/{exception_id}/waive"),
        json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "resolution": "historical row predating upstream cost tracking",
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "waived");
    assert!(body["data"]["resolution"].is_string());
    // A second close on a settled exception is a not-found, never silent.
    let (status, _body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/business/reconciliation-exceptions/{exception_id}/waive"),
        json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "resolution": "again",
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A `reclassifyCost` repair rewrites the usage row's `costBasis` with a
/// before/after snapshot and an audit row — the explicit financial action,
/// never a balance overwrite.
#[tokio::test]
async fn a_repair_reclassifies_cost_with_an_audit() {
    let Some(world) = world().await else { return };
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(world.customer_org)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.usage_records \
         (request_id, tenant_id, model, channel, price_version, kind, entry_id, \
          input_tokens, output_tokens, charged_minor, freeze_minor, status, cost_basis) \
         VALUES ('req-http-repair', $1, 'm', 'c', 1, 'usage', $2, 1, 1, 10, 10, 'settled', 'missing')",
    )
    .bind(&tenant)
    .bind(Uuid::new_v4())
    .execute(&world.pool)
    .await
    .unwrap();
    let today = time::OffsetDateTime::now_utc().date();
    let day = format!("{today}");
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/business/reconciliation-runs",
        json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "periodStart": day,
            "periodEnd": day,
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let run_id = body["data"]["runId"].as_str().unwrap();
    let exception_id: String = sqlx::query_scalar(
        "SELECT exception_id FROM oxsum.reconciliation_exceptions \
         WHERE run_id=$1 AND source_ref='req-http-repair'",
    )
    .bind(Uuid::parse_str(run_id).unwrap())
    .fetch_one(&world.pool)
    .await
    .map(|id: Uuid| id.to_string())
    .unwrap();

    let (status, body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/business/reconciliation-exceptions/{exception_id}/repairs"),
        json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "action": "reclassifyCost",
            "reason": "provider bill arrived and reconciled the figure",
            "patch": {"costBasis": "provider_reconciled", "upstreamCostMinor": 7},
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["action"].as_str().unwrap(), "reclassify_cost");
    assert_eq!(
        body["data"]["before"]["costBasis"].as_str().unwrap(),
        "missing"
    );
    assert_eq!(
        body["data"]["after"]["costBasis"].as_str().unwrap(),
        "provider_reconciled"
    );
    assert!(body["data"]["auditRef"].is_string(), "{body}");
    // The usage row's basis moved — the corrective write landed.
    let basis: String = sqlx::query_scalar(
        "SELECT cost_basis FROM oxsum.usage_records WHERE request_id='req-http-repair'",
    )
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert_eq!(basis, "provider_reconciled");
    // The audit row stands.
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.finance_audit WHERE action='reconciliation.repair'",
    )
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert!(audited >= 1);
}

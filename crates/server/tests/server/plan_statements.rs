//! Plan statements over HTTP (issue #612, T21): the customer reads its own
//! per-period billing documents (`/api/v1/org/statements`), the staff lists
//! cross-organization (`/api/v1/business/statements`, `ReportsRead`) and runs
//! release-review under `FundsWrite` — every endpoint carries one integration
//! test, and the release walks a `pastDue` statement back to `issued`.
//! Needs DATABASE_URL; skips without it.

#[path = "../../../core/tests/support/legacy_plan.rs"]
mod legacy_plan;

#[path = "../../../core/tests/support/plan_payment.rs"]
mod plan_payment;
#[path = "../../../core/tests/support/integration.rs"]
mod setup;

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

const PASSWORD: &str = "statement http fixture password";

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
    crate::support::login(app, email, PASSWORD).await
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
    let u = oxsum_testkit::suite_db_url("plan_statements_http")?;
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
        "plan_stmt_customer_{}@example.com",
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

/// Seeds a retained v1 sold plan and answers its version id.
async fn publish(world: &World, name: &str, price: i64) -> Uuid {
    legacy_plan::publish(
        &world.db,
        world.db.default_business().await.unwrap(),
        &oxsum_core::NewSoldPlan {
            plan_id: None,
            name: name.into(),
            price_minor: price,
            currency: "USD".into(),
            quota_minor: Some(10_000_000),
            period_kind: oxsum_core::PeriodKind::CalendarMonth,
            overage_enabled: true,
            sort_order: 0,
        },
    )
    .await
    .unwrap()
    .plan_version_id
}

/// Bind and activate using a real quoted paid fixture. The close helper seeds
/// a historical partial journal for HTTP reporting and dunning assertions.
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
        .activate_plan_period(
            plan_payment::paid_order(
                &world.db,
                &world.owner,
                world.customer_org,
                customer_plan_id,
            )
            .await
            .order_id,
            customer_plan_id,
        )
        .await
        .unwrap();
    customer_plan_id
}

/// Forces the customer's open period past its boundary and runs the rollover
/// — the close that issues the period's statement beside its journal.
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
    let plan: Uuid = sqlx::query_scalar("SELECT customer_plan_id FROM oxsum.customer_plans WHERE organization_id=$1 AND state='active'")
        .bind(world.customer_org).fetch_one(&world.pool).await.unwrap();
    plan_payment::legacy_close(&world.db, world.customer_org, plan, 1000).await;
    world.db.rollover_due_subscriptions().await.unwrap();
}

/// The one closed statement the fixture produced, read through the customer
/// list — the id the detail and staff calls then exercise.
async fn the_statement(world: &World) -> Value {
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/org/statements",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let statements = body["data"]["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 1, "{body}");
    statements[0].clone()
}

#[tokio::test]
async fn the_customer_lists_and_reads_its_statements() {
    let Some(world) = world().await else { return };
    let version = publish(&world, "pro", 2500).await;
    bound_and_active(&world, version).await;
    close_period(&world).await;

    // The paid base fee reconciles; separate overage leaves this statement issued.
    let statement = the_statement(&world).await;
    assert_eq!(statement["state"].as_str().unwrap(), "issued");
    assert_eq!(statement["periodIndex"].as_i64().unwrap(), 0);
    assert_eq!(statement["totalMinor"].as_i64().unwrap(), 3500);
    assert_eq!(statement["paidMinor"].as_i64().unwrap(), 2500);
    assert!(statement["dueAt"].is_string());
    let statement_id = statement["statementId"].as_str().unwrap();

    // The detail carries the lines — the base fee restated as a `planFee`.
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/org/statements/{statement_id}"),
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lines = body["data"]["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l["kind"].as_str().unwrap() == "planFee"
                && l["amountMinor"].as_i64().unwrap() == 2500)
    );
}

#[tokio::test]
async fn the_staff_lists_and_reads_statements() {
    let Some(world) = world().await else { return };
    let version = publish(&world, "pro", 2500).await;
    bound_and_active(&world, version).await;
    close_period(&world).await;
    let statement_id = the_statement(&world).await["statementId"]
        .as_str()
        .unwrap()
        .to_owned();

    // The staff list scopes to the customer organization.
    let (status, body) = call(
        &world.app,
        "GET",
        &format!(
            "/api/v1/business/statements?organizationId={}",
            world.customer_org
        ),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["statements"][0]["statementId"]
            .as_str()
            .unwrap(),
        statement_id
    );

    // The staff detail is the same statement the customer reads.
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/statements/{statement_id}"),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["statementId"].as_str().unwrap(), statement_id);
    assert_eq!(body["data"]["totalMinor"].as_i64().unwrap(), 3500);
}

#[tokio::test]
async fn the_staff_release_review_restores_the_statement() {
    let Some(world) = world().await else { return };
    let version = publish(&world, "pro", 2500).await;
    bound_and_active(&world, version).await;
    close_period(&world).await;
    let statement_id = the_statement(&world).await["statementId"]
        .as_str()
        .unwrap()
        .to_owned();

    // Drive the statement past due: back its due date, run the sweep.
    sqlx::query(
        "UPDATE oxsum.plan_statements SET due_at = now() - interval '1 day' \
         WHERE statement_id=$1",
    )
    .bind(Uuid::parse_str(&statement_id).unwrap())
    .execute(&world.pool)
    .await
    .unwrap();
    world.db.run_statement_past_due(50).await.unwrap();
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/business/statements?state=pastDue",
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["data"]["statements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["statementId"] == statement_id)
    );

    // `FundsWrite`: the release restores `pastDue → issued`, re-opening the
    // grace window, and the audit keeps the reason.
    let (status, body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/business/statements/{statement_id}/release-review"),
        json!({
            "idempotencyKey": format!("release-{}", Uuid::new_v4()),
            "action": "extendGrace",
            "reason": "customer called; payment in flight",
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "issued");

    let audit: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM oxsum.finance_audit \
         WHERE action='statement.release' AND target=$1",
    )
    .bind(&statement_id)
    .fetch_optional(&world.pool)
    .await
    .unwrap();
    assert!(audit.is_some(), "the release is audited");
}

#[tokio::test]
async fn financial_void_requires_staff_and_replays_the_exact_request_once() {
    let Some(world) = world().await else { return };
    let version = publish(&world, "financial review", 2500).await;
    bound_and_active(&world, version).await;
    close_period(&world).await;
    let statement = the_statement(&world).await;
    let id = statement["statementId"].as_str().unwrap();
    let path = format!("/api/v1/business/statements/{id}/release-review");
    let key = Uuid::new_v4().to_string();
    let terms =
        json!({"action":"void","reason":"original journal correction","idempotencyKey":key});
    let (status, body) = call(
        &world.app,
        "POST",
        &path,
        terms.clone(),
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let mut invalid = terms.clone();
    invalid["reason"] = json!("");
    let (status, body) = call(&world.app, "POST", &path, invalid, Some(&world.owner)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    for _ in 0..2 {
        let (status, body) =
            call(&world.app, "POST", &path, terms.clone(), Some(&world.owner)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["state"], "void");
        assert_eq!(body["data"]["totalMinor"], 0);
    }
    let (status, body) = call(
        &world.app,
        "GET",
        &format!(
            "/api/v1/org/plan/journals/{}",
            statement["journalId"].as_str().unwrap()
        ),
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"], "void");
    let mut changed = terms;
    changed["reason"] = json!("different terms");
    let (status, body) = call(&world.app, "POST", &path, changed, Some(&world.owner)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let audit: (i64,Option<String>)=sqlx::query_as("SELECT COUNT(*)::bigint,MAX(detail->>'reversalEntryId') FROM oxsum.finance_audit WHERE action='statement.release' AND idempotency_key=$1").bind(&key).fetch_one(&world.pool).await.unwrap();
    assert_eq!(audit.0, 1);
    assert!(audit.1.is_some());
}

#[tokio::test]
async fn statement_payment_quote_is_personal_scoped_and_refuses_historical_duplicate_usage() {
    let Some(w) = world().await else {
        return;
    };
    let version = publish(&w, "repayment HTTP source", 2500).await;
    bound_and_active(&w, version).await;
    close_period(&w).await;
    let statement = the_statement(&w).await;
    let id = statement["statementId"].as_str().unwrap();
    let path = format!("/api/v1/org/statements/{id}/payment-quote");
    let (status, body) = call(&w.app, "GET", &path, Value::Null, Some(&w.customer)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["reason"], "historicalUsageReviewRequired");
    assert_eq!(body["data"]["outstandingMinor"], 1000);
    assert!(body["data"]["terms"].is_null());
    assert_eq!(
        call(
            &w.app,
            "GET",
            &format!("{path}?amountMinor=0"),
            Value::Null,
            Some(&w.customer)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(&w.app, "GET", &path, Value::Null, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &w.app,
            "GET",
            &format!("/api/v1/org/statements/{}/payment-quote", Uuid::new_v4()),
            Value::Null,
            Some(&w.customer)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

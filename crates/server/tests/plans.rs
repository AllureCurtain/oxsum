//! Sold plans and the paid plan lifecycle over HTTP (issue #610, T19): the
//! staff publishes a versioned catalogue, the customer binds and buys through
//! a `planPeriod` order, the boundary lifecycle (renewal, cancel, change) runs
//! under the personal session, and a timer alone never issues an unpaid
//! period. Needs DATABASE_URL; skips without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core/tests/support/plan_payment.rs"]
mod plan_payment;
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

const PASSWORD: &str = "plan http fixture password";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("plans_http")
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
    // The business owner: `setup::owner` mints the business once per shared
    // database — its OnceCell guards the second `world()` against the
    // already-initialized conflict — and the returned session token is already
    // in business context, the staff routes' credential. It must run before
    // the registration fixture, which stamps `initialized_at` itself.
    let owner = setup::owner(&db).await;
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Open, None).with_secret(SecretKey::from_bytes([7; 32])),
    );

    // The customer: register and log in — its cookie is the `/org/plan`
    // surface's credential. The email is unique per `world()` because the
    // scratch database is shared across the file's tests.
    let email = format!(
        "customer_{}@example.com",
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
        owner,
        customer_org,
        customer,
    })
}

/// Publishes a sold plan through the staff route and answers its version id.
async fn publish(world: &World, name: &str, price: i64, period_kind: &str) -> Uuid {
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/business/plans",
        json!({
            "name": name,
            "priceMinor": price,
            "currency": "USD",
            "quotaMinor": 10_000_000,
            "periodKind": period_kind,
            "overageEnabled": false,
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Uuid::parse_str(body["data"]["planVersionId"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn the_catalogue_publishes_and_binds_over_http() {
    let Some(world) = world().await else { return };

    // The customer reads the catalogue — published versions only.
    let version = publish(&world, "pro", 2500, "calendarMonth").await;
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/plans",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(names.contains(&"pro"), "{names:?}");
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/plans/{version}"),
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["priceMinor"].as_i64().unwrap(), 2500);
    assert_eq!(
        body["data"]["periodKind"].as_str().unwrap(),
        "calendarMonth"
    );

    // The staff binds the priced plan: pending_payment, no entitlement yet.
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/business/customers/{}/plan", world.customer_org),
        json!({"planVersionId": version}),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "pendingPayment");
    let customer_plan_id =
        Uuid::parse_str(body["data"]["customerPlanId"].as_str().unwrap()).unwrap();

    // The customer's own read shows the pending plan, no periods.
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/org/plan",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["plan"]["state"].as_str().unwrap(),
        "pendingPayment"
    );
    assert!(body["data"]["periods"].as_array().unwrap().is_empty());

    world
        .db
        .bootstrap_collection_terms(&oxsum_core::CollectionCurrency {
            code: "USD".into(),
            rate_per_credit: "1".into(),
        })
        .await
        .unwrap();
    let (status, quote) = call(
        &world.app,
        "GET",
        "/api/v1/org/plan/payment-quote",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(
        quote["data"]["terms"]["customerPlanId"],
        customer_plan_id.to_string()
    );
    assert_eq!(quote["data"]["terms"]["planVersionId"], version.to_string());
    assert_eq!(quote["data"]["terms"]["periodIndex"], 0);
    assert!(quote["data"]["terms"]["periodStart"].is_null());
    assert_eq!(quote["data"]["fingerprint"].as_str().unwrap().len(), 64);
    let (status, _) = call(
        &world.app,
        "GET",
        "/api/v1/org/plan/payment-quote",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &world.app,
        "GET",
        "/api/v1/org/plan/payment-quote",
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A second live bind conflicts — one active plan per customer.
    let (status, _) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/business/customers/{}/plan", world.customer_org),
        json!({"planVersionId": version}),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Activation under an order's authorization opens period 0 — the staff
    // read shows the live plan and its `paymentOrder`-authorized period.
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
    let (status, body) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/customers/{}/plan", world.customer_org),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["plan"]["state"].as_str().unwrap(), "active");
    let periods = body["data"]["periods"].as_array().unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(
        periods[0]["authorizationKind"].as_str().unwrap(),
        "paymentOrder"
    );
    assert_eq!(periods[0]["state"].as_str().unwrap(), "active");
    assert_eq!(periods[0]["accountingPolicyVersion"], 2);
    let (status, journals) = call(
        &world.app,
        "GET",
        "/api/v1/org/plan/journals",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{journals}");
    assert_eq!(journals["data"]["journals"].as_array().unwrap().len(), 1);
    assert_eq!(
        journals["data"]["journals"][0]["accountingPolicyVersion"],
        2
    );
    assert_eq!(journals["data"]["journals"][0]["state"], "paid");
    let (status, facts) = call(
        &world.app,
        "GET",
        "/api/v1/org/usage-report",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{facts}");
    assert_eq!(facts["data"]["earnedMinor"], 0);
    assert_eq!(facts["data"]["deferredMinor"], facts["data"]["billedMinor"]);
    assert_eq!(
        facts["data"]["collectedMinor"],
        facts["data"]["billedMinor"]
    );
}

#[tokio::test]
async fn a_free_plan_binds_on_a_complimentary_grant() {
    let Some(world) = world().await else { return };
    let version = publish(&world, "community", 0, "calendarMonth").await;

    // A free plan with no grant is refused — explicit, never fabricated paid.
    let (status, _) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/business/customers/{}/plan", world.customer_org),
        json!({"planVersionId": version}),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let grant = Uuid::new_v4();
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/business/customers/{}/plan", world.customer_org),
        json!({"planVersionId": version, "complimentaryGrant": grant}),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["state"].as_str().unwrap(), "active");
    assert!(!body["data"]["subscriptionId"].is_null());
}

#[tokio::test]
async fn the_customer_cancels_and_schedules_at_the_boundary() {
    let Some(world) = world().await else { return };
    let basic = publish(&world, "basic", 1000, "calendarMonth").await;
    let pro = publish(&world, "pro", 2500, "calendarMonth").await;

    // Bind + activate under a complimentary grant on a priced plan is refused;
    // bind pending, then activate through the db the way a fulfilled order does.
    let (status, body) = call(
        &world.app,
        "PUT",
        &format!("/api/v1/business/customers/{}/plan", world.customer_org),
        json!({"planVersionId": basic}),
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

    // The customer books a change to `pro`: scheduled, not applied mid-period.
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/org/plan/change",
        json!({"planVersionId": pro}),
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["planVersionId"].as_str().unwrap(),
        basic.to_string()
    );
    assert_eq!(
        body["data"]["scheduledChange"].as_str().unwrap(),
        pro.to_string()
    );

    // Cancelling clears the scheduled change and marks the boundary.
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/org/plan/cancel",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["cancelAtPeriodEnd"].as_bool().unwrap());
    assert!(body["data"]["scheduledChange"].is_null());
}

#[tokio::test]
async fn staff_routes_refuse_a_customer_credential() {
    let Some(world) = world().await else { return };
    // The customer's own session is not staff: catalogue and binding writes
    // refuse, while its own plan read still answers.
    let (status, _) = call(
        &world.app,
        "POST",
        "/api/v1/business/plans",
        json!({"name":"x","priceMinor":1,"currency":"USD","quotaMinor":1,"periodKind":"calendarMonth"}),
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(
        &world.app,
        "GET",
        "/api/v1/org/plan",
        Value::Null,
        Some(&world.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["plan"].is_null());
}

//! Plan fee journals over HTTP (issue #611, T20): the customer reads its own
//! per-period invoices (`/api/v1/org/plan/journals`), the staff lists
//! cross-organization (`/api/v1/business/journals`, `ReportsRead`) and voids
//! under `FundsWrite` — every endpoint carries one integration test, and the
//! void walks a settled journal to `void` with its reversing lines.
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

const PASSWORD: &str = "journal http fixture password";

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
    let u = oxsum_testkit::suite_db_url("journals_http")?;
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

#[tokio::test]
async fn correction_preview_checks_authority_source_and_exact_amounts_over_http() {
    let Some(w) = world().await else {
        return;
    };
    let sold =
        w.db.publish_sold_plan(
            w.db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: format!("preview-{}", Uuid::new_v4()),
                price_minor: 2_500_000,
                currency: "USD".into(),
                quota_minor: Some(10_000_000),
                period_kind: oxsum_core::PeriodKind::Anniversary,
                overage_enabled: true,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    bound_and_active(&w, sold.plan_version_id).await;
    let journal =
        w.db.plan_journals(w.customer_org, None, 50)
            .await
            .unwrap()
            .journals
            .remove(0);
    let detail =
        w.db.plan_journal(w.customer_org, journal.journal_id)
            .await
            .unwrap()
            .unwrap();
    let base = detail
        .lines
        .iter()
        .find(|line| line.kind == oxsum_core::JournalLineKind::BaseFee)
        .unwrap();
    let path = format!(
        "/api/v1/business/journals/{}/correction-preview",
        journal.journal_id
    );
    let request = json!({"lines": [{"sourceLineId": base.line_id, "amountMinor": 2_000_001}]});
    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(w.customer.as_str()), StatusCode::FORBIDDEN),
    ] {
        let (status, body) = call(&w.app, "POST", &path, request.clone(), token).await;
        assert_eq!(status, expected, "{body}");
    }
    let (status, first) = call(&w.app, "POST", &path, request.clone(), Some(&w.owner)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (_, second) = call(&w.app, "POST", &path, request, Some(&w.owner)).await;
    assert_eq!(
        first, second,
        "preview is stable until source or completed service changes"
    );
    assert_eq!(first["data"]["correctedTotalMinor"], 2_000_001);
    assert_eq!(first["data"]["successorReceivableMinor"], 2_000_001);
    assert_eq!(first["data"]["originalPayableMinor"], 2_500_000);
    assert_eq!(first["data"]["successorCollectedMinor"], 0);
    assert_eq!(first["data"]["cashMovementMinor"], 0);
    assert_eq!(
        first["data"]["planVersionId"],
        sold.plan_version_id.to_string()
    );
    for invalid in [
        json!({"lines": [{"sourceLineId": base.line_id, "amountMinor": 2_500_001}]}),
        json!({"lines": [{"sourceLineId": Uuid::new_v4(), "amountMinor": 1}]}),
        json!({"lines": [{"sourceLineId": base.line_id, "amountMinor": 1.5}]}),
        json!({"lines": [], "force": true}),
    ] {
        let (status, body) = call(&w.app, "POST", &path, invalid, Some(&w.owner)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    let after =
        w.db.plan_journal(w.customer_org, journal.journal_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        serde_json::to_value(detail).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert_eq!(
        w.db.plan_journals(w.customer_org, None, 50)
            .await
            .unwrap()
            .journals
            .len(),
        1
    );
}

#[tokio::test]
async fn correction_apply_http_retains_exact_response_and_current_authority() {
    let Some(w) = world().await else {
        return;
    };
    let sold =
        w.db.publish_sold_plan(
            w.db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: format!("correct-{}", Uuid::new_v4()),
                price_minor: 2_500_000,
                currency: "USD".into(),
                quota_minor: Some(10_000_000),
                period_kind: oxsum_core::PeriodKind::Anniversary,
                overage_enabled: true,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    bound_and_active(&w, sold.plan_version_id).await;
    let journal =
        w.db.plan_journals(w.customer_org, None, 50)
            .await
            .unwrap()
            .journals
            .remove(0);
    let detail =
        w.db.plan_journal(w.customer_org, journal.journal_id)
            .await
            .unwrap()
            .unwrap();
    let path = format!("/api/v1/business/journals/{}/correct", journal.journal_id);
    let preview_path = format!(
        "/api/v1/business/journals/{}/correction-preview",
        journal.journal_id
    );
    let lines = json!([{"sourceLineId": detail.lines[0].line_id, "amountMinor": 2_000_001}]);
    let (status, preview) = call(
        &w.app,
        "POST",
        &preview_path,
        json!({"lines":lines}),
        Some(&w.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let request = json!({"lines":lines,"sourceRevision":preview["data"]["sourceRevision"],"reason":"Correct original service fee","idempotencyKey":Uuid::new_v4()});
    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(w.customer.as_str()), StatusCode::FORBIDDEN),
    ] {
        let (status, body) = call(&w.app, "POST", &path, request.clone(), token).await;
        assert_eq!(status, expected, "{body}");
    }
    let mut bad = request.clone();
    bad["force"] = json!(true);
    assert_eq!(
        call(&w.app, "POST", &path, bad, Some(&w.owner)).await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, first) = call(&w.app, "POST", &path, request.clone(), Some(&w.owner)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, retry) = call(&w.app, "POST", &path, request.clone(), Some(&w.owner)).await;
    assert_eq!(status, StatusCode::OK, "{retry}");
    assert_eq!(first, retry);
    assert_eq!(first["data"]["preview"]["correctedTotalMinor"], 2_000_001);
    assert_eq!(first["data"]["preview"]["originalPayableMinor"], 2_500_000);
    let new = first["data"]["journalId"].as_str().unwrap();
    let (status, customer) = call(
        &w.app,
        "GET",
        &format!("/api/v1/org/plan/journals/{new}"),
        Value::Null,
        Some(&w.customer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{customer}");
    assert_eq!(
        customer["data"]["replacesJournalId"],
        json!(journal.journal_id)
    );
    let mut changed = request;
    changed["reason"] = json!("Different intent");
    assert_eq!(
        call(&w.app, "POST", &path, changed, Some(&w.owner)).await.0,
        StatusCode::CONFLICT
    );
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
    let plan: Uuid = sqlx::query_scalar("SELECT customer_plan_id FROM oxsum.customer_plans WHERE organization_id=$1 AND state='active'")
        .bind(world.customer_org).fetch_one(&world.pool).await.unwrap();
    plan_payment::legacy_close(&world.db, world.customer_org, plan, 1000).await;
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

    // The paid base fee reconciles; overage leaves a posted receivable.
    let journal = the_journal(&world).await;
    assert_eq!(journal["state"].as_str().unwrap(), "posted");
    assert_eq!(journal["periodIndex"].as_i64().unwrap(), 0);
    assert_eq!(journal["totalMinor"].as_i64().unwrap(), 3500);
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
    let mut amounts = lines
        .iter()
        .map(|line| {
            (
                line["kind"].as_str().unwrap(),
                line["amountMinor"].as_i64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    amounts.sort_unstable();
    assert_eq!(amounts, vec![("baseFee", 2500), ("usageOverage", 1000)]);
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
    assert_eq!(body["data"]["totalMinor"].as_i64().unwrap(), 3500);
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

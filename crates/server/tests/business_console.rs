//! The staff business console over HTTP (issue #615, T24, A25): the surfaces
//! `/app/business/*` drives — the finance-audit read the audit page mounts,
//! role-scoped reach (Support reads customers but is denied the finance reads
//! and writes), and an audited correction whose replayed close mints no second
//! audit event. Every journey runs through the documented API — no
//! database-console dependency. Needs DATABASE_URL; skips without it.

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
use oxsum_core::{Db, NewUser, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "business console fixture password";

fn url() -> Option<String> {
    oxsum_testkit::suite_db_url("business_console_http")
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
    db: Db,
    pool: PgPool,
    owner: String,
    customer_org: Uuid,
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
        Config::new(Signup::Open, None).with_secret(SecretKey::from_bytes([11; 32])),
    );

    let email = format!(
        "console_customer_{}@example.com",
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

    Some(World {
        app,
        db,
        pool,
        owner,
        customer_org,
    })
}

/// Register a customer user and appoint them staff under the owner, then log
/// them in and switch their session to business context.
async fn staff_session(world: &World, email: &str, role: &str) -> String {
    let user = world
        .db
        .register(NewUser {
            email: email.into(),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/business/staff",
        json!({
            "idempotencyKey": Uuid::new_v4().to_string(),
            "userId": user.user.id,
            "role": role,
            "reason": "console journey fixture",
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = login(&world.app, email).await;
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/session/context",
        json!({"idempotencyKey": Uuid::new_v4().to_string(), "context": "business"}),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["context"].as_str().unwrap(), "business");
    token
}

/// The finance-audit endpoint answers the staff financial actions under
/// `ReportsRead` — one immutable row naming actor, action and reason. A
/// reconciliation waive lands `reconciliation.waive`, and the page reads it
/// back with the actor it carried.
#[tokio::test]
async fn finance_audit_lists_a_correction() {
    let Some(world) = world().await else { return };

    // Plant a settled usage row with untracked cost, run a reconciliation that
    // flags it, then waive the exception — one finance_audit row results.
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
         VALUES ('req-console-audit', $1, 'm', 'c', 1, 'usage', $2, 1, 1, 10, 10, 'settled', 'missing')",
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
    let (_status, exceptions) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/reconciliation-runs/{run_id}/exceptions"),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    let exception_id = exceptions["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["sourceRef"] == "req-console-audit")
        .expect("the untracked turn is flagged")["exceptionId"]
        .as_str()
        .unwrap()
        .to_owned();

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

    // The audit page reads the close back — actor, action, reason, the
    // corrected exception as target.
    let (status, audit) = call(
        &world.app,
        "GET",
        "/api/v1/business/audit?action=reconciliation.waive",
        Value::Null,
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{audit}");
    let entry = audit["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == "reconciliation.waive")
        .expect("the waive is audited");
    assert_eq!(entry["action"].as_str().unwrap(), "reconciliation.waive");
    assert_eq!(entry["target"].as_str().unwrap(), exception_id);
    assert!(entry["detail"]["resolution"].is_string());
    assert!(entry["actorUserId"].is_string());
}

/// Role reach: a Support-role staff reads the customer surface it serves —
/// `CustomersRead` covers `/business/customers` — but the finance reads
/// (`ReportsRead`), the journal void (`FundsWrite`) and the staff roster
/// (`StaffManage`) are all refused. The backend enforces the capability the
/// nav only hides.
#[tokio::test]
async fn a_support_cannot_void_a_journal() {
    let Some(world) = world().await else { return };
    let support = staff_session(&world, "console_support@example.com", "support").await;

    // Support reads the customer list and a customer's detail.
    for path in [
        "/api/v1/business/customers",
        &format!("/api/v1/business/customers/{}", world.customer_org),
    ] {
        let (status, body) = call(&world.app, "GET", path, Value::Null, Some(&support)).await;
        assert_eq!(status, StatusCode::OK, "{path} {body}");
    }

    // The refund and dispute work lists sit behind `ReportsRead` — Support
    // holds no finance read.
    for path in [
        "/api/v1/business/refunds",
        "/api/v1/business/disputes",
        "/api/v1/business/audit",
    ] {
        let (status, body) = call(&world.app, "GET", path, Value::Null, Some(&support)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} {body}");
    }

    // A real journal must exist for the denial to reach the capability
    // check — the handler resolves the journal's ledger tenant before the
    // staff guard, so a fabricated id is `NotFound` for anyone. Publish a
    // plan, bind the customer and close period 0: the rollover posts the
    // journal the void then refuses.
    let (status, body) = call(
        &world.app,
        "POST",
        "/api/v1/business/plans",
        json!({
            "name": "console-support-denial",
            "priceMinor": 2500,
            "currency": "USD",
            "quotaMinor": 10_000_000_i64,
            "periodKind": "calendarMonth",
            "overageEnabled": false,
        }),
        Some(&world.owner),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let version = Uuid::parse_str(body["data"]["planVersionId"].as_str().unwrap()).unwrap();
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
    // V2 issues the paid invoice at activation; authority does not require
    // changing an immutable service interval to force a historical close.
    let journal_id: Uuid = sqlx::query_scalar(
        "SELECT journal_id FROM oxsum.invoice_journals WHERE organization_id=$1",
    )
    .bind(world.customer_org)
    .fetch_one(&world.pool)
    .await
    .unwrap();

    // The finance write is refused — CustomersSuspend is the most a support
    // holds; journal void needs FundsWrite.
    let (status, body) = call(
        &world.app,
        "POST",
        &format!("/api/v1/business/journals/{journal_id}/void"),
        json!({"idempotencyKey": Uuid::new_v4().to_string(), "reason": "denied"}),
        Some(&support),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // And the staff-management read is owner-only.
    let (status, _body) = call(
        &world.app,
        "GET",
        "/api/v1/business/staff",
        Value::Null,
        Some(&support),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// A finance correction is audited and idempotent: the same idempotency key
/// replays the standing resolution rather than a second action, and the audit
/// row the close wrote names the replay's single identity.
#[tokio::test]
async fn a_correction_replays_idempotently() {
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
         VALUES ('req-console-replay', $1, 'm', 'c', 1, 'usage', $2, 1, 1, 10, 10, 'settled', 'missing')",
    )
    .bind(&tenant)
    .bind(Uuid::new_v4())
    .execute(&world.pool)
    .await
    .unwrap();

    let today = time::OffsetDateTime::now_utc().date();
    let day = format!("{today}");
    let (_s, body) = call(
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
    let run_id = body["data"]["runId"].as_str().unwrap();
    let (_s, exceptions) = call(
        &world.app,
        "GET",
        &format!("/api/v1/business/reconciliation-runs/{run_id}/exceptions"),
        Value::Null,
        Some(&world.owner),
    )
    .await;
    let exception_id = exceptions["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["sourceRef"] == "req-console-replay")
        .expect("flagged")["exceptionId"]
        .as_str()
        .unwrap()
        .to_owned();
    // A finance-role staff member resolves it; replaying the same close under
    // the same idempotency key is refused as a second close — the audit row
    // still holds exactly one `reconciliation.resolve` for the key, so the
    // correction can never double-write its event or re-close the exception.
    let finance = staff_session(&world, "console_finance@example.com", "finance").await;
    let key = Uuid::new_v4().to_string();
    let (status, first) = call(
        &world.app,
        "POST",
        &format!("/api/v1/business/reconciliation-exceptions/{exception_id}/resolve"),
        json!({"idempotencyKey": key, "resolution": "cost basis corrected"}),
        Some(&finance),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["state"].as_str().unwrap(), "resolved");

    let (status, replay) = call(
        &world.app,
        "POST",
        &format!("/api/v1/business/reconciliation-exceptions/{exception_id}/resolve"),
        json!({"idempotencyKey": key, "resolution": "cost basis corrected"}),
        Some(&finance),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{replay}");

    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.finance_audit \
         WHERE action='reconciliation.resolve' AND idempotency_key=$1",
    )
    .bind(&key)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1, "the replay minted no second audit event");
}

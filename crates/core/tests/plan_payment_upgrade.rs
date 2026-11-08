//! Upgrade preserves authorized plan history while fencing pending unquoted orders.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/legacy_plan.rs"]
mod legacy_plan;
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{Db, NewSoldPlan, NewUser, PeriodKind, WalletError};
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn historical_authorization_replays_but_pending_legacy_intents_cannot_grant() {
    let Some(url) = oxsum_testkit::suite_db_url("plan_payment_upgrade") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.pool().execute("CREATE SCHEMA oxsum;CREATE TABLE oxsum._migrations(version text PRIMARY KEY,name text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0109_plan_payment_authority" {
            continue;
        }
        let mut tx = db.pool().begin().await.unwrap();
        tx.execute("SET LOCAL search_path=''").await.unwrap();
        tx.execute(std::fs::read_to_string(&file).unwrap().as_str())
            .await
            .unwrap();
        sqlx::query("INSERT INTO oxsum._migrations(version,name) VALUES($1,$1)")
            .bind(version)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: "plan-upgrade@example.test".into(),
            password: "plan upgrade fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = customer.organization.id;
    let sold = legacy_plan::publish(
        &db,
        db.default_business().await.unwrap(),
        &NewSoldPlan {
            plan_id: None,
            name: "Historical plan".into(),
            price_minor: 1_000_000,
            currency: "USD".into(),
            quota_minor: Some(3_000_000),
            period_kind: PeriodKind::CalendarMonth,
            overage_enabled: false,
            sort_order: 0,
        },
    )
    .await
    .unwrap();
    // Seed the binding with the historical schema. Today's reader requires the
    // policy column added by 0113 and must not be used before that migration.
    let plan = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.customer_plans(customer_plan_id,organization_id,plan_version_id,state) VALUES($1,$2,$3,'pending_payment')")
        .bind(plan).bind(org).bind(sold.plan_version_id).execute(db.pool()).await.unwrap();
    let fulfilled = Uuid::new_v4();
    let pending = Uuid::new_v4();
    let insert = "INSERT INTO oxsum.payment_orders(order_id,business_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,purpose_ref,promo_minor,idempotency_key,status,paid_at,source,evidence) VALUES($1,oxsum.default_business_id(),$2,'plan_period',1000000,'USD','USD',100,1,0,$3,0,$1::text,$4,now(),'manual',jsonb_build_object('reference',$1::text))";
    for (id, status) in [(fulfilled, "fulfilled"), (pending, "paid")] {
        sqlx::query(insert)
            .bind(id)
            .bind(org)
            .bind(plan)
            .bind(status)
            .execute(db.pool())
            .await
            .unwrap();
    }
    // This is historical database state from before the new writer fence;
    // current application APIs must never manufacture missing authority.
    sqlx::query("UPDATE oxsum.customer_plans SET state='active',activated_at=now() WHERE customer_plan_id=$1").bind(plan).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_periods(plan_period_id,customer_plan_id,period_index,period_start,period_end,quota_minor,authorization_kind,authorization_ref,state) VALUES($1,$2,0,now(),now()+interval '1 month',3000000,'payment_order',$3,'active')")
        .bind(Uuid::new_v4()).bind(plan).bind(fulfilled).execute(db.pool()).await.unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM oxsum.payment_orders o WHERE order_id=$1")
            .bind(fulfilled)
            .fetch_one(db.pool())
            .await
            .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let mut after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM oxsum.payment_orders o WHERE order_id=$1")
            .bind(fulfilled)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        after.as_object_mut().unwrap().remove("plan_payment"),
        Some(serde_json::Value::Null),
        "historical orders must not receive invented period authority"
    );
    assert_eq!(before, after);
    assert_eq!(
        db.activate_plan_period(fulfilled, plan)
            .await
            .unwrap()
            .customer_plan_id,
        plan
    );
    assert!(matches!(
        db.activate_plan_period(pending, plan).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(db.activate_plan_period(Uuid::new_v4(), plan).await.is_err());
    let periods = db.plan_periods(plan).await.unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].accounting_policy_version, 1);
    assert_eq!(
        db.sold_plan(sold.plan_version_id)
            .await
            .unwrap()
            .unwrap()
            .accounting_policy_version,
        1
    );
    let daily: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.plan_accounting_periods WHERE customer_plan_id=$1",
    )
    .bind(plan)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(daily, 0);
    assert!(db.unfulfilled_payment_orders(100).await.unwrap().is_empty());
    assert!(
        sqlx::query(insert)
            .bind(Uuid::new_v4())
            .bind(org)
            .bind(plan)
            .bind("created")
            .execute(db.pool())
            .await
            .is_err()
    );
}

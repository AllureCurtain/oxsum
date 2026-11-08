//! Upgrade preserves authorized plan history while fencing pending unquoted orders.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{Db, NewSoldPlan, NewUser, PeriodAuthorization, PeriodKind, WalletError};
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
        if version >= "0103" {
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
    let sold = db
        .publish_sold_plan(
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
    let plan = db
        .bind_customer_plan(
            org,
            sold.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    let fulfilled = Uuid::new_v4();
    let pending = Uuid::new_v4();
    let insert = "INSERT INTO oxsum.payment_orders(order_id,business_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,purpose_ref,promo_minor,idempotency_key,status,paid_at,source,evidence) VALUES($1,oxsum.default_business_id(),$2,'plan_period',1000000,'USD','USD',100,1,0,$3,0,$1::text,$4,now(),'manual',jsonb_build_object('reference',$1::text))";
    for (id, status) in [(fulfilled, "fulfilled"), (pending, "paid")] {
        sqlx::query(insert)
            .bind(id)
            .bind(org)
            .bind(plan.customer_plan_id)
            .bind(status)
            .execute(db.pool())
            .await
            .unwrap();
    }
    // This is historical database state from before the new writer fence;
    // current application APIs must never manufacture missing authority.
    sqlx::query("UPDATE oxsum.customer_plans SET state='active',activated_at=now() WHERE customer_plan_id=$1").bind(plan.customer_plan_id).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_periods(plan_period_id,customer_plan_id,period_index,period_start,period_end,quota_minor,authorization_kind,authorization_ref,state) VALUES($1,$2,0,now(),now()+interval '1 month',3000000,'payment_order',$3,'active')")
        .bind(Uuid::new_v4()).bind(plan.customer_plan_id).bind(fulfilled).execute(db.pool()).await.unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM oxsum.payment_orders o WHERE order_id=$1")
            .bind(fulfilled)
            .fetch_one(db.pool())
            .await
            .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let after: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(o)-'plan_payment' FROM oxsum.payment_orders o WHERE order_id=$1",
    )
    .bind(fulfilled)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        db.activate_plan_period(fulfilled, plan.customer_plan_id)
            .await
            .unwrap()
            .customer_plan_id,
        plan.customer_plan_id
    );
    assert!(matches!(
        db.activate_plan_period(pending, plan.customer_plan_id)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        db.activate_plan_period(Uuid::new_v4(), plan.customer_plan_id)
            .await
            .is_err()
    );
    assert_eq!(
        db.plan_periods(plan.customer_plan_id).await.unwrap().len(),
        1
    );
    assert!(db.unfulfilled_payment_orders(100).await.unwrap().is_empty());
    assert!(
        sqlx::query(insert)
            .bind(Uuid::new_v4())
            .bind(org)
            .bind(plan.customer_plan_id)
            .bind("created")
            .execute(db.pool())
            .await
            .is_err()
    );
}

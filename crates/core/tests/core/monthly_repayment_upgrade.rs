//! Shared receipt-reference upgrade preserves old cash intents and fences old writers.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{Db, NewUser};
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn existing_manual_references_are_backfilled_without_rewriting_history() {
    let Some(url) = oxsum_testkit::suite_db_url("monthly_repayment_upgrade") else {
        return;
    };
    let db = Db::connect(&url, 4).await.unwrap();
    db.pool().execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations(version text PRIMARY KEY,name text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0130_monthly_repayments" {
            continue;
        }
        let mut tx = db.pool().begin().await.unwrap();
        tx.execute("SET LOCAL search_path = ''").await.unwrap();
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
            email: "monthly-upgrade@example.test".into(),
            password: "monthly upgrade fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let insert = "INSERT INTO oxsum.manual_receipts(receipt_id,organization_id,idempotency_key,reference,reason,amount_minor) VALUES($1,$2,'old-key','old-bank-ref','old verified receipt',100) ON CONFLICT DO NOTHING";
    sqlx::query(insert)
        .bind(id)
        .bind(customer.organization.id)
        .execute(db.pool())
        .await
        .unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM oxsum.manual_receipts m WHERE receipt_id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let order = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.payment_orders(order_id,business_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,promo_minor,idempotency_key,status,source,evidence) VALUES($1,oxsum.default_business_id(),$2,'wallet_topup',100,'USD','USD',100,1,0,0,'old-order','paid','manual','{\"reference\":\"old-bank-ref\"}')")
        .bind(order).bind(customer.organization.id).execute(db.pool()).await.unwrap();
    let singleton = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.manual_receipts(receipt_id,organization_id,idempotency_key,reference,reason,amount_minor) VALUES($1,$2,'singleton-key','singleton-bank-ref','old verified singleton',100)")
        .bind(singleton).bind(customer.organization.id).execute(db.pool()).await.unwrap();
    let order_before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM oxsum.payment_orders o WHERE order_id=$1")
            .bind(order)
            .fetch_one(db.pool())
            .await
            .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(m) FROM oxsum.manual_receipts m WHERE receipt_id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(before, after);
    let source:Option<Uuid>=sqlx::query_scalar("SELECT source_id FROM oxsum.payment_receipt_references WHERE reference='old-bank-ref' AND source_kind='historical_conflict'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(source, None);
    let source: Uuid = sqlx::query_scalar("SELECT source_id FROM oxsum.payment_receipt_references WHERE reference='singleton-bank-ref' AND source_kind='manual'")
        .fetch_one(db.pool()).await.unwrap();
    assert_eq!(source, singleton);
    let order_after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM oxsum.payment_orders o WHERE order_id=$1")
            .bind(order)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(order_before, order_after);
    sqlx::query(insert)
        .bind(id)
        .bind(customer.organization.id)
        .execute(db.pool())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.payment_receipt_references")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
}

//! Collection adoption preserves old mixed-currency orders without relabeling.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{CollectionCurrency, CollectionTermsRequest, Db, NewUser, WalletError};
use serde_json::{Value, json};
use sqlx::Executor;
use uuid::Uuid;
#[path = "support/integration.rs"]
mod setup;
#[tokio::test]
async fn mixed_historical_collection_currencies_remain_fenced_and_original() {
    let Some(url) = oxsum_testkit::suite_db_url("collection_upgrade") else {
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.pool().execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations(version text PRIMARY KEY,name text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0101" {
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
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: "collection-history@example.test".into(),
            password: "collection upgrade fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    for code in ["USD", "EUR"] {
        sqlx::query("INSERT INTO oxsum.payment_orders(order_id,business_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,idempotency_key,source,evidence,status,paid_at) VALUES($1,oxsum.default_business_id(),$2,'wallet_topup',2000000,'USD',$3,200,1,0,$3,'manual',$4,'paid',now())")
            .bind(Uuid::new_v4()).bind(user.organization.id).bind(code).bind(json!({"reference":code})).execute(db.pool()).await.unwrap();
    }
    // `plan_payment` is a nullable column added by 0103: it lands as NULL on
    // these historical orders, which is not a relabel of their contents.
    let before: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(p)-'plan_payment' FROM oxsum.payment_orders p ORDER BY order_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    db.bootstrap_collection_terms(&CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let terms = db.collection_terms().await.unwrap();
    assert!(!terms.ready);
    assert_eq!(terms.historical_currencies, vec!["EUR", "USD"]);
    for code in ["USD", "EUR"] {
        assert!(matches!(
            db.set_collection_terms(
                &owner,
                &CollectionTermsRequest {
                    idempotency_key: Uuid::new_v4().to_string(),
                    reason: "cannot relabel".into(),
                    expected_revision: 1,
                    currency: CollectionCurrency {
                        code: code.into(),
                        rate_per_credit: "1".into()
                    }
                }
            )
            .await,
            Err(WalletError::Conflict(_))
        ));
    }
    let after: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(p)-'plan_payment' FROM oxsum.payment_orders p ORDER BY order_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(before, after);
}

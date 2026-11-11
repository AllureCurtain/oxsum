//! Legacy bundles keep historical effects but cannot gain invented offer authority.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{Db, NewRefundRequest, NewUser, Wallet, WalletError};
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn upgrade_retains_fulfilled_history_and_fences_pending_bundles_and_old_writers() {
    let Some(url) = oxsum_testkit::suite_db_url("bundle_upgrade") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.pool().execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations(version text PRIMARY KEY,name text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|file| file.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0108_bundle_offers" {
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
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: "bundle-upgrade@example.test".into(),
            password: "bundle upgrade fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let wallet = Wallet::open(db.pool().clone(), &customer.organization.tenant_id)
        .await
        .unwrap();
    let on = time::OffsetDateTime::now_utc().date();
    let fulfilled = Uuid::new_v4();
    let pending = Uuid::new_v4();
    let insert = "INSERT INTO oxsum.payment_orders(order_id,business_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,promo_minor,idempotency_key,status,paid_at,source,evidence) VALUES($1,oxsum.default_business_id(),$2,'bundle',5000000,'USD','USD',500,1,0,1000000,$1::text,$3,now(),'manual',jsonb_build_object('reference',$1::text))";
    for (id, status) in [(fulfilled, "fulfilled"), (pending, "paid")] {
        sqlx::query(insert)
            .bind(id)
            .bind(customer.organization.id)
            .bind(status)
            .execute(db.pool())
            .await
            .unwrap();
    }
    wallet
        .top_up(&format!("payment-order:{fulfilled}"), 5_000_000, on)
        .await
        .unwrap();
    db.grant_promo_lot(
        &wallet,
        customer.organization.id,
        "bundle_promo",
        &fulfilled.to_string(),
        1_000_000,
        None,
        on,
    )
    .await
    .unwrap();
    let fulfillment = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.payment_fulfillments(fulfillment_id,order_id,purpose,purpose_payload) VALUES($1,$2,'bundle','{\"creditedMinor\":5000000,\"promoMinor\":1000000}')")
        .bind(fulfillment).bind(fulfilled).execute(db.pool()).await.unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM oxsum.payment_orders o WHERE order_id=$1")
            .bind(fulfilled)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let historical_refund = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.refunds(refund_id,order_id,organization_id,business_id,amount_minor,display_amount_minor,display_currency,reason,state,request_key) SELECT $1,order_id,organization_id,business_id,2000000,200,display_currency,'historical manual review','requested','legacy-refund' FROM oxsum.payment_orders WHERE order_id=$2")
        .bind(historical_refund).bind(fulfilled).execute(db.pool()).await.unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let mut after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(o) FROM oxsum.payment_orders o WHERE order_id=$1")
            .bind(fulfilled)
            .fetch_one(db.pool())
            .await
            .unwrap();
    // Newly introduced authority snapshots must remain absent on historical
    // orders; every field that existed before the upgrade remains unchanged.
    for column in ["bundle_offer", "plan_payment", "statement_payment"] {
        assert_eq!(
            after.as_object_mut().unwrap().remove(column),
            Some(serde_json::Value::Null)
        );
    }
    assert_eq!(before, after);
    assert!(db.bundle_catalog().await.unwrap().is_empty());
    assert!(
        db.payment_order(customer.organization.id, fulfilled)
            .await
            .unwrap()
            .unwrap()
            .bundle_offer
            .is_none()
    );
    assert_eq!(
        db.fulfill_payment_order(fulfilled, &wallet, on)
            .await
            .unwrap()
            .fulfillment_id,
        fulfillment
    );
    assert_eq!(wallet.available().await.unwrap(), 6_000_000);
    assert!(matches!(
        db.fulfill_payment_order(pending, &wallet, on).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        db.unfulfilled_payment_orders(100).await.unwrap().is_empty(),
        "legacy fenced intents cannot starve the automatic recovery queue"
    );
    assert_eq!(wallet.available().await.unwrap(), 6_000_000);
    // A historical request is still replayable, without inventing a provider
    // route for the original manual receipt or authorizing a new automatic return.
    let replay = db
        .request_order_refund(
            customer.organization.id,
            fulfilled,
            &NewRefundRequest {
                amount_minor: Some(2_000_000),
                reason: "historical manual review".into(),
                request_key: "legacy-refund".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(replay.refund_id, historical_refund);
    assert_eq!(replay.amount_minor, 2_000_000);
    assert_eq!(replay.state, "requested");
    assert!(matches!(
        db.request_order_refund(
            customer.organization.id,
            fulfilled,
            &NewRefundRequest {
                amount_minor: Some(1_000_000),
                reason: "new automatic request".into(),
                request_key: "new-refund".into(),
                requested_by: None,
            },
        ).await,
        Err(WalletError::Conflict(message)) if message.contains("no provider refund route")
    ));
    let refunds = db
        .order_refunds(customer.organization.id, fulfilled)
        .await
        .unwrap();
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0].refund_id, historical_refund);
    assert_eq!(
        db.order_refundable_minor(customer.organization.id, fulfilled)
            .await
            .unwrap(),
        None
    );
    assert_eq!(wallet.available().await.unwrap(), 6_000_000);
    assert!(
        sqlx::query(insert)
            .bind(Uuid::new_v4())
            .bind(customer.organization.id)
            .bind("paid")
            .execute(db.pool())
            .await
            .is_err(),
        "old application inserts are fenced after migration"
    );
    assert!(db.business_bundle_offers(&owner).await.unwrap().is_empty());
}

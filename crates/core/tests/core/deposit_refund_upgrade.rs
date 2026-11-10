//! Legacy reversal migration preserves history and never invents create IDs.
use oxsum_core::{Db, MerchantBootstrap, MerchantMode, MerchantRail, NewUser, SecretKey};
use serde_json::{Value, json};
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn historical_reversals_keep_original_rows_and_only_known_pinned_handles_are_due() {
    let Some(url) = oxsum_testkit::suite_db_url("deposit_refund_upgrade") else {
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
        if version >= "0107_deposit_refund_recovery" {
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
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let customer = db
        .register(NewUser {
            email: "refund-upgrade@example.test".into(),
            password: "upgrade fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let secret = SecretKey::from_bytes([14; 32]);
    let (merchant, version) = db
        .bootstrap_merchant(
            &MerchantBootstrap {
                rail: MerchantRail::Hyperswitch,
                mode: MerchantMode::Test,
                account_id: "historical-fixture".into(),
                config: serde_json::from_value(json!({"apiUrl":"https://sandbox.hyperswitch.io",
            "apiKey":"historical-key","webhookSecret":"historical-webhook-key",
            "publishableKey":"pk_snd_historical","sdkUrl":"https://sdk.example.test/v1"}))
                .unwrap(),
            },
            &secret,
        )
        .await
        .unwrap();
    let context = db
        .merchant_signing_context(merchant, version, &secret)
        .await
        .unwrap()
        .unwrap();
    let mut ids = Vec::new();
    for (name, handle, received) in [
        ("known", Some("refund-known"), 100),
        ("missing", None, 100),
        ("mismatch", Some("refund-mismatch"), 99),
    ] {
        let deposit = db
            .begin_merchant_deposit(&context, customer.organization.id, name, 100)
            .await
            .unwrap();
        let id = deposit.payment.payment_id;
        db.record_merchant_payment_reference(id, &format!("pay-{name}"))
            .await
            .unwrap();
        sqlx::query("UPDATE oxsum.deposits SET status='reversed',received_minor=$2,reversed_at=now(),reversal_entry_id=$3,meta=$4 WHERE deposit_id=$1")
            .bind(id).bind(received).bind(Uuid::new_v4()).bind(json!({"hyperswitchRefundId":handle,"refundStatus":"succeeded","retain":"original"}))
            .execute(db.pool()).await.unwrap();
        ids.push(id);
    }
    let unbound = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.deposits(deposit_id,organization_id,rail,payment_ref,amount_minor,received_minor,status,meta) VALUES($1,$2,'hyperswitch','old-unbound',100,0,'reversed',$3)")
        .bind(unbound).bind(customer.organization.id).bind(json!({"hyperswitchRefundId":"unbound-handle","refundStatus":"failed"}))
        .execute(db.pool()).await.unwrap();
    ids.push(unbound);
    let before: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(d) FROM oxsum.deposits d ORDER BY deposit_id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let after: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(d) FROM oxsum.deposits d ORDER BY deposit_id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(
        before, after,
        "migration must preserve the original metadata and entry references"
    );
    for (index, id) in ids.iter().enumerate() {
        let target = db.deposit_refund(*id).await.unwrap().unwrap();
        assert!(!target.body_identity);
        assert_eq!(
            target.state, "unknown",
            "old HTTP status is not terminal evidence"
        );
        assert_eq!(target.request_id, *id);
        assert_eq!(target.attempt_count, 0);
        assert_eq!(target.next_attempt_at.is_some(), index == 0);
        assert_eq!(
            db.admin_deposit(*id)
                .await
                .unwrap()
                .unwrap()
                .refund_status
                .as_deref(),
            Some("unknown")
        );
    }
    let due = db.due_deposit_refunds(100).await.unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].deposit_id, ids[0]);
    assert_eq!(due[0].provider_refund_id.as_deref(), Some("refund-known"));
    assert_eq!(due[0].merchant_id, Some(merchant));
    assert_eq!(due[0].version_id, Some(version));
    assert!(
        sqlx::query("DELETE FROM oxsum.deposit_refunds WHERE deposit_id=$1")
            .bind(ids[0])
            .execute(db.pool())
            .await
            .is_err()
    );
}

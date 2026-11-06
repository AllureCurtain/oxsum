//! A03/A08: the registered upgrade discards unsafe cache keys, never ledger evidence.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, NewUser, SecretKey, Tenants};
use serde_json::json;
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn upgrading_discards_old_cache_rows_and_preserves_customer_receipts() {
    let Some(url) = oxsum_testkit::suite_db_url("cache_upgrade") else {
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.pool().execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations(version text PRIMARY KEY,name text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|file| file.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0080" {
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
    let customer = db
        .register(NewUser {
            email: "cache-upgrade@example.com".into(),
            password: "cache upgrade fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let tenants = Tenants::new(db.pool().clone());
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    wallet
        .top_up(
            "legacy-funding",
            1234,
            time::OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let entry_id = oxsum_core::entry_id_for("legacy-funding");
    let before = wallet.receipt_proof(entry_id, None).await.unwrap().unwrap();
    db.set_channel(
        "cache-upgrade",
        "https://provider.example/v1",
        "upgrade-secret",
        "openai",
        &SecretKey::from_bytes([7; 32]),
    )
    .await
    .unwrap();
    let channel = db.channel_id_named("cache-upgrade").await.unwrap().unwrap();
    sqlx::query("INSERT INTO oxsum.cached_responses(cache_id,organization_id,channel_id,model,prompt_digest,response_body,usage_json,source_entry_id) VALUES($1,$2,$3,'legacy-model',$4,$5,$6,$7)")
        .bind(Uuid::new_v4()).bind(customer.organization.id).bind(channel).bind("0".repeat(64))
        .bind(json!({"private": "legacy answer"})).bind(json!({})).bind(entry_id.as_uuid())
        .execute(db.pool()).await.unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.cached_responses")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    assert_eq!(wallet.available().await.unwrap(), 1234);
    assert_eq!(wallet.log_size().await.unwrap(), 1);
    let after = wallet.receipt_proof(entry_id, None).await.unwrap().unwrap();
    assert_eq!(json!(before), json!(after));
    assert_eq!(
        db.authenticate(&customer.api_key.secret)
            .await
            .unwrap()
            .unwrap()
            .0
            .id,
        customer.organization.id
    );
}

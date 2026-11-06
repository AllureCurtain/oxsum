//! A03/A07: adding business routing ownership does not rewrite sealed keys/prices.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::*;
use serde_json::json;
use sqlx::Executor;
#[tokio::test]
async fn old_provider_identities_sealed_credentials_and_prices_survive_upgrade() {
    let Some(url) = oxsum_testkit::suite_db_url("provider_upgrade") else {
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
        if version >= "0078" {
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
    let key = SecretKey::from_bytes([5; 32]);
    db.set_channel(
        "legacy",
        "https://provider.example/v1",
        "legacy-key-with-secret",
        "openai",
        &key,
    )
    .await
    .unwrap();
    for m in ["legacy-a", "legacy-b"] {
        db.append_price(
            "legacy",
            m,
            serde_json::from_value(
                json!({"inputPricePerMillion":1,"outputPricePerMillion":2,"maxOutputTokens":100}),
            )
            .unwrap(),
            100,
        )
        .await
        .unwrap();
    }
    db.create_alias("legacy", "legacy-alias", "legacy-a")
        .await
        .unwrap();
    db.set_fallbacks("legacy-a", &["legacy-b".into()])
        .await
        .unwrap();
    let id = db.channel_id_named("legacy").await.unwrap().unwrap();
    let before: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT credential_id,api_key_sealed FROM oxsum.channel_credentials WHERE channel_id=$1",
    )
    .bind(id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    let prices = serde_json::to_value(db.channel_prices("legacy").await.unwrap()).unwrap();
    let snapshot = db.servings("legacy-a", &key).await.unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    assert_eq!(db.channel_id_named("legacy").await.unwrap().unwrap(), id);
    let after: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT credential_id,api_key_sealed FROM oxsum.channel_credentials WHERE channel_id=$1",
    )
    .bind(id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        prices,
        serde_json::to_value(db.channel_prices("legacy").await.unwrap()).unwrap()
    );
    assert_eq!(
        db.fallbacks_for("legacy-a").await.unwrap(),
        vec!["legacy-b"]
    );
    let matches:bool=sqlx::query_scalar("SELECT business_id=oxsum.default_business_id() FROM oxsum.model_fallbacks WHERE model='legacy-a'").fetch_one(db.pool()).await.unwrap();
    assert!(matches);
    assert_eq!(snapshot[0].credentials[0].api_key, "legacy-key-with-secret");
    assert_eq!(snapshot[0].version, 1);
    assert_eq!(
        db.resolve_alias("legacy-alias").await.unwrap(),
        Some(("legacy".into(), "legacy-a".into()))
    );
}

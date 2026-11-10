//! The operator's preflight runs against a fresh database without booting the
//! server, applying migrations or requiring provider/telemetry configuration.
#[path = "../support/binary.rs"]
mod binary;

use oxsum_core::Db;
use serde_json::{Value, json};

#[tokio::test]
async fn preflight_is_json_only_and_leaves_a_fresh_database_unmigrated() {
    let Some(url) = oxsum_testkit::suite_db_url("ownership_preview") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 2).await.unwrap();
    let dir = std::env::temp_dir().join(format!("oxsum-preview-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    // Stop dotenv traversal at this isolated directory.
    std::fs::write(dir.join(".env"), "").unwrap();
    let output = binary::oxsum()
        .arg("ownership-preview")
        .current_dir(&dir)
        .env_clear()
        .env("DATABASE_URL", &url)
        .env("OXSUM_SECRET_KEY", "intentionally-invalid")
        .env("OXSUM_OTLP_ENDPOINT", "intentionally-invalid")
        .output()
        .unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        output.status.success(),
        "preflight must not boot the server"
    );
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        json!({"alreadyMapped":false,"customers":0,"channels":0,"plans":0,"ledgerSchemas":0})
    );
    let no_schema: bool = sqlx::query_scalar("SELECT to_regnamespace('oxsum') IS NULL")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(no_schema, "preflight must not even create the oxsum schema");
    let ledger_schemas: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'ledger_%'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(ledger_schemas, 0);
}

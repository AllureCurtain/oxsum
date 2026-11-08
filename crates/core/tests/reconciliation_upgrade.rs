//! Projection-only histories remain immutable legacy evidence after upgrade.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::Db;
use serde_json::json;
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn old_projection_runs_keep_values_and_gain_explicit_legacy_coverage() {
    let Some(url) = oxsum_testkit::suite_db_url("reconciliation_upgrade") else {
        return;
    };
    let db = Db::connect(&url, 3).await.unwrap();
    db.pool().execute("CREATE SCHEMA oxsum;CREATE TABLE oxsum._migrations(version text PRIMARY KEY,name text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0117_independent_reconciliation" {
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
    let run = Uuid::new_v4();
    let old = json!({"usageMinor":17,"earnedMinor":200,"refundMinor":13});
    sqlx::query("INSERT INTO oxsum.reconciliation_runs(run_id,kind,period_start,period_end,state,expected_totals,actual_totals,finished_at) VALUES($1,'daily','2026-11-01','2026-11-01','matched',$2,$2,now())").bind(run).bind(&old).execute(db.pool()).await.unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let snapshot: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(s) FROM oxsum.reconciliation_snapshots s WHERE snapshot_id=$1",
    )
    .bind(run)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(snapshot["state"], "legacy");
    assert_eq!(snapshot["expected_totals"], old);
    assert_eq!(snapshot["actual_totals"], old);
    assert_eq!(snapshot["coverage"]["version"], 0);
    assert_eq!(snapshot["coverage"]["complete"], false);
    assert_eq!(snapshot["coverage"]["comparedFields"], json!([]));
    let latest: Uuid = sqlx::query_scalar(
        "SELECT latest_snapshot_id FROM oxsum.reconciliation_runs WHERE run_id=$1",
    )
    .bind(run)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(latest, run);
    assert!(
        sqlx::query("DELETE FROM oxsum.reconciliation_snapshots WHERE snapshot_id=$1")
            .bind(run)
            .execute(db.pool())
            .await
            .is_err()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.reconciliation_snapshots WHERE run_id=$1")
            .bind(run)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
}

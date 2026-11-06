//! A03/A05: migrate genuine pre-0075 suspension provenance without identity moves.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{Db, NewUser};
use sqlx::Executor;
#[tokio::test]
async fn upgrade_preserves_manual_and_dunning_reasons_and_customer_identity() {
    let Some(url) = oxsum_testkit::suite_db_url("customer_upgrade") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.pool().execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations (version text PRIMARY KEY,name text NOT NULL,applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0075" {
            continue;
        }
        let mut tx = db.pool().begin().await.unwrap();
        tx.execute("SET LOCAL search_path = ''").await.unwrap();
        tx.execute(std::fs::read_to_string(&file).unwrap().as_str())
            .await
            .unwrap();
        sqlx::query("INSERT INTO oxsum._migrations (version,name) VALUES ($1,$1)")
            .bind(version)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let mut customers = Vec::new();
    for name in ["manual", "dunning", "active"] {
        let reg = db
            .register(NewUser {
                email: format!("{name}@example.com"),
                password: "customer upgrade fixture password".into(),
                organization_name: None,
            })
            .await
            .unwrap();
        customers.push(reg);
    }
    sqlx::query(
        "UPDATE oxsum.organizations SET suspended_at=now() WHERE organization_id IN ($1,$2)",
    )
    .bind(customers[0].organization.id)
    .bind(customers[1].organization.id)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE oxsum.organizations SET dunning_suspended_at=suspended_at WHERE organization_id=$1",
    )
    .bind(customers[1].organization.id)
    .execute(db.pool())
    .await
    .unwrap();
    let mappings:Vec<(uuid::Uuid,uuid::Uuid,String)>=sqlx::query_as("SELECT organization_id,business_id,external_id FROM oxsum.customer_profiles ORDER BY organization_id").fetch_all(db.pool()).await.unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let after:Vec<(uuid::Uuid,uuid::Uuid,String)>=sqlx::query_as("SELECT organization_id,business_id,external_id FROM oxsum.customer_profiles ORDER BY organization_id").fetch_all(db.pool()).await.unwrap();
    assert_eq!(after, mappings);
    for (i, c) in customers.iter().enumerate() {
        let flags:(bool,bool)=sqlx::query_as("SELECT suspended_at IS NOT NULL,dunning_suspended_at IS NOT NULL FROM oxsum.organizations WHERE organization_id=$1").bind(c.organization.id).fetch_one(db.pool()).await.unwrap();
        assert_eq!(flags, [(true, false), (false, true), (false, false)][i]);
        let (identity, _) = db.authenticate(&c.api_key.secret).await.unwrap().unwrap();
        assert_eq!(identity.id, c.organization.id);
        assert_eq!(identity.tenant_id, c.organization.tenant_id);
    }
}

//! A03: pre-scope service secrets become disabled without changing historical identity.
use oxsum_core::{Db, NewUser, OpenHold, SettlementKind, Tenants, sweep_stale_holds};
use sha2::{Digest, Sha256};
use sqlx::Executor;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;
#[tokio::test]
async fn legacy_services_are_disabled_and_customer_identity_is_preserved() {
    let Some(url) = oxsum_testkit::suite_db_url("integration_upgrade") else {
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
        if version >= "0077" {
            continue;
        }
        let mut tx = db.pool().begin().await.unwrap();
        tx.execute("SET LOCAL search_path = ''").await.unwrap();
        tx.execute(std::fs::read_to_string(file.clone()).unwrap().as_str())
            .await
            .unwrap();
        sqlx::query("INSERT INTO oxsum._migrations (version,name) VALUES ($1,$1)")
            .bind(version)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let id = Uuid::new_v4();
    let secret = "oxs-svc-legacy-test-only-secret";
    let hash = Sha256::digest(secret.as_bytes()).to_vec();
    sqlx::query("INSERT INTO oxsum.service_credentials (credential_id,name,prefix,secret_hash) VALUES ($1,'legacy','oxs-svc-legacy',$2)").bind(id).bind(&hash).execute(db.pool()).await.unwrap();
    let customer = db
        .register(NewUser {
            email: "upgrade@example.com".into(),
            password: "legacy integration password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let tenants = Tenants::new(db.pool().clone());
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    let on = OffsetDateTime::now_utc().date();
    wallet.top_up("legacy-fund", 1000, on).await.unwrap();
    let hold = "metering:old-service:hold";
    db.note_open_hold(&OpenHold {
        hold_key: hold.into(),
        tenant_id: customer.organization.tenant_id.clone(),
        request_id: hold.into(),
        model: "legacy-event".into(),
        channel: "legacy-service".into(),
        price_version: 1,
        input_price: 1_000_000,
        output_price: 0,
        freeze_minor: 100,
        key_id: None,
        mandate_id: None,
        end_user: None,
        service_tier: None,
        tags: Default::default(),
        sweep_attempts: 0,
        last_error: None,
        dead_at: None,
    })
    .await
    .unwrap();
    wallet
        .hold_for_service(
            &id,
            hold,
            &oxsum_core::hold_description(hold, "legacy-event", 100).unwrap(),
            100,
            on,
        )
        .await
        .unwrap();
    let before: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(&format!(
        "SELECT entry_id,content_hash FROM ledger_{}.entries ORDER BY entry_id",
        customer.organization.tenant_id
    ))
    .fetch_all(db.pool())
    .await
    .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    assert_eq!(wallet.settled().await.unwrap(), 1000);
    assert_eq!(wallet.reserved().await.unwrap(), 100);
    let after: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(&format!(
        "SELECT entry_id,content_hash FROM ledger_{}.entries ORDER BY entry_id",
        customer.organization.tenant_id
    ))
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(before, after);
    let report = sweep_stale_holds(
        &db,
        &tenants,
        OffsetDateTime::now_utc() + Duration::minutes(1),
        on,
    )
    .await
    .unwrap();
    assert_eq!(report.resolved, 1);
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(
        wallet.settled_record(hold).await.unwrap().unwrap().kind,
        SettlementKind::Swept
    );

    let row:(Uuid,Vec<u8>,bool)=sqlx::query_as("SELECT credential_id,secret_hash,revoked_at IS NOT NULL FROM oxsum.service_credentials WHERE credential_id=$1").bind(id).fetch_one(db.pool()).await.unwrap();
    assert_eq!(row, (id, hash, true));
    assert!(db.authenticate_service(secret).await.unwrap().is_none());
    let resolved = db
        .authenticate(&customer.api_key.secret)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.0.id, customer.organization.id);
    assert_eq!(resolved.0.tenant_id, customer.organization.tenant_id);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.integration_credentials")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}

//! A03/A07: an upgrade pins old tasks without guessing internal permission.
use oxsum_core::{Db, NewUser, ProviderDestinationPolicy, ProviderNetwork, SecretKey};
use serde_json::json;
use sqlx::Executor;
use uuid::Uuid;
#[tokio::test]
async fn legacy_task_coordinates_are_captured_and_later_rotation_cannot_retarget_them() {
    let Some(url) = oxsum_testkit::suite_db_url("provider_destination_upgrade") else {
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
        if version >= "0079" {
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
    let secret = SecretKey::from_bytes([5; 32]);
    db.set_channel(
        "old-task-channel",
        "http://127.0.0.1:1111",
        "old-task-provider-key",
        "tasks",
        &secret,
    )
    .await
    .unwrap();
    let channel = db
        .channel_id_named("old-task-channel")
        .await
        .unwrap()
        .unwrap();
    let customer = db
        .register(NewUser {
            email: "upgrade-task@example.com".into(),
            password: "upgrade task fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let task = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.tasks(task_id,organization_id,key_id,channel_id,external_task_id,kind,hold_key,freeze_minor,model) VALUES($1,$2,$3,$4,'old-external-id','custom','old-hold',500,'old-model')")
        .bind(task).bind(customer.organization.id).bind(customer.api_key.key.id).bind(channel).execute(db.pool()).await.unwrap();
    let sealed: String = sqlx::query_scalar(
        "SELECT api_key_sealed FROM oxsum.channel_credentials WHERE channel_id=$1",
    )
    .bind(channel)
    .fetch_one(db.pool())
    .await
    .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let before = db.task_provider_endpoint(task).await.unwrap().unwrap();
    assert_eq!(before.base_url, "http://127.0.0.1:1111");
    assert_eq!(before.api_key_sealed.as_deref(), Some(sealed.as_str()));
    assert_eq!(
        before.destination_policy,
        ProviderDestinationPolicy::default()
    );
    assert!(
        before
            .destination_policy
            .endpoint(&before.base_url)
            .is_err()
    );
    db.set_channel_pool_policy(
        "old-task-channel",
        "http://127.0.0.1:2222",
        Some(&["new-provider-key".into()]),
        "tasks",
        &secret,
        Some(&ProviderDestinationPolicy {
            network: ProviderNetwork::Internal,
            allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        json!(db.task_provider_endpoint(task).await.unwrap().unwrap()),
        json!(before)
    );
    assert!(
        sqlx::query("UPDATE oxsum.tasks SET provider_endpoint=NULL WHERE task_id=$1")
            .bind(task)
            .execute(db.pool())
            .await
            .is_err()
    );
    assert_eq!(
        db.task(customer.organization.id, task)
            .await
            .unwrap()
            .unwrap()
            .external_task_id,
        "old-external-id"
    );
}

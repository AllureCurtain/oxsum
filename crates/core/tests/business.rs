//! A03: upgrade a genuine pre-business schema without moving customer ledgers.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{Db, NewUser, SecretKey, Tenants, WalletError, signing_key, verify_bundle};
use serde_json::{Value, json};
use sqlx::Executor;
use uuid::Uuid;

const DAY: time::Date = time::macros::date!(2026 - 09 - 10);

async fn baseline() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("business")?;
    let db = Db::connect(&url, 5).await.unwrap();
    // Apply the actual pre-ownership migration files, with the same migration
    // bookkeeping. This suite has one test so the upgrade fixture is isolated.
    db.pool().execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations (version text PRIMARY KEY, name text NOT NULL, applied_at timestamptz NOT NULL DEFAULT now())").await.unwrap();
    let mut files = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let version = file.file_stem().unwrap().to_str().unwrap();
        if version >= "0072" {
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
    Some(db)
}

async fn identity_snapshot(db: &Db) -> Value {
    // Ownership changes may add columns to config only; identity/history tables
    // must compare byte-for-byte at the JSON value level across migration.
    let mut out = serde_json::Map::new();
    for table in [
        "organizations",
        "users",
        "memberships",
        "api_keys",
        "channel_prices",
        "statements",
        "statement_lines",
        "deposits",
        "usage_records",
        "usage_daily",
    ] {
        // `cost_basis` is a column the upgrade adds to `usage_records`: its key
        // is absent from `to_jsonb` before the migration and `NULL` after, so it
        // is stripped to compare the *history content* the snapshot exists for —
        // a migration adding a column is schema change, not history change.
        let rows:Value=sqlx::query_scalar(&format!("SELECT COALESCE(jsonb_agg(v ORDER BY v::text),'[]'::jsonb) FROM (SELECT to_jsonb(t)-'cost_basis' AS v FROM oxsum.{table} t) s")).fetch_one(db.pool()).await.unwrap();
        out.insert(table.into(), rows);
    }
    Value::Object(out)
}

#[tokio::test]
async fn ownership_upgrade_preserves_history_and_maps_new_customers_without_staff() {
    let Some(db) = baseline().await else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let before_empty = db.business_migration_preview().await.unwrap();
    assert!(!before_empty.already_mapped);
    assert_eq!(before_empty.customers, 0);
    let registered = db
        .register(NewUser {
            email: "legacy@example.com".into(),
            password: "legacy password fixture".into(),
            organization_name: Some("Legacy Customer".into()),
        })
        .await
        .unwrap();
    // Current admission/organization reads consult the additive profile table.
    // Supply an empty read-only compatibility view only while constructing the
    // historical ledger fixture, then remove it before snapshot and migration.
    // No ownership rows, defaults, triggers or migration receipts are introduced.
    sqlx::query("CREATE VIEW oxsum.customer_profiles AS SELECT NULL::uuid AS organization_id,NULL::timestamptz AS archived_at,NULL::text AS state WHERE false").execute(db.pool()).await.unwrap();
    let tenants = Tenants::new(db.pool().clone());
    let wallet = tenants
        .get(&registered.organization.tenant_id)
        .await
        .unwrap();
    wallet.top_up("legacy-funds", 5_000_000, DAY).await.unwrap();
    wallet
        .hold("legacy-request", "", 1_000_000, DAY)
        .await
        .unwrap();
    let receipt = wallet
        .settle("legacy-request", "", 700_000, DAY)
        .await
        .unwrap();
    let secret = SecretKey::from_bytes([7; 32]);
    db.set_channel(
        "legacy",
        "https://example.invalid/v1",
        "test-secret",
        "openai",
        &secret,
    )
    .await
    .unwrap();
    let book=oxsum_core::PriceBook::from_json("legacy", &json!({"test-model":{"inputPricePerMillion":1,"outputPricePerMillion":2,"maxOutputTokens":100}}).to_string()).unwrap();
    for (model, price) in book.models() {
        db.append_price("legacy", model, price.clone(), 100)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO oxsum.subscription_plans (plan_id,name,quota_minor,reset_period) VALUES ($1,'legacy',100,'monthly')").bind(Uuid::new_v4()).execute(db.pool()).await.unwrap();
    // The pre-0088 usage_records shape: `cost_basis` is a column the upgrade
    // adds, so the historical fixture writes the row under the column set the
    // old schema actually had — leaving `cost_basis` NULL, exactly as a real
    // pre-column row reads after the migration lands.
    sqlx::query(
        "INSERT INTO oxsum.usage_records \
         (request_id, tenant_id, key_id, mandate_id, model, channel, price_version, kind, entry_id, \
          input_tokens, output_tokens, charged_minor, freeze_minor, upstream_cost_minor, \
          upstream_attempts, provider_input_tokens, provider_output_tokens, status) \
         VALUES ($1,$2,$3,NULL,$4,$5,$6,$7,$8,100,50,700000,1000000,NULL,1,100,50,'settled')",
    )
    .bind("legacy-request")
    .bind(&registered.organization.tenant_id)
    .bind(registered.api_key.key.id)
    .bind("test-model")
    .bind("legacy")
    .bind(1_i32)
    .bind(oxsum_core::SettlementKind::Usage.as_str())
    .bind(*receipt.entry_id.as_uuid())
    .execute(db.pool())
    .await
    .unwrap();
    let org = db
        .organization_by_id(registered.organization.id)
        .await
        .unwrap();
    let statement = db
        .generate_statement(
            &org,
            &wallet,
            &oxsum_core::statement_period("2026-09").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let statement = db
        .finalize_statement(statement.id, &wallet, time::macros::date!(2026 - 10 - 01))
        .await
        .unwrap();
    assert!(
        statement.anchor_entry_id.is_some(),
        "upgrade fixture includes genuine anchored statement"
    );
    sqlx::query("DROP VIEW oxsum.customer_profiles")
        .execute(db.pool())
        .await
        .unwrap();
    let snapshot = identity_snapshot(&db).await;
    let key = signing_key([4; 32]).unwrap();
    let head = wallet.signed_head(&key).await.unwrap().note;
    let bundle = serde_json::to_value(
        wallet
            .receipt_proof(receipt.entry_id, Some(&key))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(verify_bundle(&bundle.to_string(), &receipt.content_hash).unwrap());
    let preview = db.business_migration_preview().await.unwrap();
    assert_eq!(
        (
            preview.customers,
            preview.channels,
            preview.plans,
            preview.ledger_schemas
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(
        identity_snapshot(&db).await,
        snapshot,
        "preview is read-only"
    );
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let (a, b) = tokio::join!(db.migrate(), db.migrate());
    a.unwrap();
    b.unwrap();
    assert_eq!(
        identity_snapshot(&db).await,
        snapshot,
        "customer identities/history untouched"
    );
    assert_eq!(wallet.available().await.unwrap(), 4_300_000);
    assert_eq!(wallet.signed_head(&key).await.unwrap().note, head);
    assert_eq!(
        serde_json::to_value(
            wallet
                .receipt_proof(receipt.entry_id, Some(&key))
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        bundle
    );
    assert_eq!(
        db.authenticate(&registered.api_key.secret)
            .await
            .unwrap()
            .unwrap()
            .0
            .id,
        registered.organization.id
    );
    let ownership = db.business_ownership().await.unwrap();
    assert!(ownership.business.initialized_at.is_none());
    assert_eq!(ownership.business.registration_mode, "invite");
    assert_eq!(
        (
            ownership.migration.customers,
            ownership.migration.channels,
            ownership.migration.plans
        ),
        (1, 1, 1)
    );
    let customer = db
        .customer_profile(ownership.business.id, registered.organization.id)
        .await
        .unwrap();
    assert_eq!(customer.tenant_id, registered.organization.tenant_id);
    assert_eq!(customer.external_id, registered.organization.id.to_string());
    assert!(matches!(
        db.customer_profile(Uuid::new_v4(), registered.organization.id)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let staff: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.business_staff")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(staff, 0);
    assert!(
        sqlx::query("INSERT INTO oxsum.businesses DEFAULT VALUES")
            .execute(db.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE oxsum.business_migrations SET customers=0")
            .execute(db.pool())
            .await
            .is_err()
    );
    let mapped: Uuid =
        sqlx::query_scalar("SELECT business_id FROM oxsum.channels WHERE name='legacy'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(mapped, ownership.business.id);
    let mapped: Uuid =
        sqlx::query_scalar("SELECT business_id FROM oxsum.subscription_plans WHERE name='legacy'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(mapped, ownership.business.id);
    // A 1,000-customer identity smoke uses the real insertion trigger. Unused
    // customers must not eagerly create 1,000 physical ledger schemas.
    let start = std::time::Instant::now();
    sqlx::query("INSERT INTO oxsum.organizations (organization_id,tenant_id,name,kind) SELECT id,replace(id::text,'-',''),'capacity-'||n,'personal' FROM (SELECT gen_random_uuid() id,n FROM generate_series(1,1000) n) s").execute(db.pool()).await.unwrap();
    let preview = db.business_migration_preview().await.unwrap();
    assert!(preview.already_mapped);
    assert_eq!(preview.customers, 1001);
    assert_eq!(preview.ledger_schemas, 1);
    let mapped: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.customer_profiles WHERE business_id=$1")
            .bind(ownership.business.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(mapped, 1001);
    let w = tenants
        .get(&registered.organization.tenant_id)
        .await
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&w, &wallet));
    eprintln!(
        "1,000 customer mapping smoke: {:?}; ledger schemas remain 1",
        start.elapsed()
    );
    assert_eq!(
        db.business_ownership().await.unwrap().migration.customers,
        1,
        "migration receipt stays historical"
    );
}

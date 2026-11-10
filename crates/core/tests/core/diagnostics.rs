//! Operator diagnostics read fixed counts without leaking or changing row data.
use oxsum_core::{Db, DiagnosticSnapshot, NewUser};
use sqlx::Executor;
use uuid::Uuid;

async fn database(slug: &str) -> Option<Db> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url(slug)?;
    Some(Db::connect(&url, 5).await.unwrap())
}

#[tokio::test]
async fn schema_states_do_not_read_tables_from_another_version() {
    let Some(db) = database("diagnostics_schema").await else {
        return;
    };
    assert!(matches!(
        db.diagnostic_snapshot().await.unwrap(),
        DiagnosticSnapshot::Unmigrated
    ));
    db.pool()
        .execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations(version text)")
        .await
        .unwrap();
    assert!(matches!(
        db.diagnostic_snapshot().await.unwrap(),
        DiagnosticSnapshot::SchemaMismatch(None)
    ));
    sqlx::query("INSERT INTO oxsum._migrations VALUES ('0001_old')")
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        matches!(db.diagnostic_snapshot().await.unwrap(), DiagnosticSnapshot::SchemaMismatch(Some(version)) if version == "0001_old")
    );
}

#[tokio::test]
async fn aggregates_distinguish_live_due_and_historical_rows_without_mutations() {
    let Some(db) = database("diagnostics_counts").await else {
        return;
    };
    db.migrate().await.unwrap();
    let DiagnosticSnapshot::Ready(empty) = db.diagnostic_snapshot().await.unwrap() else {
        panic!("migrated")
    };
    assert!(
        serde_json::to_value(empty)
            .unwrap()
            .as_object()
            .unwrap()
            .values()
            .all(|value| value == 0)
    );
    let registration = db
        .register(NewUser {
            email: "diagnostic-private@example.test".into(),
            password: "diagnostic-private-password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = registration.organization.id;
    let mut tx = db.pool().begin().await.unwrap();
    let channel = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.channels(channel_id,name,base_url) VALUES ($1,'private-provider-name','https://private-provider.test')")
        .bind(channel).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO oxsum.channel_credentials(credential_id,channel_id,api_key_sealed,api_key_last4,position,status,cooldown_until,last_error) \
        SELECT gen_random_uuid(),$1,'private-sealed-value','TAIL',n,s,CASE WHEN n IN (2,3) THEN now()+interval '1 hour' ELSE NULL END,'private-error' \
        FROM (VALUES (1,'active'),(2,'active'),(3,'disabled'),(4,'active')) AS fixture(n,s)")
        .bind(channel).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO oxsum.jobs(kind,run_at,status,claimed_at,last_error,payload) \
        SELECT 'private-job-'||n,now()+make_interval(secs=>delay),s,CASE WHEN age IS NULL THEN NULL ELSE now()-make_interval(secs=>age) END,'private-job-error','{\"privatePayload\":true}' \
        FROM (VALUES (1,3600,'pending',NULL::int),(2,-3600,'pending',NULL),(3,-3600,'running',60),(4,-3600,'running',3600),(5,-3600,'running',NULL),(6,-3600,'dead',3600),(7,-3600,'done',3600)) AS fixture(n,delay,s,age)")
        .execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO oxsum.request_operations(organization_id,surface,fingerprint,bound_minor,model,state,leased_until,created_at) \
        SELECT $1,CASE WHEN n=6 THEN 'metering' ELSE 'chat' END,'private-fingerprint-'||n,1,'private-model',s,CASE WHEN lease IS NULL THEN NULL ELSE now()+make_interval(secs=>lease) END, \
        CASE WHEN n=5 THEN now() ELSE now()-interval '1 hour' END \
        FROM (VALUES (1,'created',NULL::int),(2,'leased',3600),(3,'running',-3600),(4,'succeeded',-3600),(5,'created',NULL),(6,'running',-3600),(7,'leased',NULL)) AS fixture(n,s,lease)")
        .bind(org).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO oxsum.open_holds(hold_key,tenant_id,request_id,model,channel,price_version,input_price,output_price,freeze_minor,dead_at,last_error) \
        SELECT 'private-hold-'||n,'private-tenant','private-request','private-model','private-channel',1,1,1,1,CASE WHEN n=2 THEN now() END,'private-hold-error' FROM generate_series(1,2) n")
        .execute(&mut *tx).await.unwrap();
    let merchant = Uuid::new_v4();
    let version = Uuid::new_v4();
    for enabled in [false, true] {
        let (merchant, version) = if enabled {
            (merchant, version)
        } else {
            (Uuid::new_v4(), Uuid::new_v4())
        };
        sqlx::query("INSERT INTO oxsum.merchant_accounts(merchant_id,business_id,name,rail,mode,account_id,current_version_id,enabled) \
            VALUES ($1,oxsum.default_business_id(),$1::text,'stripe','test',$1::text,$2,$3)")
            .bind(merchant).bind(version).bind(enabled).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO oxsum.merchant_versions(version_id,merchant_id,revision,api_url,destination_policy,credentials_sealed,source) \
            VALUES ($1,$2,1,'https://private-merchant.test','{}','private-merchant-sealed','owner')")
            .bind(version).bind(merchant).execute(&mut *tx).await.unwrap();
    }
    for state in ["pending", "accepted", "applied", "rejected"] {
        sqlx::query("INSERT INTO oxsum.merchant_event_inbox(receipt_id,merchant_id,version_id,event_id,payload_digest,normalized,state,reason) \
            VALUES (gen_random_uuid(),$1,$2,$3,decode(repeat('00',32),'hex'),'{\"privateEvent\":true}',$3,'private-reason')")
            .bind(merchant).bind(version).bind(state).execute(&mut *tx).await.unwrap();
    }
    for status in ["paid", "needs_attention", "fulfilled", "provider_pending"] {
        let order = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.merchant_payment_bindings(payment_id,merchant_id,version_id,organization_id,amount_minor,currency,purpose) \
            VALUES ($1,$2,$3,$4,100,'USD','order')")
            .bind(order).bind(merchant).bind(version).bind(org).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO oxsum.payment_orders(order_id,business_id,merchant_id,merchant_version_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,idempotency_key,status) \
            VALUES ($1,oxsum.default_business_id(),$2,$3,$4,'wallet_topup',100,'USD','USD',100,1,0,$5,$5)")
            .bind(order).bind(merchant).bind(version).bind(org).bind(status).execute(&mut *tx).await.unwrap();
        if status == "fulfilled" {
            sqlx::query("INSERT INTO oxsum.refunds(refund_id,order_id,organization_id,business_id,amount_minor,display_amount_minor,display_currency,reason,state,request_key,next_retry_at) \
                SELECT gen_random_uuid(),$1,$2,oxsum.default_business_id(),1,1,'USD','private-refund-reason',s,'private-refund-'||n,CASE WHEN delay IS NULL THEN NULL ELSE now()+make_interval(secs=>delay) END \
                FROM (VALUES (1,'provider_pending',-3600::int),(2,'provider_pending',NULL),(3,'provider_pending',3600),(4,'succeeded',-3600),(5,'failed',NULL)) AS fixture(n,s,delay)")
                .bind(order).bind(org).execute(&mut *tx).await.unwrap();
        }
    }
    for (index, state, delay) in [
        (1, "unknown", Some(-3600)),
        (2, "pending", None),
        (3, "pending", Some(3600)),
        (4, "succeeded", Some(-3600)),
        (5, "failed", None),
    ] {
        let deposit = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.deposits(deposit_id,organization_id,rail,payment_ref,amount_minor,status) VALUES ($1,$2,'manual',$3,1,'reversed')")
            .bind(deposit).bind(org).bind(format!("private-deposit-{index}")).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO oxsum.deposit_refunds(deposit_id,request_id,organization_id,amount_minor,currency,reason,state,next_attempt_at) \
            VALUES ($1,$1,$2,1,'USD','private-deposit-refund-reason',$3,CASE WHEN $4::int IS NULL THEN NULL ELSE now()+make_interval(secs=>$4) END)")
            .bind(deposit).bind(org).bind(state).bind(delay).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(j) ORDER BY job_id) FROM oxsum.jobs j")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let DiagnosticSnapshot::Ready(counts) = db.diagnostic_snapshot().await.unwrap() else {
        panic!("migrated")
    };
    let actual = serde_json::to_value(counts).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({
            "providerChannels":1,"providerCredentialsActive":2,"providerCredentialsCooling":1,"providerCredentialsDisabled":1,
            "merchantsEnabled":1,"merchantsDisabled":1,"jobsScheduled":1,"jobsDue":1,"jobsRunning":3,"jobClaimsExpired":2,"jobsDead":1,
        "operationsOpen":6,"operationLeasesExpired":2,"holdsOpen":2,"holdsDead":1,"paidOrdersUnfulfilled":1,"paymentOrdersNeedAttention":1,
            "merchantEventsPending":1,"merchantEventsAccepted":1,"refundsRetryDue":1,"refundsNeedReview":1,"legacyRefundsRetryDue":1,"legacyRefundsNeedReview":1
        })
    );
    assert!(!actual.to_string().contains("private"));
    assert_eq!(db.operations_past_lease(100).await.unwrap().len(), 2);
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(j) ORDER BY job_id) FROM oxsum.jobs j")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        before, after,
        "diagnostics never claim/retry jobs or rewrite errors"
    );
}

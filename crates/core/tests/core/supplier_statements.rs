//! Immutable real-format supplier imports and concurrent authority regression tests.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::supplier_statements::{SupplierComparisonRequest, SupplierStatementImport};
use oxsum_core::{Db, NewUser, SecretKey, WalletError};
use serde_json::json;
use uuid::Uuid;

struct World {
    db: Db,
    owner: String,
    input: SupplierStatementImport,
    org: Uuid,
    channel: Uuid,
}
async fn world() -> World {
    let url = oxsum_testkit::suite_url!().expect("supplier acceptance requires PostgreSQL");
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "supplier fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let name = format!("s-{}", &Uuid::new_v4().simple().to_string()[..8]);
    let secret = SecretKey::from_bytes([21; 32]);
    db.set_channel(
        &name,
        "https://api.openai.com/v1",
        "fixture-private-key",
        "openai",
        &secret,
    )
    .await
    .unwrap();
    let channel: Uuid = sqlx::query_scalar("SELECT channel_id FROM oxsum.channels WHERE name=$1")
        .bind(name)
        .fetch_one(db.pool())
        .await
        .unwrap();
    let input:SupplierStatementImport=serde_json::from_value(json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"Synthetic official Costs format acceptance","providerOrganizationId":"org-fixture","billDate":"2026-11-01","fetchedAt":"2026-11-02T12:00:00Z","scopeKind":"organization","projectIds":[],"channelIds":[channel],"usdPerCredit":"1","mappingEvidence":"Synthetic channel scope and USD-per-credit assertion","query":{"startTime":1793491200_i64,"endTime":1793577600_i64,"bucketWidth":"1d","groupBy":[],"projectIds":[],"limit":7},"pages":[{"requestPage":null,"body":r#"{"object":"page","data":[{"object":"bucket","start_time":1793491200,"end_time":1793577600,"results":[{"object":"organization.costs.result","amount":{"value":0.06,"currency":"usd"}}]}],"has_more":false,"next_page":null}"#}]})).unwrap();
    World {
        db,
        owner,
        input,
        org: user.organization.id,
        channel,
    }
}
async fn durable_attempt(w: &World, cost: Option<i64>, at: &str) -> Uuid {
    let operation = Uuid::new_v4();
    let attempt = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.operation_billing_terms(operation_id,organization_id,canonical,digest) VALUES($1,$2,'{}','synthetic-comparison-only')").bind(operation).bind(w.org).execute(w.db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.usage_recovery(operation_id,organization_id,hold_key,execution_owner,window_secs,supplier_cost_minor,closed_at) VALUES($1,$2,$3,'fixture',900,$4,CASE WHEN $4::bigint IS NOT NULL THEN clock_timestamp() ELSE NULL END)").bind(operation).bind(w.org).bind(operation.to_string()).bind(cost).execute(w.db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.usage_recovery_attempts(attempt_id,operation_id,route_index,channel_id,credential_id,base_url,configuration_updated_at,credential_configuration,destination_policy,supplier_price,lookup_supported,started_at) VALUES($1,$2,0,$3,$4,'https://api.openai.com/v1',clock_timestamp(),'fixture','{}','{}',false,$5::text::timestamptz)").bind(attempt).bind(operation).bind(w.channel).bind(Uuid::new_v4()).bind(at).execute(w.db.pool()).await.unwrap();
    operation
}
#[tokio::test]
async fn exact_import_replay_revision_and_recompare_preserve_old_export() {
    let w = world().await;
    durable_attempt(&w, Some(60000), "2026-11-01T12:00:00Z").await;
    let first =
        w.db.import_supplier_statement(&w.owner, &w.input)
            .await
            .unwrap();
    let source = first["sourceId"].as_str().unwrap().parse().unwrap();
    assert_eq!(
        first["comparison"]["document"]["amountRelation"],
        "equal_to_known_estimate"
    );
    assert_eq!(
        first["comparison"]["document"]["coverage"]["complete"],
        false
    );
    let op = durable_attempt(&w, None, "2026-11-01T13:00:00Z").await;
    let request = SupplierComparisonRequest {
        idempotency_key: "recompare".into(),
        reason: "Capture newly visible attempt".into(),
    };
    let next =
        w.db.compare_supplier_statement(&w.owner, source, &request)
            .await
            .unwrap();
    assert_eq!(next["document"]["coverage"]["unknownAttempts"], 1);
    assert_eq!(
        w.db.import_supplier_statement(&w.owner, &w.input)
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        w.db.supplier_statement(&w.owner, source, None)
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        w.db.compare_supplier_statement(&w.owner, source, &request)
            .await
            .unwrap(),
        next
    );
    sqlx::query("UPDATE oxsum.usage_recovery SET supplier_cost_minor=100,closed_at=clock_timestamp() WHERE operation_id=$1").bind(op).execute(w.db.pool()).await.unwrap();
    assert_eq!(
        w.db.supplier_statement(&w.owner, source, None)
            .await
            .unwrap(),
        first
    );
    let mut revised = w.input.clone();
    revised.idempotency_key = Uuid::new_v4().to_string();
    revised.pages[0].body = revised.pages[0].body.replace("0.06", "0.07");
    let second =
        w.db.import_supplier_statement(&w.owner, &revised)
            .await
            .unwrap();
    assert_eq!(second["sourceVersion"], 2);
    assert_eq!(
        w.db.supplier_statement_comparisons(&w.owner, source, None)
            .await
            .unwrap()
            .len(),
        2
    );
    use sha2::{Digest, Sha256};
    for doc in [&first, &first["comparison"], &next] {
        let mut h = Sha256::new();
        h.update(b"oxsum/supplier-statement/v1\0");
        h.update(doc["canonical"].as_str().unwrap().as_bytes());
        assert_eq!(
            doubleentry::Hash::from_bytes(h.finalize().into()).to_hex(),
            doc["digest"]
        );
    }
    assert!(
        sqlx::query("UPDATE oxsum.supplier_statement_sources SET digest='bad' WHERE source_id=$1")
            .bind(source)
            .execute(w.db.pool())
            .await
            .is_err()
    );
    let mut changed = w.input.clone();
    changed.reason = "different".into();
    assert!(matches!(
        w.db.import_supplier_statement(&w.owner, &changed).await,
        Err(WalletError::Conflict(_))
    ));
}

#[tokio::test]
async fn busy_authority_fails_fast_then_replay_and_new_versions_serialize() {
    let w = world().await;
    let business = w.db.business_ownership().await.unwrap().business.id;
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("SELECT business_id FROM oxsum.businesses WHERE business_id=$1 FOR UPDATE")
        .bind(business)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        w.db.import_supplier_statement(&w.owner, &w.input),
        w.db.import_supplier_statement(&w.owner, &w.input)
    );
    assert!(matches!(a, Err(WalletError::PoolBusy)));
    assert!(matches!(b, Err(WalletError::PoolBusy)));
    tx.commit().await.unwrap();
    let (a, b) = tokio::join!(
        w.db.import_supplier_statement(&w.owner, &w.input),
        w.db.import_supplier_statement(&w.owner, &w.input)
    );
    let mut successes = Vec::new();
    for result in [a, b] {
        match result {
            Ok(value) => successes.push(value),
            Err(error) => assert!(matches!(error, WalletError::PoolBusy), "{error:?}"),
        }
    }
    let first = successes.first().unwrap().clone();
    assert!(successes.iter().all(|value| *value == first));
    let retry =
        w.db.import_supplier_statement(&w.owner, &w.input)
            .await
            .unwrap();
    assert_eq!(first, retry);
    let mut second = w.input.clone();
    second.idempotency_key = Uuid::new_v4().to_string();
    let mut third = second.clone();
    third.idempotency_key = Uuid::new_v4().to_string();
    let (a, b) = tokio::join!(
        w.db.import_supplier_statement(&w.owner, &second),
        w.db.import_supplier_statement(&w.owner, &third)
    );
    for (result, input) in [(a, &second), (b, &third)] {
        if let Err(error) = result {
            assert!(matches!(error, WalletError::PoolBusy));
            w.db.import_supplier_statement(&w.owner, input)
                .await
                .unwrap();
        }
    }
    let versions:Vec<i64>=sqlx::query_scalar("SELECT source_version FROM oxsum.supplier_statement_sources WHERE business_id=$1 ORDER BY source_version").bind(business).fetch_all(w.db.pool()).await.unwrap();
    assert_eq!(versions, vec![1, 2, 3]);
}
#[tokio::test]
async fn revoked_authority_cannot_retry_or_import_after_waiting_for_role_change() {
    let w = world().await;
    let first =
        w.db.import_supplier_statement(&w.owner, &w.input)
            .await
            .unwrap();
    let principal =
        w.db.business_capability(&w.owner, oxsum_core::BusinessCapability::FundsWrite)
            .await
            .unwrap();
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("SELECT business_id FROM oxsum.businesses WHERE business_id=$1 FOR UPDATE")
        .bind(principal.business_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.business_staff SET role='viewer' WHERE business_id=$1 AND user_id=$2",
    )
    .bind(principal.business_id)
    .bind(principal.user_id)
    .execute(&mut *tx)
    .await
    .unwrap();
    assert!(matches!(
        w.db.import_supplier_statement(&w.owner, &w.input).await,
        Err(WalletError::PoolBusy)
    ));
    tx.commit().await.unwrap();
    assert!(matches!(
        w.db.import_supplier_statement(&w.owner, &w.input).await,
        Err(WalletError::Forbidden(_))
    ));
    let id = first["sourceId"].as_str().unwrap().parse().unwrap();
    assert!(w.db.supplier_statement(&w.owner, id, None).await.is_ok());
}
#[tokio::test]
async fn utc_attempt_cohort_keeps_unknown_failures_and_retries_separate() {
    let w = world().await;
    durable_attempt(&w, Some(50000), "2026-11-01T23:59:59Z").await;
    durable_attempt(&w, Some(99999), "2026-11-02T00:00:00Z").await;
    durable_attempt(&w, None, "2026-11-01T00:00:00Z").await;
    let name: String = sqlx::query_scalar("SELECT name FROM oxsum.channels WHERE channel_id=$1")
        .bind(w.channel)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    let op = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.request_operations(operation_id,organization_id,surface,fingerprint,bound_minor,model,channel,created_at) VALUES($1,$2,'chat',$3,0,'fixture',$4,'2026-11-01T12:00:00Z')").bind(op).bind(w.org).bind(op.to_string()).bind(&name).execute(w.db.pool()).await.unwrap();
    for status in ["refused", "answered"] {
        sqlx::query("INSERT INTO oxsum.request_attempts(attempt_id,operation_id,channel,status,started_at) VALUES($1,$2,$3,$4,'2026-11-01T12:00:00Z')").bind(Uuid::new_v4()).bind(op).bind(&name).bind(status).execute(w.db.pool()).await.unwrap();
    }
    let result =
        w.db.import_supplier_statement(&w.owner, &w.input)
            .await
            .unwrap();
    let c = &result["comparison"]["document"];
    assert_eq!(c["knownLocalCostMinor"], "50000");
    assert_eq!(c["coverage"]["unknownAttempts"], 3);
    assert_eq!(c["coverage"]["retryAttempts"], 1);
    assert_eq!(c["coverage"]["failedOrCancelledAttempts"], 1);
    assert_eq!(c["deltaAgainstKnownUsd"], "0.01");
    let text = serde_json::to_string(&result).unwrap();
    assert!(!text.contains("fixture-private-key"));
}

#[tokio::test]
async fn conversion_residual_and_overflow_never_corrupt_prior_versions() {
    let mut w = world().await;
    durable_attempt(&w, Some(1), "2026-11-01T12:00:00Z").await;
    w.input.usd_per_credit = "0.000000000000000001".into();
    w.input.pages[0].body = w.input.pages[0].body.replace("0.06", "0");
    let result =
        w.db.import_supplier_statement(&w.owner, &w.input)
            .await
            .unwrap();
    let c = &result["comparison"]["document"];
    assert_eq!(c["knownLocalCostUsd"], "0");
    assert_eq!(c["deltaAgainstKnownUsd"], "0");
    assert_eq!(c["deltaResidualNumerator"], "-1");
    assert_eq!(c["amountRelation"], "different_from_known_estimate");
    durable_attempt(&w, Some(i64::MAX), "2026-11-01T13:00:00Z").await;
    let mut huge = w.input.clone();
    huge.idempotency_key = "overflow".into();
    huge.usd_per_credit = "100000000000000000000".into();
    assert!(
        w.db.import_supplier_statement(&w.owner, &huge)
            .await
            .is_err()
    );
    assert_eq!(
        w.db.supplier_statements(&w.owner, None)
            .await
            .unwrap()
            .len(),
        1
    );
}
#[tokio::test]
async fn retained_attempt_deficit_cache_and_provider_identity_are_honest() {
    let w = world().await;
    let operation = durable_attempt(&w, Some(60000), "2026-11-01T12:00:00Z").await;
    // Simulate retained historical context from a compatible non-OpenAI origin.
    // Only this test fixture temporarily disables append protection.
    sqlx::raw_sql("ALTER TABLE oxsum.usage_recovery_attempts DISABLE TRIGGER usage_recovery_attempts_immutable").execute(w.db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.usage_recovery_attempts SET base_url='https://other.example/v1' WHERE operation_id=$1").bind(operation).execute(w.db.pool()).await.unwrap();
    sqlx::raw_sql("ALTER TABLE oxsum.usage_recovery_attempts ENABLE TRIGGER usage_recovery_attempts_immutable").execute(w.db.pool()).await.unwrap();
    let name: String = sqlx::query_scalar("SELECT name FROM oxsum.channels WHERE channel_id=$1")
        .bind(w.channel)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    let tenant = w.db.organization_by_id(w.org).await.unwrap().tenant_id;
    for (kind, basis, attempts) in [("cache_hit", "cache_reuse", 0), ("usage", "missing", 3)] {
        let op = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.request_operations(operation_id,organization_id,surface,fingerprint,bound_minor,model,channel,created_at,outcome_kind) VALUES($1,$2,'chat',$3,0,'fixture',$4,'2026-11-01T12:00:00Z',$5)").bind(op).bind(w.org).bind(op.to_string()).bind(&name).bind(kind).execute(w.db.pool()).await.unwrap();
        if attempts > 0 {
            sqlx::query("INSERT INTO oxsum.request_attempts(attempt_id,operation_id,channel,status,started_at) VALUES($1,$2,$3,'refused','2026-11-01T12:00:00Z')").bind(Uuid::new_v4()).bind(op).bind(&name).execute(w.db.pool()).await.unwrap();
        }
        sqlx::query("INSERT INTO oxsum.usage_records(request_id,tenant_id,model,channel,price_version,kind,entry_id,input_tokens,output_tokens,charged_minor,freeze_minor,settled_at,cost_basis,upstream_attempts) VALUES($1,$2,'fixture',$3,1,$4,$5,0,0,0,0,'2026-11-01T12:00:00Z',$6,$7)").bind(op.to_string()).bind(&tenant).bind(&name).bind(kind).bind(Uuid::new_v4()).bind(basis).bind(attempts).execute(w.db.pool()).await.unwrap();
    }
    let result =
        w.db.import_supplier_statement(&w.owner, &w.input)
            .await
            .unwrap();
    let c = &result["comparison"]["document"];
    assert_eq!(c["knownLocalCostMinor"], "0");
    assert_eq!(c["coverage"]["unknownAttempts"], 2);
    assert_eq!(c["coverage"]["diagnosticCacheRows"], 1);
    assert_eq!(c["coverage"]["operationsWithoutAttempts"], 0);
    assert_eq!(c["coverage"]["diagnosticUnrecordedAttempts"], 2);
}
#[tokio::test]
async fn exact_comparison_is_source_scoped_and_history_is_bounded() {
    let w = world().await;
    let first =
        w.db.import_supplier_statement(&w.owner, &w.input)
            .await
            .unwrap();
    let id = first["sourceId"].as_str().unwrap().parse().unwrap();
    let mut next = w.input.clone();
    next.idempotency_key = "second-source".into();
    let second =
        w.db.import_supplier_statement(&w.owner, &next)
            .await
            .unwrap();
    let other = second["comparison"]["comparisonId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(matches!(
        w.db.supplier_statement(&w.owner, id, Some(other)).await,
        Err(WalletError::NotFound(_))
    ));
    for n in 0..51 {
        w.db.compare_supplier_statement(
            &w.owner,
            id,
            &SupplierComparisonRequest {
                idempotency_key: format!("history-{n}"),
                reason: "Pagination fixture".into(),
            },
        )
        .await
        .unwrap();
    }
    let page =
        w.db.supplier_statement_comparisons(&w.owner, id, None)
            .await
            .unwrap();
    assert_eq!(page.len(), 50);
    let cursor = page[49]["comparisonId"].as_str().unwrap().parse().unwrap();
    let rest =
        w.db.supplier_statement_comparisons(&w.owner, id, Some(cursor))
            .await
            .unwrap();
    assert_eq!(rest.len(), 2);
    assert_eq!(
        w.db.supplier_statement(&w.owner, id, None).await.unwrap(),
        first
    );
}

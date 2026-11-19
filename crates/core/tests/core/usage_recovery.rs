//! Durable provider evidence closure and hostile raw usage regression coverage.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::usage_recovery::{ObservedTextUsage, price_observed_text};
use oxsum_core::{
    BillingPolicyAccept, BillingPolicyPublish, BillingRouteTerms, Db, GatewayBillingTerms,
    NewOperation, NewUser, OpenHold, Price, Tenants, Wallet,
};
use serde_json::{Value, json};
use uuid::Uuid;

fn price() -> Price {
    serde_json::from_value(json!({"inputPricePerMillion":1000000,"outputPricePerMillion":2000000,"maxOutputTokens":100})).unwrap()
}
fn report(usage: Value) -> ObservedTextUsage {
    ObservedTextUsage::parse(
        &json!({"id":"chatcmpl-evidence","object":"chat.completion","choices":[{"index":0,"message":{"content":"ok"},"finish_reason":"stop"}],"usage":usage}),
    )
}
#[test]
fn strict_presence_contradictions_and_partial_rate_selection() {
    let p = price();
    let r = report(json!({"prompt_tokens":100,"total_tokens":50}));
    assert_eq!(r.input, None);
    for value in [json!(-1), json!(1.5), json!("8"), json!(i64::MAX)] {
        let r = report(json!({"prompt_tokens":value,"completion_tokens":3}));
        assert_eq!(r.input, None);
        assert_eq!(r.output, Some(3));
    }
    let r = report(
        json!({"prompt_tokens":10,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":11}}),
    );
    assert_eq!(r.input, None);
    assert_eq!(r.output, Some(4));
    let partial = report(json!({"completion_tokens":4}));
    let (_, lines, waived) = price_observed_text(&p, &partial, None, true).unwrap();
    assert_eq!(lines.total_minor().unwrap(), 8);
    assert!(!waived.is_empty());
    let mut conditional = p.clone();
    conditional.rules=serde_json::from_value(json!([{"match":{"minInputTokens":5},"price":{"inputPricePerMillion":1,"outputPricePerMillion":1,"maxOutputTokens":100}}])).unwrap();
    assert_eq!(
        price_observed_text(&conditional, &partial, None, true)
            .unwrap()
            .1
            .total_minor()
            .unwrap(),
        0
    );
    let mut split = p.clone();
    split.cache_read_price_per_million = Some(100);
    split.reasoning_price_per_million = Some(100);
    assert_eq!(
        price_observed_text(
            &split,
            &report(json!({"prompt_tokens":10,"completion_tokens":4})),
            None,
            true
        )
        .unwrap()
        .1
        .total_minor()
        .unwrap(),
        0
    );
    split.cost_per_request = Some(7);
    assert_eq!(
        price_observed_text(&split, &report(json!({})), None, true)
            .unwrap()
            .1
            .total_minor()
            .unwrap(),
        7
    );
}
#[test]
fn cumulative_evidence_merges_without_summing_or_overwriting_conflicts() {
    let mut contradiction = report(json!({"prompt_tokens":100,"total_tokens":150}));
    contradiction.merge(&report(json!({"completion_tokens":100,"total_tokens":150})));
    assert_eq!((contradiction.input, contradiction.output), (None, None));
    let mut r = report(json!({"prompt_tokens":10}));
    r.merge(&report(json!({"completion_tokens":4})));
    assert_eq!((r.input, r.output), (Some(10), Some(4)));
    r.merge(&report(json!({"prompt_tokens":10,"completion_tokens":4})));
    assert_eq!(r.output, Some(4));
    r.merge(&report(json!({"prompt_tokens":20})));
    assert_eq!(r.input, None);
    r.merge(&report(json!({"prompt_tokens":10})));
    assert_eq!(r.input, None);
}
struct World {
    db: Db,
    org: Uuid,
    operation: Uuid,
    wallet: Wallet,
    tenants: Tenants,
    serving: oxsum_core::Serving,
    secret: oxsum_core::SecretKey,
}
async fn world(discount: Option<i64>) -> World {
    funded_world(discount, false).await
}
async fn funded_world(discount: Option<i64>, subscription: bool) -> World {
    let url = oxsum_testkit::suite_url!().expect("usage recovery acceptance requires PostgreSQL");
    let db = Db::connect(&url, 1).await.unwrap();
    db.migrate().await.unwrap();
    let staff = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "recovery customer password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = user.organization.id;
    let session = db
        .login(&user.user.email, "recovery customer password")
        .await
        .unwrap()
        .into_session();
    let versions = db.business_billing_policies(&staff).await.unwrap();
    let policy = db
        .publish_billing_policy(
            &staff,
            &BillingPolicyPublish {
                idempotency_key: Uuid::new_v4().to_string(),
                expected_version: versions.first().map_or(0, |p| p.version),
                implementation: "provider-reported-text-v1".into(),
                evidence_window_secs: 900,
                reason: "Fixture policy acceptance".into(),
            },
        )
        .await
        .unwrap();
    db.accept_billing_policy(
        &session.token,
        &BillingPolicyAccept {
            expected_organization_id: org,
            idempotency_key: Uuid::new_v4().to_string(),
            version: policy.version,
            digest: policy.digest,
            expected_acceptance_id: None,
        },
    )
    .await
    .unwrap();
    let minted = db
        .create_key(org, None, None, None, Default::default())
        .await
        .unwrap();
    let (organization, key) = db.authenticate(&minted.secret).await.unwrap().unwrap();
    let secret = oxsum_core::SecretKey::from_bytes([19; 32]);
    let channel = format!("r-{}", Uuid::new_v4().simple());
    db.set_channel_pool_policy(
        &channel,
        "https://api.openai.com/v1",
        Some(&["fixture-secret".into()]),
        "openai",
        &secret,
        None,
    )
    .await
    .unwrap();
    db.append_price(&channel, "text", price(), 100)
        .await
        .unwrap();
    let serving = db
        .servings("text", &secret)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.channel == channel)
        .unwrap();
    if subscription {
        let plan = db
            .set_subscription_plan(
                &format!("q-{}", &Uuid::new_v4().simple().to_string()[..8]),
                Some(100),
                oxsum_core::ResetPeriod::Daily,
                true,
                None,
                None,
                true,
                1,
            )
            .await
            .unwrap();
        db.bind_subscription(org, plan.plan_id, "fixture", None)
            .await
            .unwrap();
    }
    let access = db.serving_access(org, Some(key.key_id)).await.unwrap();
    let wallet = Wallet::open(db.pool().clone(), &organization.tenant_id)
        .await
        .unwrap();
    let day = time::OffsetDateTime::now_utc().date();
    wallet.top_up("fund", 1000, day).await.unwrap();
    let operation = Uuid::new_v4();
    let hold_key = format!("req-{operation}:hold");
    let watch = OpenHold {
        hold_key: hold_key.clone(),
        tenant_id: organization.tenant_id.clone(),
        request_id: operation.to_string(),
        model: "text".into(),
        channel: serving.channel.clone(),
        price_version: serving.version,
        input_price: 1000000,
        output_price: 2000000,
        freeze_minor: 100,
        key_id: Some(key.key_id),
        mandate_id: None,
        end_user: None,
        service_tier: None,
        tags: Default::default(),
        sweep_attempts: 0,
        last_error: None,
        dead_at: None,
    };
    let new = NewOperation {
        operation_id: operation,
        organization_id: org,
        key_id: Some(key.key_id),
        surface: "chat".into(),
        fingerprint: operation.to_string(),
        bound_minor: 100,
        model: "text".into(),
        channel: Some(serving.channel.clone()),
        customer_id: None,
        terms_version: Some(serving.version),
    };
    let terms = GatewayBillingTerms {
        business_id: access.business_id,
        organization_id: org,
        operation_id: operation,
        key_id: key.key_id,
        hold_key: hold_key.clone(),
        estimator: "unused".into(),
        version: 1,
        surface: "chat/completions".into(),
        stream: false,
        arithmetic_version: "credit-1e6-ceil-discount-cap-v1".into(),
        policy_applies: true,
        selection: db.billing_policy_selection(org).await.unwrap(),
        sale_digest: None,
        discount_percent: discount,
        requested_model: "text".into(),
        output_bound: 100,
        ceiling_minor: 100,
        routes: vec![BillingRouteTerms {
            channel_id: serving.channel_id,
            channel: serving.channel.clone(),
            price_version: serving.version,
            model: "text".into(),
            upstream_model: None,
            price: price(),
        }],
        arithmetic: "fixture".into(),
    };
    wallet
        .admit_gateway_with_terms(
            &db,
            &key,
            &watch,
            &new,
            &access,
            &oxsum_core::hold_description(&operation.to_string(), "text", 100).unwrap(),
            "worker",
            day,
            None,
            Some(&terms),
        )
        .await
        .unwrap();
    World {
        tenants: Tenants::new(db.pool().clone()),
        db,
        org,
        operation,
        wallet,
        serving,
        secret,
    }
}
async fn terminal(w: &World, r: &ObservedTextUsage) {
    w.db.observe_recovery_terminal(w.operation, "worker", "answered", r, &[])
        .await
        .unwrap();
}
#[tokio::test]
async fn no_discount_and_explicit_zero_close_once_with_pool_one() {
    for discount in [None, Some(0), Some(50)] {
        let w = world(discount).await;
        terminal(
            &w,
            &report(json!({"prompt_tokens":10,"completion_tokens":4,"total_tokens":14})),
        )
        .await;
        let claim =
            w.db.claim_usage_recovery(w.operation)
                .await
                .unwrap()
                .unwrap();
        let amount =
            w.db.finish_usage_recovery(&w.tenants, &claim, None, true)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(amount, if discount == Some(50) { 9 } else { 18 });
        assert_eq!(w.wallet.reserved().await.unwrap(), 0);
        assert_eq!(w.wallet.settled().await.unwrap(), 1000 - amount);
        assert!(
            w.db.finish_usage_recovery(
                &w.tenants,
                &claim,
                Some(&report(json!({"prompt_tokens":90,"completion_tokens":50}))),
                true
            )
            .await
            .is_err()
        );
        assert_eq!(w.wallet.settled().await.unwrap(), 1000 - amount);
        let receipt =
            w.db.usage_recovery_receipt(w.org, w.operation)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(receipt.state, "closed");
        assert!(!receipt.supplier_cost_known);
    }
}
#[tokio::test]
async fn no_lookup_closes_partial_and_never_invents_missing_usage() {
    let w = world(None).await;
    terminal(&w, &report(json!({"completion_tokens":4}))).await;
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, true)
            .await
            .unwrap(),
        Some(8)
    );
    let r =
        w.db.usage_recovery_receipt(w.org, w.operation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(r.observed.input, None);
    assert_eq!(r.observed.output, Some(4));
    assert_eq!(r.waived, vec!["input_or_cached_split"]);
}
#[tokio::test]
async fn generic_sweeper_cannot_take_pending_or_live_policy_reservations() {
    let w = world(None).await;
    let now = time::OffsetDateTime::now_utc();
    oxsum_core::sweep_stale_holds(&w.db, &w.tenants, now + time::Duration::days(1), now.date())
        .await
        .unwrap();
    assert_eq!(w.wallet.reserved().await.unwrap(), 100);
    assert!(
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE oxsum.request_operations SET leased_until=clock_timestamp()-interval '1 minute' WHERE operation_id=$1").bind(w.operation).execute(w.db.pool()).await.unwrap();
    assert!(w.db.operations_past_lease(64).await.unwrap().is_empty());
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, true)
            .await
            .unwrap(),
        Some(0)
    );
    assert_eq!(w.wallet.reserved().await.unwrap(), 0);
}
#[tokio::test]
async fn stale_claim_rolls_back_and_deadline_does_not_accept_late_report() {
    let w = world(None).await;
    terminal(&w, &report(json!({"prompt_tokens":10}))).await;
    let old =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    sqlx::query("UPDATE oxsum.usage_recovery SET claim_until=clock_timestamp()-interval '1 second' WHERE operation_id=$1").bind(w.operation).execute(w.db.pool()).await.unwrap();
    let new =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert!(
        w.db.finish_usage_recovery(&w.tenants, &old, None, true)
            .await
            .is_err()
    );
    assert_eq!(w.wallet.reserved().await.unwrap(), 100);
    sqlx::query("UPDATE oxsum.usage_recovery SET deadline=clock_timestamp()-interval '1 second' WHERE operation_id=$1").bind(w.operation).execute(w.db.pool()).await.unwrap();
    assert_eq!(
        w.db.finish_usage_recovery(
            &w.tenants,
            &new,
            Some(&report(json!({"prompt_tokens":10,"completion_tokens":20}))),
            false
        )
        .await
        .unwrap(),
        Some(10)
    );
}
#[tokio::test]
async fn recovered_partial_retains_first_report_and_huge_valid_quantity_caps() {
    let w = world(Some(0)).await;
    terminal(&w, &report(json!({"prompt_tokens":10}))).await;
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        w.db.finish_usage_recovery(
            &w.tenants,
            &claim,
            Some(&report(json!({"completion_tokens":4}))),
            true
        )
        .await
        .unwrap(),
        Some(18)
    );
    let w = world(None).await;
    terminal(
        &w,
        &report(json!({"prompt_tokens":1000000000,"completion_tokens":1000000000})),
    )
    .await;
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, true)
            .await
            .unwrap(),
        Some(100)
    );
    assert_eq!(w.wallet.reserved().await.unwrap(), 0);
}

#[tokio::test]
async fn withholding_retains_long_guardrail_reason_and_closes_without_waiting() {
    let w = world(None).await;
    let reason = "完整 reason ".repeat(900);
    let note = oxsum_core::SettlementGuardrailOwned {
        policy: "fixture".into(),
        verdict: "blocked",
        reason: reason.clone(),
        phase: "post_call",
        streamed: false,
    };
    w.db.observe_recovery_terminal(
        w.operation,
        "worker",
        "withheld",
        &report(json!({})),
        &[note],
    )
    .await
    .unwrap();
    let mut claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    claim.needs_lookup = true;
    assert_eq!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, false)
            .await
            .unwrap(),
        Some(0)
    );
    let details: Value =
        sqlx::query_scalar("SELECT usage_details FROM oxsum.usage_records WHERE request_id=$1")
            .bind(w.operation.to_string())
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(
        details["oxsumReasonEvidence"]["guardrails"][0]["reason"],
        reason
    );
}

#[tokio::test]
async fn durable_dispatch_lookup_configuration_and_pending_deadline_are_pinned() {
    let w = world(None).await;
    let experiment = oxsum_core::ExperimentBinding {
        experiment_id: Uuid::new_v4(),
        variant: "text".into(),
    };
    let dispatch = oxsum_core::usage_recovery::RecoveryDispatch {
        route_index: 0,
        serving: &w.serving,
        credential_id: w.serving.credentials[0].credential_id,
        stored: true,
        experiment: Some(&experiment),
    };
    let attempt =
        w.db.begin_recovery_attempt(w.operation, "worker", &dispatch, &w.secret)
            .await
            .unwrap();
    assert!(
        w.db.begin_recovery_attempt(w.operation, "worker", &dispatch, &w.secret)
            .await
            .is_err()
    );
    terminal(&w, &report(json!({"prompt_tokens":10}))).await;
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert!(claim.needs_lookup);
    assert!(
        w.db.recovery_lookup(&claim, &w.secret)
            .await
            .unwrap()
            .is_some()
    );
    let deadline = claim.deadline;
    assert_eq!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, false)
            .await
            .unwrap(),
        None
    );
    assert_eq!(w.wallet.reserved().await.unwrap(), 100);
    w.db.observe_recovery_terminal(
        w.operation,
        "worker",
        "answered",
        &report(json!({"prompt_tokens":20})),
        &[],
    )
    .await
    .unwrap();
    let next =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(next.deadline, deadline);
    assert_eq!(
        w.db.finish_usage_recovery(
            &w.tenants,
            &next,
            Some(&report(json!({"completion_tokens":4}))),
            false
        )
        .await
        .unwrap(),
        Some(18)
    );
    let (status, details): (String, Value) =
        sqlx::query_as("SELECT status,usage_json FROM oxsum.request_attempts WHERE attempt_id=$1")
            .bind(attempt)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(status, "answered");
    assert_eq!(details["output"], 4);
    let details: Value =
        sqlx::query_scalar("SELECT usage_details FROM oxsum.usage_records WHERE request_id=$1")
            .bind(w.operation.to_string())
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(
        details["experiment"]["id"],
        experiment.experiment_id.to_string()
    );
}
#[tokio::test]
async fn changed_provider_configuration_refuses_dispatch_or_lookup() {
    let w = world(None).await;
    let mut changed = w.serving.clone();
    changed.configuration_updated_at -= time::Duration::seconds(1);
    let dispatch = oxsum_core::usage_recovery::RecoveryDispatch {
        route_index: 0,
        serving: &changed,
        credential_id: w.serving.credentials[0].credential_id,
        stored: true,
        experiment: None,
    };
    assert!(
        w.db.begin_recovery_attempt(w.operation, "worker", &dispatch, &w.secret)
            .await
            .is_err()
    );
    let dispatch = oxsum_core::usage_recovery::RecoveryDispatch {
        serving: &w.serving,
        ..dispatch
    };
    w.db.begin_recovery_attempt(w.operation, "worker", &dispatch, &w.secret)
        .await
        .unwrap();
    terminal(&w, &report(json!({}))).await;
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    sqlx::query("UPDATE oxsum.channels SET base_url='https://example.com/v1',updated_at=clock_timestamp() WHERE channel_id=$1").bind(w.serving.channel_id).execute(w.db.pool()).await.unwrap();
    assert!(
        w.db.recovery_lookup(&claim, &w.secret)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, true)
            .await
            .unwrap(),
        Some(0)
    );
}

#[tokio::test]
async fn sealed_posting_day_rolls_back_and_sealed_execution_day_posts_today() {
    use doubleentry::storage::postgres::PostgresStore;
    use doubleentry::{LedgerId, LedgerStore, Period, PeriodId, PeriodState};
    let w = world(None).await;
    let org = w.db.organization_by_id(w.org).await.unwrap();
    let day = time::OffsetDateTime::now_utc().date();
    let store = PostgresStore::<6>::new(
        w.db.pool().clone(),
        LedgerId::new(format!("tenant-{}", org.tenant_id)).unwrap(),
    )
    .in_schema(&format!("ledger_{}", org.tenant_id));
    let period = PeriodId::new("sealed-current").unwrap();
    store
        .define_period(&Period::new(period.clone(), day, day).unwrap())
        .await
        .unwrap();
    store
        .transition_period(&period, PeriodState::Closing)
        .await
        .unwrap();
    store.seal_period(&period).await.unwrap();
    terminal(
        &w,
        &report(json!({"prompt_tokens":10,"completion_tokens":4})),
    )
    .await;
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, true)
            .await
            .is_err()
    );
    assert_eq!(w.wallet.reserved().await.unwrap(), 100);
    assert_eq!(
        w.db.open_holds_for_tenant(&org.tenant_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        w.db.operation(w.org, w.operation)
            .await
            .unwrap()
            .unwrap()
            .completed_at
            .is_none()
    );
    assert!(
        w.db.usage_recovery_receipt(w.org, w.operation)
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_none()
    );
    let w = world(None).await;
    let org = w.db.organization_by_id(w.org).await.unwrap();
    let store = PostgresStore::<6>::new(
        w.db.pool().clone(),
        LedgerId::new(format!("tenant-{}", org.tenant_id)).unwrap(),
    )
    .in_schema(&format!("ledger_{}", org.tenant_id));
    let yesterday = day.previous_day().unwrap();
    let period = PeriodId::new("sealed-execution").unwrap();
    store
        .define_period(&Period::new(period.clone(), yesterday, yesterday).unwrap())
        .await
        .unwrap();
    store
        .transition_period(&period, PeriodState::Closing)
        .await
        .unwrap();
    store.seal_period(&period).await.unwrap();
    sqlx::query("UPDATE oxsum.request_operations SET created_at=created_at-interval '1 day' WHERE operation_id=$1").bind(w.operation).execute(w.db.pool()).await.unwrap();
    terminal(
        &w,
        &report(json!({"prompt_tokens":10,"completion_tokens":4})),
    )
    .await;
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, true)
            .await
            .unwrap(),
        Some(18)
    );
}

#[tokio::test]
async fn post_ledger_failure_rolls_back_then_concurrent_finalizers_debit_once() {
    let w = world(None).await;
    terminal(
        &w,
        &report(json!({"prompt_tokens":10,"completion_tokens":4})),
    )
    .await;
    let (a, b) = tokio::join!(
        w.db.claim_usage_recovery(w.operation),
        w.db.claim_usage_recovery(w.operation)
    );
    let claims: Vec<_> = [a.unwrap(), b.unwrap()].into_iter().flatten().collect();
    assert_eq!(claims.len(), 1);
    let claim = &claims[0];
    sqlx::raw_sql("CREATE FUNCTION oxsum.fail_recovery_close_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.closed_at IS NOT NULL THEN RAISE EXCEPTION 'fixture post-ledger rollback'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_recovery_close_fixture BEFORE UPDATE ON oxsum.usage_recovery FOR EACH ROW EXECUTE FUNCTION oxsum.fail_recovery_close_fixture();").execute(w.db.pool()).await.unwrap();
    assert!(
        w.db.finish_usage_recovery(&w.tenants, claim, None, true)
            .await
            .is_err()
    );
    assert_eq!(w.wallet.reserved().await.unwrap(), 100);
    assert_eq!(w.wallet.settled().await.unwrap(), 1000);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.usage_records WHERE request_id=$1")
            .bind(w.operation.to_string())
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
    sqlx::raw_sql("DROP TRIGGER fail_recovery_close_fixture ON oxsum.usage_recovery; DROP FUNCTION oxsum.fail_recovery_close_fixture();").execute(w.db.pool()).await.unwrap();
    let (a, b) = tokio::join!(
        w.db.finish_usage_recovery(&w.tenants, claim, None, true),
        w.db.finish_usage_recovery(&w.tenants, claim, None, true)
    );
    assert_eq!(
        [a, b]
            .into_iter()
            .filter(|r| matches!(r, Ok(Some(18))))
            .count(),
        1
    );
    assert_eq!(w.wallet.reserved().await.unwrap(), 0);
    assert_eq!(w.wallet.settled().await.unwrap(), 982);
}

#[tokio::test]
async fn deferred_subscription_closure_releases_only_its_original_period() {
    let w = funded_world(None, true).await;
    assert_eq!(w.wallet.reserved().await.unwrap(), 0);
    let sub = w.db.active_subscription(w.org).await.unwrap().unwrap();
    assert_eq!(sub.consumed_minor, 100);
    let old =
        w.db.subscription_periods(sub.subscription_id)
            .await
            .unwrap()[0]
            .period_id;
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_end=clock_timestamp()-interval '1 second' WHERE subscription_id=$1").bind(sub.subscription_id).execute(w.db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.subscription_periods SET period_end=clock_timestamp()-interval '1 second' WHERE period_id=$1").bind(old).execute(w.db.pool()).await.unwrap();
    w.db.rollover_due_subscriptions().await.unwrap();
    let fresh = format!("fresh-{}:hold", Uuid::new_v4());
    w.db.admit_subscription_draw(w.org, &fresh, 70)
        .await
        .unwrap();
    terminal(
        &w,
        &report(json!({"prompt_tokens":10,"completion_tokens":4})),
    )
    .await;
    let claim =
        w.db.claim_usage_recovery(w.operation)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        w.db.finish_usage_recovery(&w.tenants, &claim, None, true)
            .await
            .unwrap(),
        Some(18)
    );
    assert_eq!(w.wallet.settled().await.unwrap(), 1000);
    assert_eq!(
        w.db.active_subscription(w.org)
            .await
            .unwrap()
            .unwrap()
            .consumed_minor,
        70
    );
    let old_consumed: i64 = sqlx::query_scalar(
        "SELECT consumed_minor FROM oxsum.subscription_periods WHERE period_id=$1",
    )
    .bind(old)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert_eq!(old_consumed, 18);
    assert!(
        w.db.open_holds_for_tenant(&w.db.organization_by_id(w.org).await.unwrap().tenant_id)
            .await
            .unwrap()
            .is_empty()
    );
}

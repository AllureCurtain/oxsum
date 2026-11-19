//! Immutable offers and explicit customer acceptance under live authority.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{BillingPolicyAccept, BillingPolicyPublish, Db, NewUser, WalletError};
use uuid::Uuid;

async fn world() -> (Db, String, String, Uuid, Uuid) {
    let url = oxsum_testkit::suite_url!().expect("policy acceptance requires PostgreSQL");
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let staff = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "policy customer password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let session = db
        .login(&user.user.email, "policy customer password")
        .await
        .unwrap()
        .into_session();
    (db, staff, session.token, user.organization.id, user.user.id)
}
fn publish(key: &str, version: i64, implementation: &str) -> BillingPolicyPublish {
    BillingPolicyPublish {
        idempotency_key: key.into(),
        expected_version: version,
        implementation: implementation.into(),
        evidence_window_secs: 900,
        reason: "Reviewed prospective exception policy".into(),
    }
}

#[tokio::test]
async fn publication_is_not_consent_and_versions_do_not_replace_acceptance() {
    let (db, staff, customer, org, _) = world().await;
    let v = db
        .publish_billing_policy(&staff, &publish("legacy", 0, "legacy-estimation-v1"))
        .await
        .unwrap();
    assert!(v.runtime_ready);
    assert!(
        db.billing_policy_selection(org)
            .await
            .unwrap()
            .acceptance
            .is_none()
    );
    let request = BillingPolicyAccept {
        expected_organization_id: org,
        idempotency_key: "accept".into(),
        version: v.version,
        digest: v.digest.clone(),
        expected_acceptance_id: None,
    };
    let accepted = db.accept_billing_policy(&customer, &request).await.unwrap();
    assert_eq!(
        db.accept_billing_policy(&customer, &request).await.unwrap(),
        accepted
    );
    let v2 = db
        .publish_billing_policy(
            &staff,
            &publish("provider", v.version, "provider-reported-text-v1"),
        )
        .await
        .unwrap();
    assert!(!v2.runtime_ready);
    assert_eq!(
        db.billing_policy_selection(org)
            .await
            .unwrap()
            .acceptance
            .unwrap(),
        accepted
    );
    assert!(matches!(
        db.accept_billing_policy(
            &customer,
            &BillingPolicyAccept {
                expected_organization_id: org,
                idempotency_key: "not-ready".into(),
                version: v2.version,
                digest: v2.digest,
                expected_acceptance_id: Some(accepted.id)
            }
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(
        db.publish_billing_policy(&staff, &publish("legacy", 0, "legacy-estimation-v1"))
            .await
            .unwrap(),
        v
    );
    assert!(
        sqlx::query("UPDATE oxsum.billing_policy_versions SET canonical='{}' WHERE version=$1")
            .bind(v.version)
            .execute(db.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn acceptance_rechecks_customer_context_membership_digest_and_exact_intent() {
    let (db, staff, customer, org, user) = world().await;
    let v = db
        .publish_billing_policy(&staff, &publish("authority", 0, "legacy-estimation-v1"))
        .await
        .unwrap();
    let mut request = BillingPolicyAccept {
        expected_organization_id: org,
        idempotency_key: "accept".into(),
        version: v.version,
        digest: "wrong".into(),
        expected_acceptance_id: None,
    };
    assert!(matches!(
        db.accept_billing_policy(&customer, &request).await,
        Err(WalletError::Conflict(_))
    ));
    request.digest = v.digest;
    assert!(matches!(
        db.accept_billing_policy(&staff, &request).await,
        Err(WalletError::Forbidden(_))
    ));
    let first = db.accept_billing_policy(&customer, &request).await.unwrap();
    request.expected_acceptance_id = Some(first.id);
    assert!(matches!(
        db.accept_billing_policy(&customer, &request).await,
        Err(WalletError::Conflict(_))
    ));
    request.expected_acceptance_id = None;
    sqlx::query(
        "UPDATE oxsum.memberships SET role='member' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(org)
    .bind(user)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(matches!(
        db.accept_billing_policy(&customer, &request).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(db.customer_billing_policy(&customer).await.is_err());
}

#[tokio::test]
async fn concurrent_customer_acceptances_have_one_winner() {
    let (db, staff, customer, org, _) = world().await;
    let v = db
        .publish_billing_policy(&staff, &publish("concurrent", 0, "legacy-estimation-v1"))
        .await
        .unwrap();
    let a = BillingPolicyAccept {
        expected_organization_id: org,
        idempotency_key: "a".into(),
        version: v.version,
        digest: v.digest.clone(),
        expected_acceptance_id: None,
    };
    let b = BillingPolicyAccept {
        expected_organization_id: org,
        idempotency_key: "b".into(),
        version: v.version,
        digest: v.digest,
        expected_acceptance_id: None,
    };
    let (a, b) = tokio::join!(
        db.accept_billing_policy(&customer, &a),
        db.accept_billing_policy(&customer, &b)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.billing_policy_acceptances WHERE organization_id=$1",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn canonical_offer_and_old_current_selection_remain_readable_after_history_page() {
    use sha2::{Digest, Sha256};
    let (db, staff, customer, org, _) = world().await;
    let first = db
        .publish_billing_policy(&staff, &publish("first", 0, "legacy-estimation-v1"))
        .await
        .unwrap();
    let mut hash = Sha256::new();
    hash.update(b"oxsum/exception-policy/v1\0");
    hash.update(first.canonical.as_bytes());
    assert_eq!(
        doubleentry::Hash::from_bytes(hash.finalize().into()).to_hex(),
        first.digest
    );
    db.accept_billing_policy(
        &customer,
        &BillingPolicyAccept {
            expected_organization_id: org,
            idempotency_key: "first-accept".into(),
            version: first.version,
            digest: first.digest.clone(),
            expected_acceptance_id: None,
        },
    )
    .await
    .unwrap();
    for version in 1..=101 {
        db.publish_billing_policy(
            &staff,
            &publish(&format!("later-{version}"), version, "legacy-estimation-v1"),
        )
        .await
        .unwrap();
    }
    let state = db.customer_billing_policy(&customer).await.unwrap();
    assert_eq!(state.versions.len(), 100);
    assert_eq!(state.current_policy.unwrap(), first);
}

#[tokio::test]
async fn acceptance_racing_account_deletion_cannot_deadlock_or_reuse_removed_authority() {
    let (db, staff, customer, org, user) = world().await;
    let policy = db
        .publish_billing_policy(&staff, &publish("delete-race", 0, "legacy-estimation-v1"))
        .await
        .unwrap();
    let request = BillingPolicyAccept {
        expected_organization_id: org,
        idempotency_key: "accept-delete".into(),
        version: policy.version,
        digest: policy.digest,
        expected_acceptance_id: None,
    };
    let (accepted, deleted) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            db.accept_billing_policy(&customer, &request),
            db.delete_account(user, "policy customer password")
        )
    })
    .await
    .expect("no authority lock cycle");
    assert!(deleted.is_ok());
    let _ = accepted;
    assert!(db.accept_billing_policy(&customer, &request).await.is_err());
}

#[tokio::test]
async fn admission_and_acceptance_share_one_boundary_and_terms_survive_restart() {
    use oxsum_core::{BillingRouteTerms, GatewayBillingTerms, NewOperation, OpenHold, Wallet};
    use std::collections::BTreeMap;
    let (db, staff, customer, org, _) = world().await;
    let minted = db
        .create_key(
            org,
            Some("policy race".into()),
            None,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let (organization, key) = db.authenticate(&minted.secret).await.unwrap().unwrap();
    let access = db.serving_access(org, Some(key.key_id)).await.unwrap();
    let wallet = Wallet::open(db.pool().clone(), &organization.tenant_id)
        .await
        .unwrap();
    let day = time::OffsetDateTime::now_utc().date();
    wallet.top_up("policy-fund", 1000, day).await.unwrap();
    let policy = db
        .publish_billing_policy(&staff, &publish("race-boundary", 0, "legacy-estimation-v1"))
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let hold_key = format!("req-{id}:hold");
    let watch = OpenHold {
        hold_key: hold_key.clone(),
        tenant_id: organization.tenant_id.clone(),
        request_id: id.to_string(),
        model: "model".into(),
        channel: "fixture".into(),
        price_version: 1,
        input_price: 1000000,
        output_price: 1000000,
        freeze_minor: 100,
        key_id: Some(key.key_id),
        mandate_id: None,
        end_user: None,
        service_tier: None,
        tags: BTreeMap::new(),
        sweep_attempts: 0,
        last_error: None,
        dead_at: None,
    };
    let operation = NewOperation {
        operation_id: id,
        organization_id: org,
        key_id: Some(key.key_id),
        surface: "chat".into(),
        fingerprint: format!("policy/{id}"),
        bound_minor: 100,
        model: "model".into(),
        channel: Some("fixture".into()),
        customer_id: None,
        terms_version: Some(1),
    };
    let mut terms=GatewayBillingTerms{business_id:access.business_id,organization_id:org,operation_id:id,key_id:key.key_id,hold_key:hold_key.clone(),estimator:"oxsum-o200k-text-v1/tiktoken-rs-0.12.1".into(),version:1,surface:"chat/completions".into(),stream:false,arithmetic_version:"credit-1e6-ceil-discount-cap-v1".into(),policy_applies:true,selection:db.billing_policy_selection(org).await.unwrap(),sale_digest:None,discount_percent:None,requested_model:"model".into(),output_bound:10,ceiling_minor:100,routes:vec![BillingRouteTerms{channel_id:Uuid::new_v4(),channel:"fixture".into(),price_version:1,model:"model".into(),upstream_model:None,price:serde_json::from_value(serde_json::json!({"inputPricePerMillion":1000000,"outputPricePerMillion":1000000,"maxOutputTokens":10})).unwrap()}],arithmetic:"fixture".into()};
    let description = oxsum_core::hold_description(&id.to_string(), "model", 100).unwrap();
    let accept = BillingPolicyAccept {
        expected_organization_id: org,
        idempotency_key: "accept-race".into(),
        version: policy.version,
        digest: policy.digest,
        expected_acceptance_id: None,
    };
    let (admitted, accepted) = tokio::join!(
        wallet.admit_gateway_with_terms(
            &db,
            &key,
            &watch,
            &operation,
            &access,
            &description,
            "fixture",
            day,
            None,
            Some(&terms)
        ),
        db.accept_billing_policy(&customer, &accept)
    );
    let accepted = accepted.unwrap();
    if admitted.is_ok() {
        assert!(
            db.operation_billing_terms(org, id)
                .await
                .unwrap()
                .unwrap()
                .selection
                .acceptance
                .is_none()
        );
    } else {
        assert!(matches!(admitted, Err(WalletError::Conflict(_))));
        assert_eq!(wallet.reserved().await.unwrap(), 0);
        assert!(db.operation_billing_terms(org, id).await.unwrap().is_none());
        terms.selection = db.billing_policy_selection(org).await.unwrap();
        wallet
            .admit_gateway_with_terms(
                &db,
                &key,
                &watch,
                &operation,
                &access,
                &description,
                "fixture",
                day,
                None,
                Some(&terms),
            )
            .await
            .unwrap();
    }
    let stored = db.operation_billing_terms(org, id).await.unwrap().unwrap();
    assert!(
        db.operation_billing_terms(Uuid::new_v4(), id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(stored.routes[0].price.input_price_per_million, 1000000);
    let mut stale = terms.clone();
    stale.operation_id = Uuid::new_v4();
    stale.hold_key = format!("req-{}:hold", stale.operation_id);
    stale.selection.acceptance = None;
    stale.selection.policy = None;
    let mut new_operation = operation;
    new_operation.operation_id = stale.operation_id;
    new_operation.fingerprint = Uuid::new_v4().to_string();
    let mut new_watch = watch;
    new_watch.request_id = stale.operation_id.to_string();
    new_watch.hold_key = stale.hold_key.clone();
    assert!(matches!(
        wallet
            .admit_gateway_with_terms(
                &db,
                &key,
                &new_watch,
                &new_operation,
                &access,
                &description,
                "fixture",
                day,
                None,
                Some(&stale)
            )
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(
        db.billing_policy_selection(org)
            .await
            .unwrap()
            .acceptance
            .unwrap(),
        accepted
    );
    let mut tampered = stale.clone();
    tampered.selection = db.billing_policy_selection(org).await.unwrap();
    tampered
        .selection
        .policy
        .as_mut()
        .unwrap()
        .evidence_window_secs = 1;
    assert!(matches!(
        wallet
            .admit_gateway_with_terms(
                &db,
                &key,
                &new_watch,
                &new_operation,
                &access,
                &description,
                "fixture",
                day,
                None,
                Some(&tampered)
            )
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    assert_eq!(
        db.operation_billing_terms(org, id).await.unwrap().unwrap(),
        stored
    );
}

#[tokio::test]
async fn reviewed_customer_identity_cannot_follow_a_switched_browser_session() {
    let (db, staff, customer, org, user) = world().await;
    let policy = db
        .publish_billing_policy(&staff, &publish("context-guard", 0, "legacy-estimation-v1"))
        .await
        .unwrap();
    let review = db.customer_billing_policy(&customer).await.unwrap();
    assert_eq!(review.organization_id, org);
    let second = db
        .create_team_organization(user, "Second billing customer")
        .await
        .unwrap();
    let session = db.authenticate_session(&customer).await.unwrap().unwrap();
    db.switch_organization(session.session.id, user, second.id)
        .await
        .unwrap();
    let request = BillingPolicyAccept {
        expected_organization_id: review.organization_id,
        idempotency_key: "reviewed-first".into(),
        version: policy.version,
        digest: policy.digest,
        expected_acceptance_id: None,
    };
    assert!(matches!(
        db.accept_billing_policy(&customer, &request).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        db.billing_policy_selection(org)
            .await
            .unwrap()
            .acceptance
            .is_none()
    );
    assert!(
        db.billing_policy_selection(second.id)
            .await
            .unwrap()
            .acceptance
            .is_none()
    );
}

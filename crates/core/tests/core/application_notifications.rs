//! Notification authority, transactional lifecycle signals and retry lease fencing.
#[path = "../support/plan_payment.rs"]
mod payment;
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{
    ActingService, ApplicationNotificationEndpointRequest, ApplicationNotificationMutation, Db,
    IntegrationScope, SecretKey, WalletError,
};
use uuid::Uuid;
async fn db() -> Option<Db> {
    let url = oxsum_testkit::suite_db_url("application_notifications")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}
async fn grant(db: &Db, app: Uuid, scopes: Vec<IntegrationScope>) -> ActingService {
    let mut r = setup::request(vec![], scopes);
    r.application_id = Some(app);
    let made = db
        .create_integration_credential(&setup::owner(db).await, &r)
        .await
        .unwrap();
    db.authenticate_service(made.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap()
}
async fn application(db: &Db) -> Uuid {
    db.create_integration_application(
        &setup::owner(db).await,
        &oxsum_core::IntegrationApplicationRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            key: Uuid::new_v4().simple().to_string(),
            name: "Notifications".into(),
            customer_limit: 20,
            reason: "fixture".into(),
        },
    )
    .await
    .unwrap()
    .application_id
}
fn request() -> ApplicationNotificationEndpointRequest {
    ApplicationNotificationEndpointRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "owned receiver".into(),
        url: "https://receiver.example/oxsum".into(),
        destination_policy: Default::default(),
        events: vec![
            "customer.changed".into(),
            "key.changed".into(),
            "entitlement.changed".into(),
            "order.changed".into(),
            "statement.changed".into(),
            "evidence.published".into(),
            "balance.changed".into(),
        ],
    }
}
fn mutation() -> ApplicationNotificationMutation {
    ApplicationNotificationMutation {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "fixture mutation".into(),
    }
}
async fn provision(db: &Db, service: &ActingService) -> Uuid {
    db.provision_application_customer(
        service,
        &oxsum_core::ProvisionCustomerRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            external_id: Uuid::new_v4().to_string(),
            name: "Customer".into(),
            contact_email: None,
        },
    )
    .await
    .unwrap()
    .customer
    .organization_id
}
#[tokio::test]
async fn endpoint_replay_scope_limits_rotation_and_one_time_secret() {
    let Some(db) = db().await else { return };
    let owner = setup::owner(&db).await;
    let app = application(&db).await;
    let key = SecretKey::from_bytes([91; 32]);
    let req = request();
    let (a, b) = tokio::join!(
        db.create_application_notification_endpoint(&owner, app, &req, &key),
        db.create_application_notification_endpoint(&owner, app, &req, &key)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.endpoint.endpoint_id, b.endpoint.endpoint_id);
    assert_eq!(
        usize::from(a.secret.is_some()) + usize::from(b.secret.is_some()),
        1
    );
    let actor = grant(&db, app, vec![IntegrationScope::NotificationsRead]).await;
    assert_eq!(
        db.application_notification_endpoints(&actor)
            .await
            .unwrap()
            .endpoints
            .len(),
        1
    );
    let plain = a.secret.or(b.secret).unwrap();
    let stored: String = sqlx::query_scalar(
        "SELECT request::text FROM oxsum.business_mutations WHERE mutation_id=$1",
    )
    .bind(a.endpoint.endpoint_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(!stored.contains(&plain));
    let mut changed = req;
    changed.url = "https://other.example/receiver".into();
    assert!(matches!(
        db.create_application_notification_endpoint(&owner, app, &changed, &key)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let foreign_app = application(&db).await;
    let foreign = grant(&db, foreign_app, vec![IntegrationScope::NotificationsRead]).await;
    assert!(matches!(
        db.application_notification_deliveries(&foreign, a.endpoint.endpoint_id, None, 50)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let narrow = grant(&db, app, vec![IntegrationScope::CustomersRead]).await;
    assert!(matches!(
        db.application_notification_endpoints(&narrow).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(
        db.create_integration_credential(
            &owner,
            &setup::request(vec![], vec![IntegrationScope::NotificationsRead])
        )
        .await
        .is_err()
    );
    let second = db
        .create_application_notification_endpoint(&owner, app, &request(), &key)
        .await
        .unwrap();
    assert!(matches!(
        db.create_application_notification_endpoint(&owner, app, &request(), &key)
            .await,
        Err(WalletError::Conflict(_))
    ));
    db.disable_application_notification_endpoint(
        &owner,
        app,
        second.endpoint.endpoint_id,
        &mutation(),
    )
    .await
    .unwrap();
    assert!(
        db.create_application_notification_endpoint(&owner, app, &request(), &key)
            .await
            .is_ok()
    );
    db.revoke_integration_credential(
        &owner,
        actor.credential_id(),
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotation".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.application_notification_endpoints(&actor).await,
        Err(WalletError::InvalidCredentials)
    ));
    let rotated = grant(&db, app, vec![IntegrationScope::NotificationsRead]).await;
    assert_eq!(
        db.application_notification_endpoints(&rotated)
            .await
            .unwrap()
            .endpoints
            .len(),
        3
    );
}
#[tokio::test]
async fn transactional_signals_replay_rollback_and_private_destination_refusal() {
    let Some(db) = db().await else { return };
    let owner = setup::owner(&db).await;
    let app = application(&db).await;
    let key = SecretKey::from_bytes([91; 32]);
    for url in [
        "http://127.0.0.1/receiver",
        "https://127.0.0.1/receiver",
        "https://receiver.example/?secret=bad",
        "https://user@receiver.example/",
        "https://receiver.example/#fragment",
    ] {
        let mut bad = request();
        bad.url = url.into();
        assert!(matches!(
            db.create_application_notification_endpoint(&owner, app, &bad, &key)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    let endpoint = db
        .create_application_notification_endpoint(&owner, app, &request(), &key)
        .await
        .unwrap()
        .endpoint;
    let service = grant(
        &db,
        app,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::CustomersLifecycleWrite,
            IntegrationScope::CustomerKeysWrite,
            IntegrationScope::EvidenceWrite,
            IntegrationScope::NotificationsRead,
        ],
    )
    .await;
    let org = provision(&db, &service).await;
    let initial = db
        .application_notification_deliveries(&service, endpoint.endpoint_id, None, 50)
        .await
        .unwrap();
    assert_eq!(initial.deliveries.len(), 1);
    assert_eq!(initial.deliveries[0].event_type, "customer.changed");
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query(
        "UPDATE oxsum.customer_profiles SET contact_email='uncommitted@example.com' WHERE organization_id=$1",
    )
    .bind(org)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        db.application_notification_deliveries(&service, endpoint.endpoint_id, None, 50)
            .await
            .unwrap()
            .deliveries
            .len(),
        1
    );
    let req = oxsum_core::ApplicationCustomerKeyRequest {
        idempotency_key: "key-create".into(),
        name: "Private key".into(),
        reason: "fixture".into(),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        spend_limit_minor: 1_000_000,
        model_allowlist: vec!["test-model".into()],
        requests_per_minute: 10,
        max_concurrent_holds: 1,
    };
    let made = db
        .create_application_customer_key(&service, org, &req)
        .await
        .unwrap();
    db.create_application_customer_key(&service, org, &req)
        .await
        .unwrap();
    db.revoke_application_customer_key(
        &service,
        org,
        made.key.id,
        &oxsum_core::ApplicationCustomerArchiveRequest {
            idempotency_key: "key-revoke".into(),
            reason: "fixture".into(),
        },
    )
    .await
    .unwrap();
    let day = (time::OffsetDateTime::now_utc().date() - time::Duration::days(1)).to_string();
    db.publish_application_evidence(
        &service,
        org,
        &oxsum_core::ApplicationEvidencePublishRequest {
            idempotency_key: "publish".into(),
            kind: oxsum_core::EvidenceKind::Period,
            from: Some(day.clone()),
            to: Some(day),
            statement_id: None,
        },
        None,
    )
    .await
    .unwrap();
    let archive = oxsum_core::ApplicationCustomerArchiveRequest {
        idempotency_key: "archive".into(),
        reason: "fixture".into(),
    };
    db.archive_application_customer(&service, org, &archive)
        .await
        .unwrap();
    db.archive_application_customer(&service, org, &archive)
        .await
        .unwrap();
    let rows = db
        .application_notification_deliveries(&service, endpoint.endpoint_id, None, 50)
        .await
        .unwrap()
        .deliveries;
    assert_eq!(
        rows.iter()
            .filter(|r| r.event_type == "key.changed")
            .count(),
        2
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.event_type == "evidence.published")
            .count(),
        1
    );
    let bodies: Vec<String> = sqlx::query_scalar(
        "SELECT body FROM oxsum.application_notification_deliveries WHERE endpoint_id=$1",
    )
    .bind(endpoint.endpoint_id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    for body in bodies {
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["action"], "readCurrentState");
        assert_eq!(v["applicationId"], app.to_string());
        assert!(!body.contains(made.secret.as_ref().unwrap()));
        assert!(!body.contains("Private key"));
        assert!(body.len() < 4096);
    }
}
#[tokio::test]
async fn notification_claim_lease_failure_retry_and_disable_keep_original_bytes() {
    let Some(db) = db().await else { return };
    let owner = setup::owner(&db).await;
    let app = application(&db).await;
    let key = SecretKey::from_bytes([91; 32]);
    let endpoint = db
        .create_application_notification_endpoint(&owner, app, &request(), &key)
        .await
        .unwrap()
        .endpoint;
    let service = grant(
        &db,
        app,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::NotificationsRead,
        ],
    )
    .await;
    provision(&db, &service).await;
    let first = db
        .claim_application_notifications(Some(app))
        .await
        .unwrap()
        .remove(0);
    assert!(
        db.claim_application_notifications(Some(app))
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE oxsum.application_notification_deliveries SET claimed_at=clock_timestamp()-interval '121 seconds' WHERE delivery_id=$1").bind(first.delivery.delivery_id).execute(db.pool()).await.unwrap();
    let second = db
        .claim_application_notifications(Some(app))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(second.body, first.body);
    db.record_application_notification(
        &first.delivery,
        &oxsum_core::DeliveryOutcome::Delivered { status: 200 },
    )
    .await
    .unwrap();
    assert_eq!(
        db.application_notification_deliveries(&service, endpoint.endpoint_id, None, 50)
            .await
            .unwrap()
            .deliveries[0]
            .status,
        "sending"
    );
    db.record_application_notification(
        &second.delivery,
        &oxsum_core::DeliveryOutcome::AttemptFailed {
            status: Some(503),
            error: "receiver rejected".into(),
            attempt: 1,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE oxsum.application_notification_deliveries SET attempts=9,next_attempt_at=clock_timestamp() WHERE delivery_id=$1").bind(first.delivery.delivery_id).execute(db.pool()).await.unwrap();
    let last = db
        .claim_application_notifications(Some(app))
        .await
        .unwrap()
        .remove(0);
    db.record_application_notification(
        &last.delivery,
        &oxsum_core::DeliveryOutcome::AttemptFailed {
            status: None,
            error: "secret url must not persist".into(),
            attempt: 10,
        },
    )
    .await
    .unwrap();
    let retry = mutation();
    let result = db
        .retry_application_notification(
            &owner,
            app,
            endpoint.endpoint_id,
            first.delivery.delivery_id,
            &retry,
        )
        .await
        .unwrap();
    assert_eq!(result.status, "pending");
    assert_eq!(result.last_error.as_deref(), Some("delivery failed"));
    let retried = db
        .claim_application_notifications(Some(app))
        .await
        .unwrap()
        .remove(0);
    assert_eq!(retried.body, first.body);
    db.record_application_notification(
        &retried.delivery,
        &oxsum_core::DeliveryOutcome::Delivered { status: 204 },
    )
    .await
    .unwrap();
    assert_eq!(
        db.retry_application_notification(
            &owner,
            app,
            endpoint.endpoint_id,
            first.delivery.delivery_id,
            &retry
        )
        .await
        .unwrap()
        .status,
        "delivered"
    );
    provision(&db, &service).await;
    db.disable_application_notification_endpoint(&owner, app, endpoint.endpoint_id, &mutation())
        .await
        .unwrap();
    assert!(
        db.claim_application_notifications(Some(app))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        sqlx::query(
            "UPDATE oxsum.application_notification_deliveries SET body='{}' WHERE delivery_id=$1"
        )
        .bind(first.delivery.delivery_id)
        .execute(db.pool())
        .await
        .is_err()
    );
}

#[tokio::test]
async fn real_plan_order_refund_and_statement_writers_enqueue_only_committed_signals() {
    let Some(db) = db().await else { return };
    let owner = setup::owner(&db).await;
    let app = application(&db).await;
    let key = SecretKey::from_bytes([71; 32]);
    let endpoint = db
        .create_application_notification_endpoint(&owner, app, &request(), &key)
        .await
        .unwrap()
        .endpoint;
    let service = grant(
        &db,
        app,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::CheckoutWrite,
            IntegrationScope::NotificationsRead,
        ],
    )
    .await;
    let org = provision(&db, &service).await;
    let context = payment::merchant_context(&db, &owner).await;
    let revision = db
        .business_checkout_routes(&owner)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.rail == oxsum_core::MerchantRail::Hyperswitch)
        .unwrap()
        .revision;
    db.set_checkout_route(
        &owner,
        &oxsum_core::CheckoutRouteRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "fixture".into(),
            rail: oxsum_core::MerchantRail::Hyperswitch,
            expected_revision: revision,
            merchant_id: Some(context.merchant_id),
        },
    )
    .await
    .unwrap();
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: "Notification plan".into(),
                price_minor: 2_000_000,
                currency: "USD".into(),
                quota_minor: Some(3_000_000),
                period_kind: oxsum_core::PeriodKind::Anniversary,
                overage_enabled: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    let plan = db
        .bind_customer_plan(
            org,
            sold.plan_version_id,
            oxsum_core::PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    let selection=serde_json::from_value(serde_json::json!({"purpose":"planPeriod","rail":"hyperswitch","customerPlanId":plan.customer_plan_id})).unwrap();
    let req = oxsum_core::ApplicationCheckoutRequest {
        idempotency_key: "plan".into(),
        quote: db
            .application_checkout_quote(&service, org, &selection)
            .await
            .unwrap(),
        return_url: "https://application.example/paid".into(),
        cancel_url: "https://application.example/cancel".into(),
    };
    let order = db
        .create_application_checkout(&service, org, &req, &key)
        .await
        .unwrap()
        .0;
    payment::mark_paid(&db, &context, &order).await;
    payment::fulfill(&db, &order).await;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.application_notification_deliveries WHERE endpoint_id=$1",
    )
    .bind(endpoint.endpoint_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    db.create_application_checkout(&service, org, &req, &key)
        .await
        .unwrap();
    payment::fulfill(&db, &order).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM oxsum.application_notification_deliveries WHERE endpoint_id=$1"
        )
        .bind(endpoint.endpoint_id)
        .fetch_one(db.pool())
        .await
        .unwrap(),
        count
    );
    let refund = db
        .request_order_refund(
            org,
            order.order_id,
            &oxsum_core::NewRefundRequest {
                amount_minor: Some(500_000),
                reason: "notification refund fixture".into(),
                request_key: "refund".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    db.resolve_order_refund(
        &owner,
        refund.refund_id,
        oxsum_core::RefundDecision::Approve,
        None,
        "approve",
    )
    .await
    .unwrap();
    let provider = format!("refund_{}", refund.refund_id);
    db.note_refund_provider(refund.refund_id, &provider)
        .await
        .unwrap();
    let receipt = db
        .receive_merchant_event(
            &context,
            &oxsum_core::VerifiedMerchantEvent {
                event_id: Uuid::new_v4().to_string(),
                kind: oxsum_core::MerchantEventKind::RefundSucceeded,
                payment_id: Some(order.order_id),
                provider_payment_id: Some(format!("pay_{}", order.order_id)),
                organization_id: Some(org),
                provider_account_id: Some(context.account_id.clone()),
                mode: Some(oxsum_core::MerchantMode::Test),
                amount_minor: Some(refund.display_amount_minor),
                currency: Some(refund.display_currency),
                refund_id: Some(provider),
            },
            &[84; 32],
        )
        .await
        .unwrap();
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open_existing(db.pool().clone(), &tenant)
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    db.cancel_customer_plan(org).await.unwrap();
    let rows = db
        .application_notification_deliveries(&service, endpoint.endpoint_id, None, 50)
        .await
        .unwrap()
        .deliveries;
    for event in [
        "customer.changed",
        "entitlement.changed",
        "order.changed",
        "statement.changed",
    ] {
        assert!(
            rows.iter().any(|d| d.event_type == event),
            "missing {event}"
        );
    }
    assert!(
        rows.iter()
            .filter(|d| d.event_type == "order.changed")
            .all(|d| d.entity_id == order.order_id)
    );
    let bodies: Vec<String> = sqlx::query_scalar(
        "SELECT body FROM oxsum.application_notification_deliveries WHERE endpoint_id=$1",
    )
    .bind(endpoint.endpoint_id)
    .fetch_all(db.pool())
    .await
    .unwrap();
    for body in bodies {
        assert!(!body.contains("apiKey"));
        assert!(!body.contains("amountMinor"));
        assert!(!body.contains("clientSecret"));
    }
}

#[tokio::test]
async fn delivery_pages_and_concurrent_claims_are_bounded_without_duplicate_ownership() {
    let Some(db) = db().await else { return };
    let owner = setup::owner(&db).await;
    let app = application(&db).await;
    let key = SecretKey::from_bytes([91; 32]);
    let endpoint = db
        .create_application_notification_endpoint(&owner, app, &request(), &key)
        .await
        .unwrap()
        .endpoint;
    let actor = grant(
        &db,
        app,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::NotificationsRead,
        ],
    )
    .await;
    let org = provision(&db, &actor).await;
    for i in 0..64 {
        sqlx::query("UPDATE oxsum.customer_profiles SET contact_email=$2 WHERE organization_id=$1")
            .bind(org)
            .bind(format!("revision-{i}@example.com"))
            .execute(db.pool())
            .await
            .unwrap();
    }
    sqlx::query(
        "UPDATE oxsum.customer_profiles SET contact_email=contact_email WHERE organization_id=$1",
    )
    .bind(org)
    .execute(db.pool())
    .await
    .unwrap();
    for limit in [0, 51] {
        assert!(matches!(
            db.application_notification_deliveries(&actor, endpoint.endpoint_id, None, limit)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    let first = db
        .application_notification_deliveries(&actor, endpoint.endpoint_id, None, 50)
        .await
        .unwrap();
    assert_eq!(first.deliveries.len(), 50);
    assert_eq!(
        first.next_cursor,
        first.deliveries.last().map(|row| row.delivery_id)
    );
    let second = db
        .application_notification_deliveries(&actor, endpoint.endpoint_id, first.next_cursor, 50)
        .await
        .unwrap();
    assert_eq!(second.deliveries.len(), 15);
    assert!(second.next_cursor.is_none());
    let mut ids = first
        .deliveries
        .iter()
        .chain(&second.deliveries)
        .map(|row| row.delivery_id)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 65, "unchanged updates emit no extra signal");
    let (a, b) = tokio::join!(
        db.claim_application_notifications(Some(app)),
        db.claim_application_notifications(Some(app))
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.len(), 32);
    assert_eq!(b.len(), 32);
    let mut claimed = a
        .iter()
        .chain(&b)
        .map(|row| row.delivery.delivery_id)
        .collect::<Vec<_>>();
    claimed.sort();
    claimed.dedup();
    assert_eq!(claimed.len(), 64, "workers must own disjoint leases");
    assert_eq!(
        db.claim_application_notifications(Some(app))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        db.claim_application_notifications(Some(app))
            .await
            .unwrap()
            .is_empty()
    );
}

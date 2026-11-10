//! Application checkout authority, exact quote, durable identity and rollback.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/legacy_plan.rs"]
mod legacy_plan;
#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    ActingService, ApplicationCheckoutRequest, ApplicationCheckoutSelection, Db, IntegrationScope,
    MerchantRail, PaymentOrderPurpose, SecretKey, WalletError,
};
use uuid::Uuid;
struct World {
    db: Db,
    owner: String,
    app: Uuid,
    org: Uuid,
    service: ActingService,
    context: oxsum_core::MerchantSigningContext,
}
async fn grant(db: &Db, app: Uuid, scopes: Vec<IntegrationScope>) -> ActingService {
    let mut request = setup::request(vec![], scopes);
    request.application_id = Some(app);
    let secret = db
        .create_integration_credential(&setup::owner(db).await, &request)
        .await
        .unwrap()
        .secret
        .unwrap();
    db.authenticate_service(&secret).await.unwrap().unwrap()
}
async fn world() -> Option<World> {
    let url = oxsum_testkit::suite_db_url("application_checkout")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                key: Uuid::new_v4().simple().to_string(),
                name: "Checkout app".into(),
                customer_limit: 10,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let service = grant(
        &db,
        app,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::CheckoutWrite,
            IntegrationScope::CustomersLifecycleWrite,
        ],
    )
    .await;
    let org = db
        .provision_application_customer(
            &service,
            &oxsum_core::ProvisionCustomerRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                external_id: "customer".into(),
                name: "Checkout customer".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    let context = payment::merchant_context(&db, &owner).await;
    let revision = db
        .business_checkout_routes(&owner)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.rail == MerchantRail::Hyperswitch)
        .unwrap()
        .revision;
    db.set_checkout_route(
        &owner,
        &oxsum_core::CheckoutRouteRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "fixture route".into(),
            rail: MerchantRail::Hyperswitch,
            expected_revision: revision,
            merchant_id: Some(context.merchant_id),
        },
    )
    .await
    .unwrap();
    Some(World {
        db,
        owner,
        app,
        org,
        service,
        context,
    })
}
fn selection() -> ApplicationCheckoutSelection {
    ApplicationCheckoutSelection {
        purpose: PaymentOrderPurpose::WalletTopup,
        rail: MerchantRail::Hyperswitch,
        display_amount_minor: Some(250),
        bundle_version_id: None,
        customer_plan_id: None,
    }
}
async fn request(w: &World) -> ApplicationCheckoutRequest {
    ApplicationCheckoutRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        quote: w
            .db
            .application_checkout_quote(&w.service, w.org, &selection())
            .await
            .unwrap(),
        return_url: "https://app.example/paid".into(),
        cancel_url: "http://127.0.0.1:4567/cancel".into(),
    }
}
#[tokio::test]
async fn application_order_replay_rotation_archive_and_verified_fulfillment() {
    let Some(w) = world().await else { return };
    let secret = SecretKey::from_bytes([71; 32]);
    let request = request(&w).await;
    let (a, b) = tokio::join!(
        w.db.create_application_checkout(&w.service, w.org, &request, &secret),
        w.db.create_application_checkout(&w.service, w.org, &request, &secret)
    );
    let order = a.unwrap().0;
    w.db.rotate_merchant(&w.owner,w.context.merchant_id,&oxsum_core::MerchantVersionRequest {
        idempotency_key:Uuid::new_v4().to_string(),reason:"rotate checkout credentials".into(),
        config:serde_json::from_value(serde_json::json!({"apiUrl":"https://sandbox.hyperswitch.io","apiKey":"rotated-api","webhookSecret":"rotated-webhook","publishableKey":"pk_rotated","sdkUrl":"https://sdk.example/rotated.js"})).unwrap(),
    },&secret).await.unwrap();
    let replay =
        w.db.create_application_checkout(&w.service, w.org, &request, &secret)
            .await
            .unwrap();
    assert_eq!(replay.1.version.id, w.context.version.id);
    let mut fresh = request.clone();
    fresh.idempotency_key = Uuid::new_v4().to_string();
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &fresh, &secret)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(
        w.db.application_checkout_destinations(w.org, order.order_id)
            .await
            .unwrap(),
        Some((request.return_url.clone(), request.cancel_url.clone()))
    );
    assert_eq!(b.unwrap().0.order_id, order.order_id);
    assert_eq!(order.amount_minor, 2_500_000);
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(w.org)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let absent: Option<String> = sqlx::query_scalar("SELECT to_regnamespace($1)::text")
        .bind(format!("ledger_{tenant}"))
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    assert!(absent.is_none());
    let mut changed = request.clone();
    changed.return_url = "https://app.example/another".into();
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &changed, &secret)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let rotated = grant(&w.db, w.app, vec![IntegrationScope::CheckoutWrite]).await;
    w.db.revoke_integration_credential(
        &w.owner,
        w.service.credential_id(),
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "rotate".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &request, &secret)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    assert_eq!(
        w.db.create_application_checkout(&rotated, w.org, &request, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        order.order_id
    );
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    payment::fulfill(&w.db, &order).await;
    let wallet = oxsum_core::Wallet::open_existing(w.db.pool().clone(), &tenant)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 2_500_000);
    let lifecycle = grant(
        &w.db,
        w.app,
        vec![IntegrationScope::CustomersLifecycleWrite],
    )
    .await;
    w.db.archive_application_customer(
        &lifecycle,
        w.org,
        &oxsum_core::ApplicationCustomerArchiveRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "archive".into(),
        },
    )
    .await
    .unwrap();
    assert!(
        w.db.application_checkout_order(&rotated, w.org, order.order_id, false)
            .await
            .is_ok()
    );
    assert!(matches!(
        w.db.application_checkout_order(&rotated, w.org, order.order_id, true)
            .await,
        Err(WalletError::Conflict(_))
    ));
}
#[tokio::test]
async fn scope_namespace_quote_currency_bundle_and_return_destinations_are_exact() {
    let Some(w) = world().await else { return };
    let secret = SecretKey::from_bytes([71; 32]);
    let req = request(&w).await;
    for scope in [
        IntegrationScope::BillingRead,
        IntegrationScope::EntitlementsRead,
        IntegrationScope::CustomersProvision,
    ] {
        let narrow = grant(&w.db, w.app, vec![scope]).await;
        assert!(matches!(
            w.db.create_application_checkout(&narrow, w.org, &req, &secret)
                .await,
            Err(WalletError::Forbidden(_))
        ));
    }
    assert!(
        w.db.create_integration_credential(
            &w.owner,
            &setup::request(vec![w.org], vec![IntegrationScope::CheckoutWrite])
        )
        .await
        .is_err()
    );
    for url in [
        "http://app.example/paid",
        "//app.example/paid",
        "https://user:secret@app.example/paid",
        "https://app.example/paid#token",
        "https://app.example/not normalized",
        "https://app.example/white\u{00a0}space",
        "file:///tmp/paid",
    ] {
        let mut bad = req.clone();
        bad.return_url = url.into();
        assert!(matches!(
            w.db.create_application_checkout(&w.service, w.org, &bad, &secret)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    let mut bad = req.clone();
    bad.quote.purchased_minor += 1;
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &bad, &secret)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let mut bad = req.clone();
    bad.quote.organization_id = Uuid::new_v4();
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &bad, &secret)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let (order, _) =
        w.db.create_application_checkout(&w.service, w.org, &req, &secret)
            .await
            .unwrap();
    let terms = w.db.collection_terms().await.unwrap();
    w.db.set_collection_terms(
        &w.owner,
        &oxsum_core::CollectionTermsRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "future rate".into(),
            expected_revision: terms.revision,
            currency: oxsum_core::CollectionCurrency {
                code: "USD".into(),
                rate_per_credit: "2".into(),
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        w.db.create_application_checkout(&w.service, w.org, &req, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        order.order_id
    );
    let mut stale = req.clone();
    stale.idempotency_key = Uuid::new_v4().to_string();
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &stale, &secret)
            .await,
        Err(WalletError::Conflict(_))
    ));
    w.db.set_collection_terms(
        &w.owner,
        &oxsum_core::CollectionTermsRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "restore".into(),
            expected_revision: terms.revision + 1,
            currency: terms.currency.unwrap(),
        },
    )
    .await
    .unwrap();
    let published =
        w.db.publish_bundle(
            &w.owner,
            &oxsum_core::BundlePublishRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "bundle".into(),
                bundle_id: None,
                expected_version: 0,
                terms: oxsum_core::BundleTerms {
                    name: "Bundle".into(),
                    display_currency: "USD".into(),
                    display_amount_minor: 300,
                    purchased_minor: 3_000_000,
                    promo_minor: 1_000_000,
                    promo_lifetime_days: Some(30),
                },
            },
        )
        .await
        .unwrap()
        .offer;
    let selection = ApplicationCheckoutSelection {
        purpose: PaymentOrderPurpose::Bundle,
        rail: MerchantRail::Hyperswitch,
        display_amount_minor: None,
        bundle_version_id: Some(published.bundle_version_id),
        customer_plan_id: None,
    };
    let mut bundle = request(&w).await;
    bundle.quote =
        w.db.application_checkout_quote(&w.service, w.org, &selection)
            .await
            .unwrap();
    assert_eq!(bundle.quote.promo_minor, 1_000_000);
    let created =
        w.db.create_application_checkout(&w.service, w.org, &bundle, &secret)
            .await
            .unwrap()
            .0;
    assert_eq!(created.promo_minor, 1_000_000);
    w.db.retire_bundle(
        &w.owner,
        published.bundle_version_id,
        &oxsum_core::BundleRetireRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "retire".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        w.db.create_application_checkout(&w.service, w.org, &bundle, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        created.order_id
    );
    assert!(
        w.db.application_checkout_quote(&w.service, w.org, &selection)
            .await
            .is_err()
    );
    let page =
        w.db.application_checkout_orders(&w.service, w.org, None, 1)
            .await
            .unwrap();
    assert_eq!(page.orders.len(), 1);
    assert!(page.next_cursor.is_some());
    assert!(
        w.db.application_checkout_orders(&w.service, w.org, page.next_cursor, 1)
            .await
            .unwrap()
            .next_cursor
            .is_none()
    );
}
#[tokio::test]
async fn application_binding_failure_rolls_back_order_and_payment_binding() {
    let Some(w) = world().await else { return };
    let secret = SecretKey::from_bytes([71; 32]);
    let request = request(&w).await;
    let constraint = format!("fixture_app_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("ALTER TABLE oxsum.application_checkout_orders ADD CONSTRAINT {constraint} CHECK(organization_id<>'{}'::uuid) NOT VALID",w.org)).execute(w.db.pool()).await.unwrap();
    let result =
        w.db.create_application_checkout(&w.service, w.org, &request, &secret)
            .await;
    sqlx::query(&format!(
        "ALTER TABLE oxsum.application_checkout_orders DROP CONSTRAINT {constraint}"
    ))
    .execute(w.db.pool())
    .await
    .unwrap();
    assert!(result.is_err());
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM oxsum.payment_orders WHERE organization_id=$1),(SELECT count(*) FROM oxsum.merchant_payment_bindings WHERE organization_id=$1)").bind(w.org).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(counts, (0, 0));
    assert!(
        w.db.create_application_checkout(&w.service, w.org, &request, &secret)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn foreign_application_portal_orders_and_expiry_do_not_inherit_checkout_authority() {
    let Some(w) = world().await else { return };
    let secret = SecretKey::from_bytes([71; 32]);
    let request = request(&w).await;
    let other =
        w.db.create_integration_application(
            &w.owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                key: Uuid::new_v4().simple().to_string(),
                name: "Other app".into(),
                customer_limit: 2,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let foreign = grant(&w.db, other, vec![IntegrationScope::CheckoutWrite]).await;
    assert!(matches!(
        w.db.create_application_checkout(&foreign, w.org, &request, &secret)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let portal =
        w.db.create_payment_order(
            &w.context,
            w.org,
            &oxsum_core::NewPaymentOrder {
                expected_plan_quote: None,
                expected_collection_revision: Some(request.quote.collection_revision),
                purpose: PaymentOrderPurpose::WalletTopup,
                display_amount_minor: 250,
                display_currency: "USD".into(),
                purpose_ref: None,
                idempotency_key: request.idempotency_key.clone(),
                promo_minor: 0,
            },
        )
        .await
        .unwrap();
    let app =
        w.db.create_application_checkout(&w.service, w.org, &request, &secret)
            .await
            .unwrap()
            .0;
    assert_ne!(app.order_id, portal.order_id);
    assert!(matches!(
        w.db.application_checkout_order(&w.service, w.org, portal.order_id, false)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        w.db.application_checkout_order(&foreign, w.org, app.order_id, true)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert_eq!(
        w.db.application_checkout_orders(&w.service, w.org, None, 50)
            .await
            .unwrap()
            .orders
            .len(),
        1
    );
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=clock_timestamp()-interval '2 days',expires_at=clock_timestamp()-interval '1 day' WHERE credential_id=$1").bind(w.service.credential_id()).execute(w.db.pool()).await.unwrap();
    assert!(matches!(
        w.db.application_checkout_order(&w.service, w.org, app.order_id, false)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn order_transaction_rechecks_revocation_and_archive_after_preflight() {
    for archive in [false, true] {
        let Some(w) = world().await else { return };
        let request = request(&w).await;
        let mut locked = w.db.pool().begin().await.unwrap();
        sqlx::query("SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE")
            .fetch_one(&mut *locked)
            .await
            .unwrap();
        let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *locked)
            .await
            .unwrap();
        let db = w.db.clone();
        let service = w.service.clone();
        let org = w.org;
        let create = tokio::spawn(async move {
            db.create_application_checkout(
                &service,
                org,
                &request,
                &SecretKey::from_bytes([71; 32]),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            loop {
                let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))").bind(blocker).fetch_one(w.db.pool()).await.unwrap();
                if waiting {break} tokio::task::yield_now().await;
            }
        }).await.unwrap();
        if archive {
            sqlx::query(
                "UPDATE oxsum.customer_profiles SET state='archived',archived_at=clock_timestamp() WHERE organization_id=$1",
            )
            .bind(w.org)
            .execute(&mut *locked)
            .await
            .unwrap();
        } else {
            sqlx::query("UPDATE oxsum.integration_credentials SET revoked_at=clock_timestamp() WHERE credential_id=$1").bind(w.service.credential_id()).execute(&mut *locked).await.unwrap();
        }
        locked.commit().await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), create)
            .await
            .unwrap()
            .unwrap();
        if archive {
            assert!(matches!(result, Err(WalletError::Conflict(_))))
        } else {
            assert!(matches!(result, Err(WalletError::InvalidCredentials)))
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.payment_orders WHERE organization_id=$1",
        )
        .bind(w.org)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        assert_eq!(count, 0);
    }
}

async fn assigned_plan(w: &World, price: i64) -> Uuid {
    let sold =
        w.db.publish_sold_plan(
            w.db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: "Application sold plan".into(),
                price_minor: price,
                currency: "USD".into(),
                quota_minor: Some(7_000_000),
                period_kind: oxsum_core::PeriodKind::CalendarMonth,
                overage_enabled: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    w.db.bind_customer_plan(
        w.org,
        sold.plan_version_id,
        if price == 0 {
            oxsum_core::PeriodAuthorization::Complimentary(Uuid::new_v4())
        } else {
            oxsum_core::PeriodAuthorization::PaymentOrder(Uuid::nil())
        },
    )
    .await
    .unwrap()
    .customer_plan_id
}
fn plan_selection(plan: Uuid) -> ApplicationCheckoutSelection {
    ApplicationCheckoutSelection {
        purpose: PaymentOrderPurpose::PlanPeriod,
        rail: MerchantRail::Hyperswitch,
        display_amount_minor: None,
        bundle_version_id: None,
        customer_plan_id: Some(plan),
    }
}
async fn plan_request(w: &World, plan: Uuid) -> ApplicationCheckoutRequest {
    ApplicationCheckoutRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        quote: w
            .db
            .application_checkout_quote(&w.service, w.org, &plan_selection(plan))
            .await
            .unwrap(),
        return_url: "https://app.example/plan".into(),
        cancel_url: "https://app.example/cancel".into(),
    }
}
#[tokio::test]
async fn assigned_plan_quotes_preserve_exact_fee_period_and_once_only_entitlement() {
    let Some(w) = world().await else { return };
    let secret = SecretKey::from_bytes([71; 32]);
    let plan = assigned_plan(&w, 1_000_001).await;
    let request = plan_request(&w, plan).await;
    assert_eq!(request.quote.display_amount_minor, 101);
    assert_eq!(request.quote.purchased_minor, 0);
    assert_eq!(request.quote.promo_minor, 0);
    assert_eq!(
        request.quote.plan_payment.as_ref().unwrap().fee_minor,
        1_000_001
    );
    assert_eq!(request.quote.plan_payment.as_ref().unwrap().period_index, 0);
    for mutation in 0..4 {
        let mut bad = request.clone();
        match mutation {
            0 => bad.quote.plan_payment.as_mut().unwrap().fee_minor += 1,
            1 => bad.quote.purchased_minor = 1,
            2 => bad.quote.plan_payment = None,
            _ => bad.quote.plan_payment.as_mut().unwrap().period_index += 1,
        }
        assert!(matches!(
            w.db.create_application_checkout(&w.service, w.org, &bad, &secret)
                .await,
            Err(WalletError::Conflict(_))
        ));
    }
    let mut other = request.clone();
    other.idempotency_key = Uuid::new_v4().to_string();
    let (a, b) = tokio::join!(
        w.db.create_application_checkout(&w.service, w.org, &request, &secret),
        w.db.create_application_checkout(&w.service, w.org, &other, &secret),
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let (order, original) = if let Ok((order, _)) = a {
        (order, request)
    } else {
        (b.unwrap().0, other)
    };
    assert_eq!(order.amount_minor, 1_000_001);
    assert_eq!(order.plan_payment, original.quote.plan_payment);
    assert!(w.db.plan_periods(plan).await.unwrap().is_empty());
    assert_eq!(
        w.db.create_application_checkout(&w.service, w.org, &original, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        order.order_id
    );
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    payment::fulfill(&w.db, &order).await;
    assert_eq!(w.db.plan_periods(plan).await.unwrap().len(), 1);
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(w.org)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open_existing(w.db.pool().clone(), &tenant)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(
        w.db.create_application_checkout(&w.service, w.org, &original, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        order.order_id
    );
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, w.org, &plan_selection(plan))
            .await,
        Err(WalletError::Conflict(_))
    ));
    w.db.schedule_customer_plan_change(
        w.org,
        original
            .quote
            .plan_payment
            .as_ref()
            .unwrap()
            .plan_version_id,
    )
    .await
    .unwrap();
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, w.org, &plan_selection(plan))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let mut renewal = original.clone();
    renewal.idempotency_key = Uuid::new_v4().to_string();
    w.db.cancel_customer_plan(w.org).await.unwrap();
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &renewal, &secret)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(
        w.db.create_application_checkout(&w.service, w.org, &original, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        order.order_id
    );
}
#[tokio::test]
async fn plan_checkout_refuses_foreign_free_and_portal_intents_without_disclosing_them() {
    let Some(w) = world().await else { return };
    let secret = SecretKey::from_bytes([71; 32]);
    let plan = assigned_plan(&w, 2_000_000).await;
    let own = plan_request(&w, plan).await;
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, w.org, &plan_selection(Uuid::new_v4()))
            .await,
        Err(WalletError::NotFound(_))
    ));
    let mut mixed = plan_selection(plan);
    mixed.display_amount_minor = Some(1);
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, w.org, &mixed)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    let quote = w.db.plan_payment_quote(w.org).await.unwrap();
    let portal =
        w.db.create_payment_order(
            &w.context,
            w.org,
            &oxsum_core::NewPaymentOrder {
                expected_collection_revision: Some(quote.terms.collection_revision),
                expected_plan_quote: Some(quote.fingerprint),
                purpose: PaymentOrderPurpose::PlanPeriod,
                display_amount_minor: quote.terms.display_amount_minor,
                display_currency: quote.terms.display_currency,
                purpose_ref: Some(plan),
                idempotency_key: Uuid::new_v4().to_string(),
                promo_minor: 0,
            },
        )
        .await
        .unwrap();
    let error =
        w.db.create_application_checkout(&w.service, w.org, &own, &secret)
            .await
            .err()
            .unwrap();
    assert!(matches!(error, WalletError::Conflict(_)));
    assert!(!error.to_string().contains(&portal.order_id.to_string()));
    assert!(
        w.db.application_checkout_orders(&w.service, w.org, None, 20)
            .await
            .unwrap()
            .orders
            .is_empty()
    );
    assert!(matches!(
        w.db.application_checkout_order(&w.service, w.org, portal.order_id, false)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let Some(free) = world().await else { return };
    let free_plan = assigned_plan(&free, 0).await;
    assert!(matches!(
        free.db
            .application_checkout_quote(&free.service, free.org, &plan_selection(free_plan))
            .await,
        Err(WalletError::InvalidInput(_))
    ));
}
#[tokio::test]
async fn retained_wallet_json_without_plan_fields_replays_after_plan_extension() {
    let Some(w) = world().await else { return };
    let secret = SecretKey::from_bytes([71; 32]);
    let req = request(&w).await;
    let wire = serde_json::to_value(&req).unwrap();
    assert!(wire["quote"].get("planPayment").is_none());
    assert!(wire["quote"]["selection"].get("customerPlanId").is_none());
    let old: ApplicationCheckoutRequest = serde_json::from_value(wire.clone()).unwrap();
    let order =
        w.db.create_application_checkout(&w.service, w.org, &old, &secret)
            .await
            .unwrap()
            .0;
    let stored: serde_json::Value = sqlx::query_scalar(
        "SELECT request FROM oxsum.application_checkout_orders WHERE order_id=$1",
    )
    .bind(order.order_id)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert_eq!(stored, wire);
    assert_eq!(
        w.db.create_application_checkout(&w.service, w.org, &old, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        order.order_id
    );
}

#[tokio::test]
async fn application_legacy_plan_renewal_obeys_boundary_and_stale_cancellation() {
    let Some(w) = world().await else { return };
    let secret = SecretKey::from_bytes([71; 32]);
    // Historical v1 has no immutable recognition schedule. Move only this
    // fixture's clock boundaries; current v2 periods above remain untouched.
    let sold = legacy_plan::publish(
        &w.db,
        w.db.default_business().await.unwrap(),
        &oxsum_core::NewSoldPlan {
            plan_id: None,
            name: "Retained v1 application plan".into(),
            price_minor: 2_000_000,
            currency: "USD".into(),
            quota_minor: Some(7_000_000),
            period_kind: oxsum_core::PeriodKind::CalendarMonth,
            overage_enabled: false,
            sort_order: 0,
        },
    )
    .await
    .unwrap();
    let plan =
        w.db.bind_customer_plan(
            w.org,
            sold.plan_version_id,
            oxsum_core::PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap()
        .customer_plan_id;
    let initial = plan_request(&w, plan).await;
    let order =
        w.db.create_application_checkout(&w.service, w.org, &initial, &secret)
            .await
            .unwrap()
            .0;
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, w.org, &plan_selection(plan))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_end=now()-interval '1 second' WHERE customer_plan_id=$1 AND state='active'").bind(plan).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_end=now()-interval '1 second' WHERE organization_id=$1 AND state='active'").bind(w.org).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let renewal = plan_request(&w, plan).await;
    assert_eq!(renewal.quote.plan_payment.as_ref().unwrap().period_index, 1);
    assert!(
        renewal
            .quote
            .plan_payment
            .as_ref()
            .unwrap()
            .period_start
            .is_some()
    );
    let renewed =
        w.db.create_application_checkout(&w.service, w.org, &renewal, &secret)
            .await
            .unwrap()
            .0;
    assert_eq!(renewed.plan_payment, renewal.quote.plan_payment);
    assert!(matches!(
        w.db.cancel_customer_plan(w.org).await,
        Err(WalletError::Conflict(_))
    ));
    payment::mark_paid(&w.db, &w.context, &renewed).await;
    payment::fulfill(&w.db, &renewed).await;
    payment::fulfill(&w.db, &renewed).await;
    assert_eq!(w.db.plan_periods(plan).await.unwrap().len(), 2);
    w.db.cancel_customer_plan(w.org).await.unwrap();
    assert_eq!(
        w.db.create_application_checkout(&w.service, w.org, &renewal, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        renewed.order_id
    );
}

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
        statement_id: None,
        statement_amount_minor: None,
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
        statement_id: None,
        statement_amount_minor: None,
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
                expected_statement_quote: None,
                statement_amount_minor: None,
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
        statement_id: None,
        statement_amount_minor: None,
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
                expected_statement_quote: None,
                statement_amount_minor: None,
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
    assert!(wire["quote"].get("statementPayment").is_none());
    assert!(wire["quote"]["selection"].get("statementId").is_none());
    assert!(
        wire["quote"]["selection"]
            .get("statementAmountMinor")
            .is_none()
    );
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

async fn statement_debt(w: &World, fee: i64) -> (Uuid, Uuid, oxsum_core::Wallet) {
    let plan = assigned_plan(w, fee).await;
    let original = payment::created_order(&w.db, &w.context, w.org, plan).await;
    payment::mark_paid(&w.db, &w.context, &original).await;
    payment::fulfill(&w.db, &original).await;
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(w.org)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open_existing(w.db.pool().clone(), &tenant)
        .await
        .unwrap();
    lose_order(w, &wallet, &original).await;
    let statement =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .into_iter()
            .find(|s| {
                s.total_minor > s.paid_minor
                    && matches!(
                        s.state,
                        oxsum_core::PlanStatementState::Issued
                            | oxsum_core::PlanStatementState::PastDue
                    )
            })
            .unwrap()
            .statement_id;
    (statement, plan, wallet)
}
async fn lose_order(w: &World, wallet: &oxsum_core::Wallet, original: &oxsum_core::PaymentOrder) {
    let reference = Uuid::new_v4().to_string();
    let event = oxsum_core::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: oxsum_core::MerchantEventKind::Dispute,
        payment_id: Some(original.order_id),
        provider_payment_id: Some(format!("pay_{}", original.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(oxsum_core::MerchantMode::Test),
        amount_minor: Some(original.display_amount_minor),
        currency: Some(original.display_currency.clone()),
        refund_id: Some(reference.clone()),
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[84; 32])
            .await
            .unwrap();
    w.db.record_order_dispute(&event, &receipt).await.unwrap();
    let dispute: Uuid = sqlx::query_scalar(
        "SELECT dispute_id FROM oxsum.disputes WHERE order_id=$1 AND provider_dispute_id=$2",
    )
    .bind(original.order_id)
    .bind(reference)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    w.db.resolve_order_dispute(
        wallet,
        &w.owner,
        dispute,
        oxsum_core::DisputeResolution::Lost,
        Some("application debt fixture"),
        &Uuid::new_v4().to_string(),
        time::OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
}
fn debt_selection(statement: Uuid, amount: Option<i64>) -> ApplicationCheckoutSelection {
    ApplicationCheckoutSelection {
        purpose: PaymentOrderPurpose::StatementRepayment,
        rail: MerchantRail::Hyperswitch,
        display_amount_minor: None,
        bundle_version_id: None,
        customer_plan_id: None,
        statement_id: Some(statement),
        statement_amount_minor: amount,
    }
}
async fn debt_request(
    w: &World,
    statement: Uuid,
    amount: Option<i64>,
) -> ApplicationCheckoutRequest {
    ApplicationCheckoutRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        quote: w
            .db
            .application_checkout_quote(&w.service, w.org, &debt_selection(statement, amount))
            .await
            .unwrap(),
        return_url: "https://app.example/debt".into(),
        cancel_url: "https://app.example/debt-cancel".into(),
    }
}
#[tokio::test]
async fn application_statement_partial_full_replay_preserves_wallet_and_entitlement() {
    let Some(w) = world().await else { return };
    let (statement, plan, wallet) = statement_debt(&w, 2_500_001).await;
    let periods = serde_json::to_value(w.db.plan_periods(plan).await.unwrap()).unwrap();
    let funds = wallet.available().await.unwrap();
    let req = debt_request(&w, statement, Some(1_000_001)).await;
    assert_eq!(req.quote.display_amount_minor, 101);
    assert_eq!(req.quote.purchased_minor, 0);
    assert_eq!(
        req.quote.statement_payment.as_ref().unwrap().amount_minor,
        1_000_001
    );
    let secret = SecretKey::from_bytes([71; 32]);
    let (a, b) = tokio::join!(
        w.db.create_application_checkout(&w.service, w.org, &req, &secret),
        w.db.create_application_checkout(&w.service, w.org, &req, &secret)
    );
    let first = a.unwrap().0;
    assert_eq!(first.order_id, b.unwrap().0.order_id);
    assert_eq!(first.amount_minor, 1_000_001);
    assert_eq!(first.statement_payment, req.quote.statement_payment);
    payment::mark_paid(&w.db, &w.context, &first).await;
    payment::fulfill(&w.db, &first).await;
    let log = wallet.log_size().await.unwrap();
    payment::fulfill(&w.db, &first).await;
    assert_eq!(wallet.log_size().await.unwrap(), log);
    let remaining = debt_request(&w, statement, None).await;
    assert_eq!(
        remaining
            .quote
            .statement_payment
            .as_ref()
            .unwrap()
            .amount_minor,
        1_500_000
    );
    let second =
        w.db.create_application_checkout(&w.service, w.org, &remaining, &secret)
            .await
            .unwrap()
            .0;
    payment::mark_paid(&w.db, &w.context, &second).await;
    payment::fulfill(&w.db, &second).await;
    let detail =
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(detail.statement.total_minor, detail.statement.paid_minor);
    assert_eq!(wallet.available().await.unwrap(), funds);
    assert_eq!(
        serde_json::to_value(w.db.plan_periods(plan).await.unwrap()).unwrap(),
        periods
    );
    assert_eq!(
        w.db.create_application_checkout(&w.service, w.org, &req, &secret)
            .await
            .unwrap()
            .0
            .order_id,
        first.order_id
    );
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, w.org, &debt_selection(statement, None))
            .await,
        Err(WalletError::Conflict(_))
    ));
}
#[tokio::test]
async fn application_statement_concurrent_reservation_foreign_scope_and_changed_quote() {
    let Some(w) = world().await else { return };
    let (statement, _, _) = statement_debt(&w, 2_500_000).await;
    let req = debt_request(&w, statement, None).await;
    let mut racing = req.clone();
    racing.idempotency_key = Uuid::new_v4().to_string();
    let secret = SecretKey::from_bytes([71; 32]);
    let (a, b) = tokio::join!(
        w.db.create_application_checkout(&w.service, w.org, &req, &secret),
        w.db.create_application_checkout(&w.service, w.org, &racing, &secret)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let (order, accepted) = if let Ok((order, _)) = a {
        (order, req)
    } else {
        (b.unwrap().0, racing)
    };
    let reserved:i64=sqlx::query_scalar("SELECT sum(amount_minor)::bigint FROM oxsum.statement_payment_reservations WHERE statement_id=$1 AND state='reserved'").bind(statement).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(reserved, 2_500_000);
    let mut changed = accepted.clone();
    changed
        .quote
        .statement_payment
        .as_mut()
        .unwrap()
        .amount_minor -= 1;
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &changed, &secret)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let readonly = grant(&w.db, w.app, vec![IntegrationScope::BillingRead]).await;
    assert!(matches!(
        w.db.application_checkout_quote(&readonly, w.org, &debt_selection(statement, None))
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, w.org, &debt_selection(Uuid::new_v4(), None))
            .await,
        Err(WalletError::NotFound(_))
    ));
    let mut wrong = selection();
    wrong.statement_id = Some(statement);
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, w.org, &wrong)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    let returned =
        w.db.application_checkout_orders(&w.service, w.org, None, 50)
            .await
            .unwrap();
    assert_eq!(returned.orders.len(), 1);
    assert_eq!(returned.orders[0].order_id, order.order_id);
}

#[tokio::test]
async fn application_mixed_statement_refund_and_loss_keep_original_allocations() {
    let Some(w) = world().await else { return };
    let sold = legacy_plan::publish(
        &w.db,
        w.db.default_business().await.unwrap(),
        &oxsum_core::NewSoldPlan {
            plan_id: None,
            name: "Legacy mixed source".into(),
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
    let old = payment::created_order(&w.db, &w.context, w.org, plan).await;
    payment::mark_paid(&w.db, &w.context, &old).await;
    payment::fulfill(&w.db, &old).await;
    let next =
        w.db.publish_sold_plan(
            w.db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: Some(sold.plan_id),
                name: "Daily mixed source".into(),
                price_minor: 3_000_000,
                currency: "USD".into(),
                quota_minor: Some(7_000_000),
                period_kind: oxsum_core::PeriodKind::CalendarMonth,
                overage_enabled: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    w.db.schedule_customer_plan_change(w.org, next.plan_version_id)
        .await
        .unwrap();
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=now()-interval '32 days',period_end=now()-interval '1 second' WHERE customer_plan_id=$1 AND state='active'").bind(plan).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_end=now()-interval '1 second' WHERE organization_id=$1 AND state='active'").bind(w.org).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    w.db.rollover_due_subscriptions().await.unwrap();
    let daily = payment::created_order(&w.db, &w.context, w.org, plan).await;
    payment::mark_paid(&w.db, &w.context, &daily).await;
    payment::fulfill(&w.db, &daily).await;
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(w.org)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open_existing(w.db.pool().clone(), &tenant)
        .await
        .unwrap();
    lose_order(&w, &wallet, &old).await;
    lose_order(&w, &wallet, &daily).await;
    let statement =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .into_iter()
            .find(|s| {
                s.total_minor > s.paid_minor
                    && matches!(
                        s.state,
                        oxsum_core::PlanStatementState::Issued
                            | oxsum_core::PlanStatementState::PastDue
                    )
            })
            .unwrap()
            .statement_id;
    let req = debt_request(&w, statement, None).await;
    let terms = req.quote.statement_payment.as_ref().unwrap();
    assert_eq!(terms.amount_minor, 5_000_000);
    assert_eq!(terms.allocations.len(), 2);
    let secret = SecretKey::from_bytes([71; 32]);
    let order =
        w.db.create_application_checkout(&w.service, w.org, &req, &secret)
            .await
            .unwrap()
            .0;
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &oxsum_core::NewRefundRequest {
                amount_minor: Some(1_000_000),
                reason: "partial debt refund".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    w.db.resolve_order_refund(
        &w.owner,
        refund.refund_id,
        oxsum_core::RefundDecision::Approve,
        None,
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    let provider = format!("refund_{}", refund.refund_id);
    w.db.note_refund_provider(refund.refund_id, &provider)
        .await
        .unwrap();
    let receipt =
        w.db.receive_merchant_event(
            &w.context,
            &oxsum_core::VerifiedMerchantEvent {
                event_id: Uuid::new_v4().to_string(),
                kind: oxsum_core::MerchantEventKind::RefundSucceeded,
                payment_id: Some(order.order_id),
                provider_payment_id: Some(format!("pay_{}", order.order_id)),
                organization_id: Some(w.org),
                provider_account_id: Some(w.context.account_id.clone()),
                mode: Some(oxsum_core::MerchantMode::Test),
                amount_minor: Some(refund.display_amount_minor),
                currency: Some(refund.display_currency),
                refund_id: Some(provider),
            },
            &[84; 32],
        )
        .await
        .unwrap();
    w.db.apply_refund_event(&wallet, &receipt, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    assert_eq!(
        debt_request(&w, statement, None)
            .await
            .quote
            .statement_payment
            .as_ref()
            .unwrap()
            .amount_minor,
        1_000_000
    );
    lose_order(&w, &wallet, &order).await;
    let restored = debt_request(&w, statement, None).await;
    assert_eq!(
        restored
            .quote
            .statement_payment
            .as_ref()
            .unwrap()
            .amount_minor,
        5_000_000
    );
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(w.db.plan_periods(plan).await.unwrap().len(), 2);
    let reader = grant(&w.db, w.app, vec![IntegrationScope::BillingRead]).await;
    let read =
        w.db.application_customer_statement(&reader, w.org, statement)
            .await
            .unwrap();
    let original =
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        serde_json::to_value(&read).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    let effects: Vec<Uuid> = sqlx::query_scalar(
        "SELECT effect_id FROM oxsum.statement_payment_effects WHERE order_id=$1 AND receivable_minor>0",
    )
    .bind(order.order_id)
    .fetch_all(w.db.pool())
    .await
    .unwrap();
    assert!(!effects.is_empty());
    assert!(
        effects
            .iter()
            .all(|id| read.lines.iter().any(|line| line.source_ref == Some(*id)))
    );
    assert_eq!(
        read.statement.total_minor - read.statement.paid_minor,
        5_000_000
    );
}

#[tokio::test]
async fn statement_checkout_hides_portal_intents_and_rechecks_collection_before_reservation() {
    let Some(w) = world().await else { return };
    let (statement, _, _) = statement_debt(&w, 2_500_000).await;
    let original = debt_request(&w, statement, Some(1_000_000)).await;
    let collection = w.db.collection_terms().await.unwrap();
    w.db.set_collection_terms(
        &w.owner,
        &oxsum_core::CollectionTermsRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "future statement collection terms".into(),
            expected_revision: collection.revision,
            currency: oxsum_core::CollectionCurrency {
                code: "USD".into(),
                rate_per_credit: "2".into(),
            },
        },
    )
    .await
    .unwrap();
    let secret = SecretKey::from_bytes([71; 32]);
    assert!(matches!(
        w.db.create_application_checkout(&w.service, w.org, &original, &secret)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.statement_payment_reservations WHERE statement_id=$1",
    )
    .bind(statement)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert_eq!(held, 0);
    let revised = debt_request(&w, statement, Some(1_000_000)).await;
    let t = revised.quote.statement_payment.as_ref().unwrap();
    assert_eq!(t.display_amount_minor, 200);
    let portal =
        w.db.create_payment_order(
            &w.context,
            w.org,
            &oxsum_core::NewPaymentOrder {
                expected_statement_quote: Some(t.fingerprint().unwrap()),
                statement_amount_minor: Some(t.amount_minor),
                expected_plan_quote: None,
                expected_collection_revision: Some(t.collection_revision),
                purpose: PaymentOrderPurpose::StatementRepayment,
                display_amount_minor: t.display_amount_minor,
                display_currency: t.display_currency.clone(),
                purpose_ref: Some(statement),
                idempotency_key: Uuid::new_v4().to_string(),
                promo_minor: 0,
            },
        )
        .await
        .unwrap();
    let remainder = debt_request(&w, statement, None).await;
    assert_eq!(
        remainder
            .quote
            .statement_payment
            .as_ref()
            .unwrap()
            .amount_minor,
        1_500_000
    );
    assert!(
        !serde_json::to_string(&remainder.quote)
            .unwrap()
            .contains(&portal.order_id.to_string())
    );
    assert!(
        w.db.application_checkout_orders(&w.service, w.org, None, 50)
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
    let another =
        w.db.provision_application_customer(
            &w.service,
            &oxsum_core::ProvisionCustomerRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                external_id: "foreign-statement-owner".into(),
                name: "Another application customer".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    assert!(matches!(
        w.db.application_checkout_quote(&w.service, another, &debt_selection(statement, None))
            .await,
        Err(WalletError::NotFound(_))
    ));
}

//! Customer ownership, exact sold fees and one due period bound every paid activation.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/plan_payment.rs"]
mod fixture;
#[path = "support/legacy_plan.rs"]
mod legacy_plan;
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    Db, NewPaymentOrder, NewSoldPlan, NewUser, PaymentOrderPurpose, PeriodAuthorization,
    PeriodKind, WalletError,
};
use uuid::Uuid;

async fn world() -> Option<(Db, String, Uuid, Uuid)> {
    let url = oxsum_testkit::suite_db_url("plan_payments")?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: format!("{}@example.test", Uuid::new_v4()),
            password: "plan payment fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let sold = legacy_plan::publish(
        &db,
        db.default_business().await.unwrap(),
        &NewSoldPlan {
            plan_id: None,
            name: "Exact quoted plan".into(),
            price_minor: 1_000_001,
            currency: "USD".into(),
            quota_minor: Some(7_000_000),
            period_kind: PeriodKind::CalendarMonth,
            overage_enabled: false,
            sort_order: 0,
        },
    )
    .await
    .unwrap();
    let plan = db
        .bind_customer_plan(
            customer.organization.id,
            sold.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    Some((db, owner, customer.organization.id, plan.customer_plan_id))
}
fn intent(quote: &oxsum_core::PlanPaymentQuote) -> NewPaymentOrder {
    NewPaymentOrder {
        expected_collection_revision: Some(quote.terms.collection_revision),
        expected_statement_quote: None,
        statement_amount_minor: None,
        expected_plan_quote: Some(quote.fingerprint.clone()),
        purpose: PaymentOrderPurpose::PlanPeriod,
        display_amount_minor: quote.terms.display_amount_minor,
        display_currency: quote.terms.display_currency.clone(),
        purpose_ref: Some(quote.terms.customer_plan_id),
        idempotency_key: Uuid::new_v4().to_string(),
        promo_minor: 0,
    }
}
async fn due(db: &Db, organization: Uuid, plan: Uuid) {
    // Use one DB timestamp for both bounds; renewal compares exact snapshots.
    let mut tx = db.pool().begin().await.unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_end=now()-interval '1 second' WHERE customer_plan_id=$1 AND state='active'").bind(plan).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_end=now()-interval '1 second' WHERE organization_id=$1 AND state='active'").bind(organization).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn an_order_cannot_choose_its_customer_fee_currency_or_bonus() {
    let Some((db, owner, organization, plan)) = world().await else {
        return;
    };
    let context = fixture::merchant_context(&db, &owner).await;
    let quote = db.plan_payment_quote(organization).await.unwrap();
    assert_eq!(quote.terms.fee_minor, 1_000_001);
    assert_eq!(
        quote.terms.display_amount_minor, 101,
        "fiat ceiling never inflates sold entitlement"
    );
    let good = intent(&quote);
    for mutation in 0..6 {
        let mut request = intent(&quote);
        match mutation {
            0 => request.display_amount_minor -= 1,
            1 => request.display_amount_minor += 1,
            2 => request.display_currency = "EUR".into(),
            3 => request.promo_minor = 1,
            4 => request.expected_plan_quote = Some("0".repeat(64)),
            _ => request.purpose_ref = Some(Uuid::new_v4()),
        }
        assert!(
            db.create_payment_order(&context, organization, &request)
                .await
                .is_err()
        );
    }
    assert!(matches!(
        db.activate_plan_period(Uuid::new_v4(), plan).await,
        Err(WalletError::NotFound(_))
    ));
    let order = db
        .create_payment_order(&context, organization, &good)
        .await
        .unwrap();
    assert_eq!(order.amount_minor, 1_000_001);
    assert!(matches!(
        db.activate_plan_period(order.order_id, plan).await,
        Err(WalletError::Conflict(_))
    ));
    let foreign = db
        .register(NewUser {
            email: format!("{}@example.test", Uuid::new_v4()),
            password: "foreign plan fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    assert!(
        db.create_payment_order(&context, foreign.organization.id, &good)
            .await
            .is_err()
    );
    assert!(db.plan_periods(plan).await.unwrap().is_empty());
    fixture::mark_paid(&db, &context, &order).await;
    let (left, right) = tokio::join!(
        db.activate_plan_period(order.order_id, plan),
        db.activate_plan_period(order.order_id, plan)
    );
    assert_eq!(
        left.unwrap().subscription_id,
        right.unwrap().subscription_id
    );
    fixture::fulfill(&db, &order).await;
    assert_eq!(db.plan_periods(plan).await.unwrap().len(), 1);
    assert!(
        db.activate_plan_period(order.order_id, Uuid::new_v4())
            .await
            .is_err()
    );
    let replay = db
        .create_payment_order(&context, organization, &good)
        .await
        .unwrap();
    assert_eq!(replay.order_id, order.order_id);
    for fingerprint in [None, Some("0".repeat(64))] {
        let mut altered = good.clone();
        altered.expected_plan_quote = fingerprint;
        assert!(matches!(
            db.create_payment_order(&context, organization, &altered)
                .await,
            Err(WalletError::Conflict(_))
        ));
    }
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(organization)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        oxsum_core::Wallet::open(db.pool().clone(), &tenant)
            .await
            .unwrap()
            .available()
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn concurrent_intents_resolve_to_one_period_and_manual_renewal_waits_for_boundary() {
    let Some((db, owner, organization, plan)) = world().await else {
        return;
    };
    let context = fixture::merchant_context(&db, &owner).await;
    let quote = db.plan_payment_quote(organization).await.unwrap();
    let a = intent(&quote);
    let b = intent(&quote);
    let (a, b) = tokio::join!(
        db.create_payment_order(&context, organization, &a),
        db.create_payment_order(&context, organization, &b)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let order = a.or(b).unwrap();
    assert_eq!(
        db.plan_payment_quote(organization)
            .await
            .unwrap()
            .existing_order_id,
        Some(order.order_id)
    );
    fixture::mark_paid(&db, &context, &order).await;
    fixture::fulfill(&db, &order).await;
    let before = db.plan_periods(plan).await.unwrap();
    assert!(matches!(
        db.plan_payment_quote(organization).await,
        Err(WalletError::Conflict(_))
    ));
    let stale = intent(&quote);
    assert!(
        db.create_payment_order(&context, organization, &stale)
            .await
            .is_err()
    );
    assert_eq!(
        db.plan_periods(plan).await.unwrap()[0].period_end,
        before[0].period_end
    );
    due(&db, organization, plan).await;
    let renewal = fixture::created_order(&db, &context, organization, plan).await;
    assert!(matches!(
        db.cancel_customer_plan(organization).await,
        Err(WalletError::Conflict(_))
    ));
    fixture::mark_paid(&db, &context, &renewal).await;
    // A boundary worker racing paid activation cannot close the wrong version
    // or deadlock its subscription/assignment locks.
    let (activated, rollover) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(
            db.activate_plan_period(renewal.order_id, plan),
            db.rollover_due_subscriptions()
        )
    })
    .await
    .unwrap();
    activated.unwrap();
    rollover.unwrap();
    fixture::fulfill(&db, &renewal).await;
    let periods = db.plan_periods(plan).await.unwrap();
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[1].period_start, periods[0].period_end);
    assert_eq!(periods[1].authorization_ref, renewal.order_id);
    assert_eq!(periods[1].quota_minor, Some(7_000_000));
    db.cancel_customer_plan(organization).await.unwrap();
}

#[tokio::test]
async fn order_snapshot_and_database_writer_cannot_bypass_period_authority() {
    let Some((db, owner, organization, plan)) = world().await else {
        return;
    };
    let context = fixture::merchant_context(&db, &owner).await;
    let order = fixture::created_order(&db, &context, organization, plan).await;
    assert!(sqlx::query("UPDATE oxsum.payment_orders SET plan_payment=jsonb_set(plan_payment,'{feeMinor}','1') WHERE order_id=$1").bind(order.order_id).execute(db.pool()).await.is_err());
    let insert = "INSERT INTO oxsum.payment_orders(order_id,business_id,merchant_id,merchant_version_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,purpose_ref,promo_minor,idempotency_key,status,plan_payment) SELECT $2,business_id,merchant_id,merchant_version_id,organization_id,purpose,amount_minor,currency,display_currency,display_amount_minor,rate_mantissa,rate_scale,purpose_ref,promo_minor,$2::text,'created',$3 FROM oxsum.payment_orders WHERE order_id=$1";
    let stored = serde_json::to_value(order.plan_payment.unwrap()).unwrap();
    for field in [
        "periodIndex",
        "periodStart",
        "periodEnd",
        "collectionRevision",
        "feeMinor",
        "planVersionId",
    ] {
        let mut bad = stored.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            sqlx::query(insert)
                .bind(order.order_id)
                .bind(Uuid::new_v4())
                .bind(bad)
                .execute(db.pool())
                .await
                .is_err(),
            "missing {field}"
        );
    }
    assert!(
        sqlx::query(insert)
            .bind(order.order_id)
            .bind(Uuid::new_v4())
            .bind(serde_json::Value::Null)
            .execute(db.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_refund_reservation_cannot_race_a_new_entitlement_and_decline_restores_eligibility() {
    let Some((db, owner, organization, plan)) = world().await else {
        return;
    };
    let context = fixture::merchant_context(&db, &owner).await;
    let order = fixture::created_order(&db, &context, organization, plan).await;
    fixture::mark_paid(&db, &context, &order).await;
    let refund = db
        .request_order_refund(
            organization,
            order.order_id,
            &oxsum_core::NewRefundRequest {
                request_key: Uuid::new_v4().to_string(),
                reason: "cancel before activation".into(),
                amount_minor: None,
                requested_by: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        db.activate_plan_period(order.order_id, plan).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(db.plan_periods(plan).await.unwrap().is_empty());
    assert!(
        db.unfulfilled_payment_orders(100)
            .await
            .unwrap()
            .iter()
            .all(|pending| pending.order_id != order.order_id)
    );
    db.resolve_order_refund(
        &owner,
        refund.refund_id,
        oxsum_core::RefundDecision::Decline,
        Some("customer retains service"),
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    db.activate_plan_period(order.order_id, plan).await.unwrap();
    assert_eq!(db.plan_periods(plan).await.unwrap().len(), 1);
    // A crash after activation but before the outer fulfillment can still
    // finish its original effect if a later refund reserves this order.
    db.request_order_refund(
        organization,
        order.order_id,
        &oxsum_core::NewRefundRequest {
            request_key: Uuid::new_v4().to_string(),
            reason: "request after committed activation".into(),
            amount_minor: None,
            requested_by: None,
        },
    )
    .await
    .unwrap();
    assert!(
        db.unfulfilled_payment_orders(100)
            .await
            .unwrap()
            .iter()
            .any(|pending| pending.order_id == order.order_id)
    );
    fixture::fulfill(&db, &order).await;
    assert_eq!(db.plan_periods(plan).await.unwrap().len(), 1);
    assert!(
        db.payment_fulfillment(order.order_id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_standing_period_keeps_its_original_quote_after_operator_conversion_changes() {
    let Some((db, owner, organization, plan)) = world().await else {
        return;
    };
    let context = fixture::merchant_context(&db, &owner).await;
    let quote = db.plan_payment_quote(organization).await.unwrap();
    let new = intent(&quote);
    let order = db
        .create_payment_order(&context, organization, &new)
        .await
        .unwrap();
    db.set_collection_terms(
        &owner,
        &oxsum_core::CollectionTermsRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "new future collection rate".into(),
            expected_revision: quote.terms.collection_revision,
            currency: oxsum_core::CollectionCurrency {
                code: "USD".into(),
                rate_per_credit: "2".into(),
            },
        },
    )
    .await
    .unwrap();
    let standing = db.plan_payment_quote(organization).await.unwrap();
    assert_eq!(standing.fingerprint, quote.fingerprint);
    assert_eq!(standing.existing_order_id, Some(order.order_id));
    assert_eq!(
        db.create_payment_order(&context, organization, &new)
            .await
            .unwrap()
            .order_id,
        order.order_id
    );
    fixture::mark_paid(&db, &context, &order).await;
    fixture::fulfill(&db, &order).await;
    assert_eq!(db.plan_periods(plan).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_scheduled_paid_version_renews_with_its_own_quota_and_overage_policy() {
    let Some((db, owner, organization, plan)) = world().await else {
        return;
    };
    let old = fixture::paid_order(&db, &owner, organization, plan).await;
    fixture::fulfill(&db, &old).await;
    let next = legacy_plan::publish(
        &db,
        db.default_business().await.unwrap(),
        &NewSoldPlan {
            plan_id: None,
            name: "Changed paid terms".into(),
            price_minor: 2_000_000,
            currency: "USD".into(),
            quota_minor: Some(9_000_000),
            period_kind: PeriodKind::Anniversary,
            overage_enabled: true,
            sort_order: 0,
        },
    )
    .await
    .unwrap();
    db.schedule_customer_plan_change(organization, next.plan_version_id)
        .await
        .unwrap();
    assert!(db.plan_payment_quote(organization).await.is_err());
    due(&db, organization, plan).await;
    db.rollover_due_subscriptions().await.unwrap();
    let quote = db.plan_payment_quote(organization).await.unwrap();
    assert_eq!(quote.terms.plan_version_id, next.plan_version_id);
    assert_eq!(quote.terms.fee_minor, 2_000_000);
    assert_eq!(quote.terms.period_index, 1);
    let renewal = fixture::paid_order(&db, &owner, organization, plan).await;
    fixture::fulfill(&db, &renewal).await;
    let subscription = db.active_subscription(organization).await.unwrap().unwrap();
    assert_eq!(subscription.quota_minor, Some(9_000_000));
    assert!(subscription.allow_wallet_overflow);
    let draw = db
        .admit_subscription_draw(organization, &Uuid::new_v4().to_string(), 10_000_000)
        .await
        .unwrap();
    assert_eq!(
        draw.drawn_minor, 9_000_000,
        "admission uses the new sold mirror"
    );
}

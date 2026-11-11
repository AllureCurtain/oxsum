//! Controlled financial source, drift, snapshot and concurrency acceptance.
// Each acceptance module owns its independent setup OnceCell and test database.
#![allow(clippy::duplicate_mod, clippy::unwrap_used, clippy::expect_used)]

#[path = "../../tests/support/legacy_plan.rs"]
mod legacy_plan;
#[path = "../../tests/support/plan_payment.rs"]
mod payment;
#[path = "../../tests/support/integration.rs"]
mod setup;

use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::compare;
use super::types::{Comparison, Scope};
use crate::{
    Db, NewSoldPlan, NewUser, PeriodAuthorization, PeriodKind, TimeBasis, Wallet, WalletError,
};

const FEE: i64 = 2_500_000;
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct World {
    db: Db,
    owner: String,
    org: Uuid,
    tenant: String,
    wallet: Wallet,
    plan: Uuid,
    period: crate::PlanPeriod,
    journal: Uuid,
    order: crate::PaymentOrder,
    context: crate::MerchantSigningContext,
}
async fn world(days: i64, legacy: bool) -> Option<World> {
    world_with_fee(days, legacy, FEE).await
}
async fn world_with_fee(days: i64, legacy: bool, fee: i64) -> Option<World> {
    world_with_history(days, legacy, fee, None).await
}
async fn world_with_history(
    days: i64,
    legacy: bool,
    fee: i64,
    historical_overage: Option<i64>,
) -> Option<World> {
    let url = oxsum_testkit::suite_db_url("independent_reconciliation")?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "independent accounting fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = user.organization.id;
    let tenant = user.organization.tenant_id;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let business = db.default_business().await.unwrap();
    let input = NewSoldPlan {
        plan_id: None,
        name: format!("reconcile-{}", Uuid::new_v4()),
        price_minor: fee,
        currency: "USD".into(),
        quota_minor: Some(10_000_000),
        period_kind: PeriodKind::Anniversary,
        overage_enabled: true,
        sort_order: 0,
    };
    let sold = if legacy {
        legacy_plan::publish(&db, business, &input).await.unwrap()
    } else {
        db.publish_sold_plan(business, &input).await.unwrap()
    };
    let plan = db
        .bind_customer_plan(
            org,
            sold.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap()
        .customer_plan_id;
    let context = payment::merchant_context(&db, &owner).await;
    if fee < 10_000 {
        let terms = db.collection_terms().await.unwrap();
        db.set_collection_terms(
            &owner,
            &crate::CollectionTermsRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "exact minor-unit tiny plan fixture".into(),
                expected_revision: terms.revision,
                currency: crate::CollectionCurrency {
                    code: "USD".into(),
                    rate_per_credit: "10000".into(),
                },
            },
        )
        .await
        .unwrap();
    }
    let order = payment::created_order(&db, &context, org, plan).await;
    payment::mark_paid(&db, &context, &order).await;
    if fee < 10_000 {
        let terms = db.collection_terms().await.unwrap();
        db.set_collection_terms(
            &owner,
            &crate::CollectionTermsRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "restore ordinary fixture collection terms".into(),
                expected_revision: terms.revision,
                currency: crate::CollectionCurrency {
                    code: "USD".into(),
                    rate_per_credit: "1".into(),
                },
            },
        )
        .await
        .unwrap();
    }
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.activate_plan_period_at(order.order_id, plan, now - Duration::days(days))
        .await
        .unwrap();
    if legacy {
        if let Some(overage) = historical_overage {
            payment::legacy_close(&db, org, plan, overage).await;
        }
        db.rollover_due_subscriptions().await.unwrap();
    }
    let period = db.plan_periods(plan).await.unwrap().remove(0);
    let journal = db.plan_journals(org, None, 50).await.unwrap().journals[0].journal_id;
    if !legacy {
        let mut tx = db.pool().begin().await.unwrap();
        db.recognize_plan_period_in(
            &mut tx,
            &wallet,
            journal,
            OffsetDateTime::now_utc(),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    Some(World {
        db,
        owner,
        org,
        tenant,
        wallet,
        plan,
        period,
        journal,
        order,
        context,
    })
}
fn scope(w: &World, basis: TimeBasis) -> Scope {
    Scope {
        start: w.period.period_start.date(),
        end: OffsetDateTime::now_utc().date(),
        basis,
    }
}
async fn capture(w: &World, basis: TimeBasis) -> Comparison {
    capture_scope(w, scope(w, basis)).await
}
async fn capture_scope(w: &World, scope: Scope) -> Comparison {
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SET LOCAL TIME ZONE 'UTC'")
        .execute(&mut *tx)
        .await
        .unwrap();
    let mut result = Comparison::new(scope.basis);
    compare::customer(&mut tx, w.org, &w.tenant, scope, &mut result)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    result
}
fn clean(result: &Comparison) {
    assert!(
        result.coverage.complete,
        "{}",
        serde_json::to_string_pretty(&result.differences).unwrap()
    );
    assert!(
        result.differences.is_empty(),
        "{}",
        serde_json::to_string_pretty(&result.differences).unwrap()
    );
    assert_eq!(result.expected, result.actual);
    assert!(result.coverage.entries > 0);
    assert_eq!(result.coverage.entries, result.coverage.verified_entries);
}
async fn refund(w: &World, amount: i64) -> Uuid {
    refund_on(w, amount, OffsetDateTime::now_utc().date()).await
}
async fn refund_on(w: &World, amount: i64, on: time::Date) -> Uuid {
    refund_order_on(w, &w.order, amount, on).await
}
async fn refund_order_on(
    w: &World,
    order: &crate::PaymentOrder,
    amount: i64,
    on: time::Date,
) -> Uuid {
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &crate::NewRefundRequest {
                amount_minor: Some(amount),
                reason: "independent cash fixture".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    w.db.resolve_order_refund(
        &w.owner,
        refund.refund_id,
        crate::RefundDecision::Approve,
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
            &crate::VerifiedMerchantEvent {
                event_id: Uuid::new_v4().to_string(),
                kind: crate::MerchantEventKind::RefundSucceeded,
                payment_id: Some(order.order_id),
                provider_payment_id: Some(format!("pay_{}", order.order_id)),
                organization_id: Some(w.org),
                provider_account_id: Some(w.context.account_id.clone()),
                mode: Some(crate::MerchantMode::Test),
                amount_minor: Some(refund.display_amount_minor),
                currency: Some(refund.display_currency),
                refund_id: Some(provider),
            },
            &[84; 32],
        )
        .await
        .unwrap();
    w.db.apply_refund_event(&w.wallet, &receipt, on)
        .await
        .unwrap();
    refund.refund_id
}
async fn lost(w: &World, order: &crate::PaymentOrder) -> Uuid {
    lost_amount(w, order, order.display_amount_minor).await
}

async fn lost_amount(w: &World, order: &crate::PaymentOrder, display_amount: i64) -> Uuid {
    let reference = format!("dispute_{}", Uuid::new_v4());
    let event = crate::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: crate::MerchantEventKind::Dispute,
        payment_id: Some(order.order_id),
        provider_payment_id: Some(format!("pay_{}", order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(crate::MerchantMode::Test),
        amount_minor: Some(display_amount),
        currency: Some(order.display_currency.clone()),
        refund_id: Some(reference.clone()),
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[84; 32])
            .await
            .unwrap();
    w.db.record_order_dispute(&event, &receipt).await.unwrap();
    let id: Uuid = sqlx::query_scalar(
        "SELECT dispute_id FROM oxsum.disputes WHERE order_id=$1 AND provider_dispute_id=$2",
    )
    .bind(order.order_id)
    .bind(reference)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    w.db.resolve_order_dispute(
        &w.wallet,
        &w.owner,
        id,
        crate::DisputeResolution::Lost,
        Some("independent accounting loss"),
        &Uuid::new_v4().to_string(),
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn partial_plan_dispute_compares_to_authenticated_event_not_just_matching_books() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [false, true] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        let dispute = lost_amount(&w, &w.order, 100).await;
        clean(&capture(&w, TimeBasis::Booking).await);
        let receipt: Uuid =
            sqlx::query_scalar("SELECT inbox_receipt_id FROM oxsum.disputes WHERE dispute_id=$1")
                .bind(dispute)
                .fetch_one(w.db.pool())
                .await
                .unwrap();
        // Reproduce the former defect's shape: the books and dispute agree on a
        // larger loss than the retained event. Mutate only the captured event in
        // this isolated fault fixture; never rewrite real financial history.
        corrupt(&w, &format!("UPDATE oxsum.merchant_event_inbox SET normalized=jsonb_set(normalized,'{{amountMinor}}','50'::jsonb) WHERE receipt_id='{receipt}'")).await;
        let result = capture(&w, TimeBasis::Booking).await;
        assert_eq!(
            result.expected, result.actual,
            "ledger agreement alone is insufficient"
        );
        assert!(
            result
                .differences
                .iter()
                .any(|difference| difference.fact == "authenticatedAmountMinor")
        );
    }
}

#[tokio::test]
async fn plan_dispute_nonintegral_partial_fiat_requires_review_without_rounding() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world_with_fee(0, false, 10_001).await else {
        return;
    };
    assert_eq!(w.order.display_amount_minor, 2);
    let event = crate::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: crate::MerchantEventKind::Dispute,
        payment_id: Some(w.order.order_id),
        provider_payment_id: Some(format!("pay_{}", w.order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(crate::MerchantMode::Test),
        amount_minor: Some(1),
        currency: Some(w.order.display_currency.clone()),
        refund_id: Some(format!("case_{}", Uuid::new_v4())),
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[101; 32])
            .await
            .unwrap();
    assert_eq!(receipt.state, "rejected");
    assert_eq!(receipt.reason.as_deref(), Some("disputeAmountNotIntegral"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.disputes WHERE order_id=$1")
        .bind(w.order.order_id)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    clean(&capture(&w, TimeBasis::Booking).await);
}
async fn corrupt(w: &World, sql: &str) {
    let mut tx = w.db.pool().begin().await.unwrap();
    // Controlled privileged corruption only inside a disposable acceptance DB.
    sqlx::query("SET LOCAL session_replication_role='replica'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql(sql).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn v1_and_v2_real_books_have_independent_amounts_and_explicit_noncoverage() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [false, true] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        for basis in [TimeBasis::Booking, TimeBasis::Service, TimeBasis::Payment] {
            let result = capture(&w, basis).await;
            clean(&result);
            assert_eq!(result.actual.billed_minor, FEE);
            assert_eq!(result.actual.collected_minor, FEE);
            assert_eq!(
                result.actual.billed_minor,
                result.actual.earned_minor + result.actual.deferred_minor
            );
            assert_eq!(result.actual.receivable_minor, 0);
            assert!(
                result
                    .coverage
                    .not_compared
                    .contains(&"fundingMinor".to_owned())
            );
            assert!(
                serde_json::to_value(&result.actual)
                    .unwrap()
                    .get("usageMinor")
                    .is_none()
            );
        }
    }
}

#[tokio::test]
async fn original_refund_cash_and_partial_dispute_do_not_duplicate_income_or_cash() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [false, true] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        refund(&w, 1_000_000).await;
        let refunded = capture(&w, TimeBasis::Booking).await;
        clean(&refunded);
        assert_eq!(refunded.actual.refund_minor, 1_000_000);
        assert_eq!(refunded.actual.billed_minor, FEE - 1_000_000);
        lost(&w, &w.order).await;
        let disputed = capture(&w, TimeBasis::Booking).await;
        clean(&disputed);
        assert_eq!(disputed.actual.refund_minor, 1_000_000);
        assert_eq!(disputed.actual.earned_minor, refunded.actual.earned_minor);
        assert_eq!(disputed.actual.receivable_minor, FEE - 1_000_000);
        assert_eq!(disputed.actual.collected_minor, 0);
    }
}

#[tokio::test]
async fn void_refund_and_excess_dispute_keep_payable_and_suspense_outside_facts() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [false, true] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        w.db.void_invoice_journal(
            &w.owner,
            &w.wallet,
            w.journal,
            "independent cancellation",
            &Uuid::new_v4().to_string(),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
        clean(&capture(&w, TimeBasis::Booking).await);
        refund(&w, 1_000_000).await;
        lost(&w, &w.order).await;
        let result = capture(&w, TimeBasis::Booking).await;
        clean(&result);
        assert_eq!(result.actual.billed_minor, 0);
        assert_eq!(result.actual.earned_minor, 0);
        assert_eq!(result.actual.receivable_minor, 0);
        assert_eq!(result.actual.refund_minor, 1_000_000);
    }
}

#[tokio::test]
async fn internally_balanced_effect_drift_disagrees_with_real_official_entries() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    clean(&capture(&w, TimeBasis::Booking).await);
    corrupt(&w,&format!("UPDATE oxsum.plan_accounting_effects SET billed_minor=billed_minor+17,deferred_minor=deferred_minor+17,collected_minor=collected_minor+17 WHERE plan_period_id='{}' AND kind='activation'",w.period.plan_period_id)).await;
    let result = capture(&w, TimeBasis::Booking).await;
    assert_eq!(
        result.expected.billed_minor,
        result.expected.collected_minor + result.expected.receivable_minor
    );
    assert_ne!(result.expected, result.actual);
    assert_eq!(result.actual.billed_minor, FEE);
    assert!(
        result.differences.iter().any(|d| d.fact == "billedMinor"
            && d.expected == Some(FEE + 17)
            && d.actual == Some(FEE))
    );
}

#[tokio::test]
async fn balanced_legacy_header_drift_cannot_be_its_own_actual() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(40, true).await else {
        return;
    };
    corrupt(&w,&format!("UPDATE oxsum.invoice_journals SET total_minor=total_minor+29,settled_minor=settled_minor+29 WHERE journal_id='{}'",w.journal)).await;
    let result = capture(&w, TimeBasis::Booking).await;
    assert_eq!(result.expected.receivable_minor, 0);
    assert_eq!(result.actual.billed_minor, FEE);
    assert_ne!(result.expected.billed_minor, result.actual.billed_minor);
    assert!(
        result
            .differences
            .iter()
            .any(|d| d.fact == "lineTotalMinor")
    );
}

#[tokio::test]
async fn equal_payment_total_cannot_hide_a_different_original_order() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [false, true] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        clean(&capture(&w, TimeBasis::Booking).await);
        let replaced = Uuid::new_v4();
        corrupt(&w, &format!("UPDATE oxsum.plan_statement_lines SET source_ref='{replaced}' WHERE statement_id IN (SELECT statement_id FROM oxsum.plan_statements WHERE journal_id='{}') AND kind='payment_applied' AND source_ref='{}'", w.journal, w.order.order_id)).await;
        let result = capture(&w, TimeBasis::Booking).await;
        assert_eq!(result.expected, result.actual);
        assert!(!result.coverage.complete);
        assert!(
            result
                .differences
                .iter()
                .any(|d| d.fact == "paymentSources")
        );
        assert!(
            !result
                .differences
                .iter()
                .any(|d| d.fact == "paymentLinesMinor")
        );
    }
}

#[tokio::test]
async fn tiny_zero_days_use_service_boundaries_across_earlier_started_refund_and_void() {
    let _guard = TEST_LOCK.lock().await;
    for fee in [3, 17] {
        let Some(w) = world_with_fee(10, false, fee).await else {
            return;
        };
        let zeros: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 AND kind='recognition' AND entry_id IS NULL")
        .bind(w.period.plan_period_id).fetch_one(w.db.pool()).await.unwrap();
        assert!(zeros > 0);
        clean(&capture(&w, TimeBasis::Booking).await);
        refund(&w, 1).await;
        // Transaction start can predate an already completed zero-only pass: the
        // refund waits for plan ownership and must still follow its service days.
        corrupt(&w, &format!("UPDATE oxsum.plan_accounting_effects SET created_at=(SELECT MIN(created_at)-interval '1 second' FROM oxsum.plan_accounting_effects WHERE plan_period_id='{}' AND kind='recognition') WHERE plan_period_id='{}' AND kind='refund'",w.period.plan_period_id,w.period.plan_period_id)).await;
        clean(&capture(&w, TimeBasis::Booking).await);
        w.db.void_invoice_journal(
            &w.owner,
            &w.wallet,
            w.journal,
            "tiny complete cancellation",
            &Uuid::new_v4().to_string(),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
        clean(&capture(&w, TimeBasis::Service).await);
        // Equal microsecond transaction starts cannot merge nonzero movement order.
        corrupt(&w, &format!("UPDATE oxsum.plan_accounting_effects SET created_at=(SELECT MIN(created_at) FROM oxsum.plan_accounting_effects WHERE plan_period_id='{}') WHERE plan_period_id='{}'",w.period.plan_period_id,w.period.plan_period_id)).await;
        let result = capture(&w, TimeBasis::Booking).await;
        clean(&result);
        assert_eq!(result.actual.billed_minor, 0);
        assert_eq!(result.actual.refund_minor, 1);
    }
}

#[tokio::test]
async fn refund_waiting_across_midnight_does_not_invent_zero_day_chronology() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world_with_fee(1, false, 3).await else {
        return;
    };
    let midnight = OffsetDateTime::now_utc().date().midnight().assume_utc();
    let zeros: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 AND kind='recognition' AND entry_id IS NULL")
        .bind(w.period.plan_period_id).fetch_one(w.db.pool()).await.unwrap();
    assert!(zeros > 0);
    // Keep a real, verified refund entry booked on the caller's earlier date.
    // A transaction that waited for plan ownership across midnight retains this
    // date and its earlier start, while recognition samples the later clock.
    refund_on(&w, 1, midnight.date().previous_day().unwrap()).await;
    clean(&capture(&w, TimeBasis::Booking).await);
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("SET LOCAL session_replication_role='replica'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.plan_accounting_effects SET created_at=$2 WHERE plan_period_id=$1 AND kind='refund'")
        .bind(w.period.plan_period_id).bind(midnight-Duration::seconds(1)).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let result = capture(&w, TimeBasis::Booking).await;
    assert_eq!(result.expected, result.actual);
    assert_eq!(result.coverage.entries, result.coverage.verified_entries);
    assert!(!result.coverage.complete);
    assert!(
        result
            .differences
            .iter()
            .any(|d| d.fact == "ambiguousZeroRecognitionOrder")
    );
    assert!(result.coverage.not_compared.contains(&format!(
        "planRecognitionSchedule/{}",
        w.period.plan_period_id
    )));
    assert!(
        !result
            .differences
            .iter()
            .any(|d| d.fact.starts_with("schedule."))
    );
}

#[tokio::test]
async fn free_plan_activation_and_void_compare_without_inventing_official_money() {
    let _guard = TEST_LOCK.lock().await;
    let Some(base) = world(0, false).await else {
        return;
    };
    let user = base
        .db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "free source fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let sold = base
        .db
        .publish_sold_plan(
            base.db.default_business().await.unwrap(),
            &NewSoldPlan {
                plan_id: None,
                name: "Free reconciled daily plan".into(),
                price_minor: 0,
                currency: "USD".into(),
                quota_minor: Some(5),
                period_kind: PeriodKind::CalendarMonth,
                overage_enabled: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    let plan = base
        .db
        .bind_customer_plan(
            user.organization.id,
            sold.plan_version_id,
            PeriodAuthorization::Complimentary(Uuid::new_v4()),
        )
        .await
        .unwrap();
    let period = base
        .db
        .plan_periods(plan.customer_plan_id)
        .await
        .unwrap()
        .remove(0);
    let journal = base
        .db
        .plan_journals(user.organization.id, None, 50)
        .await
        .unwrap()
        .journals[0]
        .journal_id;
    let wallet = Wallet::open_existing(base.db.pool().clone(), &user.organization.tenant_id)
        .await
        .unwrap();
    let w = World {
        db: base.db,
        owner: base.owner,
        org: user.organization.id,
        tenant: user.organization.tenant_id,
        wallet,
        plan: plan.customer_plan_id,
        period,
        journal,
        order: base.order,
        context: base.context,
    };
    for void in [false, true] {
        if void {
            w.db.void_invoice_journal(
                &w.owner,
                &w.wallet,
                w.journal,
                "free invoice cancellation",
                &Uuid::new_v4().to_string(),
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
        }
        let result = capture(&w, TimeBasis::Booking).await;
        assert!(
            result.coverage.complete,
            "{}",
            serde_json::to_string_pretty(&result.differences).unwrap()
        );
        assert!(result.differences.is_empty());
        assert_eq!(result.actual, super::types::Amounts::default());
        assert_eq!(result.expected, result.actual);
        let monetary: i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 AND entry_id IS NOT NULL").bind(w.period.plan_period_id).fetch_one(w.db.pool()).await.unwrap();
        assert_eq!(monetary, 0);
    }
}

#[tokio::test]
async fn changed_registered_financial_path_is_explicitly_incomplete() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    corrupt(&w, &format!("UPDATE ledger_{}.accounts SET path='Income:UnexpectedPlanFees' WHERE path='Income:PlanFees'",w.tenant)).await;
    let result = capture(&w, TimeBasis::Booking).await;
    assert!(!result.coverage.complete);
    assert!(
        result
            .differences
            .iter()
            .any(|difference| difference.fact == "postingShape")
    );
    assert!(result.coverage.verified_entries > 0);
    assert_eq!(result.coverage.verified_entries, result.coverage.entries);
}

#[tokio::test]
async fn corrupt_posting_hash_and_missing_effect_have_durable_exact_source_differences() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    let entry =
        *crate::entry_id_for(&format!("payment-order/{}/prepay", w.order.order_id)).as_uuid();
    corrupt(&w,&format!("UPDATE ledger_{}.postings SET amount_minor=amount_minor+1 WHERE entry_id='{}'; DELETE FROM oxsum.plan_accounting_effects WHERE effect_id=(SELECT effect_id FROM oxsum.plan_accounting_effects WHERE plan_period_id='{}' AND kind='recognition' ORDER BY service_day LIMIT 1)",w.tenant,entry,w.period.plan_period_id)).await;
    let result = capture(&w, TimeBasis::Booking).await;
    assert!(!result.coverage.complete);
    assert!(result.coverage.verified_entries < result.coverage.entries);
    assert!(result.differences.iter().any(|d| d.fact == "officialEntry"));
    assert!(
        result
            .differences
            .iter()
            .any(|d| d.fact.starts_with("missingRecognition/"))
    );
    assert!(
        result
            .differences
            .iter()
            .any(|d| d.fact == "unclaimedEntry")
    );
}

#[tokio::test]
async fn wrong_raw_key_cannot_pass_by_preserving_a_valid_stored_hash() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let foreign_order = Uuid::new_v4();
    let wrong = *crate::entry_id_for(&format!("payment-order/{foreign_order}/prepay")).as_uuid();
    let mut tx = w.db.pool().begin().await.unwrap();
    w.wallet
        .plan_prepay_in(
            &mut tx,
            &foreign_order,
            FEE,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    corrupt(&w,&format!("UPDATE oxsum.plan_accounting_effects SET entry_id='{wrong}' WHERE plan_period_id='{}' AND kind='activation'",w.period.plan_period_id)).await;
    let result = capture(&w, TimeBasis::Booking).await;
    assert!(!result.coverage.complete);
    assert!(result.differences.iter().any(|d| d.fact == "officialEntry"));
}

#[tokio::test]
async fn repeatable_read_keeps_source_and_ledger_capture_on_the_same_snapshot() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT total_minor FROM oxsum.invoice_journals WHERE journal_id=$1")
            .bind(w.journal)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    refund(&w, 1_000_000).await;
    let mut result = Comparison::new(TimeBasis::Booking);
    compare::customer(
        &mut tx,
        w.org,
        &w.tenant,
        scope(&w, TimeBasis::Booking),
        &mut result,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    clean(&result);
    assert_eq!(result.actual.billed_minor, before);
    assert_eq!(result.actual.refund_minor, 0);
    let after = capture(&w, TimeBasis::Booking).await;
    clean(&after);
    assert_eq!(after.actual.refund_minor, 1_000_000);
}

#[tokio::test]
async fn retained_attempts_exact_replay_and_failed_bounds_preserve_original_evidence() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let day = OffsetDateTime::now_utc().date();
    let key = Uuid::new_v4().to_string();
    let first =
        w.db.request_reconciliation_run_with_basis(&w.owner, day, day, TimeBasis::Booking, &key)
            .await
            .unwrap();
    let snapshot = first.latest_snapshot_id.unwrap();
    let old =
        w.db.reconciliation_snapshot(&w.owner, first.run_id, snapshot)
            .await
            .unwrap()
            .unwrap();
    refund(&w, 1_000_000).await;
    let second =
        w.db.request_reconciliation_run_with_basis(
            &w.owner,
            day,
            day,
            TimeBasis::Booking,
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    assert_eq!(first.run_id, second.run_id);
    assert_ne!(Some(snapshot), second.latest_snapshot_id);
    let replay =
        w.db.request_reconciliation_run_with_basis(&w.owner, day, day, TimeBasis::Booking, &key)
            .await
            .unwrap();
    assert_eq!(replay.latest_snapshot_id, Some(snapshot));
    assert_eq!(replay.actual_totals, first.actual_totals);
    let read =
        w.db.reconciliation_snapshot(&w.owner, first.run_id, snapshot)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(read.evidence, old.evidence);
    assert!(
        sqlx::query(
            "UPDATE oxsum.reconciliation_snapshots SET state='matched' WHERE snapshot_id=$1"
        )
        .bind(snapshot)
        .execute(w.db.pool())
        .await
        .is_err()
    );
    let list =
        w.db.reconciliation_snapshots(&w.owner, first.run_id, None, 1)
            .await
            .unwrap();
    assert_eq!(list[0].snapshot_id, second.latest_snapshot_id.unwrap());
    let older =
        w.db.reconciliation_snapshots(&w.owner, first.run_id, Some(list[0].snapshot_id), 1)
            .await
            .unwrap();
    assert_eq!(older[0].snapshot_id, snapshot);
    assert!(matches!(
        w.db.request_reconciliation_run_with_basis(&w.owner, day, day, TimeBasis::Service, &key)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        w.db.run_reconciliation("daily", day - Duration::days(366), day, None)
            .await
            .is_err()
    );
    assert_eq!(
        w.db.reconciliation_run(&w.owner, first.run_id)
            .await
            .unwrap()
            .unwrap()
            .latest_snapshot_id,
        second.latest_snapshot_id
    );
}

#[tokio::test]
async fn one_connection_capture_and_busy_identical_run_do_not_wait_for_nested_pools() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let day = OffsetDateTime::now_utc().date();
    let mut blocker = w.db.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("oxsum/reconciliation/daily/{day}/{day}/booking"))
        .execute(&mut *blocker)
        .await
        .unwrap();
    let url = oxsum_testkit::suite_db_url("independent_reconciliation").unwrap();
    let one = Db::connect(&url, 1).await.unwrap();
    let busy = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        one.run_reconciliation("daily", day, day, None),
    )
    .await
    .unwrap();
    assert!(matches!(busy, Err(WalletError::PoolBusy)));
    blocker.rollback().await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        one.run_reconciliation("daily", day, day, None),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn late_daily_refund_after_lost_payment_is_cash_suspense_without_fee_cancellation() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    // Reserve the payout first; the later chargeback consumes the original
    // collected fee before the already-authorized provider success arrives.
    let refund =
        w.db.request_order_refund(
            w.org,
            w.order.order_id,
            &crate::NewRefundRequest {
                amount_minor: Some(1_000_000),
                reason: "late external fact".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    w.db.resolve_order_refund(
        &w.owner,
        refund.refund_id,
        crate::RefundDecision::Approve,
        None,
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    let provider = format!("refund_{}", refund.refund_id);
    w.db.note_refund_provider(refund.refund_id, &provider)
        .await
        .unwrap();
    lost(&w, &w.order).await;
    let before = capture(&w, TimeBasis::Booking).await;
    clean(&before);
    let receipt =
        w.db.receive_merchant_event(
            &w.context,
            &crate::VerifiedMerchantEvent {
                event_id: Uuid::new_v4().to_string(),
                kind: crate::MerchantEventKind::RefundSucceeded,
                payment_id: Some(w.order.order_id),
                provider_payment_id: Some(format!("pay_{}", w.order.order_id)),
                organization_id: Some(w.org),
                provider_account_id: Some(w.context.account_id.clone()),
                mode: Some(crate::MerchantMode::Test),
                amount_minor: Some(refund.display_amount_minor),
                currency: Some(refund.display_currency),
                refund_id: Some(provider),
            },
            &[84; 32],
        )
        .await
        .unwrap();
    w.db.apply_refund_event(&w.wallet, &receipt, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    let after = capture(&w, TimeBasis::Booking).await;
    clean(&after);
    assert_eq!(after.actual.billed_minor, FEE);
    assert_eq!(after.actual.earned_minor, before.actual.earned_minor);
    assert_eq!(after.actual.receivable_minor, FEE);
    assert_eq!(after.actual.refund_minor, 1_000_000);
}

#[tokio::test]
async fn mixed_policy_chargebacks_keep_each_original_customer_debt() {
    let _guard = TEST_LOCK.lock().await;
    for daily_first in [false, true] {
        let Some(w) = world(40, true).await else {
            return;
        };
        refund(&w, 1_000_000).await;
        let current = w.db.customer_plan(w.org).await.unwrap().unwrap();
        let old =
            w.db.sold_plan(current.plan_version_id)
                .await
                .unwrap()
                .unwrap();
        let version =
            w.db.publish_sold_plan(
                old.business_id,
                &NewSoldPlan {
                    plan_id: Some(old.plan_id),
                    name: old.name,
                    price_minor: FEE,
                    currency: "USD".into(),
                    quota_minor: Some(10_000_000),
                    period_kind: PeriodKind::Anniversary,
                    overage_enabled: true,
                    sort_order: 0,
                },
            )
            .await
            .unwrap();
        w.db.schedule_customer_plan_change(w.org, version.plan_version_id)
            .await
            .unwrap();
        w.db.rollover_due_subscriptions().await.unwrap();
        let daily = payment::created_order(&w.db, &w.context, w.org, w.plan).await;
        payment::mark_paid(&w.db, &w.context, &daily).await;
        w.db.activate_plan_period(daily.order_id, w.plan)
            .await
            .unwrap();
        let current =
            w.db.plan_journals(w.org, None, 50)
                .await
                .unwrap()
                .journals
                .into_iter()
                .find(|j| j.accounting_policy_version == 2)
                .unwrap();
        let mut tx = w.db.pool().begin().await.unwrap();
        w.db.recognize_plan_period_in(
            &mut tx,
            &w.wallet,
            current.journal_id,
            OffsetDateTime::now_utc(),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let (first, second) = if daily_first {
            (&daily, &w.order)
        } else {
            (&w.order, &daily)
        };
        lost(&w, first).await;
        lost(&w, second).await;
        let result = capture(&w, TimeBasis::Booking).await;
        clean(&result);
        assert_eq!(result.actual.receivable_minor, 2 * FEE - 1_000_000);
        let statement = unpaid_statement(&w).await;
        let sources =
            w.db.statement_debt_sources(&debt_principal(&w).await, statement)
                .await
                .unwrap();
        assert!(sources.verified, "{sources:?}");
        assert_eq!(sources.slices.len(), 2);
        assert_eq!(sources.outstanding_minor, 2 * FEE - 1_000_000);
        assert_eq!(
            sources
                .slices
                .iter()
                .map(|slice| slice.remaining_minor)
                .sum::<i64>(),
            sources.outstanding_minor
        );
        assert!(
            sources
                .slices
                .iter()
                .any(|slice| slice.journal_id == w.journal
                    && slice.remaining_minor == FEE - 1_000_000)
        );
        assert!(sources.slices.iter().any(|slice|slice.journal_id == current.journal_id && slice.remaining_minor == FEE));
        let order = statement_order(&w, statement, None).await;
        assert_eq!(
            order.statement_payment.as_ref().unwrap().allocations.len(),
            2
        );
        payment::mark_paid(&w.db, &w.context, &order).await;
        payment::fulfill(&w.db, &order).await;
        clean(&capture(&w, TimeBasis::Booking).await);
        refund_order_on(&w, &order, 2_000_000, OffsetDateTime::now_utc().date()).await;
        clean(&capture(&w, TimeBasis::Booking).await);
        let reopened =
            w.db.statement_debt_sources(&debt_principal(&w).await, statement)
                .await
                .unwrap();
        assert!(reopened.verified, "{reopened:?}");
        assert_eq!(reopened.outstanding_minor, 2_000_000);
    }
}

#[tokio::test]
async fn current_booking_and_past_service_dates_are_separate_verified_movements() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    let day = w.period.period_start.date();
    let service = capture_scope(
        &w,
        Scope {
            start: day,
            end: day,
            basis: TimeBasis::Service,
        },
    )
    .await;
    clean(&service);
    assert_eq!(service.actual.billed_minor, FEE);
    assert!(service.actual.earned_minor > 0);
    let payment = capture_scope(
        &w,
        Scope {
            start: day,
            end: day,
            basis: TimeBasis::Payment,
        },
    )
    .await;
    clean(&payment);
    assert_eq!(payment.actual.billed_minor, 0);
    assert_eq!(payment.actual.earned_minor, 0);
    let today = OffsetDateTime::now_utc().date();
    let booked = capture_scope(
        &w,
        Scope {
            start: today,
            end: today,
            basis: TimeBasis::Booking,
        },
    )
    .await;
    clean(&booked);
    assert_eq!(booked.actual.billed_minor, 0);
    assert_eq!(
        booked.actual.earned_minor,
        capture(&w, TimeBasis::Booking).await.actual.earned_minor
    );
}

#[tokio::test]
async fn same_amount_receipt_from_another_refund_cannot_prove_this_source() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let first = refund(&w, 1_000_000).await;
    let second = refund(&w, 1_000_000).await;
    clean(&capture(&w, TimeBasis::Booking).await);
    corrupt(&w,&format!("UPDATE oxsum.plan_refund_effects SET event_receipt_id=(SELECT event_receipt_id FROM oxsum.plan_refund_effects WHERE refund_id='{second}') WHERE refund_id='{first}'")).await;
    let result = capture(&w, TimeBasis::Booking).await;
    assert!(!result.coverage.complete);
    assert!(
        result
            .differences
            .iter()
            .any(|d| d.source_ref == first.to_string() && d.fact == "allocationSource")
    );
}

#[tokio::test]
async fn report_readers_cannot_run_and_exact_replay_rechecks_live_funds_authority() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let user =
        w.db.register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "report viewer fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let token =
        w.db.login(&user.user.email, "report viewer fixture password")
            .await
            .unwrap()
            .into_session()
            .token;
    w.db.set_business_staff(
        &w.owner,
        &crate::BusinessStaffRequest {
            user_id: user.user.id,
            role: crate::BusinessRole::Finance,
            reason: "financial comparison".into(),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    w.db.switch_session_context(
        &token,
        &crate::SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: crate::SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    let day = OffsetDateTime::now_utc().date();
    let key = Uuid::new_v4().to_string();
    let run =
        w.db.request_reconciliation_run(&token, day, day, &key)
            .await
            .unwrap();
    w.db.set_business_staff(
        &w.owner,
        &crate::BusinessStaffRequest {
            user_id: user.user.id,
            role: crate::BusinessRole::Viewer,
            reason: "remove finance write".into(),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        w.db.request_reconciliation_run(&token, day, day, &key)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(
        !w.db
            .reconciliation_snapshots(&token, run.run_id, None, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        w.db.reconciliation_snapshot(&token, run.run_id, run.latest_snapshot_id.unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn snapshot_write_failure_keeps_the_preceding_complete_attempt() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let day = OffsetDateTime::now_utc().date();
    let first =
        w.db.run_reconciliation("daily", day, day, None)
            .await
            .unwrap();
    let before =
        w.db.reconciliation_run(&w.owner, first.run_id)
            .await
            .unwrap()
            .unwrap();
    sqlx::raw_sql("CREATE FUNCTION oxsum.refuse_reconciliation_attempt() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'injected snapshot persistence fault'; END; $$ LANGUAGE plpgsql; CREATE TRIGGER refuse_reconciliation_attempt BEFORE INSERT ON oxsum.reconciliation_snapshots FOR EACH ROW EXECUTE FUNCTION oxsum.refuse_reconciliation_attempt()").execute(w.db.pool()).await.unwrap();
    let failed = w.db.run_reconciliation("daily", day, day, None).await;
    sqlx::query("DROP TRIGGER refuse_reconciliation_attempt ON oxsum.reconciliation_snapshots")
        .execute(w.db.pool())
        .await
        .unwrap();
    assert!(failed.is_err());
    let retained =
        w.db.reconciliation_run(&w.owner, first.run_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(retained.latest_snapshot_id, before.latest_snapshot_id);
    assert_eq!(retained.actual_totals, before.actual_totals);
    w.db.run_reconciliation("daily", day, day, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn nonplan_partial_returns_and_payment_only_receipts_reconcile_original_cash() {
    let _guard = TEST_LOCK.lock().await;
    for payment_only in [false, true] {
        let Some(w) = world(0, false).await else {
            return;
        };
        let order =
            w.db.create_payment_order(
                &w.context,
                w.org,
                &crate::NewPaymentOrder {
                    purpose: crate::PaymentOrderPurpose::WalletTopup,
                    purpose_ref: None,
                    display_amount_minor: 250,
                    display_currency: "USD".into(),
                    idempotency_key: Uuid::new_v4().to_string(),
                    promo_minor: 0,
                    expected_statement_quote: None,
                    statement_amount_minor: None,
                    expected_plan_quote: None,
                    expected_collection_revision: None,
                },
            )
            .await
            .unwrap();
        payment::mark_paid(&w.db, &w.context, &order).await;
        w.db.fulfill_payment_order(order.order_id, &w.wallet, OffsetDateTime::now_utc().date())
            .await
            .unwrap();
        let refund =
            w.db.request_order_refund(
                w.org,
                order.order_id,
                &crate::NewRefundRequest {
                    amount_minor: Some(1_000_000),
                    reason: "nonplan source comparison".into(),
                    request_key: Uuid::new_v4().to_string(),
                    requested_by: None,
                },
            )
            .await
            .unwrap();
        w.db.resolve_order_refund(
            &w.owner,
            refund.refund_id,
            crate::RefundDecision::Approve,
            None,
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
        let event = crate::VerifiedMerchantEvent {
            event_id: Uuid::new_v4().to_string(),
            kind: crate::MerchantEventKind::RefundSucceeded,
            payment_id: if payment_only {
                None
            } else {
                Some(order.order_id)
            },
            provider_payment_id: Some(format!("pay_{}", order.order_id)),
            organization_id: if payment_only { None } else { Some(w.org) },
            provider_account_id: Some(w.context.account_id.clone()),
            mode: Some(crate::MerchantMode::Test),
            amount_minor: Some(refund.display_amount_minor),
            currency: Some(refund.display_currency),
            refund_id: if payment_only {
                None
            } else {
                Some(refund.refund_id.to_string())
            },
        };
        let receipt =
            w.db.receive_merchant_event(&w.context, &event, &[84; 32])
                .await
                .unwrap();
        assert_eq!(receipt.state, "accepted");
        w.db.apply_refund_event(&w.wallet, &receipt, OffsetDateTime::now_utc().date())
            .await
            .unwrap();
        let result = capture(&w, TimeBasis::Booking).await;
        clean(&result);
        assert_eq!(result.actual.refund_minor, 1_000_000);
        assert_eq!(w.wallet.available().await.unwrap(), FEE - 1_000_000);
        corrupt(&w,&format!("UPDATE oxsum.nonplan_refund_effects SET wallet_restored_minor=17,correction_entry_id='{}',correction_entry_hash=repeat('0',64),legacy_recovery=true WHERE refund_id='{}'",Uuid::new_v4(),refund.refund_id)).await;
        let drift = capture(&w, TimeBasis::Booking).await;
        assert!(!drift.coverage.complete);
        assert!(
            drift
                .differences
                .iter()
                .any(|d| d.fact == "retainedMetadata")
        );
    }
}

async fn debt_principal(w: &World) -> crate::Principal {
    let email: String = sqlx::query_scalar("SELECT u.email FROM oxsum.users u JOIN oxsum.memberships m USING(user_id) WHERE m.organization_id=$1 AND m.role='owner' LIMIT 1")
        .bind(w.org).fetch_one(w.db.pool()).await.unwrap();
    crate::Principal::Session(
        w.db.login(&email, "independent accounting fixture password")
            .await
            .unwrap()
            .into_session()
            .principal
            .unwrap(),
    )
}
async fn unpaid_statement(w: &World) -> Uuid {
    w.db.plan_statements(w.org, None, 50)
        .await
        .unwrap()
        .statements
        .into_iter()
        .find(|s| {
            s.total_minor > s.paid_minor
                && matches!(
                    s.state,
                    crate::PlanStatementState::Issued | crate::PlanStatementState::PastDue
                )
        })
        .unwrap()
        .statement_id
}
#[tokio::test]
async fn debt_sources_verify_chargeback_only_v1_and_v2_without_writes() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [true, false] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        let principal = debt_principal(&w).await;
        let paid =
            w.db.plan_statements(w.org, None, 50)
                .await
                .unwrap()
                .statements[0]
                .statement_id;
        assert_eq!(
            w.db.statement_debt_sources(&principal, paid)
                .await
                .unwrap()
                .reason,
            Some("statementNotPayable")
        );
        let dispute = lost(&w, &w.order).await;
        let statement = unpaid_statement(&w).await;
        assert_ne!(
            statement, paid,
            "the new debt must not target the old paid statement"
        );
        let log = w.wallet.log_size().await.unwrap();
        let first =
            w.db.statement_debt_sources(&principal, statement)
                .await
                .unwrap();
        assert!(first.verified, "{first:?}");
        assert_eq!(first.outstanding_minor, FEE);
        assert_eq!(first.slices.len(), 1);
        assert_eq!(first.slices[0].journal_id, w.journal);
        assert_eq!(first.slices[0].dispute_ids, vec![dispute]);
        assert_eq!(first.slices[0].remaining_minor, FEE);
        let second =
            w.db.statement_debt_sources(&principal, statement)
                .await
                .unwrap();
        assert_eq!(first.revision, second.revision);
        assert_eq!(log, w.wallet.log_size().await.unwrap());
        assert_eq!(
            w.db.plan_statement(w.org, statement)
                .await
                .unwrap()
                .unwrap()
                .statement
                .paid_minor,
            0
        );
    }
}
#[tokio::test]
async fn debt_sources_hide_foreign_statement_and_refuse_consumption_credentials() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let other =
        w.db.register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "another source fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let foreign = crate::Principal::Session(
        w.db.login(&other.user.email, "another source fixture password")
            .await
            .unwrap()
            .into_session()
            .principal
            .unwrap(),
    );
    assert!(matches!(
        w.db.statement_debt_sources(&foreign, statement).await,
        Err(WalletError::NotFound(_))
    ));
    let (organization, key) =
        w.db.authenticate(&other.api_key.secret)
            .await
            .unwrap()
            .unwrap();
    assert!(matches!(
        w.db.statement_debt_sources(
            &crate::Principal::Key(crate::KeyPrincipal { organization, key }),
            statement
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
}
#[tokio::test]
async fn debt_sources_preserve_amount_when_canonical_history_needs_review() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let principal = debt_principal(&w).await;
    assert!(
        w.db.statement_debt_sources(&principal, statement)
            .await
            .unwrap()
            .verified
    );
    // Official balanced append, but no supported business source: no invented
    // allocation may treat this receivable movement as a payment of the bill.
    let mut tx = w.db.pool().begin().await.unwrap();
    w.wallet
        .plan_journal_dispute_recharge_in(
            &mut tx,
            &w.journal,
            &Uuid::new_v4(),
            1,
            1,
            0,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let result =
        w.db.statement_debt_sources(&principal, statement)
            .await
            .unwrap();
    assert!(!result.verified);
    assert_eq!(result.outstanding_minor, FEE);
    assert_eq!(result.reason, Some("sourceReviewRequired"));
    assert!(result.slices.is_empty() && result.revision.is_none());
}

#[tokio::test]
async fn debt_sources_refuse_duplicate_document_chargeback_and_keep_visible_debt() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    let dispute = lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    // Normal append-only document insertion followed by its guarded header
    // update; no guard is disabled and only one cash/AR source exists.
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'dispute_chargeback',$3,'duplicate source fixture',$4)")
        .bind(Uuid::new_v4()).bind(statement).bind(FEE).bind(dispute).execute(&mut *tx).await.unwrap();
    sqlx::query(
        "UPDATE oxsum.plan_statements SET total_minor=total_minor+$2 WHERE statement_id=$1",
    )
    .bind(statement)
    .bind(FEE)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let result =
        w.db.statement_debt_sources(&debt_principal(&w).await, statement)
            .await
            .unwrap();
    assert!(!result.verified);
    assert_eq!(result.outstanding_minor, 2 * FEE);
    assert!(result.slices.is_empty());
}

#[tokio::test]
async fn debt_sources_cancelled_plan_debt_is_still_exact_and_readable() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(40, true).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    w.db.cancel_customer_plan(w.org).await.unwrap();
    w.db.rollover_due_subscriptions().await.unwrap();
    let principal = debt_principal(&w).await;
    let result =
        w.db.statement_debt_sources(&principal, statement)
            .await
            .unwrap();
    assert!(result.verified, "{result:?}");
    assert_eq!(result.outstanding_minor, FEE);
}

#[tokio::test]
async fn debt_sources_do_not_collect_balanced_historical_duplicate_usage() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world_with_history(40, true, FEE, Some(700)).await else {
        return;
    };
    let statement = unpaid_statement(&w).await;
    clean(&capture(&w, TimeBasis::Booking).await);
    let log = w.wallet.log_size().await.unwrap();
    let result =
        w.db.statement_debt_sources(&debt_principal(&w).await, statement)
            .await
            .unwrap();
    assert!(!result.verified);
    assert_eq!(result.reason, Some("historicalUsageReviewRequired"));
    assert_eq!(result.outstanding_minor, 700);
    assert!(result.slices.is_empty() && result.revision.is_none());
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
}

#[tokio::test]
async fn debt_sources_refuse_oversized_history_without_truncating_it() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    // Zero-value immutable lines leave displayed debt unchanged but exceed the
    // shared capture bound. The reader must fail, never prove a truncated set.
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description) SELECT gen_random_uuid(),$1,'discount',0,'source bound fixture' FROM generate_series(1,10001)")
        .bind(statement).execute(w.db.pool()).await.unwrap();
    let log = w.wallet.log_size().await.unwrap();
    let result =
        w.db.statement_debt_sources(&debt_principal(&w).await, statement)
            .await;
    assert!(
        matches!(result, Err(WalletError::InvalidInput(message)) if message.contains("source limit"))
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
    assert_eq!(
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap()
            .statement
            .total_minor,
        FEE
    );
}

async fn statement_order(w: &World, statement: Uuid, amount: Option<i64>) -> crate::PaymentOrder {
    let quote =
        w.db.statement_payment_quote(&debt_principal(w).await, statement, amount)
            .await
            .unwrap();
    let terms = quote
        .terms
        .unwrap_or_else(|| panic!("quote refused: {:?}", quote.reason));
    w.db.create_payment_order(
        &w.context,
        w.org,
        &statement_request(terms, quote.fingerprint),
    )
    .await
    .unwrap()
}
fn statement_request(
    terms: crate::StatementPaymentTerms,
    fingerprint: Option<String>,
) -> crate::NewPaymentOrder {
    crate::NewPaymentOrder {
        expected_statement_quote: fingerprint,
        statement_amount_minor: Some(terms.amount_minor),
        expected_plan_quote: None,
        expected_collection_revision: Some(terms.collection_revision),
        purpose: crate::PaymentOrderPurpose::StatementRepayment,
        display_amount_minor: terms.display_amount_minor,
        display_currency: terms.display_currency,
        purpose_ref: Some(terms.statement_id),
        idempotency_key: Uuid::new_v4().to_string(),
        promo_minor: 0,
    }
}

#[tokio::test]
async fn statement_repayment_partial_full_and_chargeback_keep_exact_sources() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [true, false] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        lost(&w, &w.order).await;
        let statement = unpaid_statement(&w).await;
        let first = statement_order(&w, statement, Some(1_000_000)).await;
        let held =
            w.db.statement_payment_quote(&debt_principal(&w).await, statement, None)
                .await
                .unwrap();
        assert_eq!(held.reserved_minor, 1_000_000);
        assert_eq!(held.available_minor, FEE - 1_000_000);
        payment::mark_paid(&w.db, &w.context, &first).await;
        payment::fulfill(&w.db, &first).await;
        clean(&capture(&w, TimeBasis::Booking).await);
        let partial =
            w.db.statement_debt_sources(&debt_principal(&w).await, statement)
                .await
                .unwrap();
        assert!(partial.verified, "{partial:?}");
        assert_eq!(partial.outstanding_minor, FEE - 1_000_000);
        let second = statement_order(&w, statement, None).await;
        payment::mark_paid(&w.db, &w.context, &second).await;
        payment::fulfill(&w.db, &second).await;
        let log = w.wallet.log_size().await.unwrap();
        payment::fulfill(&w.db, &second).await;
        assert_eq!(log, w.wallet.log_size().await.unwrap());
        clean(&capture(&w, TimeBasis::Booking).await);
        assert_eq!(
            w.db.plan_statement(w.org, statement)
                .await
                .unwrap()
                .unwrap()
                .statement
                .state,
            crate::PlanStatementState::Paid
        );
        lost(&w, &second).await;
        clean(&capture(&w, TimeBasis::Booking).await);
        let reopened =
            w.db.statement_debt_sources(&debt_principal(&w).await, statement)
                .await
                .unwrap();
        assert!(reopened.verified, "{reopened:?}");
        assert_eq!(reopened.outstanding_minor, second.amount_minor);
        let third = statement_order(&w, statement, None).await;
        payment::mark_paid(&w.db, &w.context, &third).await;
        payment::fulfill(&w.db, &third).await;
        clean(&capture(&w, TimeBasis::Booking).await);
    }
}

#[tokio::test]
async fn statement_repayment_refund_restores_only_its_original_debt() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [true, false] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        lost(&w, &w.order).await;
        let statement = unpaid_statement(&w).await;
        let order = statement_order(&w, statement, None).await;
        payment::mark_paid(&w.db, &w.context, &order).await;
        payment::fulfill(&w.db, &order).await;
        let before_disputes: i64 = sqlx::query_scalar(
            "SELECT disputed_minor FROM oxsum.invoice_journals WHERE journal_id=$1",
        )
        .bind(w.journal)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        refund_order_on(&w, &order, 1_000_000, OffsetDateTime::now_utc().date()).await;
        clean(&capture(&w, TimeBasis::Booking).await);
        let result =
            w.db.statement_debt_sources(&debt_principal(&w).await, statement)
                .await
                .unwrap();
        assert!(result.verified, "{result:?}");
        assert_eq!(result.outstanding_minor, 1_000_000);
        let disputes: i64 = sqlx::query_scalar(
            "SELECT disputed_minor FROM oxsum.invoice_journals WHERE journal_id=$1",
        )
        .bind(w.journal)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        assert_eq!(
            disputes, before_disputes,
            "voluntary refund must not fabricate a dispute counter"
        );
        let next = statement_order(&w, statement, None).await;
        payment::mark_paid(&w.db, &w.context, &next).await;
        payment::fulfill(&w.db, &next).await;
        clean(&capture(&w, TimeBasis::Booking).await);
    }
}

#[tokio::test]
async fn statement_repayment_reservations_replay_and_late_cash_are_explicit() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let order = statement_order(&w, statement, None).await;
    let quote =
        w.db.statement_payment_quote(&debt_principal(&w).await, statement, None)
            .await
            .unwrap();
    assert_eq!(quote.available_minor, 0);
    assert_eq!(quote.pending_order_ids, vec![order.order_id]);
    let event = crate::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: crate::MerchantEventKind::Failed,
        payment_id: Some(order.order_id),
        provider_payment_id: Some(format!("pay_{}", order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(crate::MerchantMode::Test),
        amount_minor: Some(order.display_amount_minor),
        currency: Some(order.display_currency.clone()),
        refund_id: None,
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[75; 32])
            .await
            .unwrap();
    w.db.apply_order_event(&receipt).await.unwrap();
    let replacement = statement_order(&w, statement, None).await;
    payment::mark_paid(&w.db, &w.context, &replacement).await;
    payment::fulfill(&w.db, &replacement).await;
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    let fulfillment =
        w.db.payment_fulfillment(order.order_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(fulfillment.purpose_payload["repaidMinor"], 0);
    assert_eq!(fulfillment.purpose_payload["payableMinor"], FEE);
    assert_eq!(
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap()
            .statement
            .paid_minor,
        FEE
    );
    clean(&capture(&w, TimeBasis::Booking).await);
}

#[tokio::test]
async fn statement_repayment_concurrent_quotes_and_void_fence_keep_one_intent() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let quote =
        w.db.statement_payment_quote(&debt_principal(&w).await, statement, None)
            .await
            .unwrap();
    let request = statement_request(quote.terms.unwrap(), quote.fingerprint);
    let mut competing = request.clone();
    competing.idempotency_key = Uuid::new_v4().to_string();
    let (first, second) = tokio::join!(
        w.db.create_payment_order(&w.context, w.org, &request),
        w.db.create_payment_order(&w.context, w.org, &competing)
    );
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let (order, request) = if let Ok(order) = first {
        (order, request)
    } else {
        (second.unwrap(), competing)
    };
    assert_eq!(
        w.db.create_payment_order(&w.context, w.org, &request)
            .await
            .unwrap()
            .order_id,
        order.order_id
    );
    let mut wrong = request.clone();
    wrong.statement_amount_minor = Some(1);
    assert!(matches!(
        w.db.create_payment_order(&w.context, w.org, &wrong).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        matches!(w.db.void_invoice_journal(&w.owner,&w.wallet,w.journal,"live reservation must prevent void",&Uuid::new_v4().to_string(),OffsetDateTime::now_utc().date()).await,Err(WalletError::Conflict(message)) if message.contains("live statement payment"))
    );
    let old_terms = w.db.collection_terms().await.unwrap();
    w.db.set_collection_terms(
        &w.owner,
        &crate::CollectionTermsRequest {
            reason: "changed future rate".into(),
            idempotency_key: Uuid::new_v4().to_string(),
            expected_revision: old_terms.revision,
            currency: crate::CollectionCurrency {
                code: "USD".into(),
                rate_per_credit: "2".into(),
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        w.db.create_payment_order(&w.context, w.org, &request)
            .await
            .unwrap()
            .display_amount_minor,
        order.display_amount_minor
    );
    w.db.set_collection_terms(
        &w.owner,
        &crate::CollectionTermsRequest {
            reason: "restore fixture rate".into(),
            idempotency_key: Uuid::new_v4().to_string(),
            expected_revision: old_terms.revision + 1,
            currency: old_terms.currency.unwrap(),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn statement_repayment_effect_failure_rolls_back_cash_documents_and_outbox() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let order = statement_order(&w, statement, None).await;
    payment::mark_paid(&w.db, &w.context, &order).await;
    let size = w.wallet.log_size().await.unwrap();
    // Task-owned rejecting trigger exercises a real failure after cash and
    // documents but before fulfillment commit. No integrity guard is disabled.
    sqlx::raw_sql(&format!("CREATE FUNCTION oxsum.reject_statement_test() RETURNS trigger LANGUAGE plpgsql AS $f$ BEGIN IF NEW.order_id='{}'::uuid THEN RAISE EXCEPTION 'statement test fault'; END IF; RETURN NEW; END $f$; CREATE TRIGGER statement_test_fault BEFORE INSERT ON oxsum.payment_fulfillments FOR EACH ROW EXECUTE FUNCTION oxsum.reject_statement_test()",order.order_id)).execute(w.db.pool()).await.unwrap();
    let result =
        w.db.fulfill_payment_order(order.order_id, &w.wallet, OffsetDateTime::now_utc().date())
            .await;
    sqlx::raw_sql("DROP TRIGGER statement_test_fault ON oxsum.payment_fulfillments; DROP FUNCTION oxsum.reject_statement_test()").execute(w.db.pool()).await.unwrap();
    assert!(result.is_err());
    assert_eq!(w.wallet.log_size().await.unwrap(), size);
    assert_eq!(
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap()
            .statement
            .paid_minor,
        0
    );
    let n:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM oxsum.statement_payment_effects WHERE order_id=$1)+(SELECT count(*) FROM oxsum.payment_outbox WHERE order_id=$1)").bind(order.order_id).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(n, 0);
    payment::fulfill(&w.db, &order).await;
    clean(&capture(&w, TimeBasis::Booking).await);
}

#[tokio::test]
async fn statement_repayment_does_not_replenish_an_old_orders_return_source() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [true, false] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        lost(&w, &w.order).await;
        let statement = unpaid_statement(&w).await;
        let order = statement_order(&w, statement, None).await;
        payment::mark_paid(&w.db, &w.context, &order).await;
        payment::fulfill(&w.db, &order).await;
        lost(&w, &w.order).await;
        assert_eq!(
            w.db.plan_statement(w.org, statement)
                .await
                .unwrap()
                .unwrap()
                .statement
                .state,
            crate::PlanStatementState::Paid
        );
        assert_eq!(
            w.db.plan_journal(w.org, w.journal)
                .await
                .unwrap()
                .unwrap()
                .journal
                .settled_minor,
            FEE
        );
        clean(&capture(&w, TimeBasis::Booking).await);
    }
}

#[tokio::test]
async fn statement_repayment_late_refund_after_loss_retains_cash_without_double_debt() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let order = statement_order(&w, statement, None).await;
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &crate::NewRefundRequest {
                amount_minor: Some(1_000_000),
                reason: "return reserved before chargeback".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    w.db.resolve_order_refund(
        &w.owner,
        refund.refund_id,
        crate::RefundDecision::Approve,
        None,
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    lost(&w, &order).await;
    let event = crate::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: crate::MerchantEventKind::RefundSucceeded,
        payment_id: Some(order.order_id),
        provider_payment_id: Some(format!("pay_{}", order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(crate::MerchantMode::Test),
        amount_minor: Some(refund.display_amount_minor),
        currency: Some(refund.display_currency),
        refund_id: Some(refund.refund_id.to_string()),
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[76; 32])
            .await
            .unwrap();
    let on = OffsetDateTime::now_utc().date();
    w.db.apply_refund_event(&w.wallet, &receipt, on)
        .await
        .unwrap();
    let log = w.wallet.log_size().await.unwrap();
    w.db.apply_refund_event(&w.wallet, &receipt, on)
        .await
        .unwrap();
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
    let effect=sqlx::query("SELECT cash_minor,receivable_minor,payable_minor,suspense_minor FROM oxsum.statement_payment_effects WHERE source_ref=$1 AND kind='refund'").bind(refund.refund_id).fetch_one(w.db.pool()).await.unwrap();
    use sqlx::Row;
    assert_eq!(effect.get::<i64, _>("cash_minor"), 1_000_000);
    assert_eq!(effect.get::<i64, _>("receivable_minor"), 0);
    assert_eq!(effect.get::<i64, _>("payable_minor"), 0);
    assert_eq!(effect.get::<i64, _>("suspense_minor"), 1_000_000);
    assert_eq!(
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap()
            .statement
            .paid_minor,
        0
    );
    clean(&capture(&w, TimeBasis::Booking).await);
}

#[tokio::test]
async fn statement_repayment_void_refund_owns_only_its_original_payable() {
    let _guard = TEST_LOCK.lock().await;
    for legacy in [true, false] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        lost(&w, &w.order).await;
        let statement = unpaid_statement(&w).await;
        let order = statement_order(&w, statement, None).await;
        payment::mark_paid(&w.db, &w.context, &order).await;
        payment::fulfill(&w.db, &order).await;
        w.db.void_invoice_journal(
            &w.owner,
            &w.wallet,
            w.journal,
            "void original service after debt repayment",
            &Uuid::new_v4().to_string(),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
        clean(&capture(&w, TimeBasis::Booking).await);
        assert_eq!(
            w.db.plan_statement(w.org, statement)
                .await
                .unwrap()
                .unwrap()
                .statement
                .state,
            crate::PlanStatementState::Void
        );
        // A second loss of the original payment cannot borrow repayment cash.
        lost(&w, &w.order).await;
        refund_order_on(&w, &order, 1_000_000, OffsetDateTime::now_utc().date()).await;
        let amounts:(i64,i64,i64)=sqlx::query_as("SELECT SUM(receivable_minor)::bigint,SUM(payable_minor)::bigint,SUM(suspense_minor)::bigint FROM oxsum.statement_payment_effects WHERE order_id=$1 AND kind='refund'").bind(order.order_id).fetch_one(w.db.pool()).await.unwrap();
        assert_eq!(amounts, (0, 1_000_000, 0));
        lost(&w, &order).await;
        let amounts:(i64,i64,i64)=sqlx::query_as("SELECT SUM(receivable_minor)::bigint,SUM(payable_minor)::bigint,SUM(suspense_minor)::bigint FROM oxsum.statement_payment_effects WHERE order_id=$1 AND kind='dispute'").bind(order.order_id).fetch_one(w.db.pool()).await.unwrap();
        assert_eq!(amounts, (0, FEE - 1_000_000, 1_000_000));
        clean(&capture(&w, TimeBasis::Booking).await);
    }
}

#[tokio::test]
async fn statement_repayment_terminal_replay_refuses_missing_effect_without_repair() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let order = statement_order(&w, statement, None).await;
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    let dispute = lost(&w, &order).await;
    let size = w.wallet.log_size().await.unwrap();
    for _ in 0..2 {
        w.db.resolve_order_dispute(
            &w.wallet,
            &w.owner,
            dispute,
            crate::DisputeResolution::Lost,
            Some("independent accounting loss"),
            &Uuid::new_v4().to_string(),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    }
    assert_eq!(w.wallet.log_size().await.unwrap(), size);
    corrupt(&w,&format!("DELETE FROM oxsum.statement_payment_effects WHERE source_ref='{dispute}' AND kind='dispute'")).await;
    assert!(
        w.db.resolve_order_dispute(
            &w.wallet,
            &w.owner,
            dispute,
            crate::DisputeResolution::Lost,
            Some("independent accounting loss"),
            &Uuid::new_v4().to_string(),
            OffsetDateTime::now_utc().date()
        )
        .await
        .is_err()
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), size);
}

#[tokio::test]
async fn statement_repayment_provider_only_release_and_paid_callback_use_one_connection() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let order = statement_order(&w, statement, None).await;
    let provider = format!("pay_{}", order.order_id);
    w.db.note_order_provider(order.order_id, &provider, &serde_json::json!({}))
        .await
        .unwrap();
    let event = crate::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: crate::MerchantEventKind::Expired,
        payment_id: None,
        provider_payment_id: Some(provider),
        organization_id: None,
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(crate::MerchantMode::Test),
        amount_minor: Some(order.display_amount_minor),
        currency: Some(order.display_currency.clone()),
        refund_id: None,
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[81; 32])
            .await
            .unwrap();
    assert_eq!(receipt.payment_id, Some(order.order_id));
    let url = oxsum_testkit::suite_db_url("independent_reconciliation").unwrap();
    let one = Db::connect(&url, 1).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        one.apply_order_event(&receipt),
    )
    .await
    .unwrap()
    .unwrap();
    let released =
        w.db.statement_payment_quote(&debt_principal(&w).await, statement, None)
            .await
            .unwrap();
    assert_eq!(released.reserved_minor, 0);
    let event = crate::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: crate::MerchantEventKind::Succeeded,
        ..event
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[82; 32])
            .await
            .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        one.apply_order_event(&receipt),
    )
    .await
    .unwrap()
    .unwrap();
    let wallet = Wallet::open(one.pool().clone(), &w.tenant).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        one.fulfill_payment_order(order.order_id, &wallet, OffsetDateTime::now_utc().date()),
    )
    .await
    .unwrap()
    .unwrap();
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &crate::NewRefundRequest {
                amount_minor: None,
                reason: "return released late payment".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    w.db.resolve_order_refund(
        &w.owner,
        refund.refund_id,
        crate::RefundDecision::Approve,
        None,
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    let event = crate::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: crate::MerchantEventKind::RefundSucceeded,
        refund_id: None,
        ..event
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[83; 32])
            .await
            .unwrap();
    for _ in 0..2 {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            one.apply_refund_event(&wallet, &receipt, OffsetDateTime::now_utc().date()),
        )
        .await
        .unwrap()
        .unwrap();
    }
    clean(&capture(&w, TimeBasis::Booking).await);
}

#[tokio::test]
async fn statement_repayment_clears_only_debt_dunning_and_never_renews_cancelled_plans() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    sqlx::query(
        "UPDATE oxsum.plan_statements SET due_at=now()-interval '100 days' WHERE statement_id=$1",
    )
    .bind(statement)
    .execute(w.db.pool())
    .await
    .unwrap();
    w.db.run_statement_past_due(100).await.unwrap();
    let on = OffsetDateTime::now_utc().date();
    assert!(matches!(
        w.wallet
            .hold(
                &Uuid::new_v4().to_string(),
                "debt blocks wallet draw",
                1,
                on
            )
            .await,
        Err(WalletError::Forbidden(_))
    ));
    w.db.set_suspended(w.org, true).await.unwrap();
    w.db.cancel_customer_plan(w.org).await.unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM oxsum.customer_plans WHERE customer_plan_id=$1")
            .bind(w.plan)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let periods: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM oxsum.plan_periods WHERE customer_plan_id=$1")
            .bind(w.plan)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let order = statement_order(&w, statement, None).await;
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    let result:(String,bool)=sqlx::query_as("SELECT state,dunning_suspended_at IS NULL FROM oxsum.customer_plans WHERE customer_plan_id=$1").bind(w.plan).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(result, (state, true));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM oxsum.plan_periods WHERE customer_plan_id=$1"
        )
        .bind(w.plan)
        .fetch_one(w.db.pool())
        .await
        .unwrap(),
        periods
    );
    assert!(matches!(
        w.wallet
            .hold(
                &Uuid::new_v4().to_string(),
                "manual restriction remains",
                1,
                on
            )
            .await,
        Err(WalletError::Forbidden(_))
    ));
    w.db.set_suspended(w.org, false).await.unwrap();
    w.wallet
        .top_up(&Uuid::new_v4().to_string(), 1, on)
        .await
        .unwrap();
    w.wallet
        .hold(
            &Uuid::new_v4().to_string(),
            "debt cleared and manual restriction removed",
            1,
            on,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn statement_repayment_status_without_authenticated_cash_never_settles_debt() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    lost(&w, &w.order).await;
    let statement = unpaid_statement(&w).await;
    let order = statement_order(&w, statement, None).await;
    sqlx::query("UPDATE oxsum.payment_orders SET status='paid',paid_at=now() WHERE order_id=$1")
        .bind(order.order_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let size = w.wallet.log_size().await.unwrap();
    let result =
        w.db.fulfill_payment_order(order.order_id, &w.wallet, OffsetDateTime::now_utc().date())
            .await;
    assert!(
        matches!(result,Err(WalletError::Conflict(message)) if message.contains("verified merchant receipt"))
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), size);
    assert_eq!(
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap()
            .statement
            .paid_minor,
        0
    );
    payment::mark_paid(&w.db, &w.context, &order).await;
    payment::fulfill(&w.db, &order).await;
    clean(&capture(&w, TimeBasis::Booking).await);
}

async fn mixed_void_world(daily_first: bool) -> Option<(World, Uuid, Uuid)> {
    let w = world(40, true).await?;
    let current = w.db.customer_plan(w.org).await.unwrap().unwrap();
    let old =
        w.db.sold_plan(current.plan_version_id)
            .await
            .unwrap()
            .unwrap();
    let version =
        w.db.publish_sold_plan(
            old.business_id,
            &NewSoldPlan {
                plan_id: Some(old.plan_id),
                name: old.name,
                price_minor: FEE,
                currency: "USD".into(),
                quota_minor: Some(10_000_000),
                period_kind: PeriodKind::Anniversary,
                overage_enabled: true,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    w.db.schedule_customer_plan_change(w.org, version.plan_version_id)
        .await
        .unwrap();
    w.db.rollover_due_subscriptions().await.unwrap();
    let daily = payment::created_order(&w.db, &w.context, w.org, w.plan).await;
    payment::mark_paid(&w.db, &w.context, &daily).await;
    w.db.activate_plan_period(daily.order_id, w.plan)
        .await
        .unwrap();
    let journal =
        w.db.plan_journals(w.org, None, 50)
            .await
            .unwrap()
            .journals
            .into_iter()
            .find(|j| j.accounting_policy_version == 2)
            .unwrap()
            .journal_id;
    let (first, second) = if daily_first {
        (&daily, &w.order)
    } else {
        (&w.order, &daily)
    };
    lost(&w, first).await;
    lost(&w, second).await;
    clean(&capture(&w, TimeBasis::Booking).await);
    let statement = unpaid_statement(&w).await;
    Some((w, journal, statement))
}

#[tokio::test]
async fn mixed_source_void_preserves_other_debt_and_exact_return_ownership() {
    let _guard = TEST_LOCK.lock().await;
    for daily_first in [false, true] {
        for paid in [0, 1_000_000, 2 * FEE] {
            let Some((w, daily, statement)) = mixed_void_world(daily_first).await else {
                return;
            };
            let target = if daily_first { daily } else { w.journal };
            let other = if daily_first { w.journal } else { daily };
            let order = if paid > 0 {
                let order = statement_order(&w, statement, Some(paid)).await;
                payment::mark_paid(&w.db, &w.context, &order).await;
                payment::fulfill(&w.db, &order).await;
                Some(order)
            } else {
                None
            };
            let before = capture(&w, TimeBasis::Booking).await;
            clean(&before);
            let cash_before = source_void_cash(&w).await;
            sqlx::query("UPDATE oxsum.plan_statements SET due_at=now()-interval '100 days',state=CASE WHEN state='issued' THEN 'past_due' ELSE state END WHERE statement_id=$1").bind(statement).execute(w.db.pool()).await.unwrap();
            let other_before =
                w.db.business_journal(&w.owner, other)
                    .await
                    .unwrap()
                    .unwrap()
                    .journal;
            let key = Uuid::new_v4().to_string();
            w.db.void_invoice_journal(
                &w.owner,
                &w.wallet,
                target,
                "source-specific correction",
                &key,
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
            let size = w.wallet.log_size().await.unwrap();
            w.db.void_invoice_journal(
                &w.owner,
                &w.wallet,
                target,
                "source-specific correction",
                &key,
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
            assert_eq!(w.wallet.log_size().await.unwrap(), size);
            let after = capture(&w, TimeBasis::Booking).await;
            clean(&after);
            let other_after =
                w.db.business_journal(&w.owner, other)
                    .await
                    .unwrap()
                    .unwrap()
                    .journal;
            assert_eq!(other_after.total_minor, other_before.total_minor);
            assert_eq!(other_after.settled_minor, other_before.settled_minor);
            assert_eq!(other_after.state, other_before.state);
            let remaining = FEE - other_before.settled_minor;
            let document =
                w.db.plan_statement(w.org, statement)
                    .await
                    .unwrap()
                    .unwrap()
                    .statement;
            assert_ne!(document.state, crate::PlanStatementState::Void);
            assert_eq!(document.total_minor - document.paid_minor, remaining);
            assert_eq!(after.actual.receivable_minor, remaining);
            assert_eq!(source_void_cash(&w).await, cash_before);
            let suspended:bool=sqlx::query_scalar("SELECT dunning_suspended_at IS NOT NULL FROM oxsum.customer_plans WHERE customer_plan_id=$1").bind(w.plan).fetch_one(w.db.pool()).await.unwrap();
            assert_eq!(suspended, remaining > 0);
            if remaining > 0 {
                let quote =
                    w.db.statement_payment_quote(&debt_principal(&w).await, statement, None)
                        .await
                        .unwrap();
                assert_eq!(
                    quote
                        .terms
                        .unwrap()
                        .allocations
                        .iter()
                        .map(|a| a.journal_id)
                        .collect::<Vec<_>>(),
                    vec![other]
                );
            }
            if let Some(order) = order {
                refund_order_on(&w, &order, paid, OffsetDateTime::now_utc().date()).await;
                clean(&capture(&w, TimeBasis::Booking).await);
                let effects=sqlx::query_as::<_,(Uuid,i64,i64,i64)>("SELECT journal_id,receivable_minor,payable_minor,suspense_minor FROM oxsum.statement_payment_effects WHERE order_id=$1 AND kind='refund'").bind(order.order_id).fetch_all(w.db.pool()).await.unwrap();
                for (journal, ar, payable, suspense) in effects {
                    assert_eq!(suspense, 0);
                    if journal == target {
                        assert_eq!(ar, 0);
                        assert!(payable > 0)
                    } else {
                        assert_eq!(payable, 0);
                        assert!(ar > 0)
                    }
                }
                lost(&w, &order).await;
                clean(&capture(&w, TimeBasis::Booking).await);
            }
            w.db.void_invoice_journal(
                &w.owner,
                &w.wallet,
                other,
                "second source correction",
                &Uuid::new_v4().to_string(),
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
            clean(&capture(&w, TimeBasis::Booking).await);
            assert_eq!(
                w.db.plan_statement(w.org, statement)
                    .await
                    .unwrap()
                    .unwrap()
                    .statement
                    .state,
                crate::PlanStatementState::Void
            );
        }
    }
}

#[tokio::test]
async fn mixed_source_void_fences_other_source_intents_and_rolls_back_all_evidence() {
    let _guard = TEST_LOCK.lock().await;
    let Some((w, daily, statement)) = mixed_void_world(false).await else {
        return;
    };
    let quoted = statement_order(&w, statement, Some(1)).await;
    let reserved = quoted.statement_payment.as_ref().unwrap().allocations[0].journal_id;
    let unreserved = if reserved == daily { w.journal } else { daily };
    let size = w.wallet.log_size().await.unwrap();
    assert!(
        matches!(w.db.void_invoice_journal(&w.owner,&w.wallet,unreserved,"other slice still reserved",&Uuid::new_v4().to_string(),OffsetDateTime::now_utc().date()).await,Err(WalletError::Conflict(message)) if message.contains("affected statement"))
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), size);
    payment::mark_paid(&w.db, &w.context, &quoted).await;
    payment::fulfill(&w.db, &quoted).await;
    let size = w.wallet.log_size().await.unwrap();
    let original =
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap()
            .statement;
    let constraint = format!("fixture_void_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("ALTER TABLE oxsum.statement_source_voids ADD CONSTRAINT {constraint} CHECK(journal_id<>'{unreserved}'::uuid) NOT VALID")).execute(w.db.pool()).await.unwrap();
    let failed =
        w.db.void_invoice_journal(
            &w.owner,
            &w.wallet,
            unreserved,
            "fault rollback",
            &Uuid::new_v4().to_string(),
            OffsetDateTime::now_utc().date(),
        )
        .await;
    sqlx::query(&format!(
        "ALTER TABLE oxsum.statement_source_voids DROP CONSTRAINT {constraint}"
    ))
    .execute(w.db.pool())
    .await
    .unwrap();
    assert!(failed.is_err());
    assert_eq!(w.wallet.log_size().await.unwrap(), size);
    let after =
        w.db.plan_statement(w.org, statement)
            .await
            .unwrap()
            .unwrap()
            .statement;
    assert_eq!(
        (after.total_minor, after.paid_minor),
        (original.total_minor, original.paid_minor)
    );
    clean(&capture(&w, TimeBasis::Booking).await);
    let bundle =
        w.db.mint_statement_evidence(&w.wallet, w.org, &w.tenant, statement, false, None)
            .await
            .unwrap();
    let retained =
        w.db.evidence_bundle_json(bundle.bundle_id)
            .await
            .unwrap()
            .unwrap();
    w.db.void_invoice_journal(
        &w.owner,
        &w.wallet,
        unreserved,
        "confirmed source correction",
        &Uuid::new_v4().to_string(),
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    assert_eq!(
        w.db.evidence_bundle_json(bundle.bundle_id)
            .await
            .unwrap()
            .unwrap(),
        retained
    );
    let yesterday = OffsetDateTime::now_utc().date() - Duration::days(1);
    let export =
        w.db.mint_period_evidence(
            &w.wallet, w.org, &w.tenant, yesterday, yesterday, false, None,
        )
        .await
        .unwrap();
    let bytes =
        w.db.evidence_bundle_json(export.bundle_id)
            .await
            .unwrap()
            .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    oxsum_verify::verify_evidence_bundle(
        text,
        &format!("tenant-{}", w.tenant),
        &w.org.to_string(),
        None,
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        value["artifacts"]["planStatement"][statement.to_string()]["sourceVoids"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
    );
    let (original,reversal,amount):(Uuid,Uuid,i64)=sqlx::query_as("SELECT v.original_line_id,v.reversal_line_id,l.amount_minor FROM oxsum.statement_source_voids v JOIN oxsum.plan_statement_lines l ON l.line_id=v.original_line_id WHERE v.journal_id=$1 AND l.kind='plan_fee' LIMIT 1").bind(unreserved).fetch_one(w.db.pool()).await.unwrap();
    corrupt(&w,&format!("UPDATE oxsum.plan_statement_lines SET amount_minor=amount_minor+1 WHERE line_id='{original}'; UPDATE oxsum.plan_statement_lines SET amount_minor=amount_minor-1 WHERE line_id='{reversal}'")).await;
    let report = capture(&w, TimeBasis::Booking).await;
    assert!(
        !report.coverage.complete,
        "equal-and-opposite tampering must still match original journal"
    );
    corrupt(&w,&format!("UPDATE oxsum.plan_statement_lines SET amount_minor={amount} WHERE line_id='{original}'; UPDATE oxsum.plan_statement_lines SET amount_minor={} WHERE line_id='{reversal}'",-amount)).await;
    clean(&capture(&w, TimeBasis::Booking).await);
    let change =
        sqlx::query("UPDATE oxsum.statement_source_voids SET entry_hash='bad' WHERE journal_id=$1")
            .bind(unreserved)
            .execute(w.db.pool())
            .await;
    assert!(change.is_err());
    corrupt(&w,&format!("UPDATE oxsum.statement_source_voids SET entry_hash='bad' WHERE journal_id='{unreserved}'")).await;
    let report = capture(&w, TimeBasis::Booking).await;
    assert!(!report.coverage.complete);
    assert!(
        report
            .differences
            .iter()
            .any(|d| d.fact == "sourceVoidProof")
    );
    let quote =
        w.db.statement_payment_quote(&debt_principal(&w).await, statement, None)
            .await
            .unwrap();
    assert!(quote.terms.is_none());
}

async fn source_void_cash(w: &World) -> i64 {
    sqlx::query_scalar(&format!("SELECT COALESCE(SUM(CASE WHEN p.direction='D' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a USING(account_index) JOIN ledger_{}.entries e USING(entry_id) WHERE a.path='Assets:Cash' AND p.layer='settled' AND e.log_index IS NOT NULL",w.tenant,w.tenant,w.tenant)).fetch_one(w.db.pool()).await.unwrap()
}

#[tokio::test]
async fn mixed_source_void_racing_a_different_slice_reservation_has_one_winner() {
    let _guard = TEST_LOCK.lock().await;
    let Some((w, daily, statement)) = mixed_void_world(false).await else {
        return;
    };
    let quote =
        w.db.statement_payment_quote(&debt_principal(&w).await, statement, Some(1))
            .await
            .unwrap();
    let source = quote.terms.as_ref().unwrap().allocations[0].journal_id;
    let other = if source == daily { w.journal } else { daily };
    let request = statement_request(quote.terms.unwrap(), quote.fingerprint);
    let key = Uuid::new_v4().to_string();
    let create = w.db.create_payment_order(&w.context, w.org, &request);
    let cancel = w.db.void_invoice_journal(
        &w.owner,
        &w.wallet,
        other,
        "racing source correction",
        &key,
        OffsetDateTime::now_utc().date(),
    );
    let (paid, voided) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(create, cancel)
    })
    .await
    .unwrap();
    assert_eq!(
        usize::from(paid.is_ok()) + usize::from(voided.is_ok()),
        1,
        "{paid:?} {voided:?}"
    );
    clean(&capture(&w, TimeBasis::Booking).await);
}

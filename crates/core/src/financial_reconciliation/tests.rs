//! Controlled financial source, drift, snapshot and concurrency acceptance.
#![allow(clippy::unwrap_used, clippy::expect_used)]

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
        price_minor: FEE,
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
    let order = payment::created_order(&db, &context, org, plan).await;
    payment::mark_paid(&db, &context, &order).await;
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    db.activate_plan_period_at(order.order_id, plan, now - Duration::days(days))
        .await
        .unwrap();
    if legacy {
        db.rollover_due_subscriptions().await.unwrap();
    } else {
        db.run_plan_recognition(1000).await.unwrap();
    }
    let period = db.plan_periods(plan).await.unwrap().remove(0);
    let journal = db.plan_journals(org, None, 50).await.unwrap().journals[0].journal_id;
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
    let refund =
        w.db.request_order_refund(
            w.org,
            w.order.order_id,
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
    refund.refund_id
}
async fn lost(w: &World, order: &crate::PaymentOrder) -> Uuid {
    let reference = format!("dispute_{}", Uuid::new_v4());
    let event = crate::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: crate::MerchantEventKind::Dispute,
        payment_id: Some(order.order_id),
        provider_payment_id: Some(format!("pay_{}", order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(crate::MerchantMode::Test),
        amount_minor: Some(order.display_amount_minor),
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
async fn corrupt_posting_hash_and_missing_effect_have_durable_exact_source_differences() {
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
        w.db.run_plan_recognition(1000).await.unwrap();
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
    }
}

#[tokio::test]
async fn current_booking_and_past_service_dates_are_separate_verified_movements() {
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

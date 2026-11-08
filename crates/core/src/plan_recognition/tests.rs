//! Real PostgreSQL and ledger acceptance for versioned daily fee accounting.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../tests/support/legacy_plan.rs"]
mod legacy_plan;
#[path = "../../tests/support/plan_payment.rs"]
mod payment;
#[path = "../../tests/support/integration.rs"]
mod setup;

use crate::{
    Db, FactFilter, JournalState, MerchantEventKind, MerchantMode, NewRefundRequest, NewSoldPlan,
    NewUser, PeriodAuthorization, PeriodKind, RefundDecision, TimeBasis, VerifiedMerchantEvent,
    Wallet,
};
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

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
async fn world(days_ago: i64, legacy: bool) -> Option<World> {
    let url = oxsum_testkit::suite_db_url("daily_recognition")?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "daily accounting fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = user.organization.id;
    let tenant = user.organization.tenant_id;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let input = NewSoldPlan {
        plan_id: None,
        name: format!("daily-{}", Uuid::new_v4()),
        price_minor: FEE,
        currency: "USD".into(),
        quota_minor: Some(10_000_000),
        period_kind: PeriodKind::Anniversary,
        overage_enabled: true,
        sort_order: 0,
    };
    let business = db.default_business().await.unwrap();
    let sold = if legacy {
        legacy_plan::publish(&db, business, &input).await.unwrap()
    } else {
        db.publish_sold_plan(business, &input).await.unwrap()
    };
    assert_eq!(sold.accounting_policy_version, if legacy { 1 } else { 2 });
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
    // A private test-only activation clock creates the original service bounds.
    // Published policy, authorized periods and accounting snapshots stay immutable.
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let start = now - Duration::days(days_ago);
    db.activate_plan_period_at(order.order_id, plan, start)
        .await
        .unwrap();
    let period = db.plan_periods(plan).await.unwrap().remove(0);
    let journal = db
        .plan_journals(org, None, 50)
        .await
        .unwrap()
        .journals
        .first()
        .map_or(Uuid::nil(), |j| j.journal_id);
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
async fn net(w: &World, path: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a USING(account_index) JOIN ledger_{}.entries e USING(entry_id) WHERE a.path=$1 AND p.layer='settled' AND e.log_index IS NOT NULL",w.tenant,w.tenant,w.tenant)).bind(path).fetch_one(w.db.pool()).await.unwrap()
}
async fn facts(w: &World) -> crate::FinancialFacts {
    w.db.financial_facts(&FactFilter {
        organization_id: Some(w.org),
        ..FactFilter::default()
    })
    .await
    .unwrap()
}
async fn recognize(w: &World, at: OffsetDateTime) -> usize {
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query(
        "SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE",
    )
    .bind(w.plan)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let n =
        w.db.recognize_plan_period_in(
            &mut tx,
            &w.wallet,
            w.journal,
            at,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    n
}
async fn expected(w: &World, fee: i64) -> i64 {
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    let end = super::schedule::completed_boundary(w.period.period_start, w.period.period_end, now);
    let elapsed = (end - w.period.period_start).whole_microseconds();
    let duration = (w.period.period_end - w.period.period_start).whole_microseconds();
    (i128::from(fee) * elapsed / duration) as i64
}
async fn refund(w: &World, amount: i64) -> crate::OrderRefund {
    let r =
        w.db.request_order_refund(
            w.org,
            w.order.order_id,
            &NewRefundRequest {
                amount_minor: Some(amount),
                reason: "daily fee refund fixture".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    let r =
        w.db.resolve_order_refund(
            &w.owner,
            r.refund_id,
            RefundDecision::Approve,
            None,
            &Uuid::new_v4().to_string(),
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap()
        .0;
    let provider = format!("refund_{}", r.refund_id);
    w.db.note_refund_provider(r.refund_id, &provider)
        .await
        .unwrap();
    let receipt =
        w.db.receive_merchant_event(
            &w.context,
            &VerifiedMerchantEvent {
                event_id: Uuid::new_v4().to_string(),
                kind: MerchantEventKind::RefundSucceeded,
                payment_id: Some(w.order.order_id),
                provider_payment_id: Some(format!("pay_{}", w.order.order_id)),
                organization_id: Some(w.org),
                provider_account_id: Some(w.context.account_id.clone()),
                mode: Some(MerchantMode::Test),
                amount_minor: Some(r.display_amount_minor),
                currency: Some(r.display_currency.clone()),
                refund_id: Some(provider),
            },
            &[83; 32],
        )
        .await
        .unwrap();
    assert_eq!(receipt.state, "accepted");
    for _ in 0..2 {
        w.db.apply_refund_event(&w.wallet, &receipt, OffsetDateTime::now_utc().date())
            .await
            .unwrap();
    }
    r
}
async fn assert_books(w: &World, billed: i64, earned: i64, collected: i64, returned: i64) {
    let f = facts(w).await;
    assert_eq!(
        (
            f.billed_minor,
            f.earned_minor,
            f.deferred_minor,
            f.collected_minor,
            f.refund_minor,
            f.discount_minor,
            f.receivable_minor
        ),
        (
            billed,
            earned,
            billed - earned,
            collected,
            returned,
            0,
            billed - collected
        )
    );
    assert_eq!(
        net(w, "Income:PlanFees").await + net(w, "Income:Discounts").await,
        earned
    );
    assert_eq!(
        net(w, "Liabilities:UnearnedPlanFees").await,
        billed - earned
    );
    assert_eq!(net(w, "Assets:Receivables").await, -(billed - collected));
}

#[tokio::test]
async fn paid_activation_invoices_without_recognizing_and_pins_inputs() {
    let Some(w) = world(0, false).await else {
        return;
    };
    assert_books(&w, FEE, 0, FEE, 0).await;
    assert_eq!(net(&w, "Assets:Cash").await, -FEE);
    assert_eq!(w.period.accounting_policy_version, 2);
    let j =
        w.db.plan_journal(w.org, w.journal)
            .await
            .unwrap()
            .unwrap()
            .journal;
    assert_eq!(
        (
            j.accounting_policy_version,
            j.total_minor,
            j.settled_minor,
            j.state
        ),
        (2, FEE, FEE, JournalState::Paid)
    );
    let statements =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements;
    assert_eq!(statements.len(), 1);
    assert_eq!(
        (statements[0].total_minor, statements[0].paid_minor),
        (FEE, FEE)
    );
    w.db.activate_plan_period(w.order.order_id, w.plan)
        .await
        .unwrap();
    assert_eq!(
        w.db.plan_journals(w.org, None, 50)
            .await
            .unwrap()
            .journals
            .len(),
        1
    );
    for sql in [
        "UPDATE oxsum.plan_periods SET period_end=period_end+interval '1 day' WHERE plan_period_id=$1",
        "DELETE FROM oxsum.plan_periods WHERE plan_period_id=$1",
        "UPDATE oxsum.plan_accounting_periods SET fee_minor=fee_minor+1 WHERE plan_period_id=$1",
        "UPDATE oxsum.plan_accounting_effects SET earned_minor=earned_minor WHERE plan_period_id=$1",
    ] {
        assert!(
            sqlx::query(sql)
                .bind(w.period.plan_period_id)
                .execute(w.db.pool())
                .await
                .is_err()
        );
    }
    let version: Uuid = sqlx::query_scalar(
        "SELECT plan_version_id FROM oxsum.customer_plans WHERE customer_plan_id=$1",
    )
    .bind(w.plan)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert!(
        sqlx::query(
            "UPDATE oxsum.sold_plans SET accounting_policy_version=1 WHERE plan_version_id=$1"
        )
        .bind(version)
        .execute(w.db.pool())
        .await
        .is_err()
    );
}

#[tokio::test]
async fn catchup_is_once_per_day_and_final_close_does_not_bill_twice() {
    let Some(w) = world(40, false).await else {
        return;
    };
    assert!(w.db.run_plan_recognition(200).await.unwrap() > 0);
    assert_books(&w, FEE, FEE, FEE, 0).await;
    assert_eq!(w.db.run_plan_recognition(200).await.unwrap(), 0);
    w.db.rollover_due_subscriptions().await.unwrap();
    w.db.rollover_due_subscriptions().await.unwrap();
    assert_eq!(
        w.db.plan_journals(w.org, None, 50)
            .await
            .unwrap()
            .journals
            .len(),
        1
    );
    let n:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 AND kind='recognition'").bind(w.period.plan_period_id).fetch_one(w.db.pool()).await.unwrap();
    assert!((28..=32).contains(&n));
    assert_books(&w, FEE, FEE, FEE, 0).await;
}

#[tokio::test]
async fn refunds_before_during_and_after_service_reduce_net_consideration_once() {
    for days in [0, 10, 40] {
        for amount in [1_000_000, FEE] {
            let Some(w) = world(days, false).await else {
                return;
            };
            let r = refund(&w, amount).await;
            let earned = expected(&w, FEE - amount).await;
            assert_books(&w, FEE - amount, earned, FEE - amount, amount).await;
            assert_eq!(net(&w, "Assets:Cash").await, -(FEE - amount));
            let a =
                w.db.recover_plan_refund(
                    &w.owner,
                    &w.wallet,
                    r.refund_id,
                    false,
                    OffsetDateTime::now_utc().date(),
                )
                .await
                .unwrap();
            assert_eq!(a.deferred_minor + a.earned_minor, amount);
            assert_eq!(a.payable_minor + a.unallocated_minor, 0);
            assert_eq!(
                w.db.plan_periods(w.plan).await.unwrap()[0].quota_minor,
                w.period.quota_minor
            );
            recognize(&w, w.period.period_end).await;
            assert_books(&w, FEE - amount, FEE - amount, FEE - amount, amount).await;
        }
    }
}

#[tokio::test]
async fn financial_void_reverses_daily_and_deferred_to_payable_without_cash() {
    for days in [0, 10, 40] {
        let Some(w) = world(days, false).await else {
            return;
        };
        w.db.void_invoice_journal(
            &w.owner,
            &w.wallet,
            w.journal,
            "daily cancellation",
            &Uuid::new_v4().to_string(),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
        assert_books(&w, 0, 0, 0, 0).await;
        assert_eq!(net(&w, "Assets:Cash").await, -FEE);
        assert_eq!(net(&w, "Liabilities:RefundPayable").await, FEE);
        let r = refund(&w, 1_000_000).await;
        let a =
            w.db.recover_plan_refund(
                &w.owner,
                &w.wallet,
                r.refund_id,
                false,
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
        assert_eq!(a.payable_minor, 1_000_000);
        assert_eq!(net(&w, "Liabilities:RefundPayable").await, FEE - 1_000_000);
        assert_eq!(net(&w, "Assets:Cash").await, -(FEE - 1_000_000));
        recognize(&w, w.period.period_end).await;
        if days == 40 {
            w.db.rollover_due_subscriptions().await.unwrap();
        }
        assert_eq!(
            w.db.plan_journals(w.org, None, 50)
                .await
                .unwrap()
                .journals
                .len(),
            1
        );
        assert_books(&w, 0, 0, 0, 1_000_000).await;
    }
}

#[tokio::test]
async fn lost_dispute_keeps_service_revenue_and_restores_original_receivable() {
    for days in [0, 10, 40] {
        let Some(w) = world(days, false).await else {
            return;
        };
        w.db.run_plan_recognition(200).await.unwrap();
        let earned = expected(&w, FEE).await;
        let dispute = Uuid::new_v4();
        for _ in 0..2 {
            w.db.recharge_journal_dispute(
                &w.wallet,
                w.order.order_id,
                dispute,
                1_000_000,
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
        }
        assert_books(&w, FEE, earned, FEE - 1_000_000, 0).await;
        assert_eq!(net(&w, "Assets:Cash").await, -(FEE - 1_000_000));
        assert_eq!(
            w.db.plan_periods(w.plan).await.unwrap()[0].quota_minor,
            w.period.quota_minor
        );
    }
}

#[tokio::test]
async fn concurrent_catchup_has_one_ledger_entry_per_completed_day() {
    let Some(w) = world(10, false).await else {
        return;
    };
    let now = OffsetDateTime::now_utc();
    let (a, b) = tokio::join!(recognize(&w, now), recognize(&w, now));
    assert!(a + b >= 9);
    assert!(a == 0 || b == 0);
    assert_books(&w, FEE, expected(&w, FEE).await, FEE, 0).await;
    let rows=sqlx::query("SELECT count(*) AS n,count(DISTINCT service_day) AS days,count(DISTINCT entry_id) AS entries FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 AND kind='recognition'").bind(w.period.plan_period_id).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(rows.get::<i64, _>("n"), rows.get::<i64, _>("days"));
    assert_eq!(rows.get::<i64, _>("n"), rows.get::<i64, _>("entries"));
}

#[tokio::test]
async fn failed_effect_insert_rolls_back_the_ledger_and_retries_cleanly() {
    let Some(w) = world(10, false).await else {
        return;
    };
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query(
        "SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE",
    )
    .bind(w.plan)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let n =
        w.db.recognize_plan_period_in(
            &mut tx,
            &w.wallet,
            w.journal,
            OffsetDateTime::now_utc(),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert!(n > 0);
    // A later transactional failure must leave neither daily effects nor entries.
    assert!(sqlx::query("INSERT INTO oxsum.plan_accounting_effects SELECT * FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 LIMIT 1").bind(w.period.plan_period_id).execute(&mut *tx).await.is_err());
    tx.rollback().await.unwrap();
    assert_books(&w, FEE, 0, FEE, 0).await;
    recognize(&w, OffsetDateTime::now_utc()).await;
    assert_books(&w, FEE, expected(&w, FEE).await, FEE, 0).await;
}

#[tokio::test]
async fn legacy_refund_cash_report_excludes_its_matching_journal_credit() {
    let Some(w) = world(40, true).await else {
        return;
    };
    assert_eq!(w.period.accounting_policy_version, 1);
    w.db.rollover_due_subscriptions().await.unwrap();
    refund(&w, 1_000_000).await;
    let f = facts(&w).await;
    assert_eq!(
        f.refund_minor, 1_000_000,
        "one external cash return has one report amount"
    );
    assert_eq!(
        f.discount_minor, 0,
        "cash-refund journal correction is not a noncash discount"
    );
    assert_eq!(
        (f.billed_minor, f.earned_minor, f.deferred_minor),
        (FEE - 1_000_000, FEE - 1_000_000, 0)
    );
}

#[tokio::test]
async fn reporting_clocks_keep_service_recognition_separate_from_booking() {
    let Some(w) = world(10, false).await else {
        return;
    };
    recognize(&w, OffsetDateTime::now_utc()).await;
    let first = w.period.period_start.date();
    let service =
        w.db.financial_facts(&FactFilter {
            organization_id: Some(w.org),
            from: Some(first),
            to: Some(first),
            time_basis: TimeBasis::Service,
            ..FactFilter::default()
        })
        .await
        .unwrap();
    let booking =
        w.db.financial_facts(&FactFilter {
            organization_id: Some(w.org),
            from: Some(first),
            to: Some(first),
            ..FactFilter::default()
        })
        .await
        .unwrap();
    assert!(service.earned_minor > 0);
    assert_eq!(booking.earned_minor, 0);
    assert_eq!(service.billed_minor, booking.billed_minor);
    assert_eq!(
        service.billed_minor,
        service.earned_minor + service.deferred_minor
    );
    let payment =
        w.db.financial_facts(&FactFilter {
            organization_id: Some(w.org),
            from: Some(OffsetDateTime::now_utc().date()),
            to: Some(OffsetDateTime::now_utc().date()),
            time_basis: TimeBasis::Payment,
            ..FactFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(payment.earned_minor, expected(&w, FEE).await);
}

#[tokio::test]
async fn evidence_pins_daily_sources_and_keeps_previously_minted_bytes() {
    let Some(w) = world(10, false).await else {
        return;
    };
    recognize(&w, OffsetDateTime::now_utc()).await;
    let statement =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .remove(0);
    let bundle =
        w.db.mint_statement_evidence(
            &w.wallet,
            w.org,
            &w.tenant,
            statement.statement_id,
            false,
            None,
        )
        .await
        .unwrap();
    let bytes =
        w.db.evidence_bundle_json(bundle.bundle_id)
            .await
            .unwrap()
            .unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    oxsum_verify::verify_evidence_bundle(
        &text,
        &format!("tenant-{}", w.tenant),
        &w.org.to_string(),
        None,
    )
    .unwrap();
    let mut wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let artifact = &wire["artifacts"]["planStatement"][statement.statement_id.to_string()];
    assert_eq!(artifact["accounting"]["policyVersion"], 2);
    assert_eq!(artifact["accounting"]["period"]["fee_minor"], FEE);
    let effects = artifact["accounting"]["effects"].as_array().unwrap();
    assert!(effects.len() > 1);
    for effect in effects {
        if let Some(id) = effect["entry_id"].as_str() {
            assert!(text.contains(id));
        }
    }
    wire["artifacts"]["planStatement"][statement.statement_id.to_string()]["accounting"]["period"]
        ["fee_minor"] = serde_json::json!(1);
    assert!(
        oxsum_verify::verify_evidence_bundle(
            &wire.to_string(),
            &format!("tenant-{}", w.tenant),
            &w.org.to_string(),
            None
        )
        .is_err()
    );
    refund(&w, 1_000_000).await;
    let again =
        w.db.mint_statement_evidence(
            &w.wallet,
            w.org,
            &w.tenant,
            statement.statement_id,
            false,
            None,
        )
        .await
        .unwrap();
    assert_eq!(again.bundle_id, bundle.bundle_id);
    assert_eq!(
        w.db.evidence_bundle_json(bundle.bundle_id)
            .await
            .unwrap()
            .unwrap(),
        bytes
    );
}

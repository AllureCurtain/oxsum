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
    refund_with_failure(w, amount, false).await
}
async fn refund_with_failure(w: &World, amount: i64, fail_first: bool) -> crate::OrderRefund {
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
        )
        .await
        .unwrap()
        .0;
    let provider = format!("refund_{}", r.refund_id);
    w.db.note_refund_provider(r.refund_id, &provider)
        .await
        .unwrap();
    if fail_first {
        let receipt =
            w.db.receive_merchant_event(
                &w.context,
                &VerifiedMerchantEvent {
                    event_id: Uuid::new_v4().to_string(),
                    kind: MerchantEventKind::RefundFailed,
                    payment_id: Some(w.order.order_id),
                    provider_payment_id: Some(format!("pay_{}", w.order.order_id)),
                    organization_id: Some(w.org),
                    provider_account_id: Some(w.context.account_id.clone()),
                    mode: Some(MerchantMode::Test),
                    amount_minor: Some(r.display_amount_minor),
                    currency: Some(r.display_currency.clone()),
                    refund_id: Some(provider.clone()),
                },
                &[85; 32],
            )
            .await
            .unwrap();
        w.db.apply_refund_event(&w.wallet, &receipt, OffsetDateTime::now_utc().date())
            .await
            .unwrap();
        assert_eq!(
            net(w, "Assets:Cash").await,
            -FEE,
            "failure alone returns no cash"
        );
    }
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(40, true).await else {
        return;
    };
    assert_eq!(w.period.accounting_policy_version, 1);
    w.db.rollover_due_subscriptions().await.unwrap();
    refund(&w, 1_000_000).await;
    // Reproduce the old report's source formula on this controlled history:
    // succeeded refund cash plus its negative journal credit counts twice.
    let old:i64=sqlx::query_scalar("SELECT (SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.refunds WHERE organization_id=$1 AND state='succeeded')-(SELECT COALESCE(SUM(l.amount_minor),0)::bigint FROM oxsum.invoice_journal_lines l JOIN oxsum.invoice_journals j USING(journal_id) WHERE j.organization_id=$1 AND l.kind IN ('discount','promo_credit'))")
        .bind(w.org).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(old, 2_000_000);
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
    let _guard = TEST_LOCK.lock().await;
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
    let _guard = TEST_LOCK.lock().await;
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
    let exact=wire["artifacts"]["planStatement"][statement.statement_id.to_string()]["accounting"]["effects"]
        .as_array().unwrap().iter().find(|e|e["kind"]=="recognition" && e["entry_id"].is_string()).unwrap()["entry_id"].as_str().unwrap().to_owned();
    let mut missing = wire.clone();
    missing["entries"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["entry"]["id"] != exact);
    assert!(
        oxsum_verify::verify_evidence_bundle(
            &missing.to_string(),
            &format!("tenant-{}", w.tenant),
            &w.org.to_string(),
            None
        )
        .is_err()
    );
    let artifact = &wire["artifacts"]["planStatement"][statement.statement_id.to_string()];
    assert_eq!(artifact["accounting"]["policyVersion"], 2);
    assert!(artifact["accounting"]["capturedAt"].is_string());
    assert_eq!(artifact["accounting"]["period"]["fee_minor"], FEE);
    let effects = artifact["accounting"]["effects"].as_array().unwrap();
    assert!(effects.len() > 1);
    for effect in effects {
        if let Some(id) = effect["entry_id"].as_str() {
            assert!(
                wire["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["entry"]["id"] == id)
            );
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

#[tokio::test]
async fn ordinary_legacy_renewal_retains_v1_until_explicit_new_version_activation() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(40, true).await else {
        return;
    };
    let next = payment::created_order(&w.db, &w.context, w.org, w.plan).await;
    payment::mark_paid(&w.db, &w.context, &next).await;
    w.db.activate_plan_period(next.order_id, w.plan)
        .await
        .unwrap();
    let periods = w.db.plan_periods(w.plan).await.unwrap();
    assert_eq!(periods.len(), 2);
    assert!(periods.iter().all(|p| p.accounting_policy_version == 1));
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.plan_accounting_periods WHERE customer_plan_id=$1",
    )
    .bind(w.plan)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    // Explicit new-version selection is the only policy transition.
    let Some(w) = world(40, true).await else {
        return;
    };
    let current = w.db.customer_plan(w.org).await.unwrap().unwrap();
    let old =
        w.db.sold_plan(current.plan_version_id)
            .await
            .unwrap()
            .unwrap();
    let v2 =
        w.db.publish_sold_plan(
            old.business_id,
            &NewSoldPlan {
                plan_id: Some(old.plan_id),
                name: old.name,
                price_minor: FEE,
                currency: "USD".into(),
                quota_minor: Some(20_000_000),
                period_kind: PeriodKind::Anniversary,
                overage_enabled: true,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    w.db.schedule_customer_plan_change(w.org, v2.plan_version_id)
        .await
        .unwrap();
    w.db.rollover_due_subscriptions().await.unwrap();
    let quote = w.db.plan_payment_quote(w.org).await.unwrap();
    assert_eq!(quote.terms.plan_version_id, v2.plan_version_id);
    let next = payment::created_order(&w.db, &w.context, w.org, w.plan).await;
    payment::mark_paid(&w.db, &w.context, &next).await;
    w.db.activate_plan_period(next.order_id, w.plan)
        .await
        .unwrap();
    let periods = w.db.plan_periods(w.plan).await.unwrap();
    assert_eq!(periods.len(), 2);
    assert_eq!(
        periods
            .iter()
            .find(|p| p.period_index == 0)
            .unwrap()
            .accounting_policy_version,
        1
    );
    assert_eq!(
        periods
            .iter()
            .find(|p| p.period_index == 1)
            .unwrap()
            .accounting_policy_version,
        2
    );
    assert_eq!(
        w.db.plan_journals(w.org, None, 50)
            .await
            .unwrap()
            .journals
            .len(),
        2
    );
}

#[tokio::test]
async fn free_daily_version_has_zero_invoice_and_no_cash_or_income() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let user =
        w.db.register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "free daily fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let sold =
        w.db.publish_sold_plan(
            w.db.default_business().await.unwrap(),
            &NewSoldPlan {
                plan_id: None,
                name: "Free daily".into(),
                price_minor: 0,
                currency: "USD".into(),
                quota_minor: Some(5000),
                period_kind: PeriodKind::CalendarMonth,
                overage_enabled: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    let plan =
        w.db.bind_customer_plan(
            user.organization.id,
            sold.plan_version_id,
            PeriodAuthorization::Complimentary(Uuid::new_v4()),
        )
        .await
        .unwrap();
    let journal =
        w.db.plan_journals(user.organization.id, None, 50)
            .await
            .unwrap()
            .journals
            .remove(0);
    assert_eq!(
        (
            journal.total_minor,
            journal.settled_minor,
            journal.accounting_policy_version
        ),
        (0, 0, 2)
    );
    let wallet = Wallet::open_existing(w.db.pool().clone(), &user.organization.tenant_id)
        .await
        .unwrap();
    let period =
        w.db.plan_periods(plan.customer_plan_id)
            .await
            .unwrap()
            .remove(0);
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query(
        "SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE",
    )
    .bind(plan.customer_plan_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    w.db.recognize_plan_period_in(
        &mut tx,
        &wallet,
        journal.journal_id,
        period.period_end,
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let nonzero:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 AND (entry_id IS NOT NULL OR billed_minor<>0 OR earned_minor<>0 OR deferred_minor<>0 OR cash_minor<>0)").bind(period.plan_period_id).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(nonzero, 0);
    let f =
        w.db.financial_facts(&FactFilter {
            organization_id: Some(user.organization.id),
            ..FactFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(
        (
            f.billed_minor,
            f.earned_minor,
            f.deferred_minor,
            f.collected_minor
        ),
        (0, 0, 0, 0)
    );
}

#[tokio::test]
async fn recognition_and_refund_serialize_to_the_same_net_fee() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    let now = OffsetDateTime::now_utc();
    let (_, r) = tokio::join!(recognize(&w, now), refund(&w, 1_000_000));
    assert!(r.refund_id != Uuid::nil());
    assert_books(
        &w,
        FEE - 1_000_000,
        expected(&w, FEE - 1_000_000).await,
        FEE - 1_000_000,
        1_000_000,
    )
    .await;
    assert_eq!(net(&w, "Assets:Cash").await, -(FEE - 1_000_000));
}

#[tokio::test]
async fn recognition_effects_book_today_when_service_days_are_old() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(40, false).await else {
        return;
    };
    let today = OffsetDateTime::now_utc().date();
    // Seal the actual historical month before catching up. Posting by service
    // date would now fail; current-date catch-up must still succeed.
    w.wallet
        .close_month(w.period.period_start.date().replace_day(1).unwrap())
        .await
        .unwrap();
    recognize(&w, OffsetDateTime::now_utc()).await;
    let bad:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.plan_accounting_effects WHERE plan_period_id=$1 AND kind='recognition' AND (booked_on<>$2 OR service_day>=$2)").bind(w.period.plan_period_id).bind(today).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(bad, 0);
    let wrong:i64=sqlx::query_scalar(&format!("SELECT count(*) FROM oxsum.plan_accounting_effects a JOIN ledger_{}.entries e ON e.entry_id=a.entry_id WHERE a.plan_period_id=$1 AND a.kind='recognition' AND e.booking_date<>$2",w.tenant)).bind(w.period.plan_period_id).bind(today).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(wrong, 0);
}

#[tokio::test]
async fn a_locked_first_candidate_does_not_starve_an_independent_period() {
    let _guard = TEST_LOCK.lock().await;
    let Some(a) = world(10, false).await else {
        return;
    };
    let Some(b) = world(10, false).await else {
        return;
    };
    let (first, second) = if a.period.plan_period_id < b.period.plan_period_id {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let mut lock = first.db.pool().begin().await.unwrap();
    sqlx::query(
        "SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE",
    )
    .bind(first.plan)
    .fetch_one(&mut *lock)
    .await
    .unwrap();
    let count = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        first.db.run_plan_recognition(1),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(count > 0);
    assert_books(first, FEE, 0, FEE, 0).await;
    assert_books(second, FEE, expected(second, FEE).await, FEE, 0).await;
    lock.rollback().await.unwrap();
    assert!(first.db.run_plan_recognition(1).await.unwrap() > 0);
    assert_books(first, FEE, expected(first, FEE).await, FEE, 0).await;
}

async fn lost(w: &World, order: &crate::PaymentOrder) -> Uuid {
    let reference = format!("dispute_{}", Uuid::new_v4());
    let event = VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: MerchantEventKind::Dispute,
        payment_id: Some(order.order_id),
        provider_payment_id: Some(format!("pay_{}", order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(MerchantMode::Test),
        amount_minor: Some(order.display_amount_minor),
        currency: Some(order.display_currency.clone()),
        refund_id: Some(reference.clone()),
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[84; 32])
            .await
            .unwrap();
    assert_eq!(receipt.state, "accepted");
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
        Some("cross-policy accounting fixture"),
        &Uuid::new_v4().to_string(),
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    id
}
async fn transition_to_daily(w: &World) -> crate::PaymentOrder {
    let current = w.db.customer_plan(w.org).await.unwrap().unwrap();
    let old =
        w.db.sold_plan(current.plan_version_id)
            .await
            .unwrap()
            .unwrap();
    let v2 =
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
    w.db.schedule_customer_plan_change(w.org, v2.plan_version_id)
        .await
        .unwrap();
    w.db.rollover_due_subscriptions().await.unwrap();
    let order = payment::created_order(&w.db, &w.context, w.org, w.plan).await;
    payment::mark_paid(&w.db, &w.context, &order).await;
    w.db.activate_plan_period(order.order_id, w.plan)
        .await
        .unwrap();
    order
}

#[tokio::test]
async fn disputes_on_mixed_policy_statements_follow_the_original_remaining_debt() {
    let _guard = TEST_LOCK.lock().await;
    for daily_first in [false, true] {
        let Some(w) = world(40, true).await else {
            return;
        };
        // A prior partial refund reduces the old source to 1.5m. Its later
        // full dispute has only 1.5m receivable; excess is cash-loss suspense.
        refund(&w, 1_000_000).await;
        let daily = transition_to_daily(&w).await;
        let (first, second) = if daily_first {
            (&daily, &w.order)
        } else {
            (&w.order, &daily)
        };
        lost(&w, first).await;
        lost(&w, second).await;
        let f = facts(&w).await;
        assert_eq!(f.receivable_minor, 2 * FEE - 1_000_000);
        assert_eq!(f.billed_minor - f.collected_minor, f.receivable_minor);
        assert_eq!(net(&w, "Assets:Receivables").await, -f.receivable_minor);
        let open =
            w.db.plan_statements(w.org, None, 50)
                .await
                .unwrap()
                .statements
                .into_iter()
                .find(|s| s.total_minor > s.paid_minor)
                .unwrap();
        let lines = w.db.plan_statement_lines(open.statement_id).await.unwrap();
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.kind == crate::StatementLineKind::DisputeChargeback)
                .count(),
            2
        );
        // Mixed-source documents are deliberately refused by financial void;
        // refusal must preserve both policies' debt and the original ledger.
        assert!(
            w.db.release_statement_review(
                &w.owner,
                open.statement_id,
                "void",
                "mixed source review",
                &Uuid::new_v4().to_string()
            )
            .await
            .is_err()
        );
        assert_eq!(facts(&w).await.receivable_minor, f.receivable_minor);
    }
}

async fn mint(w: &World, statement: Uuid) -> Result<crate::EvidenceBundleRow, crate::WalletError> {
    w.db.mint_statement_evidence(&w.wallet, w.org, &w.tenant, statement, false, None)
        .await
}
async fn verify_export(w: &World, row: &crate::EvidenceBundleRow) -> serde_json::Value {
    let bytes =
        w.db.evidence_bundle_json(row.bundle_id)
            .await
            .unwrap()
            .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    oxsum_verify::verify_evidence_bundle(
        &text,
        &format!("tenant-{}", w.tenant),
        &w.org.to_string(),
        None,
    )
    .unwrap();
    serde_json::from_str(&text).unwrap()
}
#[tokio::test]
async fn concurrent_exports_during_refund_return_one_consistent_committed_snapshot() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    recognize(&w, OffsetDateTime::now_utc()).await;
    let statement =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .remove(0)
            .statement_id;
    let (a, b, c) = tokio::join!(
        mint(&w, statement),
        mint(&w, statement),
        refund(&w, 1_000_000)
    );
    assert!(c.refund_id != Uuid::nil());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(
        (
            a.bundle_id,
            &a.manifest_digest,
            a.anchor_log_index,
            a.entry_count,
            a.created_at
        ),
        (
            b.bundle_id,
            &b.manifest_digest,
            b.anchor_log_index,
            b.entry_count,
            b.created_at
        )
    );
    let value = verify_export(&w, &a).await;
    let artifact = &value["artifacts"]["planStatement"][statement.to_string()];
    let total = artifact["statement"]["totalMinor"].as_i64().unwrap();
    assert!([FEE, FEE - 1_000_000].contains(&total));
    assert_eq!(artifact["journal"]["totalMinor"], total);
    assert_eq!(
        artifact["journalLines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["amountMinor"].as_i64().unwrap())
            .sum::<i64>(),
        total
    );
    assert_eq!(
        artifact["accounting"]["effects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["billed_minor"].as_i64().unwrap())
            .sum::<i64>(),
        total
    );
    let published =
        w.db.evidence_bundle(w.org, a.bundle_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(published.created_at, a.created_at);
}

#[tokio::test]
async fn export_failure_after_marker_reuses_frozen_inputs_after_later_refund() {
    use sqlx::Executor as _;
    let _guard = TEST_LOCK.lock().await;
    for legacy in [false, true] {
        let Some(w) = world(if legacy { 40 } else { 10 }, legacy).await else {
            return;
        };
        if legacy {
            w.db.rollover_due_subscriptions().await.unwrap();
        } else {
            recognize(&w, OffsetDateTime::now_utc()).await;
        }
        let statement =
            w.db.plan_statements(w.org, None, 50)
                .await
                .unwrap()
                .statements
                .remove(0)
                .statement_id;
        let before = w.wallet.log_size().await.unwrap();
        // Inject exactly the crash window: the marker already committed but the
        // following exported-byte INSERT fails. The durable input snapshot survives.
        w.db.pool().execute("CREATE FUNCTION oxsum.test_fail_evidence_publish() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RAISE EXCEPTION 'controlled export publication fault'; END;$$; CREATE TRIGGER test_fail_evidence_publish BEFORE INSERT ON oxsum.evidence_bundles FOR EACH ROW EXECUTE FUNCTION oxsum.test_fail_evidence_publish()").await.unwrap();
        assert!(mint(&w, statement).await.is_err());
        w.db.pool().execute("DROP TRIGGER test_fail_evidence_publish ON oxsum.evidence_bundles; DROP FUNCTION oxsum.test_fail_evidence_publish()").await.unwrap();
        assert_eq!(w.wallet.log_size().await.unwrap(), before + 1);
        let snapshots: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.evidence_mint_inputs WHERE organization_id=$1",
        )
        .bind(w.org)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        assert_eq!(snapshots, 1);
        let exported: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.evidence_bundles WHERE organization_id=$1",
        )
        .bind(w.org)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        assert_eq!(exported, 0);
        refund(&w, 1_000_000).await;
        let after_refund = w.wallet.log_size().await.unwrap();
        let row = mint(&w, statement).await.unwrap();
        let value = verify_export(&w, &row).await;
        assert_eq!(
            value["artifacts"]["planStatement"][statement.to_string()]["statement"]["totalMinor"],
            FEE
        );
        assert_eq!(
            w.wallet.log_size().await.unwrap(),
            after_refund,
            "retry reuses the standing exact marker"
        );
        assert_eq!(
            facts(&w).await.billed_minor,
            FEE - 1_000_000,
            "later financial reality remains independent"
        );
    }
}

#[tokio::test]
async fn legacy_orphan_marker_with_different_inputs_refuses_export_publication() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    let statement =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .remove(0)
            .statement_id;
    let scope = format!("statement/{}/{statement}", w.org);
    let old = doubleentry::Hash::digest(b"old-captured-snapshot", b"historical");
    w.wallet
        .anchor_evidence_bundle(&scope, "historical orphan", old)
        .await
        .unwrap();
    assert!(mint(&w, statement).await.is_err());
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.evidence_bundles WHERE organization_id=$1")
            .bind(w.org)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn concurrent_exports_finish_with_one_pool_connection() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    recognize(&w, OffsetDateTime::now_utc()).await;
    let statement =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .remove(0)
            .statement_id;
    let url = oxsum_testkit::suite_db_url("daily_recognition").unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect(&url)
        .await
        .unwrap();
    let db = Db::from_pool(pool);
    let wallet = Wallet::open_existing(db.pool().clone(), &w.tenant)
        .await
        .unwrap();
    let call = || db.mint_statement_evidence(&wallet, w.org, &w.tenant, statement, false, None);
    let results = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(call(), call(), call(), call())
    })
    .await
    .unwrap();
    let a = results.0.unwrap();
    for b in [results.1.unwrap(), results.2.unwrap(), results.3.unwrap()] {
        assert_eq!(
            (b.bundle_id, b.manifest_digest, b.created_at),
            (a.bundle_id, a.manifest_digest.clone(), a.created_at)
        );
    }
    verify_export(&w, &a).await;
}

#[tokio::test]
async fn late_success_on_reversed_note_keeps_daily_allocation_and_cash_once() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(10, false).await else {
        return;
    };
    let r = refund_with_failure(&w, 1_000_000, true).await;
    assert_books(
        &w,
        FEE - 1_000_000,
        expected(&w, FEE - 1_000_000).await,
        FEE - 1_000_000,
        1_000_000,
    )
    .await;
    let state: String =
        sqlx::query_scalar("SELECT state FROM oxsum.credit_notes WHERE credit_note_id=$1")
            .bind(r.note_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(state, "reversed");
    assert_eq!(net(&w, "Assets:Cash").await, -(FEE - 1_000_000));
}

#[tokio::test]
async fn close_and_refund_race_cannot_recognize_or_invoice_the_fee_twice() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(40, false).await else {
        return;
    };
    let (closed, _) = tokio::join!(w.db.rollover_due_subscriptions(), refund(&w, 1_000_000));
    closed.unwrap();
    assert_books(
        &w,
        FEE - 1_000_000,
        FEE - 1_000_000,
        FEE - 1_000_000,
        1_000_000,
    )
    .await;
    assert_eq!(
        w.db.plan_journals(w.org, None, 50)
            .await
            .unwrap()
            .journals
            .len(),
        1
    );
    assert_eq!(net(&w, "Assets:Cash").await, -(FEE - 1_000_000));
}

#[tokio::test]
async fn tiny_month_end_fees_keep_exact_final_remainder_in_real_entries() {
    let _guard = TEST_LOCK.lock().await;
    let Some(w) = world(0, false).await else {
        return;
    };
    for fee in [1, 17] {
        let user =
            w.db.register(NewUser {
                email: format!("{}@example.com", Uuid::new_v4()),
                password: "tiny daily fixture password".into(),
                organization_name: None,
            })
            .await
            .unwrap();
        let sold =
            w.db.publish_sold_plan(
                w.db.default_business().await.unwrap(),
                &NewSoldPlan {
                    plan_id: None,
                    name: "Tiny daily fee".into(),
                    price_minor: fee,
                    currency: "USD".into(),
                    quota_minor: Some(500),
                    period_kind: PeriodKind::Anniversary,
                    overage_enabled: false,
                    sort_order: 0,
                },
            )
            .await
            .unwrap();
        let plan =
            w.db.bind_customer_plan(
                user.organization.id,
                sold.plan_version_id,
                PeriodAuthorization::PaymentOrder(Uuid::nil()),
            )
            .await
            .unwrap();
        let order = payment::created_order(
            &w.db,
            &w.context,
            user.organization.id,
            plan.customer_plan_id,
        )
        .await;
        payment::mark_paid(&w.db, &w.context, &order).await;
        // Use the next leap year after the ledger's fixed 2026 opening date.
        let start = time::macros::datetime!(2028-01-31 12:00 UTC);
        w.db.activate_plan_period_at(order.order_id, plan.customer_plan_id, start)
            .await
            .unwrap();
        let period =
            w.db.plan_periods(plan.customer_plan_id)
                .await
                .unwrap()
                .remove(0);
        assert_eq!(
            period.period_end.date(),
            time::macros::date!(2028 - 02 - 29)
        );
        let journal =
            w.db.plan_journals(user.organization.id, None, 50)
                .await
                .unwrap()
                .journals
                .remove(0);
        let wallet = Wallet::open_existing(w.db.pool().clone(), &user.organization.tenant_id)
            .await
            .unwrap();
        let mut tx = w.db.pool().begin().await.unwrap();
        sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE").bind(plan.customer_plan_id).fetch_one(&mut *tx).await.unwrap();
        w.db.recognize_plan_period_in(
            &mut tx,
            &wallet,
            journal.journal_id,
            period.period_end,
            period.period_end.date(),
        )
        .await
        .unwrap();
        let totals = super::totals_in(&mut tx, period.plan_period_id)
            .await
            .unwrap();
        assert_eq!((totals.earned, totals.deferred), (fee, 0));
        tx.commit().await.unwrap();
        let sum:i64=sqlx::query_scalar(&format!("SELECT COALESCE(SUM(p.amount_minor),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a USING(account_index) WHERE a.path='Income:PlanFees' AND p.direction='C' AND p.layer='settled'",user.organization.tenant_id,user.organization.tenant_id)).fetch_one(w.db.pool()).await.unwrap();
        assert_eq!(sum, fee);
    }
}

//! Confirmed plan cash, documents and append-only recovery (issue #802).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;

use oxsum_core::{
    CustomerPlan, Db, InvoiceJournal, JournalState, MerchantEventKind, MerchantEventReceipt,
    MerchantMode, MerchantSigningContext, NewRefundRequest, NewSoldPlan, NewUser, PaymentOrder,
    PeriodAuthorization, PeriodKind, PlanStatementState, RefundDecision, VerifiedMerchantEvent,
    Wallet,
};
use time::OffsetDateTime;
use uuid::Uuid;

const FEE: i64 = 2_500_000;
struct World {
    db: Db,
    owner: String,
    org: Uuid,
    tenant: String,
    wallet: Wallet,
    plan: CustomerPlan,
    order: PaymentOrder,
    context: MerchantSigningContext,
}
async fn world(activate: bool) -> Option<World> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url("plan_refunds")?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "plan refund fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = user.organization.id;
    let tenant = user.organization.tenant_id;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &NewSoldPlan {
                plan_id: None,
                name: format!("refund-{}", Uuid::new_v4()),
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
    let plan = db
        .bind_customer_plan(
            org,
            sold.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    let context = payment::merchant_context(&db, &owner).await;
    let order = payment::created_order(&db, &context, org, plan.customer_plan_id).await;
    payment::mark_paid(&db, &context, &order).await;
    let order = db
        .payment_order(org, order.order_id)
        .await
        .unwrap()
        .unwrap();
    if activate {
        db.activate_plan_period(order.order_id, plan.customer_plan_id)
            .await
            .unwrap();
    }
    Some(World {
        db,
        owner,
        org,
        tenant,
        wallet,
        plan,
        order,
        context,
    })
}
async fn due(w: &World) {
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=now()-interval '2 days',period_end=now()-interval '1 day' WHERE customer_plan_id=$1 AND state='active'").bind(w.plan.customer_plan_id).execute(w.db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE organization_id=$1 AND state='active'").bind(w.org).execute(w.db.pool()).await.unwrap();
}
async fn close(w: &World) -> InvoiceJournal {
    due(w).await;
    w.db.rollover_due_subscriptions().await.unwrap();
    w.db.plan_journals(w.org, None, 50)
        .await
        .unwrap()
        .journals
        .remove(0)
}
async fn net(w: &World, path: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a ON a.account_index=p.account_index JOIN ledger_{}.entries e ON e.entry_id=p.entry_id WHERE a.path=$1 AND p.layer='settled' AND e.log_index IS NOT NULL",w.tenant,w.tenant,w.tenant)).bind(path).fetch_one(w.db.pool()).await.unwrap()
}
async fn reserve(w: &World, amount: i64) -> oxsum_core::OrderRefund {
    let r =
        w.db.request_order_refund(
            w.org,
            w.order.order_id,
            &NewRefundRequest {
                amount_minor: Some(amount),
                reason: "confirmed plan refund fixture".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    w.db.resolve_order_refund(
        &w.owner,
        r.refund_id,
        RefundDecision::Approve,
        None,
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap()
    .0
}
async fn event(
    w: &World,
    r: &oxsum_core::OrderRefund,
    kind: MerchantEventKind,
) -> MerchantEventReceipt {
    let provider = format!("refund_{}", r.refund_id);
    w.db.note_refund_provider(r.refund_id, &provider)
        .await
        .unwrap();
    let e = VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind,
        payment_id: Some(w.order.order_id),
        provider_payment_id: Some(format!("pay_{}", w.order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(MerchantMode::Test),
        amount_minor: Some(r.display_amount_minor),
        currency: Some(r.display_currency.clone()),
        refund_id: Some(provider),
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &e, &[81; 32])
            .await
            .unwrap();
    assert_eq!(receipt.state, "accepted", "{:?}", receipt.reason);
    receipt
}
async fn apply(w: &World, r: &MerchantEventReceipt) {
    w.db.apply_refund_event(&w.wallet, r, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
}
async fn refunded(w: &World, amount: i64) -> oxsum_core::OrderRefund {
    let r = reserve(w, amount).await;
    let e = event(w, &r, MerchantEventKind::RefundSucceeded).await;
    apply(w, &e).await;
    r
}
async fn effect_count(w: &World) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM oxsum.plan_refund_effects WHERE order_id=$1")
        .bind(w.order.order_id)
        .fetch_one(w.db.pool())
        .await
        .unwrap()
}
async fn allocation(w: &World, r: Uuid) -> oxsum_core::PlanRefundRecovery {
    w.db.recover_plan_refund(
        &w.owner,
        &w.wallet,
        r,
        false,
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn h2_partial_and_full_preclose_refunds_conserve_cash_and_close_once() {
    for amount in [1_000_000, FEE] {
        let Some(w) = world(true).await else { return };
        let r = refunded(&w, amount).await;
        let a = allocation(&w, r.refund_id).await;
        assert_eq!(
            (a.cash_minor, a.deferred_minor, a.earned_minor),
            (amount, amount, 0)
        );
        assert_eq!(net(&w, "Assets:Cash").await, -(FEE - amount));
        assert_eq!(net(&w, "Liabilities:UnearnedPlanFees").await, FEE - amount);
        assert_eq!(net(&w, "Income:Discounts").await, 0);
        let j = close(&w).await;
        assert_eq!(
            (j.total_minor, j.settled_minor, j.state),
            (FEE - amount, FEE - amount, JournalState::Paid)
        );
        let s =
            w.db.plan_statements(w.org, None, 50)
                .await
                .unwrap()
                .statements
                .remove(0);
        assert_eq!(
            (s.total_minor, s.paid_minor, s.state),
            (FEE - amount, FEE - amount, PlanStatementState::Paid)
        );
        assert_eq!(net(&w, "Assets:Receivables").await, 0);
        assert_eq!(net(&w, "Liabilities:UnearnedPlanFees").await, 0);
        assert_eq!(
            net(&w, "Income:PlanFees").await + net(&w, "Income:Discounts").await,
            FEE - amount
        );
        w.db.rollover_due_subscriptions().await.unwrap();
        assert_eq!(effect_count(&w).await, 1);
        assert_eq!(net(&w, "Assets:Cash").await, -(FEE - amount));
    }
}

#[tokio::test]
async fn paid_journal_refunds_sync_net_payment_without_false_audit_failure() {
    let Some(w) = world(true).await else { return };
    let j = close(&w).await;
    let r = reserve(&w, 1_000_000).await;
    let e = event(&w, &r, MerchantEventKind::RefundSucceeded).await;
    apply(&w, &e).await;
    apply(&w, &e).await;
    let a = allocation(&w, r.refund_id).await;
    assert_eq!(
        (a.cash_minor, a.earned_minor, a.deferred_minor),
        (1_000_000, 1_000_000, 0)
    );
    let j =
        w.db.plan_journal(w.org, j.journal_id)
            .await
            .unwrap()
            .unwrap()
            .journal;
    assert_eq!((j.total_minor, j.settled_minor), (1_500_000, 1_500_000));
    let s =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .remove(0);
    assert_eq!(
        (s.total_minor, s.paid_minor, s.state),
        (1_500_000, 1_500_000, PlanStatementState::Paid)
    );
    let lines =
        w.db.plan_statement(w.org, s.statement_id)
            .await
            .unwrap()
            .unwrap()
            .lines;
    assert_eq!(
        lines.iter().filter(|l| l.source_ref == r.note_id).count(),
        2
    );
    let failures:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.finance_audit WHERE target=$1 AND detail->>'compensation'='insufficientFunds'").bind(r.refund_id.to_string()).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(failures, 0);
    assert_eq!(effect_count(&w).await, 1);
    assert_eq!(net(&w, "Assets:Cash").await, -1_500_000);
    assert_eq!(net(&w, "Assets:Receivables").await, 0);
}

#[tokio::test]
async fn a_paid_unactivated_refund_records_cash_without_quota_or_reactivation() {
    let Some(w) = world(false).await else { return };
    assert_eq!(net(&w, "Assets:Cash").await, 0);
    let r = refunded(&w, 1_000_000).await;
    let a = allocation(&w, r.refund_id).await;
    assert_eq!(a.plan_period_id, None);
    assert_eq!(a.deferred_minor, 1_000_000);
    assert_eq!(net(&w, "Assets:Cash").await, -1_500_000);
    assert_eq!(net(&w, "Liabilities:UnearnedPlanFees").await, 1_500_000);
    assert!(
        w.db.plan_periods(w.plan.customer_plan_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        w.db.activate_plan_period(w.order.order_id, w.plan.customer_plan_id)
            .await
            .is_err()
    );
    assert_eq!(w.wallet.available().await.unwrap(), 0);
}

#[tokio::test]
async fn voided_journal_refund_draws_only_its_refund_payable() {
    let Some(w) = world(true).await else { return };
    let j = close(&w).await;
    w.db.void_invoice_journal(
        &w.owner,
        &w.wallet,
        j.journal_id,
        "fixture void",
        &Uuid::new_v4().to_string(),
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    assert_eq!(net(&w, "Liabilities:RefundPayable").await, FEE);
    let r = refunded(&w, 1_000_000).await;
    assert_eq!(allocation(&w, r.refund_id).await.payable_minor, 1_000_000);
    assert_eq!(net(&w, "Liabilities:RefundPayable").await, 1_500_000);
    assert_eq!(net(&w, "Assets:Cash").await, -1_500_000);
    assert_eq!(net(&w, "Assets:Receivables").await, 0);
    assert_eq!(
        w.db.plan_journal(w.org, j.journal_id)
            .await
            .unwrap()
            .unwrap()
            .journal
            .state,
        JournalState::Void
    );
}

#[tokio::test]
async fn concurrent_refunds_and_duplicate_callbacks_do_not_overdraw_a_period() {
    let Some(w) = world(true).await else { return };
    let a = reserve(&w, 1_000_000).await;
    let b = reserve(&w, 1_500_000).await;
    let ea = event(&w, &a, MerchantEventKind::RefundSucceeded).await;
    let eb = event(&w, &b, MerchantEventKind::RefundSucceeded).await;
    let ((), (), ()) = tokio::join!(apply(&w, &ea), apply(&w, &eb), apply(&w, &ea));
    assert_eq!(effect_count(&w).await, 2);
    assert_eq!(net(&w, "Assets:Cash").await, 0);
    assert_eq!(net(&w, "Liabilities:UnearnedPlanFees").await, 0);
    let j = close(&w).await;
    assert_eq!((j.total_minor, j.settled_minor), (0, 0));
}

#[tokio::test]
async fn a_refund_racing_close_has_one_cash_return_and_one_bill_credit() {
    let Some(w) = world(true).await else { return };
    let r = reserve(&w, 1_000_000).await;
    let e = event(&w, &r, MerchantEventKind::RefundSucceeded).await;
    due(&w).await;
    let (_, closed) = tokio::join!(apply(&w, &e), w.db.rollover_due_subscriptions());
    closed.unwrap();
    let j =
        w.db.plan_journals(w.org, None, 50)
            .await
            .unwrap()
            .journals
            .remove(0);
    assert_eq!((j.total_minor, j.settled_minor), (1_500_000, 1_500_000));
    assert_eq!(net(&w, "Assets:Cash").await, -1_500_000);
    assert_eq!(net(&w, "Assets:Receivables").await, 0);
    assert_eq!(net(&w, "Liabilities:UnearnedPlanFees").await, 0);
    assert_eq!(effect_count(&w).await, 1);
}

#[tokio::test]
async fn failed_refund_books_nothing_but_a_late_success_is_a_retained_cash_fact() {
    let Some(w) = world(false).await else { return };
    let r = reserve(&w, 1_000_000).await;
    let failed = event(&w, &r, MerchantEventKind::RefundFailed).await;
    apply(&w, &failed).await;
    assert_eq!(effect_count(&w).await, 0);
    assert_eq!(net(&w, "Assets:Cash").await, 0);
    let late = event(&w, &r, MerchantEventKind::RefundSucceeded).await;
    apply(&w, &late).await;
    assert_eq!(effect_count(&w).await, 1);
    assert_eq!(net(&w, "Assets:Cash").await, -1_500_000);
    assert_eq!(
        w.db.order_refundable_minor(w.org, w.order.order_id)
            .await
            .unwrap(),
        Some(1_500_000)
    );
    assert!(
        w.db.activate_plan_period(w.order.order_id, w.plan.customer_plan_id)
            .await
            .is_err()
    );
    let note: String =
        sqlx::query_scalar("SELECT state FROM oxsum.credit_notes WHERE credit_note_id=$1")
            .bind(r.note_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(note, "reversed");
}

#[tokio::test]
async fn missing_legacy_cash_is_previewed_then_recovered_once() {
    let Some(w) = world(true).await else { return };
    let r = reserve(&w, 1_000_000).await;
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(r.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let before = w.wallet.log_size().await.unwrap();
    let read_only_wallet = Wallet::open_existing(w.db.pool().clone(), &w.tenant)
        .await
        .unwrap();
    assert_eq!(read_only_wallet.log_size().await.unwrap(), before);
    let proposed = allocation(&w, r.refund_id).await;
    assert!(!proposed.already_applied);
    assert_eq!(proposed.deferred_minor, 1_000_000);
    assert_eq!(effect_count(&w).await, 0);
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    due(&w).await;
    assert!(w.db.rollover_due_subscriptions().await.is_err());
    let a =
        w.db.recover_plan_refund(
            &w.owner,
            &w.wallet,
            r.refund_id,
            true,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert_eq!(a.deferred_minor, 1_000_000);
    let after = w.wallet.log_size().await.unwrap();
    let replay =
        w.db.recover_plan_refund(
            &w.owner,
            &w.wallet,
            r.refund_id,
            true,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert!(replay.already_applied);
    assert_eq!(w.wallet.log_size().await.unwrap(), after);
    w.db.rollover_due_subscriptions().await.unwrap();
    assert_eq!(net(&w, "Assets:Receivables").await, 0);
}

#[tokio::test]
async fn h3_partial_legacy_bill_keeps_its_debt_after_a_real_cash_refund() {
    let Some(w) = world(true).await else { return };
    due(&w).await;
    let id = payment::legacy_close(&w.db, w.org, w.plan.customer_plan_id, 1_500_000).await;
    w.db.rollover_due_subscriptions().await.unwrap();
    let r = refunded(&w, 2_000_000).await;
    let a = allocation(&w, r.refund_id).await;
    assert_eq!(
        (a.cash_minor, a.earned_minor, a.unallocated_minor),
        (2_000_000, 2_000_000, 0)
    );
    let j = w.db.plan_journal(w.org, id).await.unwrap().unwrap().journal;
    assert_eq!(
        (j.total_minor, j.settled_minor, j.state),
        (2_000_000, 500_000, JournalState::Posted)
    );
    let s =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .remove(0);
    assert_eq!((s.total_minor, s.paid_minor), (2_000_000, 500_000));
    assert_eq!(net(&w, "Assets:Receivables").await, -1_500_000);
    assert_eq!(net(&w, "Assets:Cash").await, -500_000);
}

async fn legacy_refund_entry(w: &World, refund: Uuid, amount: i64, credit: &str) {
    use doubleentry::storage::postgres::PostgresStore;
    use doubleentry::{
        AccountRegistry, Amount, Currency, Draft, Entry, EntryBatch, IdempotencyKey, LedgerId,
        LedgerPolicy, LedgerStore, SealContext,
    };
    let store = PostgresStore::<6>::new(
        w.db.pool().clone(),
        LedgerId::new(format!("tenant-{}", w.tenant)).unwrap(),
    )
    .in_schema(&format!("ledger_{}", w.tenant));
    let registry = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
    let account = |path: &str| {
        registry
            .records()
            .iter()
            .find(|r| r.account.path.to_string() == path)
            .unwrap()
            .id
    };
    let key = format!("order-refund-plan/{refund}");
    let draft = Entry::<Draft, 6>::new(
        oxsum_core::entry_id_for(&key),
        IdempotencyKey::new(key.into_bytes()).unwrap(),
        OffsetDateTime::now_utc().date(),
    )
    .debit(
        account("Income:Discounts"),
        Amount::from_minor(amount),
        Currency::USD,
    )
    .credit(account(credit), Amount::from_minor(amount), Currency::USD);
    let sealed = draft
        .seal(&SealContext {
            accounts: &registry,
            calendar: &store.calendar().await.unwrap(),
            policy: &LedgerPolicy::default(),
        })
        .unwrap();
    store.append(&EntryBatch::single(sealed)).await.unwrap();
}

#[tokio::test]
async fn legacy_wrong_receivable_refund_appends_cash_correction_and_retains_entry() {
    let Some(w) = world(true).await else { return };
    due(&w).await;
    let id = payment::legacy_close(&w.db, w.org, w.plan.customer_plan_id, 1_500_000).await;
    w.db.rollover_due_subscriptions().await.unwrap();
    let r = reserve(&w, 2_000_000).await;
    legacy_refund_entry(&w, r.refund_id, 2_000_000, "Assets:Receivables").await;
    let original =
        *oxsum_core::entry_id_for(&format!("order-refund-plan/{}", r.refund_id)).as_uuid();
    sqlx::query("INSERT INTO oxsum.invoice_journal_lines(line_id,journal_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'discount',-2000000,'legacy buggy refund',$3)")
        .bind(Uuid::new_v4()).bind(id).bind(r.note_id).execute(w.db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.invoice_journals SET total_minor=2000000,settled_minor=2000000 WHERE journal_id=$1").bind(id).execute(w.db.pool()).await.unwrap();
    sqlx::query(
        "UPDATE oxsum.credit_notes SET state='applied',entry_id=$2 WHERE credit_note_id=$1",
    )
    .bind(r.note_id)
    .bind(original)
    .execute(w.db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(r.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    assert_eq!(net(&w, "Assets:Receivables").await, 500_000);
    assert_eq!(net(&w, "Assets:Cash").await, -FEE);
    let proposal = allocation(&w, r.refund_id).await;
    assert_eq!(proposal.earned_minor, 2_000_000);
    assert_eq!(proposal.original_entry_id, Some(original));
    let a =
        w.db.recover_plan_refund(
            &w.owner,
            &w.wallet,
            r.refund_id,
            true,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert_ne!(a.entry_id, Some(original));
    assert_eq!(net(&w, "Assets:Cash").await, -500_000);
    assert_eq!(net(&w, "Assets:Receivables").await, -1_500_000);
    let j = w.db.plan_journal(w.org, id).await.unwrap().unwrap().journal;
    assert_eq!((j.total_minor, j.settled_minor), (2_000_000, 500_000));
    let retained: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM oxsum.credit_notes WHERE credit_note_id=$1")
            .bind(r.note_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(retained, original);
    let before = w.wallet.log_size().await.unwrap();
    w.db.recover_plan_refund(
        &w.owner,
        &w.wallet,
        r.refund_id,
        true,
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
}

#[tokio::test]
async fn a_chargeback_exhausted_source_retains_the_full_refund_as_review_suspense() {
    let Some(w) = world(true).await else { return };
    let j = close(&w).await;
    let r = reserve(&w, 1_000_000).await;
    let dispute = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.disputes(dispute_id,order_id,organization_id,merchant_id,provider_dispute_id,amount_minor,currency,state) VALUES($1,$2,$3,$4,$5,$6,'USD','opened')")
        .bind(dispute).bind(w.order.order_id).bind(w.org).bind(w.context.merchant_id).bind(dispute.to_string()).bind(FEE).execute(w.db.pool()).await.unwrap();
    w.db.recharge_journal_dispute(
        &w.wallet,
        w.order.order_id,
        dispute,
        FEE,
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    let e = event(&w, &r, MerchantEventKind::RefundSucceeded).await;
    apply(&w, &e).await;
    let a = allocation(&w, r.refund_id).await;
    assert_eq!((a.unallocated_minor, a.earned_minor), (1_000_000, 0));
    assert_eq!(net(&w, "Assets:Cash").await, 1_000_000);
    assert_eq!(net(&w, "Equity:Adjustments").await, -1_000_000);
    assert_eq!(net(&w, "Assets:Receivables").await, -FEE);
    let j =
        w.db.plan_journal(w.org, j.journal_id)
            .await
            .unwrap()
            .unwrap()
            .journal;
    assert_eq!(j.settled_minor, 0);
    let reviews:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.finance_audit WHERE target=$1 AND detail->>'compensation'='bookedWithReview'").bind(r.refund_id.to_string()).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(reviews, 1);
}

#[tokio::test]
async fn a_correct_legacy_cash_refund_only_repairs_its_missing_statement_projection() {
    let Some(w) = world(true).await else { return };
    let j = close(&w).await;
    let r = reserve(&w, 1_000_000).await;
    legacy_refund_entry(&w, r.refund_id, 1_000_000, "Assets:Cash").await;
    let original =
        *oxsum_core::entry_id_for(&format!("order-refund-plan/{}", r.refund_id)).as_uuid();
    sqlx::query("INSERT INTO oxsum.invoice_journal_lines(line_id,journal_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'discount',-1000000,'legacy cash refund',$3)")
        .bind(Uuid::new_v4()).bind(j.journal_id).bind(r.note_id).execute(w.db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.invoice_journals SET total_minor=1500000,settled_minor=1500000 WHERE journal_id=$1").bind(j.journal_id).execute(w.db.pool()).await.unwrap();
    sqlx::query(
        "UPDATE oxsum.credit_notes SET state='applied',entry_id=$2 WHERE credit_note_id=$1",
    )
    .bind(r.note_id)
    .bind(original)
    .execute(w.db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(r.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let before = w.wallet.log_size().await.unwrap();
    let a =
        w.db.recover_plan_refund(
            &w.owner,
            &w.wallet,
            r.refund_id,
            true,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert_eq!(a.entry_id, Some(original));
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    assert_eq!(net(&w, "Assets:Cash").await, -1_500_000);
    let s =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .remove(0);
    assert_eq!((s.total_minor, s.paid_minor), (1_500_000, 1_500_000));
}

#[tokio::test]
async fn a_failed_statement_write_rolls_back_the_refund_cash_and_can_retry() {
    let Some(w) = world(true).await else { return };
    close(&w).await;
    let statement =
        w.db.plan_statements(w.org, None, 50)
            .await
            .unwrap()
            .statements
            .remove(0);
    // An interrupted historical payment projection cannot justify a negative
    // counter. The trusted event remains retryable until its source is fixed.
    sqlx::query("UPDATE oxsum.plan_statements SET paid_minor=0 WHERE statement_id=$1")
        .bind(statement.statement_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let r = reserve(&w, 1_000_000).await;
    let e = event(&w, &r, MerchantEventKind::RefundSucceeded).await;
    assert!(
        w.db.apply_refund_event(&w.wallet, &e, OffsetDateTime::now_utc().date())
            .await
            .is_err()
    );
    assert_eq!(net(&w, "Assets:Cash").await, -FEE);
    assert_eq!(effect_count(&w).await, 0);
    let state: String = sqlx::query_scalar("SELECT state FROM oxsum.refunds WHERE refund_id=$1")
        .bind(r.refund_id)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    assert_eq!(state, "provider_pending");
    sqlx::query("UPDATE oxsum.plan_statements SET paid_minor=$2 WHERE statement_id=$1")
        .bind(statement.statement_id)
        .bind(FEE)
        .execute(w.db.pool())
        .await
        .unwrap();
    apply(&w, &e).await;
    assert_eq!(net(&w, "Assets:Cash").await, -1_500_000);
    assert_eq!(effect_count(&w).await, 1);
}

#[tokio::test]
async fn recovery_rejects_unconfirmed_or_wrong_wallet_sources_without_writes() {
    let Some(w) = world(true).await else { return };
    let r = reserve(&w, 1_000_000).await;
    assert!(
        w.db.recover_plan_refund(
            &w.owner,
            &w.wallet,
            r.refund_id,
            true,
            OffsetDateTime::now_utc().date()
        )
        .await
        .is_err()
    );
    let e = event(&w, &r, MerchantEventKind::RefundSucceeded).await;
    let other = Wallet::open(w.db.pool().clone(), &Uuid::new_v4().simple().to_string())
        .await
        .unwrap();
    assert!(
        w.db.apply_refund_event(&other, &e, OffsetDateTime::now_utc().date())
            .await
            .is_err()
    );
    assert_eq!(effect_count(&w).await, 0);
    assert_eq!(net(&w, "Assets:Cash").await, -FEE);
    assert_eq!(other.log_size().await.unwrap(), 0);
    apply(&w, &e).await;
    assert!(
        w.db.recover_plan_refund(
            "invalid-session",
            &w.wallet,
            r.refund_id,
            false,
            OffsetDateTime::now_utc().date()
        )
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE oxsum.plan_refund_effects SET cash_minor=cash_minor+1 WHERE refund_id=$1"
        )
        .bind(r.refund_id)
        .execute(w.db.pool())
        .await
        .is_err()
    );
}

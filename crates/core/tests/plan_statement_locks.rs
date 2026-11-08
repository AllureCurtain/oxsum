//! Controlled PostgreSQL lock ownership and stale-dunning regressions (#811).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;

use std::sync::Arc;

use doubleentry::LedgerId;
use doubleentry::storage::postgres::PostgresStore;
use oxsum_core::{
    Db, MerchantEventKind, MerchantMode, MerchantSigningContext, NewRefundRequest, NewSoldPlan,
    NewUser, PeriodAuthorization, PeriodKind, PlanStatement, PlanStatementState, RefundDecision,
    VerifiedMerchantEvent, Wallet,
};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

// Sweeps intentionally span the business. Isolate their candidates while each
// individual test drives real concurrent transactions on the shared suite DB.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct World {
    db: Db,
    pool: PgPool,
    owner: String,
    org: Uuid,
    plan: Uuid,
    order: Uuid,
    statement: PlanStatement,
    wallet: Arc<Wallet>,
    context: MerchantSigningContext,
}
async fn world() -> Option<World> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url("plan_statement_locks")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool().clone();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "statement locks fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = user.organization.id;
    let wallet = Arc::new(
        Wallet::open(pool.clone(), &user.organization.tenant_id)
            .await
            .unwrap(),
    );
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &NewSoldPlan {
                plan_id: None,
                name: format!("locks-{}", Uuid::new_v4()),
                price_minor: 2_500_000,
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
        .unwrap()
        .customer_plan_id;
    let context = payment::merchant_context(&db, &owner).await;
    let payment = payment::created_order(&db, &context, org, plan).await;
    payment::mark_paid(&db, &context, &payment).await;
    let order = payment.order_id;
    db.activate_plan_period(order, plan).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=now()-interval '2 days',period_end=now()-interval '1 day' WHERE customer_plan_id=$1 AND state='active'").bind(plan).execute(&pool).await.unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE organization_id=$1 AND state='active'").bind(org).execute(&pool).await.unwrap();
    payment::legacy_close(&db, org, plan, 1_000_000).await;
    db.rollover_due_subscriptions().await.unwrap();
    let statement = db
        .plan_statements(org, None, 50)
        .await
        .unwrap()
        .statements
        .remove(0);
    Some(World {
        db,
        pool,
        owner,
        org,
        plan,
        order,
        statement,
        wallet,
        context,
    })
}
async fn due(w: &World, days: i32) {
    sqlx::query(
        "UPDATE oxsum.plan_statements SET due_at=now()-($2*interval '1 day') WHERE statement_id=$1",
    )
    .bind(w.statement.statement_id)
    .bind(days)
    .execute(&w.pool)
    .await
    .unwrap();
}
async fn past_due(w: &World) {
    due(w, 30).await;
    sqlx::query(
        "UPDATE oxsum.plan_statements SET state='past_due',past_due_at=now() WHERE statement_id=$1",
    )
    .bind(w.statement.statement_id)
    .execute(&w.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE oxsum.customer_plans SET dunning_suspended_at=now() WHERE customer_plan_id=$1",
    )
    .bind(w.plan)
    .execute(&w.pool)
    .await
    .unwrap();
}
async fn pin_plan(w: &World) -> Transaction<'_, Postgres> {
    let mut tx = w.pool.begin().await.unwrap();
    sqlx::query(
        "SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE",
    )
    .bind(w.plan)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx
}
async fn wait_for_blocked(pool: &PgPool, pid: i32) -> i32 {
    timeout(Duration::from_secs(5),async {
        loop {
            let blocked:Option<i32>=sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) LIMIT 1")
                .bind(pid).fetch_optional(pool).await.unwrap();
            if let Some(blocked)=blocked {break blocked;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("the operation reaches the controlled lock")
}
async fn state(w: &World) -> PlanStatement {
    w.db.plan_statement(w.org, w.statement.statement_id)
        .await
        .unwrap()
        .unwrap()
        .statement
}
async fn suspended(w: &World) -> bool {
    sqlx::query_scalar("SELECT dunning_suspended_at IS NOT NULL FROM oxsum.customer_plans WHERE customer_plan_id=$1").bind(w.plan).fetch_one(&w.pool).await.unwrap()
}
async fn clean(w: &World) {
    let current = state(w).await;
    if !matches!(
        current.state,
        PlanStatementState::Void | PlanStatementState::Paid
    ) {
        w.db.release_statement_review(
            &w.owner,
            w.statement.statement_id,
            "void",
            "fixture cleanup",
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn staff_review_waits_for_plan_without_holding_the_statement() {
    let _serial = SERIAL.lock().await;
    for action in ["extendGrace", "void"] {
        let Some(w) = world().await else { return };
        past_due(&w).await;
        let mut pin = pin_plan(&w).await;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *pin)
            .await
            .unwrap();
        let db = w.db.clone();
        let token = w.owner.clone();
        let id = w.statement.statement_id;
        let operation = tokio::spawn(async move {
            db.release_statement_review(
                &token,
                id,
                action,
                "controlled lock ordering",
                &Uuid::new_v4().to_string(),
            )
            .await
        });
        wait_for_blocked(&w.pool, pid).await;
        // With the former inverse order, review already owned this statement
        // while waiting for our plan. NOWAIT exposes that exact second edge.
        let statement_lock=sqlx::query("SELECT statement_id FROM oxsum.plan_statements WHERE statement_id=$1 FOR UPDATE NOWAIT").bind(id).fetch_one(&mut *pin).await;
        pin.rollback().await.unwrap();
        let result = timeout(Duration::from_secs(5), operation)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            statement_lock.is_ok(),
            "review locked statement before plan: {statement_lock:?}"
        );
        assert_eq!(
            (result.statement.total_minor, result.statement.paid_minor),
            if action == "void" {
                (0, 0)
            } else {
                (3_500_000, 2_500_000)
            }
        );
        if action == "extendGrace" {
            assert!(!suspended(&w).await);
        }
        clean(&w).await;
    }
}

#[tokio::test]
async fn refund_and_staff_review_serialize_without_a_statement_plan_cycle() {
    let _serial = SERIAL.lock().await;
    for action in ["extendGrace", "void"] {
        let Some(w) = world().await else { return };
        past_due(&w).await;
        let refund =
            w.db.request_order_refund(
                w.org,
                w.order,
                &NewRefundRequest {
                    amount_minor: Some(500_000),
                    reason: "controlled concurrent plan refund".into(),
                    request_key: Uuid::new_v4().to_string(),
                    requested_by: None,
                },
            )
            .await
            .unwrap();
        let refund =
            w.db.resolve_order_refund(
                &w.owner,
                refund.refund_id,
                RefundDecision::Approve,
                None,
                &Uuid::new_v4().to_string(),
            )
            .await
            .unwrap()
            .0;
        let provider = format!("refund_{}", refund.refund_id);
        w.db.note_refund_provider(refund.refund_id, &provider)
            .await
            .unwrap();
        let receipt =
            w.db.receive_merchant_event(
                &w.context,
                &VerifiedMerchantEvent {
                    event_id: Uuid::new_v4().to_string(),
                    kind: MerchantEventKind::RefundSucceeded,
                    payment_id: Some(w.order),
                    provider_payment_id: Some(format!("pay_{}", w.order)),
                    organization_id: Some(w.org),
                    provider_account_id: Some(w.context.account_id.clone()),
                    mode: Some(MerchantMode::Test),
                    amount_minor: Some(refund.display_amount_minor),
                    currency: Some(refund.display_currency.clone()),
                    refund_id: Some(provider),
                },
                &[81; 32],
            )
            .await
            .unwrap();
        assert_eq!(receipt.state, "accepted");
        let store = PostgresStore::<6>::new(
            w.pool.clone(),
            LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
        );
        let mut pin = w.pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(store.append_lock())
            .execute(&mut *pin)
            .await
            .unwrap();
        let pin_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *pin)
            .await
            .unwrap();
        let db = w.db.clone();
        let wallet = Arc::clone(&w.wallet);
        let refund_task = tokio::spawn(async move {
            db.apply_refund_event(&wallet, &receipt, OffsetDateTime::now_utc().date())
                .await
        });
        // The real refund owns plan/journal and is stopped at ledger append.
        let refund_pid = wait_for_blocked(&w.pool, pin_pid).await;
        let db = w.db.clone();
        let owner = w.owner.clone();
        let statement_id = w.statement.statement_id;
        let review_task = tokio::spawn(async move {
            db.release_statement_review(
                &owner,
                statement_id,
                action,
                "review beside a confirmed refund",
                &Uuid::new_v4().to_string(),
            )
            .await
        });
        wait_for_blocked(&w.pool, refund_pid).await;
        // Review must wait for the refund's plan without taking its statement.
        let statement_lock = sqlx::query(
            "SELECT statement_id FROM oxsum.plan_statements \
             WHERE statement_id=$1 FOR UPDATE NOWAIT",
        )
        .bind(statement_id)
        .fetch_one(&mut *pin)
        .await;
        pin.rollback().await.unwrap();
        timeout(Duration::from_secs(5), refund_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let result = timeout(Duration::from_secs(5), review_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            statement_lock.is_ok(),
            "review inverted the refund lock order"
        );
        assert_eq!(
            (result.statement.total_minor, result.statement.paid_minor),
            if action == "void" {
                (0, 0)
            } else {
                (3_000_000, 2_000_000)
            }
        );
        let journal =
            w.db.plan_journals(w.org, None, 50)
                .await
                .unwrap()
                .journals
                .remove(0);
        assert_eq!(
            (journal.total_minor, journal.settled_minor),
            (3_000_000, 2_000_000)
        );
        let allocations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM oxsum.plan_refund_effects WHERE refund_id=$1")
                .bind(refund.refund_id)
                .fetch_one(&w.pool)
                .await
                .unwrap();
        assert_eq!(allocations, 1);
        let cash: i64 = sqlx::query_scalar(&format!(
            "SELECT SUM(CASE WHEN p.direction='D' THEN p.amount_minor ELSE -p.amount_minor END)::bigint \
             FROM ledger_{0}.postings p JOIN ledger_{0}.accounts a USING(account_index) \
             WHERE a.path='Assets:Cash' AND p.layer='settled'",
            w.wallet.tenant_id()
        ))
        .fetch_one(&w.pool)
        .await
        .unwrap();
        assert_eq!(cash, 2_000_000);
        clean(&w).await;
    }
}

#[tokio::test]
async fn a_locked_oldest_plan_does_not_block_another_due_statement() {
    let _serial = SERIAL.lock().await;
    let Some(a) = world().await else { return };
    let b = world().await.unwrap();
    due(&a, 40).await;
    due(&b, 30).await;
    let pin = pin_plan(&a).await;
    let result = timeout(Duration::from_secs(2), b.db.run_statement_past_due(1)).await;
    pin.rollback().await.unwrap();
    let report = result
        .expect("SKIP LOCKED must not wait for the oldest plan")
        .unwrap();
    assert_eq!((report.flipped, report.suspended), (1, 1));
    assert_eq!(state(&a).await.state, PlanStatementState::Issued);
    assert_eq!(state(&b).await.state, PlanStatementState::PastDue);
    assert!(!suspended(&a).await);
    assert!(suspended(&b).await);
    let report = a.db.run_statement_past_due(1).await.unwrap();
    assert_eq!((report.flipped, report.suspended), (1, 1));
    let replay = a.db.run_statement_past_due(1).await.unwrap();
    assert_eq!((replay.flipped, replay.suspended), (0, 0));
    clean(&a).await;
    clean(&b).await;
}

#[tokio::test]
async fn a_locked_statement_keeps_the_plan_available_and_is_retried_later() {
    let _serial = SERIAL.lock().await;
    let Some(w) = world().await else { return };
    due(&w, 30).await;
    let mut pin = w.pool.begin().await.unwrap();
    sqlx::query("SELECT statement_id FROM oxsum.plan_statements WHERE statement_id=$1 FOR UPDATE")
        .bind(w.statement.statement_id)
        .fetch_one(&mut *pin)
        .await
        .unwrap();
    let report = timeout(Duration::from_secs(2), w.db.run_statement_past_due(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((report.flipped, report.suspended), (0, 0));
    sqlx::query("SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE NOWAIT").bind(w.plan).fetch_one(&mut *pin).await.expect("skipped statement releases its plan lock");
    pin.rollback().await.unwrap();
    let report = w.db.run_statement_past_due(1).await.unwrap();
    assert_eq!((report.flipped, report.suspended), (1, 1));
    clean(&w).await;
}

#[tokio::test]
async fn committed_payment_or_grace_extension_cannot_be_suspended_from_stale_candidates() {
    let _serial = SERIAL.lock().await;
    for paid in [false, true] {
        let Some(w) = world().await else { return };
        due(&w, 30).await;
        let mut pin = pin_plan(&w).await;
        if paid {
            sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'payment_applied',-1000000,'controlled completed payment',$3)")
                .bind(Uuid::new_v4()).bind(w.statement.statement_id).bind(w.order).execute(&mut *pin).await.unwrap();
            sqlx::query("UPDATE oxsum.plan_statements SET state='paid',paid_minor=total_minor,paid_at=now() WHERE statement_id=$1").bind(w.statement.statement_id).execute(&mut *pin).await.unwrap();
        } else {
            sqlx::query("UPDATE oxsum.plan_statements SET due_at=now()+interval '14 days' WHERE statement_id=$1").bind(w.statement.statement_id).execute(&mut *pin).await.unwrap();
        }
        let report = timeout(Duration::from_secs(2), w.db.run_statement_past_due(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!((report.flipped, report.suspended), (0, 0));
        pin.commit().await.unwrap();
        let report = w.db.run_statement_past_due(1).await.unwrap();
        assert_eq!((report.flipped, report.suspended), (0, 0));
        assert!(!suspended(&w).await);
        assert_eq!(
            state(&w).await.state,
            if paid {
                PlanStatementState::Paid
            } else {
                PlanStatementState::Issued
            }
        );
        clean(&w).await;
    }
}

#[tokio::test]
async fn two_sweeps_claim_each_due_statement_once() {
    let _serial = SERIAL.lock().await;
    let Some(w) = world().await else { return };
    due(&w, 30).await;
    let (a, b) = tokio::join!(
        w.db.run_statement_past_due(1),
        w.db.run_statement_past_due(1)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.flipped + b.flipped, 1);
    assert_eq!(a.suspended + b.suspended, 1);
    assert_eq!(state(&w).await.state, PlanStatementState::PastDue);
    clean(&w).await;
}

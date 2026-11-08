//! Statement financial review, multi-debt restoration and retained voids (#820).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;

use oxsum_core::{
    Db, JournalState, NewSoldPlan, NewUser, PeriodAuthorization, PeriodKind, PlanStatement,
    PlanStatementState, Wallet, WalletError,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const FEE: i64 = 2_500_000;
const DEBT: i64 = 1_000_000;
struct World {
    db: Db,
    pool: PgPool,
    owner: String,
    org: Uuid,
    plan: Uuid,
    statement: PlanStatement,
    wallet: Wallet,
}
async fn world(overage: i64) -> Option<World> {
    priced_world(overage, FEE).await
}
async fn priced_world(overage: i64, fee: i64) -> Option<World> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url("statement_review_finance")?;
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
    let wallet = Wallet::open(pool.clone(), &user.organization.tenant_id)
        .await
        .unwrap();
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &NewSoldPlan {
                plan_id: None,
                name: format!("locks-{}", Uuid::new_v4()),
                price_minor: fee,
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
            if fee == 0 {
                PeriodAuthorization::Complimentary(Uuid::new_v4())
            } else {
                PeriodAuthorization::PaymentOrder(Uuid::nil())
            },
        )
        .await
        .unwrap()
        .customer_plan_id;
    if fee > 0 {
        let context = payment::merchant_context(&db, &owner).await;
        let payment = payment::created_order(&db, &context, org, plan).await;
        payment::mark_paid(&db, &context, &payment).await;
        db.activate_plan_period(payment.order_id, plan)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=now()-interval '2 days',period_end=now()-interval '1 day' WHERE customer_plan_id=$1 AND state='active'").bind(plan).execute(&pool).await.unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE organization_id=$1 AND state='active'").bind(org).execute(&pool).await.unwrap();
    if overage > 0 {
        payment::legacy_close(&db, org, plan, overage).await;
    }
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
        statement,
        wallet,
    })
}

async fn due(w: &World, id: Uuid) {
    sqlx::query("UPDATE oxsum.plan_statements SET state='past_due',past_due_at=now(),due_at=now()-interval '30 days' WHERE statement_id=$1").bind(id).execute(&w.pool).await.unwrap();
    sqlx::query(
        "UPDATE oxsum.customer_plans SET dunning_suspended_at=now() WHERE customer_plan_id=$1",
    )
    .bind(w.plan)
    .execute(&w.pool)
    .await
    .unwrap();
}
async fn suspended(w: &World) -> bool {
    sqlx::query_scalar("SELECT dunning_suspended_at IS NOT NULL FROM oxsum.customer_plans WHERE customer_plan_id=$1").bind(w.plan).fetch_one(&w.pool).await.unwrap()
}
async fn net(w: &World, path: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint FROM ledger_{0}.postings p JOIN ledger_{0}.accounts a USING(account_index) JOIN ledger_{0}.entries e USING(entry_id) WHERE a.path=$1 AND p.layer='settled' AND e.log_index IS NOT NULL",w.wallet.tenant_id())).bind(path).fetch_one(&w.pool).await.unwrap()
}
async fn review(
    w: &World,
    id: Uuid,
    action: &str,
    key: &str,
) -> Result<oxsum_core::PlanStatementDetail, WalletError> {
    w.db.release_statement_review(&w.owner, id, action, "source-bound financial review", key)
        .await
}
async fn second_period(w: &World) -> PlanStatement {
    let order = payment::paid_order(&w.db, &w.owner, w.org, w.plan).await;
    w.db.activate_plan_period(order.order_id, w.plan)
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=now()-interval '2 days',period_end=now()-interval '1 day' WHERE customer_plan_id=$1 AND state='active'").bind(w.plan).execute(&w.pool).await.unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE organization_id=$1 AND state='active'").bind(w.org).execute(&w.pool).await.unwrap();
    payment::legacy_close(&w.db, w.org, w.plan, DEBT).await;
    w.db.rollover_due_subscriptions().await.unwrap();
    w.db.plan_statements(w.org, None, 50)
        .await
        .unwrap()
        .statements
        .into_iter()
        .find(|s| s.statement_id != w.statement.statement_id)
        .unwrap()
}
async fn assert_hold(w: &World, allowed: bool) {
    let result = w
        .wallet
        .hold(
            &Uuid::new_v4().to_string(),
            "review admission",
            1,
            OffsetDateTime::now_utc().date(),
        )
        .await;
    if allowed {
        assert!(result.is_ok(), "{result:?}");
    } else {
        assert!(
            matches!(result, Err(WalletError::Forbidden(_))),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn statement_void_reverses_paid_and_partial_journals_without_moving_cash() {
    for overage in [0, DEBT] {
        let Some(w) = world(overage).await else {
            return;
        };
        if overage > 0 {
            due(&w, w.statement.statement_id).await;
        }
        let cash = net(&w, "Assets:Cash").await;
        let before = w.wallet.log_size().await.unwrap();
        let key = Uuid::new_v4().to_string();
        let result = review(&w, w.statement.statement_id, "void", &key)
            .await
            .unwrap();
        assert_eq!(result.statement.state, PlanStatementState::Void);
        assert_eq!(
            (result.statement.total_minor, result.statement.paid_minor),
            (0, 0)
        );
        let journal =
            w.db.plan_journal(w.org, w.statement.journal_id.unwrap())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(journal.journal.state, JournalState::Void);
        assert_eq!(journal.lines.iter().map(|l| l.amount_minor).sum::<i64>(), 0);
        assert_eq!(net(&w, "Assets:Cash").await, cash);
        assert_eq!(net(&w, "Assets:Receivables").await, 0);
        assert_eq!(net(&w, "Income:PlanFees").await, 0);
        assert_eq!(net(&w, "Liabilities:RefundPayable").await, FEE);
        assert!(!suspended(&w).await);
        assert_eq!(
            w.db.plan_credit_standing(w.org)
                .await
                .unwrap()
                .unwrap()
                .outstanding_minor,
            0
        );
        assert_eq!(
            review(&w, w.statement.statement_id, "void", &key)
                .await
                .unwrap()
                .statement
                .state,
            PlanStatementState::Void
        );
        assert_eq!(w.wallet.log_size().await.unwrap(), before + 1);
        assert!(
            w.db.release_statement_review(
                &w.owner,
                w.statement.statement_id,
                "void",
                "changed reason",
                &key
            )
            .await
            .is_err()
        );
        let plan_state: String =
            sqlx::query_scalar("SELECT state FROM oxsum.customer_plans WHERE customer_plan_id=$1")
                .bind(w.plan)
                .fetch_one(&w.pool)
                .await
                .unwrap();
        assert_eq!(plan_state, "past_due");
    }
}

#[tokio::test]
async fn grace_extension_preserves_other_overdue_debt_and_manual_suspension() {
    let Some(w) = world(DEBT).await else { return };
    let second = second_period(&w).await;
    w.wallet
        .top_up("review-wallet", 100, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    due(&w, w.statement.statement_id).await;
    due(&w, second.statement_id).await;
    // Effective past-grace debt blocks new spending even before the worker
    // stamps its flag; ordinary live statements share the legacy fallback rule.
    sqlx::query(
        "UPDATE oxsum.customer_plans SET dunning_suspended_at=NULL WHERE customer_plan_id=$1",
    )
    .bind(w.plan)
    .execute(&w.pool)
    .await
    .unwrap();
    assert_hold(&w, false).await;
    review(
        &w,
        w.statement.statement_id,
        "extendGrace",
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    assert!(suspended(&w).await);
    assert_hold(&w, false).await;
    review(
        &w,
        second.statement_id,
        "extendGrace",
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    assert!(!suspended(&w).await);
    assert_hold(&w, true).await;
    sqlx::query("UPDATE oxsum.organizations SET suspended_at=now() WHERE organization_id=$1")
        .bind(w.org)
        .execute(&w.pool)
        .await
        .unwrap();
    due(&w, second.statement_id).await;
    review(
        &w,
        second.statement_id,
        "extendGrace",
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    assert!(!suspended(&w).await);
    assert_hold(&w, false).await;
}

#[tokio::test]
async fn voiding_one_journal_retains_other_debt_until_the_last_source_clears() {
    let Some(w) = world(DEBT).await else { return };
    let second = second_period(&w).await;
    due(&w, w.statement.statement_id).await;
    due(&w, second.statement_id).await;
    review(
        &w,
        w.statement.statement_id,
        "void",
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    assert!(suspended(&w).await);
    assert_eq!(
        w.db.plan_credit_standing(w.org)
            .await
            .unwrap()
            .unwrap()
            .outstanding_minor,
        DEBT
    );
    w.db.void_invoice_journal(
        &w.owner,
        &w.wallet,
        second.journal_id.unwrap(),
        "last debt financial void",
        &Uuid::new_v4().to_string(),
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    assert!(!suspended(&w).await);
    assert_eq!(net(&w, "Assets:Receivables").await, 0);
    assert_eq!(net(&w, "Liabilities:RefundPayable").await, 2 * FEE);
}

async fn legacy_document_void(w: &World) {
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) SELECT gen_random_uuid(),statement_id,kind,-amount_minor,'historical document-only reversal',line_id FROM oxsum.plan_statement_lines WHERE statement_id=$1").bind(w.statement.statement_id).execute(&w.pool).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_statements SET state='void',total_minor=0,paid_minor=0,voided_at=now(),void_reason='old review writer' WHERE statement_id=$1").bind(w.statement.statement_id).execute(&w.pool).await.unwrap();
}

#[tokio::test]
async fn retained_document_only_void_stays_visible_and_can_be_financially_reconciled() {
    let Some(w) = world(DEBT).await else { return };
    due(&w, w.statement.statement_id).await;
    legacy_document_void(&w).await;
    sqlx::query(
        "UPDATE oxsum.customer_plans SET dunning_suspended_at=NULL WHERE customer_plan_id=$1",
    )
    .bind(w.plan)
    .execute(&w.pool)
    .await
    .unwrap();
    // A retained statement's due date wins even if current plan net terms
    // change: only a missing original document uses that configured fallback.
    sqlx::query("UPDATE oxsum.customer_plans SET net_terms_days=365 WHERE customer_plan_id=$1")
        .bind(w.plan)
        .execute(&w.pool)
        .await
        .unwrap();
    let standing = w.db.plan_credit_standing(w.org).await.unwrap().unwrap();
    assert_eq!(standing.outstanding_minor, DEBT);
    assert!(standing.next_due_at.unwrap() < OffsetDateTime::now_utc());
    w.wallet
        .top_up("orphan-funding", 100, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    assert_hold(&w, false).await;
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM oxsum.plan_statement_lines WHERE statement_id=$1")
            .bind(w.statement.statement_id)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    review(
        &w,
        w.statement.statement_id,
        "void",
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    assert_eq!(net(&w, "Assets:Receivables").await, 0);
    assert!(!suspended(&w).await);
    assert_hold(&w, true).await;
    let after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM oxsum.plan_statement_lines WHERE statement_id=$1")
            .bind(w.statement.statement_id)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(count, after);
}

#[tokio::test]
async fn foreign_chargeback_document_refuses_without_partial_financial_effects() {
    let Some(w) = world(DEBT).await else { return };
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'dispute_chargeback',500000,'another journal chargeback',$3)").bind(Uuid::new_v4()).bind(w.statement.statement_id).bind(Uuid::new_v4()).execute(&w.pool).await.unwrap();
    sqlx::query(
        "UPDATE oxsum.plan_statements SET total_minor=total_minor+500000 WHERE statement_id=$1",
    )
    .bind(w.statement.statement_id)
    .execute(&w.pool)
    .await
    .unwrap();
    let before = w.wallet.log_size().await.unwrap();
    assert!(matches!(
        review(
            &w,
            w.statement.statement_id,
            "void",
            &Uuid::new_v4().to_string()
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    assert_eq!(net(&w, "Assets:Receivables").await, -DEBT);
}

#[tokio::test]
async fn failed_statement_write_rolls_back_journal_ledger_and_review_audit() {
    let Some(w) = world(DEBT).await else { return };
    let function = format!("fail_review_{}", w.statement.statement_id.simple());
    sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.statement_id='{}'::uuid AND NEW.state='void' THEN RAISE EXCEPTION 'injected review write failure'; END IF; RETURN NEW; END $$",w.statement.statement_id)).execute(&w.pool).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE UPDATE ON oxsum.plan_statements FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()" )).execute(&w.pool).await.unwrap();
    let before = w.wallet.log_size().await.unwrap();
    let key = Uuid::new_v4().to_string();
    assert!(
        review(&w, w.statement.statement_id, "void", &key)
            .await
            .is_err()
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    assert_eq!(net(&w, "Assets:Receivables").await, -DEBT);
    let audits:i64=sqlx::query_scalar("SELECT COUNT(*) FROM oxsum.finance_audit WHERE action='statement.release' AND idempotency_key=$1").bind(&key).fetch_one(&w.pool).await.unwrap();
    assert_eq!(audits, 0);
    sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.plan_statements"))
        .execute(&w.pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        review(&w, w.statement.statement_id, "void", &key),
        review(&w, w.statement.statement_id, "void", &key)
    );
    assert_eq!(a.unwrap().statement.state, PlanStatementState::Void);
    assert_eq!(b.unwrap().statement.state, PlanStatementState::Void);
    assert_eq!(w.wallet.log_size().await.unwrap(), before + 1);
}

#[tokio::test]
async fn retained_void_debt_counts_against_a_renewed_plans_credit_limit() {
    let Some(w) = world(DEBT).await else { return };
    legacy_document_void(&w).await;
    let order = payment::paid_order(&w.db, &w.owner, w.org, w.plan).await;
    w.db.activate_plan_period(order.order_id, w.plan)
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET consumed_minor=quota_minor WHERE organization_id=$1 AND state='active'").bind(w.org).execute(&w.pool).await.unwrap();
    sqlx::query("UPDATE oxsum.customer_plans SET credit_limit_minor=$2 WHERE customer_plan_id=$1")
        .bind(w.plan)
        .bind(DEBT)
        .execute(&w.pool)
        .await
        .unwrap();
    assert!(!suspended(&w).await);
    assert!(matches!(
        w.db.admit_subscription_draw(w.org, &Uuid::new_v4().to_string(), 1)
            .await,
        Err(WalletError::InsufficientFunds)
    ));
    sqlx::query("UPDATE oxsum.customer_plans SET credit_limit_minor=$2 WHERE customer_plan_id=$1")
        .bind(w.plan)
        .bind(DEBT + 1)
        .execute(&w.pool)
        .await
        .unwrap();
    assert_eq!(
        w.db.admit_subscription_draw(w.org, &Uuid::new_v4().to_string(), 1)
            .await
            .unwrap()
            .drawn_minor,
        0
    );
    assert_eq!(
        w.db.plan_credit_standing(w.org)
            .await
            .unwrap()
            .unwrap()
            .outstanding_minor,
        DEBT
    );
}

#[tokio::test]
async fn zero_journal_void_audits_no_invented_entry_and_rejects_changed_key_terms() {
    let Some(w) = priced_world(0, 0).await else {
        return;
    };
    let before = w.wallet.log_size().await.unwrap();
    let key = Uuid::new_v4().to_string();
    review(&w, w.statement.statement_id, "void", &key)
        .await
        .unwrap();
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    let detail:serde_json::Value=sqlx::query_scalar("SELECT detail FROM oxsum.finance_audit WHERE action='statement.release' AND idempotency_key=$1").bind(&key).fetch_one(&w.pool).await.unwrap();
    assert!(detail["reversalEntryId"].is_null());
    assert_eq!(
        detail["journalId"],
        w.statement.journal_id.unwrap().to_string()
    );
    for (statement, action) in [
        (w.statement.statement_id, "extendGrace"),
        (Uuid::new_v4(), "void"),
    ] {
        assert!(matches!(
            review(&w, statement, action, &key).await,
            Err(WalletError::Conflict(_))
        ));
    }
}

#[tokio::test]
async fn malformed_legacy_reversal_and_foreign_wallet_refuse_before_effects() {
    let Some(w) = world(DEBT).await else { return };
    let user =
        w.db.register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "another wallet fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let other = Wallet::open(w.pool.clone(), &user.organization.tenant_id)
        .await
        .unwrap();
    let before = w.wallet.log_size().await.unwrap();
    let foreign_before = other.log_size().await.unwrap();
    assert!(matches!(
        w.db.void_invoice_journal(
            &w.owner,
            &other,
            w.statement.journal_id.unwrap(),
            "wrong wallet",
            &Uuid::new_v4().to_string(),
            OffsetDateTime::now_utc().date()
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(other.log_size().await.unwrap(), foreign_before);
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) SELECT gen_random_uuid(),statement_id,kind,-amount_minor+CASE kind WHEN 'plan_fee' THEN 1 WHEN 'usage' THEN -1 ELSE 0 END,'inconsistent historical reversal',line_id FROM oxsum.plan_statement_lines WHERE statement_id=$1").bind(w.statement.statement_id).execute(&w.pool).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_statements SET state='void',total_minor=0,paid_minor=0,voided_at=now(),void_reason='old writer' WHERE statement_id=$1").bind(w.statement.statement_id).execute(&w.pool).await.unwrap();
    assert!(matches!(
        review(
            &w,
            w.statement.statement_id,
            "void",
            &Uuid::new_v4().to_string()
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    assert_eq!(net(&w, "Assets:Receivables").await, -DEBT);
}

//! Plan statements, overdue handling and credit (issue #612, T21, A23): a
//! period close issues the statement the journal wraps, a payment reconciles
//! it (full and partial), the `statement-past-due` sweep walks
//! issued → past_due → grace → suspend, a staff release-review restores and
//! audits, the credit limit refuses at admission, and an issued statement is
//! immutable. Needs DATABASE_URL; skips without it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/plan_payment.rs"]
mod plan_payment;
#[path = "support/integration.rs"]
mod setup;

use oxsum_core::{
    CustomerPlan, Db, NewSoldPlan, PeriodAuthorization, PeriodKind, PlanStatement,
    PlanStatementState, SoldPlan, StatementLineKind, Tenants, Wallet,
};
use sqlx::PgPool;
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("plan_statements")
}

/// A migrated database, the staff owner token, a registered customer org, its
/// wallet and the business singleton.
struct World {
    db: Db,
    pool: PgPool,
    owner: String,
    organization_id: Uuid,
    wallet: Arc<Wallet>,
    business_id: Uuid,
}

async fn world() -> Option<World> {
    let url = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let owner = setup::owner(&db).await;
    let registration = db
        .register(oxsum_core::NewUser {
            email: format!(
                "plan_stmt_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "plan statement fixture password".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    let business_id = db.default_business().await.expect("a business");
    let tenants = Tenants::new(pool.clone());
    let wallet = tenants
        .get(&registration.organization.tenant_id)
        .await
        .expect("wallet opens");
    Some(World {
        db,
        pool,
        owner,
        organization_id: registration.organization.id,
        wallet,
        business_id,
    })
}

/// A published sold version — the terms a bind names.
async fn sold(
    db: &Db,
    business_id: Uuid,
    name: &str,
    price_minor: i64,
    quota: Option<i64>,
    kind: PeriodKind,
    overage: bool,
) -> SoldPlan {
    db.publish_sold_plan(
        business_id,
        &NewSoldPlan {
            plan_id: None,
            name: name.into(),
            price_minor,
            currency: "USD".into(),
            quota_minor: quota,
            period_kind: kind,
            overage_enabled: overage,
            sort_order: 0,
        },
    )
    .await
    .expect("the version publishes")
}

/// Force the live plan's open period and quota window to have ended — a due
/// boundary for the rollover pass.
async fn end_period_ago(pool: &PgPool, organization_id: Uuid, ago_secs: i64) {
    sqlx::query(
        "UPDATE oxsum.org_subscriptions \
         SET current_period_end = now() - ($2 || ' seconds')::interval \
         WHERE organization_id = $1 AND state = 'active'",
    )
    .bind(organization_id)
    .bind(ago_secs.to_string())
    .execute(pool)
    .await
    .expect("the subscription window moves");
    sqlx::query(
        "UPDATE oxsum.plan_periods SET period_end = now() - ($2 || ' seconds')::interval \
         WHERE state = 'active' AND customer_plan_id IN ( \
             SELECT customer_plan_id FROM oxsum.customer_plans WHERE organization_id = $1)",
    )
    .bind(organization_id)
    .bind(ago_secs.to_string())
    .execute(pool)
    .await
    .expect("the plan period moves");
}

/// Exact paid order through the collection quote and local verified-event fixture.
async fn plan_order(world: &World, customer_plan_id: Uuid, amount_minor: i64) -> Uuid {
    let order = plan_payment::paid_order(
        &world.db,
        &world.owner,
        world.organization_id,
        customer_plan_id,
    )
    .await;
    assert_eq!(
        order.amount_minor, amount_minor,
        "fixture pays the exact sold fee"
    );
    order.order_id
}

/// The org's plan statements, newest first.
async fn statements(db: &Db, organization_id: Uuid) -> Vec<PlanStatement> {
    db.plan_statements(organization_id, None, 50)
        .await
        .expect("statements read")
        .statements
}

/// One statement's detail, unwrapped.
async fn detail(
    db: &Db,
    organization_id: Uuid,
    statement_id: Uuid,
) -> oxsum_core::PlanStatementDetail {
    db.plan_statement(organization_id, statement_id)
        .await
        .expect("the statement reads")
        .expect("a standing statement")
}

/// Bind the org to a priced plan and open its period under a paid order.
async fn bound_plan(world: &World, price: i64, overage: bool) -> (SoldPlan, CustomerPlan) {
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        price,
        Some(10 * ONE),
        PeriodKind::CalendarMonth,
        overage,
    )
    .await;
    let plan = world
        .db
        .bind_customer_plan(
            world.organization_id,
            pro.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    (pro, plan)
}

// ── A23.1: a period close issues the statement the journal wraps ────────────

#[tokio::test]
async fn a_period_close_issues_the_statement() {
    let Some(world) = world().await else { return };
    let (_pro, plan) = bound_plan(&world, 2500, false).await;
    // The order activates period 0 — its prepay will cover the close's bill.
    world
        .db
        .activate_plan_period(
            plan_order(&world, plan.customer_plan_id, 2500).await,
            plan.customer_plan_id,
        )
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();

    let statements = statements(&world.db, world.organization_id).await;
    assert_eq!(statements.len(), 1, "one statement per closed period");
    let statement = &statements[0];
    assert_eq!(statement.period_index, Some(0));
    assert_eq!(statement.customer_plan_id, Some(plan.customer_plan_id));
    // The journal's bill restated in customer vocabulary: a base_fee journal
    // line lands as `planFee` on the statement.
    let bill = detail(&world.db, world.organization_id, statement.statement_id).await;
    assert_eq!(bill.statement.total_minor, 2500);
    assert!(
        bill.lines
            .iter()
            .any(|l| l.kind == StatementLineKind::PlanFee && l.amount_minor == 2500)
    );
    // Issued with terms — `dueAt` is `issuedAt + netTerms` (default 14).
    assert!(statement.issued_at.is_some());
    assert!(statement.due_at.is_some());
}

// ── A23.2: a payment reconciles the statement — full and partial ────────────

#[tokio::test]
async fn an_exact_paid_period_reconciles_its_statement() {
    let Some(world) = world().await else { return };
    let (_, plan) = bound_plan(&world, 2500, false).await;
    let order = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order, plan.customer_plan_id)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let statement = statements(&world.db, world.organization_id).await.remove(0);
    let bill = detail(&world.db, world.organization_id, statement.statement_id).await;
    assert_eq!(bill.statement.state, PlanStatementState::Paid);
    assert_eq!(bill.statement.paid_minor, 2500);
    assert_eq!(bill.statement.paid_order_id, Some(order));
    let payments = bill
        .lines
        .iter()
        .filter(|line| line.kind == StatementLineKind::PaymentApplied)
        .collect::<Vec<_>>();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].amount_minor, -2500);
    assert_eq!(payments[0].source_ref, Some(order));
    let journal = world
        .db
        .plan_journal(world.organization_id, bill.statement.journal_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bill.statement.total_minor, journal.journal.total_minor);
    assert_eq!(bill.statement.paid_minor, journal.journal.settled_minor);

    // Rollover and payment recovery both revisit the same committed period.
    world.db.rollover_due_subscriptions().await.unwrap();
    world
        .db
        .activate_plan_period(order, plan.customer_plan_id)
        .await
        .unwrap();
    let replayed = detail(&world.db, world.organization_id, statement.statement_id).await;
    assert_eq!(replayed.statement.state, PlanStatementState::Paid);
    assert_eq!(replayed.statement.paid_minor, 2500);
    assert_eq!(replayed.lines.len(), bill.lines.len());
}

#[tokio::test]
async fn renewal_does_not_clear_an_outstanding_statement() {
    let Some(world) = world().await else { return };
    let (_, plan) = bound_plan(&world, 2500, true).await;
    let order = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order, plan.customer_plan_id)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    plan_payment::legacy_close(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1000,
    )
    .await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let statement = statements(&world.db, world.organization_id).await.remove(0);
    let renewal = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(renewal, plan.customer_plan_id)
        .await
        .unwrap();
    let bill = detail(&world.db, world.organization_id, statement.statement_id).await;
    assert_eq!(bill.statement.state, PlanStatementState::Issued);
    assert_eq!(bill.statement.paid_minor, 2500);
    assert_eq!(bill.statement.total_minor - bill.statement.paid_minor, 1000);
    assert_ne!(bill.statement.paid_order_id, Some(renewal));
}

// ── A23.3: overdue → grace → suspend, and release-review restores ────────────

#[tokio::test]
async fn overdue_grace_and_suspend_transitions() {
    let Some(world) = world().await else { return };
    let (_pro, plan) = bound_plan(&world, 2500, true).await;
    world
        .db
        .activate_plan_period(
            plan_payment::paid_order(
                &world.db,
                &world.owner,
                world.organization_id,
                plan.customer_plan_id,
            )
            .await
            .order_id,
            plan.customer_plan_id,
        )
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    plan_payment::legacy_close(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1000,
    )
    .await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let statement = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.period_index == Some(0))
        .unwrap();

    // The statement's due date passes: the sweep flips `issued → past_due`.
    sqlx::query(
        "UPDATE oxsum.plan_statements SET due_at = now() - interval '1 day' \
                 WHERE statement_id=$1",
    )
    .bind(statement.statement_id)
    .execute(&world.pool)
    .await
    .unwrap();
    let report = world.db.run_statement_past_due(50).await.unwrap();
    assert_eq!(report.flipped, 1);
    let flipped = detail(&world.db, world.organization_id, statement.statement_id).await;
    assert_eq!(flipped.statement.state, PlanStatementState::PastDue);
    assert!(flipped.statement.past_due_at.is_some());
    // Inside grace — `due_at + grace_days` (default 7) has not passed — the
    // plan stays un-suspended: `dunning_suspended_at` is still NULL.
    let suspended: Option<OffsetDateTime> = sqlx::query_scalar(
        "SELECT dunning_suspended_at FROM oxsum.customer_plans WHERE customer_plan_id=$1",
    )
    .bind(plan.customer_plan_id)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert!(suspended.is_none(), "inside grace the plan still serves");

    // Past grace — push `due_at` out beyond `grace_days`: the next sweep
    // stamps `dunning_suspended_at`, the wallet-side admission gate.
    sqlx::query(
        "UPDATE oxsum.plan_statements SET due_at = now() - interval '30 days' \
                 WHERE statement_id=$1",
    )
    .bind(statement.statement_id)
    .execute(&world.pool)
    .await
    .unwrap();
    let report = world.db.run_statement_past_due(50).await.unwrap();
    assert_eq!(report.suspended, 1);
    let suspended: Option<OffsetDateTime> = sqlx::query_scalar(
        "SELECT dunning_suspended_at FROM oxsum.customer_plans WHERE customer_plan_id=$1",
    )
    .bind(plan.customer_plan_id)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert!(
        suspended.is_some(),
        "past grace the wallet-side admission suspends"
    );

    // The suspended plan refuses a new wallet hold — quota (already paid
    // for) still serves, but the wallet-side overage is cut.
    let refused = world
        .wallet
        .hold(
            &format!("suspend-hold-{}", Uuid::new_v4().simple()),
            "a suspended plan's wallet draw",
            ONE,
            OffsetDateTime::now_utc().date(),
        )
        .await;
    assert!(matches!(
        refused,
        Err(oxsum_core::WalletError::Forbidden(_))
    ));
}

#[tokio::test]
async fn a_staff_release_review_restores_and_audits() {
    let Some(world) = world().await else { return };
    let (_pro, plan) = bound_plan(&world, 2500, true).await;
    world
        .db
        .activate_plan_period(
            plan_payment::paid_order(
                &world.db,
                &world.owner,
                world.organization_id,
                plan.customer_plan_id,
            )
            .await
            .order_id,
            plan.customer_plan_id,
        )
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    plan_payment::legacy_close(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1000,
    )
    .await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let statement = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.period_index == Some(0))
        .unwrap();
    // Drive the statement past due.
    sqlx::query(
        "UPDATE oxsum.plan_statements SET due_at = now() - interval '1 day' \
                 WHERE statement_id=$1",
    )
    .bind(statement.statement_id)
    .execute(&world.pool)
    .await
    .unwrap();
    world.db.run_statement_past_due(50).await.unwrap();
    let past_due = detail(&world.db, world.organization_id, statement.statement_id).await;
    assert_eq!(past_due.statement.state, PlanStatementState::PastDue);

    // The staff release-review restores `past_due → issued`, re-opening the
    // grace window — the receivable stands, the customer gets fresh terms.
    let released = world
        .db
        .release_statement_review(
            &world.owner,
            statement.statement_id,
            "extendGrace",
            "customer called; payment in flight",
            &format!("release-{}", Uuid::new_v4().simple()),
        )
        .await
        .expect("the release lands");
    assert_eq!(released.statement.state, PlanStatementState::Issued);
    // Fresh terms: `due_at` re-opens from `now`.
    assert!(released.statement.due_at.unwrap() > OffsetDateTime::now_utc());

    // Every release is audited — the actor, the action and the reason.
    let audit: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM oxsum.finance_audit \
         WHERE action='statement.release' AND target=$1",
    )
    .bind(statement.statement_id.to_string())
    .fetch_optional(&world.pool)
    .await
    .unwrap();
    assert!(audit.is_some(), "the release is audited");
}

// ── A23.4: the credit limit refuses at admission ────────────────────────────

#[tokio::test]
async fn the_credit_limit_refuses_wallet_overage() {
    let Some(world) = world().await else { return };
    // An overage-enabled plan whose quota is exhausted — the whole draw is
    // wallet-side overage the credit limit caps.
    let (_pro, plan) = bound_plan(&world, 2500, true).await;
    world
        .db
        .activate_plan_period(
            plan_order(&world, plan.customer_plan_id, 2500).await,
            plan.customer_plan_id,
        )
        .await
        .unwrap();
    // Exhaust the quota so every draw is entirely wallet-side overage.
    sqlx::query(
        "UPDATE oxsum.org_subscriptions SET consumed_minor=quota_minor \
         WHERE organization_id=$1 AND state='active'",
    )
    .bind(world.organization_id)
    .execute(&world.pool)
    .await
    .unwrap();

    // A generous cap admits the whole draw as overage — quota is exhausted,
    // so the freeze is all wallet-side spend within the 2-credit limit.
    sqlx::query("UPDATE oxsum.customer_plans SET credit_limit_minor=$2 WHERE customer_plan_id=$1")
        .bind(plan.customer_plan_id)
        .bind(2 * ONE)
        .execute(&world.pool)
        .await
        .unwrap();
    let admitted = world
        .db
        .admit_subscription_draw(world.organization_id, &Uuid::new_v4().to_string(), ONE)
        .await;
    assert!(admitted.is_ok(), "an overage under the cap admits");

    // A tighter cap refuses: `credit_limit_minor` below the overage the draw
    // needs is what the admission check refuses — outstanding (zero open
    // statements, zero holds) plus this overage exceeds the cap.
    sqlx::query("UPDATE oxsum.customer_plans SET credit_limit_minor=$2 WHERE customer_plan_id=$1")
        .bind(plan.customer_plan_id)
        .bind(1i64)
        .execute(&world.pool)
        .await
        .unwrap();
    let refused = world
        .db
        .admit_subscription_draw(world.organization_id, &Uuid::new_v4().to_string(), ONE)
        .await;
    assert!(matches!(
        refused,
        Err(oxsum_core::WalletError::InsufficientFunds)
    ));
}

// ── A23.5: an issued statement is immutable ─────────────────────────────────

#[tokio::test]
async fn an_issued_statement_refuses_edits_and_deletes() {
    let Some(world) = world().await else { return };
    let (_pro, plan) = bound_plan(&world, 2500, true).await;
    world
        .db
        .activate_plan_period(
            plan_payment::paid_order(
                &world.db,
                &world.owner,
                world.organization_id,
                plan.customer_plan_id,
            )
            .await
            .order_id,
            plan.customer_plan_id,
        )
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    plan_payment::legacy_close(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1000,
    )
    .await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let statement = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.period_index == Some(0))
        .unwrap();
    assert_eq!(statement.state, PlanStatementState::Issued);

    // A terms edit is refused outright; DELETE is refused the same way.
    let updated =
        sqlx::query("UPDATE oxsum.plan_statements SET total_minor = 1 WHERE statement_id = $1")
            .bind(statement.statement_id)
            .execute(&world.pool)
            .await;
    assert!(updated.is_err(), "an issued statement's terms refuse edits");
    let deleted = sqlx::query("DELETE FROM oxsum.plan_statements WHERE statement_id = $1")
        .bind(statement.statement_id)
        .execute(&world.pool)
        .await;
    assert!(deleted.is_err(), "statement history is retained");
    // Its lines refuse edits and deletes too — corrections append, nothing
    // rewrites.
    let line_edit = sqlx::query(
        "UPDATE oxsum.plan_statement_lines SET amount_minor = 0 WHERE statement_id = $1",
    )
    .bind(statement.statement_id)
    .execute(&world.pool)
    .await;
    assert!(
        line_edit.is_err(),
        "statement lines are immutable once written"
    );
    let line_delete = sqlx::query("DELETE FROM oxsum.plan_statement_lines WHERE statement_id = $1")
        .bind(statement.statement_id)
        .execute(&world.pool)
        .await;
    assert!(line_delete.is_err(), "statement lines are never deleted");
    // A backward step the guard does not permit: issued cannot return to open.
    let backward =
        sqlx::query("UPDATE oxsum.plan_statements SET state = 'open' WHERE statement_id = $1")
            .bind(statement.statement_id)
            .execute(&world.pool)
            .await;
    assert!(backward.is_err(), "the state machine only walks forward");
}

#[tokio::test]
async fn a_partial_payment_line_does_not_change_the_statement_charge_total() {
    let Some(world) = world().await else { return };
    let (_, plan) = bound_plan(&world, 2500, true).await;
    let order = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order, plan.customer_plan_id)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    plan_payment::legacy_close(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1000,
    )
    .await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let statement = statements(&world.db, world.organization_id).await.remove(0);
    let mut tx = world.pool.begin().await.unwrap();
    // Exercise the document invariant with an appended credit. The later
    // accounting repair owns the coordinated journal/cash correction path.
    sqlx::query("INSERT INTO oxsum.plan_statement_lines (line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'discount',-100,'document invariant fixture',$3)")
        .bind(Uuid::new_v4()).bind(statement.statement_id).bind(Uuid::new_v4()).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_statements SET total_minor=3400 WHERE statement_id=$1")
        .bind(statement.statement_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let paid: i64 =
        sqlx::query_scalar("SELECT paid_minor FROM oxsum.plan_statements WHERE statement_id=$1")
            .bind(statement.statement_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(paid, 2500);
    assert!(
        sqlx::query("UPDATE oxsum.plan_statements SET total_minor=900 WHERE statement_id=$1")
            .bind(statement.statement_id)
            .execute(&mut *tx)
            .await
            .is_err(),
        "payments cannot be subtracted from gross charges a second time"
    );
    tx.rollback().await.unwrap();
}

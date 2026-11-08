//! Plan refund / settled-usage conservation across journals (issue #779): each
//! hypothesis is a real-ledger fixture that drives open → action → close and
//! asserts the conserved figure — the receivable, the recognized income, the
//! note's terminal state and the customer statement's total. A hypothesis that
//! holds under the fixture is disproved; a real violation gets a named
//! reproduction and the correction lands source-linked and idempotent. Needs
//! DATABASE_URL; skips without it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/plan_payment.rs"]
mod plan_payment;
#[path = "support/integration.rs"]
mod setup;

use oxsum_core::{
    CustomerPlan, Db, JournalLineKind, JournalState, NewSoldPlan, NewUser, PeriodAuthorization,
    PeriodKind, SoldPlan, Tenants, Wallet,
};
use sqlx::PgPool;
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("plan_refund_conservation")
}

/// A migrated database, the staff owner token, a registered customer org, its
/// wallet and the business singleton — the four facts every plan call needs.
struct World {
    db: Db,
    pool: PgPool,
    owner: String,
    organization_id: Uuid,
    tenant: String,
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
        .register(NewUser {
            email: format!(
                "cons_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "conservation fixture password".to_owned(),
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
        tenant: registration.organization.tenant_id,
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

/// A quote-backed order paid through the current verified-event boundary.
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
        "the sold fee is authoritative"
    );
    order.order_id
}

/// The org's journals, newest first.
async fn journals(db: &Db, organization_id: Uuid) -> Vec<oxsum_core::InvoiceJournal> {
    db.plan_journals(organization_id, None, 50)
        .await
        .expect("journals read")
        .journals
}

/// One period's journal detail, unwrapped.
async fn detail(
    db: &Db,
    organization_id: Uuid,
    journal_id: Uuid,
) -> oxsum_core::InvoiceJournalDetail {
    db.plan_journal(organization_id, journal_id)
        .await
        .expect("the journal reads")
        .expect("a standing journal")
}

/// The org's plan statements, newest first.
async fn statements(db: &Db, organization_id: Uuid) -> Vec<oxsum_core::PlanStatement> {
    db.plan_statements(organization_id, None, 50)
        .await
        .expect("statements read")
        .statements
}

/// The settled net balance of one ledger account path in the tenant's schema —
/// credits positive, debits negative, the same convention `settled_net_in`
/// uses for the wallet's own reads.
async fn ledger_net(pool: &PgPool, tenant: &str, path: &str) -> i64 {
    let sql = format!(
        "SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor \
                 ELSE -p.amount_minor END),0)::bigint \
         FROM ledger_{tenant}.postings p \
         JOIN ledger_{tenant}.accounts a ON a.account_index=p.account_index \
         JOIN ledger_{tenant}.entries e ON e.entry_id=p.entry_id \
         WHERE a.path=$1 AND p.layer='settled' AND e.log_index IS NOT NULL"
    );
    sqlx::query_scalar(&sql)
        .bind(path)
        .fetch_one(pool)
        .await
        .expect("the account net reads")
}

/// A real wallet hold and settlement, with its actual entry linked to usage.
async fn settle_usage(world: &World, charged_minor: i64, on_credit: bool) {
    let on = OffsetDateTime::now_utc().date();
    if on_credit {
        world
            .wallet
            .set_credit_limit("usage-credit", charged_minor * 2, on)
            .await
            .unwrap();
    } else {
        world
            .wallet
            .top_up("usage-funding", charged_minor * 2, on)
            .await
            .unwrap();
    }
    let hold = format!("settled-overflow-{}", Uuid::new_v4());
    world
        .wallet
        .hold(&hold, "overflow fixture", charged_minor, on)
        .await
        .unwrap();
    let receipt = world
        .wallet
        .settle(&hold, "overflow fixture", charged_minor, on)
        .await
        .unwrap();
    let request = Uuid::new_v4().to_string();
    world
        .db
        .record_usage(&oxsum_core::UsageRow {
            request_id: request.clone(),
            tenant_id: world.tenant.clone(),
            key_id: None,
            mandate_id: None,
            model: "fixture".into(),
            channel: "fixture".into(),
            price_version: 1,
            kind: oxsum_core::SettlementKind::Usage,
            entry_id: *receipt.entry_id.as_uuid(),
            status: "settled".into(),
            failure_reason: None,
            usage: oxsum_core::UsageRecord {
                input_tokens: 1,
                output_tokens: 1,
                ..Default::default()
            },
            counts: oxsum_core::CountsProvenance::Provider {
                input: 1,
                output: 1,
            },
            charged_minor,
            freeze_minor: charged_minor,
            upstream_cost_minor: None,
            cost_basis: oxsum_core::CostBasis::Missing,
            upstream_attempts: 1,
        })
        .await
        .unwrap();
    // Deterministic boundary clock: the real settled entry stays unchanged.
    sqlx::query("UPDATE oxsum.usage_records SET settled_at=now()-interval '120 seconds' WHERE request_id=$1")
        .bind(request).execute(&world.pool).await.unwrap();
}

/// Drive a plan through activation and one close, returning its first journal.
async fn closed_first_journal(
    world: &World,
    plan: &CustomerPlan,
    order_id: Uuid,
) -> oxsum_core::InvoiceJournal {
    world
        .db
        .activate_plan_period(order_id, plan.customer_plan_id)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    journals(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|j| j.period_index == 0)
        .expect("period 0 closed a journal")
}

// ── H1: wallet-settled usage must not become a second plan receivable ────────
//
// `journal_closing_period_in` sums the period's settled `charged_minor` into a
// `usage_overage` line; the accrual then books `debit Receivables / credit
// Income:PlanFees` over the charge legs — including the overage the wallet
// already collected. The reproduction: usage income is recognized once at
// settle (`Income:Usage`) and *again* as plan-fee receivable at close. The
// customer is asked to pay the same usage twice.
//
#[tokio::test]
async fn h1_settled_usage_is_recognized_once_not_billed_twice() {
    assert_usage_once(false).await;
}

#[tokio::test]
async fn credit_settled_usage_retains_only_its_original_receivable() {
    assert_usage_once(true).await;
}

async fn assert_usage_once(on_credit: bool) {
    let Some(world) = world().await else { return };
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::CalendarMonth,
        true,
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
    // Activate period 0 first — activation writes `period_start = now`. Back the
    // start a day only after it exists so the usage window stays positive once
    // `end_period_ago` moves just the end to `now - 60s`.
    world
        .db
        .activate_plan_period(
            plan_order(&world, plan.customer_plan_id, 2500).await,
            plan.customer_plan_id,
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.plan_periods SET period_start = period_start - interval '1 day' \
         WHERE customer_plan_id=$1 AND state='active'",
    )
    .bind(plan.customer_plan_id)
    .execute(&world.pool)
    .await
    .unwrap();
    // Settle 3 credits of overage inside the period — the wallet already
    // recognized it as usage income before the close runs.
    settle_usage(&world, 3 * ONE, on_credit).await;
    let receivable_before = ledger_net(&world.pool, &world.tenant, "Assets:Receivables").await;
    let balance_before = world.wallet.available().await.unwrap();
    let usage_before = ledger_net(&world.pool, &world.tenant, "Income:Usage").await;
    assert_eq!(usage_before, 3 * ONE);

    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let journal = journals(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|j| j.period_index == 0)
        .expect("period 0 closed a journal");
    let bill = detail(&world.db, world.organization_id, journal.journal_id).await;
    let overage: i64 = bill
        .lines
        .iter()
        .filter(|l| l.kind == JournalLineKind::UsageOverage)
        .map(|l| l.amount_minor)
        .sum();

    // The conserved figure: usage income is recognized exactly once. The
    // settled turn already credited Income:Usage for 3·ONE; the close must not
    // re-recognize that 3·ONE as plan-fee receivable, or the ledger books the
    // same usage as revenue twice — once earned, once still owed.
    let receivable = ledger_net(&world.pool, &world.tenant, "Assets:Receivables").await;
    let plan_fees = ledger_net(&world.pool, &world.tenant, "Income:PlanFees").await;
    let usage = ledger_net(&world.pool, &world.tenant, "Income:Usage").await;
    eprintln!(
        "H1: overage_line={overage} total={} settled={} receivable={receivable} \
         planFees={plan_fees} usageIncome={usage}",
        journal.total_minor, journal.settled_minor
    );
    assert_eq!(
        overage, 0,
        "settled overage must not become a plan-fee charge leg; \
         receivable={receivable} planFees={plan_fees}"
    );
    // The base fee alone is the charge — nothing else the period consumed is
    // re-billed.
    assert_eq!(journal.total_minor, 2500);
    assert_eq!(journal.settled_minor, 2500);
    assert_eq!(journal.state, JournalState::Paid);
    assert_eq!(receivable, receivable_before);
    assert_eq!(plan_fees, 2500);
    assert_eq!(usage, usage_before);
    assert_eq!(world.wallet.available().await.unwrap(), balance_before);
    if on_credit {
        assert_eq!(world.wallet.credit_used().await.unwrap(), 3 * ONE);
    }
    let statement = statements(&world.db, world.organization_id).await.remove(0);
    assert_eq!(statement.total_minor, 2500);
    assert_eq!(statement.paid_minor, 2500);
    assert_eq!(statement.state, oxsum_core::PlanStatementState::Paid);
    for _ in 0..2 {
        world.db.rollover_due_subscriptions().await.unwrap();
    }
    assert_eq!(journals(&world.db, world.organization_id).await.len(), 1);
    assert_eq!(
        ledger_net(&world.pool, &world.tenant, "Income:PlanFees").await,
        2500
    );
    assert_eq!(
        ledger_net(&world.pool, &world.tenant, "Assets:Receivables").await,
        receivable_before
    );
}

/// Insert a `requested` refund plus an `issued` credit note on the order — the
/// standing mid-period refund a settle already approved but whose journal is
/// not yet written. Returns `(refund_id, note_id)`.
async fn issued_note(
    world: &World,
    order_id: Uuid,
    amount_minor: i64,
    reason: &str,
) -> (Uuid, Uuid) {
    let refund_id = Uuid::new_v4();
    let note_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO oxsum.refunds\
         (refund_id,order_id,organization_id,business_id,amount_minor,\
          display_amount_minor,display_currency,reason,request_key,state)\
         VALUES($1,$2,$3,$4,$5,$6,'USD',$7,$8,'succeeded')",
    )
    .bind(refund_id)
    .bind(order_id)
    .bind(world.organization_id)
    .bind(world.business_id)
    .bind(amount_minor)
    .bind(amount_minor)
    .bind(reason)
    .bind(format!("req-{}", Uuid::new_v4().simple()))
    .execute(&world.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.credit_notes\
         (credit_note_id,organization_id,entry_id,order_id,refund_id,amount_minor,reason,state)\
         VALUES($1,$2,$3,$4,$5,$6,$7,'issued')",
    )
    .bind(note_id)
    .bind(world.organization_id)
    .bind(Uuid::new_v4())
    .bind(order_id)
    .bind(refund_id)
    .bind(amount_minor)
    .bind(reason)
    .execute(&world.pool)
    .await
    .unwrap();
    (refund_id, note_id)
}

async fn note_state(pool: &PgPool, note_id: Uuid) -> String {
    sqlx::query_scalar("SELECT state FROM oxsum.credit_notes WHERE credit_note_id=$1")
        .bind(note_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

// H2/H3's full verified-event, pre/post-close, concurrency and historical
// repair matrix lives in plan_refunds.rs. These short seams retain the original
// source regressions with corrected confirmed-cash expectations.
#[tokio::test]
async fn h2_a_mid_period_refund_discounts_the_closing_journal() {
    let Some(world) = world().await else { return };
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::CalendarMonth,
        false,
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
    let order = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order, plan.customer_plan_id)
        .await
        .unwrap();
    let (refund, note) = issued_note(&world, order, 1000, "confirmed mid-period refund").await;
    let applied = world
        .db
        .apply_refund_to_journal(
            &world.wallet,
            order,
            refund,
            note,
            1000,
            "refund",
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert_eq!(applied, None);
    assert_eq!(note_state(&world.pool, note).await, "applied");
    assert_eq!(
        ledger_net(&world.pool, &world.tenant, "Assets:Cash").await,
        -1500
    );
    assert_eq!(
        ledger_net(&world.pool, &world.tenant, "Liabilities:UnearnedPlanFees").await,
        1500
    );
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let journal = journals(&world.db, world.organization_id).await.remove(0);
    assert_eq!((journal.total_minor, journal.settled_minor), (1500, 1500));
    assert_eq!(
        ledger_net(&world.pool, &world.tenant, "Assets:Receivables").await,
        0
    );
}

#[tokio::test]
async fn h3_a_refund_on_a_partial_legacy_journal_returns_all_confirmed_cash() {
    let Some(world) = world().await else { return };
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::Anniversary,
        true,
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
    let order = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order, plan.customer_plan_id)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    let journal_id = plan_payment::legacy_close(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1500,
    )
    .await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let (refund, note) = issued_note(&world, order, 2000, "confirmed partial-journal refund").await;
    world
        .db
        .apply_refund_to_journal(
            &world.wallet,
            order,
            refund,
            note,
            2000,
            "refund",
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let journal = detail(&world.db, world.organization_id, journal_id)
        .await
        .journal;
    assert_eq!((journal.total_minor, journal.settled_minor), (2000, 500));
    assert_eq!(
        ledger_net(&world.pool, &world.tenant, "Assets:Cash").await,
        -500
    );
    assert_eq!(
        ledger_net(&world.pool, &world.tenant, "Assets:Receivables").await,
        -1500
    );
    let statement = statements(&world.db, world.organization_id).await.remove(0);
    assert_eq!((statement.total_minor, statement.paid_minor), (2000, 500));
}

// ── H4: the statement's charge total excludes payment lines ─────────────────
//
// `issue_statement_for_journal_in` sums the journal's charge lines into
// `total_minor`; `paymentApplied`/`disputeChargeback` append later. A correct
// total is the charge the customer owes — payment lines must only move
// `paid_minor`, never inflate `total_minor`.
#[tokio::test]
async fn h4_statement_charge_total_excludes_payment_lines() {
    let Some(world) = world().await else { return };
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::Anniversary,
        true,
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
    let order_id = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order_id, plan.customer_plan_id)
        .await
        .unwrap();
    // Historical documents retain a partial journal balance. Seed the old
    // recorded shape explicitly; new closes never duplicate settled usage.
    end_period_ago(&world.pool, world.organization_id, 60).await;
    plan_payment::legacy_close(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1000,
    )
    .await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let journal = journals(&world.db, world.organization_id).await.remove(0);
    let statement = statements(&world.db, world.organization_id).await.remove(0);
    assert_eq!(journal.total_minor, 3500);
    assert_eq!(journal.settled_minor, 2500);
    assert_eq!(statement.total_minor, journal.total_minor);
    assert_eq!(statement.paid_minor, journal.settled_minor);
    assert_eq!(statement.state, oxsum_core::PlanStatementState::Issued);
    let before = world
        .db
        .plan_statement(world.organization_id, statement.statement_id)
        .await
        .unwrap()
        .unwrap();
    let payments = before
        .lines
        .iter()
        .filter(|line| line.kind == oxsum_core::StatementLineKind::PaymentApplied)
        .collect::<Vec<_>>();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].amount_minor, -2500);
    assert_eq!(payments[0].source_ref, Some(order_id));

    // Renewal prepays its own period, never the outstanding first statement.
    let renewal = plan_order(&world, plan.customer_plan_id, 2500).await;
    for _ in 0..2 {
        world
            .db
            .activate_plan_period(renewal, plan.customer_plan_id)
            .await
            .unwrap();
        world.db.rollover_due_subscriptions().await.unwrap();
    }
    let after = world
        .db
        .plan_statement(world.organization_id, statement.statement_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.statement.total_minor, 3500);
    assert_eq!(after.statement.paid_minor, 2500);
    assert_eq!(
        after.statement.total_minor - after.statement.paid_minor,
        1000
    );
    assert_eq!(
        after.statement.state,
        oxsum_core::PlanStatementState::Issued
    );
    assert_eq!(after.lines.len(), before.lines.len());
    assert_eq!(
        after
            .lines
            .iter()
            .filter(|line| line.kind == oxsum_core::StatementLineKind::PaymentApplied)
            .count(),
        1
    );
}

// ── H5: a lost-dispute chargeback lands on exactly one open statement ───────
//
// `recharge_journal_dispute_in` restores the receivable on the order's `paid`
// journal; `chargeback_statement_in` then appends the `disputeChargeback` onto
// the next open statement. The contested journal's own statement is `paid` and
// the state machine cannot reopen it, so the restored debt surfaces on the
// document the customer still owes against — designed, not a defect. The
// conserved figure: the chargeback lands on exactly one open statement, the
// contested statement is untouched, and the restored receivable equals the
// chargeback.
#[tokio::test]
async fn h5_a_chargeback_lands_on_exactly_one_open_statement() {
    let Some(world) = world().await else { return };
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::Anniversary,
        true,
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
    let order_id = plan_order(&world, plan.customer_plan_id, 2500).await;
    let journal = closed_first_journal(&world, &plan, order_id).await;
    assert_eq!(journal.state, JournalState::Paid);
    // The already-paid document stays closed; a chargeback needs an open
    // document without rewriting this settled statement.
    let contested_stmt = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.journal_id == Some(journal.journal_id))
        .expect("the journal wrapped a statement");

    // A second exact-paid period has legacy overage, so its own statement
    // remains open. This retains H1's existing policy solely as fixture setup.
    let renewal = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(renewal, plan.customer_plan_id)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    plan_payment::legacy_close(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1500,
    )
    .await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let other_stmt = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.journal_id != Some(journal.journal_id) && s.period_index == Some(1))
        .expect("a second open statement stands");

    assert_eq!(contested_stmt.state, oxsum_core::PlanStatementState::Paid);
    assert_eq!(other_stmt.state, oxsum_core::PlanStatementState::Issued);
    assert_eq!(other_stmt.total_minor - other_stmt.paid_minor, 1500);

    // The lost dispute restores the receivable on the original journal and
    // surfaces it on the other open statement, exactly once.
    let dispute_id = Uuid::new_v4();
    world
        .db
        .recharge_journal_dispute(
            &world.wallet,
            order_id,
            dispute_id,
            2500,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();

    // A retry cannot restore the same disputed payment a second time.
    world
        .db
        .recharge_journal_dispute(
            &world.wallet,
            order_id,
            dispute_id,
            2500,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();

    let contested = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.statement_id == contested_stmt.statement_id)
        .unwrap();
    let other = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.statement_id == other_stmt.statement_id)
        .unwrap();
    let chargeback_lines: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.plan_statement_lines \
         WHERE statement_id=$1 AND kind='dispute_chargeback'",
    )
    .bind(contested.statement_id)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    let other_chargeback: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_minor),0)::bigint FROM oxsum.plan_statement_lines \
         WHERE statement_id=$1 AND kind='dispute_chargeback'",
    )
    .bind(other.statement_id)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    eprintln!(
        "H5: contested total={} chargeback={chargeback_lines}; \
         other total={} chargeback={other_chargeback}",
        contested.total_minor, other.total_minor
    );
    // The restored receivable lands on exactly one open document — the period-1
    // statement the customer still owes against — and the contested (paid)
    // statement is untouched, so the chargeback is never invisible nor doubled.
    assert_eq!(
        chargeback_lines, 0,
        "the paid contested statement stays settled"
    );
    assert_eq!(
        other_chargeback, 2500,
        "the open statement carries the restored debt exactly once"
    );
    assert_eq!(other.total_minor - other.paid_minor, 4000);
    assert_eq!(contested.state, oxsum_core::PlanStatementState::Paid);
    assert_eq!(
        ledger_net(&world.pool, &world.tenant, "Assets:Receivables").await,
        -4000
    );
}

// ── H6: voiding a paid journal must synchronize its customer statement ──────
//
// `void_invoice_journal` reverses the journal's lines and posts the ledger
// reversal but never touches `plan_statements` — the wrapped statement stays
// `paid`/`issued` while its receivable is gone. The customer-facing document
// must follow the void.
#[tokio::test]
async fn h6_voiding_a_journal_synchronizes_its_statement() {
    let Some(world) = world().await else { return };
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::CalendarMonth,
        false,
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
    let journal = closed_first_journal(
        &world,
        &plan,
        plan_order(&world, plan.customer_plan_id, 2500).await,
    )
    .await;
    assert_eq!(journal.state, JournalState::Paid);
    let statement = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.journal_id == Some(journal.journal_id))
        .expect("the journal wrapped a statement");
    assert_eq!(statement.state, oxsum_core::PlanStatementState::Paid);
    assert_eq!(statement.paid_minor, 2500);

    world
        .db
        .void_invoice_journal(
            &world.owner,
            &world.wallet,
            journal.journal_id,
            "billing correction",
            &format!("void-{}", Uuid::new_v4().simple()),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .expect("the void lands");

    let after = statements(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|s| s.journal_id == Some(journal.journal_id))
        .unwrap();
    eprintln!(
        "H6: statement state={:?} total={} paid={}",
        after.state, after.total_minor, after.paid_minor
    );
    // The customer document must not outlive its receivable as still-owed or
    // paid-in-full: a voided journal's statement is void too.
    assert!(
        matches!(after.state, oxsum_core::PlanStatementState::Void),
        "a voided journal's statement must follow it to void, found {:?}",
        after.state
    );
}

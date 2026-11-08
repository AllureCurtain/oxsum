//! Plan fee journals and revenue allocation (issue #611, T20, A22): a period
//! close writes the journal its lines sum to, a fulfilled `planPeriod` order
//! settles the receivable it opened, a refund appends the credit-note
//! discount, a lost dispute restores the receivable — and a posted journal
//! refuses UPDATE/DELETE outright. Needs DATABASE_URL; skips without it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/plan_payment.rs"]
mod plan_payment;
#[path = "support/integration.rs"]
mod setup;

use oxsum_core::{
    CustomerPlan, CustomerPlanState, Db, JournalLineKind, JournalState, NewSoldPlan, NewUser,
    PeriodAuthorization, PeriodKind, SoldPlan, Tenants, Wallet,
};
use sqlx::PgPool;
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("journals")
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
        .register(NewUser {
            email: format!(
                "journal_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "journal fixture password".to_owned(),
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

async fn live(db: &Db, organization_id: Uuid) -> CustomerPlan {
    db.customer_plan(organization_id)
        .await
        .expect("reads")
        .expect("a live plan")
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

// ── A22.1: a period close writes the journal its lines sum to ───────────────

#[tokio::test]
async fn a_period_close_writes_the_journal() {
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
    // The fulfilled order activates period 0 — its amount books the prepay
    // the close will reclassify into the receivable.
    let order_id = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order_id, plan.customer_plan_id)
        .await
        .unwrap();

    // The boundary passes: the rollover closes period 0 and writes its
    // journal in the same transaction — base fee only on a metered,
    // overage-disabled plan.
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();

    let journals = journals(&world.db, world.organization_id).await;
    assert_eq!(journals.len(), 1, "one journal per closed period");
    let journal = &journals[0];
    assert_eq!(journal.period_index, 0);
    assert_eq!(journal.total_minor, 2500);
    let bill = detail(&world.db, world.organization_id, journal.journal_id).await;
    assert_eq!(bill.lines.len(), 1);
    assert_eq!(bill.lines[0].kind, JournalLineKind::BaseFee);
    assert_eq!(bill.lines[0].amount_minor, 2500);
    // The prepaid order covered the bill — the journal is paid outright, its
    // receivable cleared in the same close.
    assert_eq!(journal.state, JournalState::Paid);
    assert_eq!(journal.settled_minor, 2500);
    assert_eq!(journal.paid_order_id, Some(order_id));
    assert!(journal.posted_at.is_some() && journal.paid_at.is_some());
}

#[tokio::test]
async fn close_bills_overage_on_an_overage_enabled_plan() {
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
    world
        .db
        .activate_plan_period(
            plan_order(&world, plan.customer_plan_id, 2500).await,
            plan.customer_plan_id,
        )
        .await
        .unwrap();

    // Overflow usage the quota could not cover charged the wallet — 3
    // credits settled inside the period's own [start, end) window. Back the
    // start a day so the window stays positive after `end_period_ago` moves
    // only the end to `now - 60s`, then settle the usage inside it — before
    // the boundary, so `now() - 120s`.
    sqlx::query(
        "UPDATE oxsum.plan_periods SET period_start = period_start - interval '1 day' \
         WHERE customer_plan_id=$1 AND state='active'",
    )
    .bind(plan.customer_plan_id)
    .execute(&world.pool)
    .await
    .unwrap();
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(world.organization_id)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.usage_records \
             (request_id, tenant_id, model, channel, price_version, kind, entry_id, \
              input_tokens, output_tokens, charged_minor, freeze_minor, settled_at) \
         VALUES ($1,$2,'m','c',1,'usage',$3,1,1,$4,$4,now() - interval '120 seconds')",
    )
    .bind(format!("req-{}", Uuid::new_v4().simple()))
    .bind(&tenant)
    .bind(Uuid::new_v4())
    .bind(3 * ONE)
    .execute(&world.pool)
    .await
    .unwrap();

    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();

    let journals = journals(&world.db, world.organization_id).await;
    let journal = journals
        .iter()
        .find(|j| j.period_index == 0)
        .expect("period 0 journal");
    let bill = detail(&world.db, world.organization_id, journal.journal_id).await;
    let kinds: Vec<JournalLineKind> = bill.lines.iter().map(|l| l.kind).collect();
    assert!(kinds.contains(&JournalLineKind::BaseFee));
    assert!(kinds.contains(&JournalLineKind::UsageOverage));
    let overage: i64 = bill
        .lines
        .iter()
        .filter(|l| l.kind == JournalLineKind::UsageOverage)
        .map(|l| l.amount_minor)
        .sum();
    assert_eq!(overage, 3 * ONE);
    // The lines sum to the total — the row guard's own invariant.
    let sum: i64 = bill.lines.iter().map(|l| l.amount_minor).sum();
    assert_eq!(sum, journal.total_minor);
    // The prepay covered the base fee; the overage is the open receivable.
    assert_eq!(journal.state, JournalState::Posted);
    assert_eq!(journal.total_minor, 2500 + 3 * ONE);
    assert_eq!(journal.settled_minor, 2500);
}

// ── A22.2: a fulfilled planPeriod order applies the journal ────────────────

#[tokio::test]
async fn a_renewal_prepays_its_period_without_consuming_old_debt() {
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
    // The exact base fee is prepaid. Separate overage remains a receivable.
    let partial = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(partial, plan.customer_plan_id)
        .await
        .unwrap();

    plan_payment::overage(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1500,
    )
    .await;
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    assert_eq!(
        live(&world.db, world.organization_id).await.state,
        CustomerPlanState::PastDue
    );
    let journal = journals(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|j| j.period_index == 0)
        .unwrap();
    assert_eq!(journal.state, JournalState::Posted);
    assert_eq!(journal.total_minor - journal.settled_minor, 1500);

    // The renewal fee funds its exact new period; the old debt remains due.
    let renewal = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(renewal, plan.customer_plan_id)
        .await
        .unwrap();

    let bill = detail(&world.db, world.organization_id, journal.journal_id).await;
    assert_eq!(bill.journal.state, JournalState::Posted);
    assert_eq!(bill.journal.settled_minor, 2500);
    assert_eq!(bill.journal.total_minor - bill.journal.settled_minor, 1500);

    // Period 1 stands authorized by the renewal order; the plan is active.
    let plan = live(&world.db, world.organization_id).await;
    assert_eq!(plan.state, CustomerPlanState::Active);
    let periods = world.db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[1].authorization_ref, renewal);
}

#[tokio::test]
async fn a_complimentary_periods_journal_is_paid_by_its_grant() {
    let Some(world) = world().await else { return };
    let free = sold(
        &world.db,
        world.business_id,
        "community",
        0,
        Some(ONE),
        PeriodKind::CalendarMonth,
        false,
    )
    .await;
    let grant = Uuid::new_v4();
    let plan = world
        .db
        .bind_customer_plan(
            world.organization_id,
            free.plan_version_id,
            PeriodAuthorization::Complimentary(grant),
        )
        .await
        .unwrap();
    assert_eq!(plan.state, CustomerPlanState::Active);

    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();

    // The grant covered the whole (zero) fee — the journal posts paid with
    // nothing owed, no ledger legs needed.
    let journals = journals(&world.db, world.organization_id).await;
    assert_eq!(journals.len(), 1);
    let journal = &journals[0];
    assert_eq!(journal.state, JournalState::Paid);
    assert_eq!(journal.total_minor, 0);
    assert_eq!(journal.settled_minor, 0);
}

// ── A22.3: a refund credits the journal ────────────────────────────────────

#[tokio::test]
async fn a_refund_credits_the_journal() {
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
    let order_id = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order_id, plan.customer_plan_id)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let journal = journals(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|j| j.period_index == 0)
        .unwrap();
    assert_eq!(journal.state, JournalState::Paid);

    // The refund path's own seam: `settle_order_refund` issues the credit
    // note under the refund it belongs to, then the journal apply appends
    // the refund's `discount` line and books `debit Discounts / credit
    // Cash` — never the wallet pools, which never carried plan-fee cash.
    let refund_id = Uuid::new_v4();
    let note_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO oxsum.refunds\
         (refund_id,order_id,organization_id,business_id,amount_minor,\
          display_amount_minor,display_currency,reason,request_key,state)\
         VALUES($1,$2,$3,$4,$5,$6,'USD',$7,$8,'requested')",
    )
    .bind(refund_id)
    .bind(order_id)
    .bind(world.organization_id)
    .bind(world.business_id)
    .bind(1000i64)
    .bind(1000i64)
    .bind("refund of the plan period")
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
    .bind(1000i64)
    .bind("refund of the plan period")
    .execute(&world.pool)
    .await
    .unwrap();

    let applied = world
        .db
        .apply_refund_to_journal(
            &world.wallet,
            order_id,
            refund_id,
            note_id,
            1000,
            "refund of the plan period",
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert_eq!(applied, Some(journal.journal_id));

    let bill = detail(&world.db, world.organization_id, journal.journal_id).await;
    assert_eq!(bill.journal.total_minor, 1500);
    assert_eq!(bill.journal.settled_minor, 1500);
    assert_eq!(bill.journal.state, JournalState::Paid);
    let discount = bill
        .lines
        .iter()
        .find(|l| l.kind == JournalLineKind::Discount)
        .expect("the refund's discount line");
    assert_eq!(discount.amount_minor, -1000);
    assert_eq!(discount.source_ref, Some(note_id));
    // The cash correction stands in the ledger, not a wallet debit — the
    // plan-fee cash was never wallet money.
    let note_entry: Option<Uuid> =
        sqlx::query_scalar("SELECT entry_id FROM oxsum.credit_notes WHERE credit_note_id=$1")
            .bind(note_id)
            .fetch_one(&world.pool)
            .await
            .unwrap();
    assert!(note_entry.is_some());
    assert_eq!(world.wallet.settled().await.unwrap(), 0);
}

// ── A22.4: a lost dispute restores the journal's receivable ─────────────────

#[tokio::test]
async fn a_lost_dispute_restores_the_journal_receivable() {
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
    let order_id = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(order_id, plan.customer_plan_id)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let journal = journals(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|j| j.period_index == 0)
        .unwrap();
    assert_eq!(journal.state, JournalState::Paid);

    // The dispute opens: the journal's contested figure tracks it — the same
    // update `record_order_dispute` runs beside its `disputes` row.
    let dispute_id = Uuid::new_v4();
    let tracked: i64 = sqlx::query_scalar(
        "UPDATE oxsum.invoice_journals SET disputed_minor=disputed_minor+$2,updated_at=now() \
         WHERE paid_order_id=$1 AND state IN ('posted','paid') RETURNING disputed_minor",
    )
    .bind(order_id)
    .bind(2500i64)
    .fetch_one(&world.pool)
    .await
    .unwrap();
    assert_eq!(tracked, 2500);

    // Lost: the chargeback re-charges the receivable — `debit Receivables /
    // credit Cash` — and the journal walks `paid → posted`, the owed figure
    // restored as past-due rather than silently erased.
    let restored = world
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
    assert_eq!(restored, Some(journal.journal_id));

    let bill = detail(&world.db, world.organization_id, journal.journal_id).await;
    assert_eq!(bill.journal.state, JournalState::Posted);
    assert_eq!(bill.journal.settled_minor, 0);
    // The opened dispute counted once at record, the lost recharge again —
    // `disputed_minor` is the cumulative contested figure.
    assert_eq!(bill.journal.disputed_minor, 5000);
    // Renewal funds only the next period; this chargeback remains a distinct
    // receivable for the explicit statement repayment path (#777).
    let recovery = plan_order(&world, plan.customer_plan_id, 2500).await;
    world
        .db
        .activate_plan_period(recovery, plan.customer_plan_id)
        .await
        .unwrap();
    let recovered = detail(&world.db, world.organization_id, journal.journal_id).await;
    assert_eq!(recovered.journal.state, JournalState::Posted);
    assert_eq!(recovered.journal.settled_minor, 0);
}

// ── A22.5: a posted journal is immutable ────────────────────────────────────

#[tokio::test]
async fn a_posted_journal_refuses_edits_and_deletes() {
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
    // An exact paid activation with overage leaves a real posted receivable.
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
    plan_payment::overage(
        &world.db,
        world.organization_id,
        plan.customer_plan_id,
        1000,
    )
    .await;
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let journal = journals(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|j| j.period_index == 0)
        .unwrap();
    assert_eq!(journal.state, JournalState::Posted);

    // A terms edit is refused outright — the bill cannot change under the
    // customer; DELETE is refused the same way, history is retained.
    let updated =
        sqlx::query("UPDATE oxsum.invoice_journals SET total_minor = 1 WHERE journal_id = $1")
            .bind(journal.journal_id)
            .execute(&world.pool)
            .await;
    assert!(updated.is_err(), "a posted journal's terms refuse edits");
    let deleted = sqlx::query("DELETE FROM oxsum.invoice_journals WHERE journal_id = $1")
        .bind(journal.journal_id)
        .execute(&world.pool)
        .await;
    assert!(deleted.is_err(), "journal history is retained");
    // Its lines refuse edits and deletes too — corrections append, nothing
    // rewrites.
    let line_edit = sqlx::query(
        "UPDATE oxsum.invoice_journal_lines SET amount_minor = 0 WHERE journal_id = $1",
    )
    .bind(journal.journal_id)
    .execute(&world.pool)
    .await;
    assert!(
        line_edit.is_err(),
        "journal lines are immutable once written"
    );
    let line_delete = sqlx::query("DELETE FROM oxsum.invoice_journal_lines WHERE journal_id = $1")
        .bind(journal.journal_id)
        .execute(&world.pool)
        .await;
    assert!(line_delete.is_err(), "journal lines are never deleted");
    // A backward step the guard does not permit: posted cannot return to
    // draft.
    let backward =
        sqlx::query("UPDATE oxsum.invoice_journals SET state = 'draft' WHERE journal_id = $1")
            .bind(journal.journal_id)
            .execute(&world.pool)
            .await;
    assert!(backward.is_err(), "the state machine only walks forward");
}

#[tokio::test]
async fn a_staff_void_nets_the_journal_to_zero() {
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
    let journal = journals(&world.db, world.organization_id)
        .await
        .into_iter()
        .find(|j| j.period_index == 0)
        .unwrap();
    assert_eq!(journal.state, JournalState::Paid);

    // The staff void: reversing lines append so the bill nets to zero, the
    // settlement unwinds to `Liabilities:RefundPayable`, and the audit keeps
    // the actor and reason.
    let voided = world
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
    assert_eq!(voided.journal.state, JournalState::Void);
    assert_eq!(
        voided.journal.void_reason.as_deref(),
        Some("billing correction")
    );
    // Every original line has its mirror — the line set sums to zero.
    let sum: i64 = voided.lines.iter().map(|l| l.amount_minor).sum();
    assert_eq!(sum, 0);
    assert_eq!(voided.lines.len(), 2);

    // A second void answers the standing voided journal — and the
    // (plan, period) slot is free for a reissue.
    let again = world
        .db
        .void_invoice_journal(
            &world.owner,
            &world.wallet,
            journal.journal_id,
            "second void attempt",
            &format!("void-{}", Uuid::new_v4().simple()),
            OffsetDateTime::now_utc().date(),
        )
        .await
        .expect("a replayed void answers the standing journal");
    assert_eq!(again.journal.state, JournalState::Void);
    // `void` keeps history — the customer list still shows it, cancelled.
    let listed = journals(&world.db, world.organization_id).await;
    assert!(listed.iter().any(|j| j.state == JournalState::Void));
    let audit: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM oxsum.finance_audit WHERE action='journal.void' AND target=$1",
    )
    .bind(journal.journal_id.to_string())
    .fetch_optional(&world.pool)
    .await
    .unwrap();
    assert!(audit.is_some(), "the void is audited");
}

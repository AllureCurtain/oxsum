//! Sold plans and the customer plan lifecycle (issue #610, T19): versioned
//! catalogue, one live plan per customer, per-period authorization, renewal
//! and boundary cancel/change, past-due service, and the anniversary
//! leap-month clamp — over a real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/legacy_plan.rs"]
mod legacy_plan;

#[path = "support/plan_payment.rs"]
mod plan_payment;
#[path = "support/integration.rs"]
mod setup;

use oxsum_core::{
    CustomerPlan, Db, NewSoldPlan, NewUser, PeriodAuthorization, PeriodKind, WalletError,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("plans")
}

macro_rules! db_or_skip {
    () => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

async fn pool(url: &str) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(url)
        .await
        .expect("connects to PostgreSQL")
}

/// A migrated database, the staff owner token, a registered customer org, and
/// the business singleton — every plan call needs all four.
struct World {
    db: Db,
    owner: String,
    pool: PgPool,
    organization_id: Uuid,
    business_id: Uuid,
}

async fn world(url: &str) -> World {
    let pool = pool(url).await;
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    // The business setup must exist before `default_business` resolves.
    let owner = setup::owner(&db).await;
    let registration = db
        .register(NewUser {
            email: format!(
                "plan_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "plan lifecycle fixture password".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    let business_id = db.default_business().await.expect("a business");
    World {
        owner,
        db,
        pool,
        organization_id: registration.organization.id,
        business_id,
    }
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
) -> oxsum_core::SoldPlan {
    legacy_plan::publish(
        db,
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

/// The customer's live plan, unwrapped.
async fn live(db: &Db, organization_id: Uuid) -> CustomerPlan {
    db.customer_plan(organization_id)
        .await
        .expect("reads")
        .expect("a live plan")
}

/// Force the live plan's open `plan_periods` row and its quota engine window
/// to have ended `ago` seconds in the past — a due boundary for the rollover.
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

// ── the catalogue ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_plan_publishes_immutable_versions() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let v1 = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::CalendarMonth,
        false,
    )
    .await;
    assert_eq!(v1.version, 1);
    assert_eq!(v1.price_minor, 2500);
    assert!(v1.published_at.is_some());
    assert!(v1.retired_at.is_none());

    // A catalogue edit mints version 2 of the same plan, never rewrites v1.
    let v2 = world
        .db
        .publish_sold_plan(
            world.business_id,
            &NewSoldPlan {
                plan_id: Some(v1.plan_id),
                name: "pro".into(),
                price_minor: 3000,
                currency: "USD".into(),
                quota_minor: Some(20 * ONE),
                period_kind: PeriodKind::CalendarMonth,
                overage_enabled: true,
                sort_order: 0,
            },
        )
        .await
        .expect("v2 publishes");
    assert_eq!(v2.plan_id, v1.plan_id);
    assert_eq!(v2.version, 2);

    // v1's stored terms stand untouched — a sold form is immutable.
    let reread = world
        .db
        .sold_plan(v1.plan_version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reread.price_minor, 2500);
    assert_eq!(reread.quota_minor, Some(10 * ONE));

    // Retiring stops new binds but keeps the row.
    assert!(world.db.retire_sold_plan(v1.plan_version_id).await.unwrap());
    let retired = world
        .db
        .sold_plan(v1.plan_version_id)
        .await
        .unwrap()
        .unwrap();
    assert!(retired.retired_at.is_some());
    assert!(matches!(
        world
            .db
            .bind_customer_plan(
                world.organization_id,
                v1.plan_version_id,
                PeriodAuthorization::Complimentary(Uuid::new_v4()),
            )
            .await,
        Err(WalletError::NotFound(_))
    ));
}

// ── binding ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_free_plan_activates_on_a_complimentary_grant() {
    let url = db_or_skip!();
    let world = world(&url).await;
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
        .expect("the free plan binds");
    assert_eq!(plan.state, oxsum_core::CustomerPlanState::Active);
    assert!(plan.subscription_id.is_some());
    assert!(plan.activated_at.is_some());

    // Period 0 stands authorized by the grant — explicit, never fabricated paid.
    let periods = world.db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].period_index, 0);
    assert_eq!(
        periods[0].authorization_kind,
        oxsum_core::PlanAuthorizationKind::Complimentary
    );
    assert_eq!(periods[0].authorization_ref, grant);
    assert_eq!(periods[0].state, oxsum_core::PlanPeriodState::Active);

    // The quota engine row exists and meters through the legacy machinery.
    let sub = world
        .db
        .active_subscription(world.organization_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sub.quota_minor, Some(ONE));

    // A free plan cannot take a payment authorization, and a second live bind
    // conflicts — one active plan per customer.
    assert!(matches!(
        world
            .db
            .bind_customer_plan(
                world.organization_id,
                free.plan_version_id,
                PeriodAuthorization::PaymentOrder(Uuid::new_v4()),
            )
            .await,
        Err(WalletError::InvalidInput(_)) | Err(WalletError::Conflict(_))
    ));
}

#[tokio::test]
async fn a_priced_plan_waits_for_its_order() {
    let url = db_or_skip!();
    let world = world(&url).await;
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

    // A priced plan refuses a complimentary grant — D06's explicit rule.
    assert!(matches!(
        world
            .db
            .bind_customer_plan(
                world.organization_id,
                pro.plan_version_id,
                PeriodAuthorization::Complimentary(Uuid::new_v4()),
            )
            .await,
        Err(WalletError::InvalidInput(_))
    ));

    // The bind is pending_payment: no subscription, no periods, no entitlement.
    let plan = world
        .db
        .bind_customer_plan(
            world.organization_id,
            pro.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .expect("a priced plan binds pending payment");
    assert_eq!(plan.state, oxsum_core::CustomerPlanState::PendingPayment);
    assert!(plan.subscription_id.is_none());
    assert!(
        world
            .db
            .plan_periods(plan.customer_plan_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        world
            .db
            .active_subscription(world.organization_id)
            .await
            .unwrap()
            .is_none()
    );

    // Activating on the funded order opens period 0 and the quota engine.
    let order_id = plan_payment::paid_order(
        &world.db,
        &world.owner,
        world.organization_id,
        plan.customer_plan_id,
    )
    .await
    .order_id;
    let active = world
        .db
        .activate_plan_period(order_id, plan.customer_plan_id)
        .await
        .expect("the order activates period 0");
    assert_eq!(active.state, oxsum_core::CustomerPlanState::Active);
    assert!(active.subscription_id.is_some());
    let periods = world.db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(
        periods[0].authorization_kind,
        oxsum_core::PlanAuthorizationKind::PaymentOrder
    );
    assert_eq!(periods[0].authorization_ref, order_id);

    // A replayed activation under the same order is a no-op — the period the
    // order already authorized answers the standing plan.
    let again = world
        .db
        .activate_plan_period(order_id, plan.customer_plan_id)
        .await
        .expect("a replay answers the plan");
    assert_eq!(again.state, oxsum_core::CustomerPlanState::Active);
    assert_eq!(
        world
            .db
            .plan_periods(plan.customer_plan_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

// ── renewal and boundary ───────────────────────────────────────────────────

#[tokio::test]
async fn the_timer_never_issues_an_unpaid_period() {
    let url = db_or_skip!();
    let world = world(&url).await;
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

    // Push the boundary into the past: the rollover pass runs and finds no
    // authorized next period — the plan goes past_due, period 0 stays the last
    // authorized one, and the subscription keeps its ended window rather than
    // free-rolling a new one.
    end_period_ago(&world.pool, world.organization_id, 60).await;
    let report = world.db.rollover_due_subscriptions().await.unwrap();
    assert_eq!(report.expired, 1);
    assert_eq!(report.rolled, 0);

    let past_due = live(&world.db, world.organization_id).await;
    assert_eq!(past_due.state, oxsum_core::CustomerPlanState::PastDue);
    let periods = world.db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods.len(), 1, "no unpaid period issued");
    assert_eq!(periods[0].state, oxsum_core::PlanPeriodState::Expired);
    // The quota engine stays in its ended window — past-due draws meter the
    // remainder, never a fresh quota.
    let sub = world
        .db
        .active_subscription(world.organization_id)
        .await
        .unwrap()
        .unwrap();
    assert!(sub.current_period_end.unwrap() <= OffsetDateTime::now_utc());
}

#[tokio::test]
async fn a_past_due_plan_renews_on_a_fulfilled_order() {
    let url = db_or_skip!();
    let world = world(&url).await;
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::Anniversary,
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
    world.db.rollover_due_subscriptions().await.unwrap();
    assert_eq!(
        live(&world.db, world.organization_id).await.state,
        oxsum_core::CustomerPlanState::PastDue
    );

    // The renewal order's fulfillment opens period 1, chaining off the ended
    // period's boundary — never `now`, which would skip the gap's audit.
    let renewal_order = plan_payment::paid_order(
        &world.db,
        &world.owner,
        world.organization_id,
        plan.customer_plan_id,
    )
    .await
    .order_id;
    let renewed = world
        .db
        .activate_plan_period(renewal_order, plan.customer_plan_id)
        .await
        .expect("the renewal activates");
    assert_eq!(renewed.state, oxsum_core::CustomerPlanState::Active);
    let periods = world.db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[1].period_index, 1);
    assert_eq!(periods[1].authorization_ref, renewal_order);
    assert_eq!(periods[1].state, oxsum_core::PlanPeriodState::Active);
    assert_eq!(periods[1].period_start, periods[0].period_end);
    // The engine's window follows the authorized period's bounds.
    let sub = world
        .db
        .active_subscription(world.organization_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sub.current_period_end.unwrap(), periods[1].period_end);
}

#[tokio::test]
async fn cancel_at_period_end_serves_then_stops() {
    let url = db_or_skip!();
    let world = world(&url).await;
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

    // Cancelling mid-period keeps the plan live — it serves what was paid.
    let marked = world
        .db
        .cancel_customer_plan(world.organization_id)
        .await
        .expect("cancel marks the boundary");
    assert_eq!(marked.state, oxsum_core::CustomerPlanState::Active);
    assert!(marked.cancel_at_period_end);

    // The boundary closes the plan outright: cancelled, no new period, and the
    // quota engine cancelled with it — the wallet takes every later draw.
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    assert!(
        world
            .db
            .customer_plan(world.organization_id)
            .await
            .unwrap()
            .is_none(),
        "a cancelled plan is history, not the live read"
    );
    assert!(
        world
            .db
            .active_subscription(world.organization_id)
            .await
            .unwrap()
            .is_none()
    );
    let periods = world.db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods[0].state, oxsum_core::PlanPeriodState::Closed);
}

/// The first boundary request performs cancellation itself; it must not take
/// an extra draw merely because the periodic rollover worker has not run.
#[tokio::test]
async fn lazy_boundary_cancellation_never_spends_the_ended_quota() {
    let url = db_or_skip!();
    for overflow in [false, true] {
        let world = world(&url).await;
        let sold = sold(
            &world.db,
            world.business_id,
            "cancel-boundary",
            2500,
            Some(10 * ONE),
            PeriodKind::CalendarMonth,
            overflow,
        )
        .await;
        let plan = world
            .db
            .bind_customer_plan(
                world.organization_id,
                sold.plan_version_id,
                PeriodAuthorization::PaymentOrder(Uuid::nil()),
            )
            .await
            .unwrap();
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
        let old_key = format!("old-cancel-draw-{}", world.organization_id);
        let next_key = format!("new-cancel-draw-{}", world.organization_id);
        let old = world
            .db
            .admit_subscription_draw(world.organization_id, &old_key, 4 * ONE)
            .await
            .unwrap();
        world
            .db
            .cancel_customer_plan(world.organization_id)
            .await
            .unwrap();
        end_period_ago(&world.pool, world.organization_id, 60).await;

        // No worker has run. The quota admission must close the cancellation
        // and answer that the wallet owns the entire new request.
        let next = world
            .db
            .admit_subscription_draw(world.organization_id, &next_key, ONE)
            .await
            .unwrap();
        assert_eq!(next.drawn_minor, 0, "ended quota cannot fund a new call");
        assert_eq!(next.draw_id, Uuid::nil());
        assert_eq!(next.subscription_id, Uuid::nil());
        let new_draws: i64 =
            sqlx::query_scalar("SELECT count(*) FROM oxsum.subscription_draws WHERE hold_key = $1")
                .bind(&next_key)
                .fetch_one(&world.pool)
                .await
                .unwrap();
        assert_eq!(new_draws, 0);
        assert!(
            world
                .db
                .active_subscription(world.organization_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            world
                .db
                .customer_plan(world.organization_id)
                .await
                .unwrap()
                .is_none()
        );
        let periods = world
            .db
            .subscription_periods(old.subscription_id)
            .await
            .unwrap();
        assert_eq!(periods.len(), 1);
        assert_eq!(periods[0].outcome, "cancelled");
        assert_eq!(periods[0].consumed_minor, 4 * ONE);

        // A pre-boundary reservation remains valid; returning its unused part
        // updates the retained old period without reopening entitlement.
        assert_eq!(
            world
                .db
                .settle_subscription_draw(&old_key, 2 * ONE)
                .await
                .unwrap(),
            Some(2 * ONE)
        );
        assert_eq!(
            world
                .db
                .settle_subscription_draw(&old_key, 2 * ONE)
                .await
                .unwrap(),
            Some(2 * ONE)
        );
        let periods = world
            .db
            .subscription_periods(old.subscription_id)
            .await
            .unwrap();
        assert_eq!(periods[0].outcome, "cancelled");
        assert_eq!(periods[0].consumed_minor, 2 * ONE);
        world.db.rollover_due_subscriptions().await.unwrap();
        assert_eq!(
            world
                .db
                .plan_periods(plan.customer_plan_id)
                .await
                .unwrap()
                .len(),
            1
        );
        let journals: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.invoice_journals WHERE customer_plan_id = $1",
        )
        .bind(plan.customer_plan_id)
        .fetch_one(&world.pool)
        .await
        .unwrap();
        assert_eq!(journals, 1, "the later worker cannot close or bill twice");
    }
}

#[tokio::test]
async fn a_scheduled_change_applies_at_the_boundary() {
    let url = db_or_skip!();
    let world = world(&url).await;
    let basic = sold(
        &world.db,
        world.business_id,
        "basic",
        1000,
        Some(5 * ONE),
        PeriodKind::CalendarMonth,
        false,
    )
    .await;
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(20 * ONE),
        PeriodKind::CalendarMonth,
        false,
    )
    .await;
    let plan = world
        .db
        .bind_customer_plan(
            world.organization_id,
            basic.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
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

    // Book the swap: it sits on `scheduled_change`, the version does not move
    // mid-period — no pro-rating.
    let booked = world
        .db
        .schedule_customer_plan_change(world.organization_id, pro.plan_version_id)
        .await
        .expect("the change books");
    assert_eq!(booked.plan_version_id, basic.plan_version_id);
    assert_eq!(booked.scheduled_change, Some(pro.plan_version_id));

    // At the boundary the version swaps and the plan waits renewal_pending for
    // the new version's first authorization — still no unpaid period.
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();
    let swapped = live(&world.db, world.organization_id).await;
    assert_eq!(swapped.state, oxsum_core::CustomerPlanState::RenewalPending);
    assert_eq!(swapped.plan_version_id, pro.plan_version_id);
    assert!(swapped.scheduled_change.is_none());
}

// ── the anniversary boundary ───────────────────────────────────────────────

#[tokio::test]
async fn the_anniversary_clamps_to_a_short_month() {
    // Jan 31 → Feb 28 (a leap-month clamp): the close lands on the last day
    // February has, not a date that does not exist.
    let anchor = time::Date::from_calendar_date(2026, time::Month::January, 31).unwrap();
    let start = anchor.midnight().assume_utc();
    let end = oxsum_core::period_end_for(PeriodKind::Anniversary, anchor, start);
    assert_eq!(
        end.date(),
        time::Date::from_calendar_date(2026, time::Month::February, 28).unwrap()
    );

    // The anchor sticks rather than the clamp: Feb 28 rolls to Mar 31 — the
    // anniversary is the activation's day-of-month, only clamped into months
    // too short to hold it, never permanently drifted by one.
    let next = oxsum_core::period_end_for(PeriodKind::Anniversary, anchor, end);
    assert_eq!(
        next.date(),
        time::Date::from_calendar_date(2026, time::Month::March, 31).unwrap()
    );

    // A mid-month anchor rolls on the same day.
    let mid = time::Date::from_calendar_date(2026, time::Month::January, 15).unwrap();
    let mid_end =
        oxsum_core::period_end_for(PeriodKind::Anniversary, mid, mid.midnight().assume_utc());
    assert_eq!(
        mid_end.date(),
        time::Date::from_calendar_date(2026, time::Month::February, 15).unwrap()
    );

    // calendar_month keeps the legacy first-of-next-month boundary.
    let cal = oxsum_core::period_end_for(PeriodKind::CalendarMonth, anchor, start);
    assert_eq!(
        cal.date(),
        time::Date::from_calendar_date(2026, time::Month::February, 1).unwrap()
    );
}

// ── draws through the lifecycle ────────────────────────────────────────────

#[tokio::test]
async fn a_plan_draw_serves_and_past_due_holds_the_remainder() {
    let url = db_or_skip!();
    let world = world(&url).await;
    let pro = sold(
        &world.db,
        world.business_id,
        "pro",
        2500,
        Some(10 * ONE),
        PeriodKind::CalendarMonth,
        true, // overage enabled: the remainder's overflow falls to the wallet
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

    // A draw inside the quota lands on the subscription the plan opened.
    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, "req-1:hold", 4 * ONE)
        .await
        .expect("the draw admits");
    assert_eq!(admission.drawn_minor, 4 * ONE);

    // Past-due: the period ends with 6 credits unspent — the remainder still
    // draws (past-due service), and no new period opens on the timer.
    end_period_ago(&world.pool, world.organization_id, 60).await;
    let second = world
        .db
        .admit_subscription_draw(world.organization_id, "req-2:hold", 8 * ONE)
        .await
        .expect("a past-due draw admits");
    assert_eq!(second.drawn_minor, 6 * ONE, "only the remainder draws");
    let past_due = live(&world.db, world.organization_id).await;
    assert_eq!(past_due.state, oxsum_core::CustomerPlanState::PastDue);
}

#[tokio::test]
async fn a_stale_draw_still_settles_against_a_plan() {
    let url = db_or_skip!();
    let world = world(&url).await;
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

    // A draw opens, then the boundary passes with the draw still unsettled —
    // the stale row must still sweep correctly: settle charges its actual
    // against the period the draw pinned.
    world
        .db
        .admit_subscription_draw(world.organization_id, "req-stale:hold", 4 * ONE)
        .await
        .unwrap();
    end_period_ago(&world.pool, world.organization_id, 60).await;
    world.db.rollover_due_subscriptions().await.unwrap();

    let settled = world
        .db
        .settle_subscription_draw("req-stale:hold", 3 * ONE)
        .await
        .expect("the stale draw settles");
    assert_eq!(settled, Some(3 * ONE));
    // The release returned to the ended period's counter — past-due draws see
    // the remainder net of what the stale draw actually consumed.
    let sub = world
        .db
        .active_subscription(world.organization_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sub.consumed_minor, 3 * ONE);
}

#[tokio::test]
async fn a_fully_covered_draw_needs_no_wallet() {
    let url = db_or_skip!();
    let world = world(&url).await;
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

    // A zero-wallet organization on a live plan: the whole freeze draws from
    // the quota — the receipt path T12 built serves it with nothing held.
    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, "req-covered:hold", 10 * ONE)
        .await
        .expect("the covered draw admits");
    assert_eq!(admission.drawn_minor, 10 * ONE);
    let settled = world
        .db
        .settle_subscription_draw("req-covered:hold", 10 * ONE)
        .await
        .unwrap();
    assert_eq!(settled, Some(10 * ONE));
}

#[tokio::test]
async fn racing_binds_conflict_on_one_live_plan() {
    let url = db_or_skip!();
    let world = world(&url).await;
    let a = sold(
        &world.db,
        world.business_id,
        "a",
        0,
        Some(ONE),
        PeriodKind::CalendarMonth,
        false,
    )
    .await;
    let b = sold(
        &world.db,
        world.business_id,
        "b",
        0,
        Some(ONE),
        PeriodKind::CalendarMonth,
        false,
    )
    .await;

    // Two binds race on the org's advisory lock: exactly one wins the
    // one-live-plan slot, the other conflicts — never two active plans.
    let (first, second) = tokio::join!(
        world.db.bind_customer_plan(
            world.organization_id,
            a.plan_version_id,
            PeriodAuthorization::Complimentary(Uuid::new_v4()),
        ),
        world.db.bind_customer_plan(
            world.organization_id,
            b.plan_version_id,
            PeriodAuthorization::Complimentary(Uuid::new_v4()),
        )
    );
    let wins = [&first, &second].iter().filter(|r| r.is_ok()).count();
    let conflicts = [&first, &second]
        .iter()
        .filter(|r| matches!(r, Err(WalletError::Conflict(_))))
        .count();
    assert_eq!(wins, 1, "one bind wins");
    assert_eq!(conflicts, 1, "the other conflicts");
    let live = live(&world.db, world.organization_id).await;
    assert_eq!(
        world
            .db
            .plan_periods(live.customer_plan_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

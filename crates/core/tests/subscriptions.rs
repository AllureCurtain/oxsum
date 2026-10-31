//! Subscription quota plans (issue #496): the entitlement leg beside the
//! wallet — plan catalogue, bind/cancel, draw/settle and the boundary
//! rollovers — over a real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, NewUser, OrgSubscription, ResetPeriod, SubscriptionPlan, WalletError};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("subscriptions")
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
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL")
}

/// A migrated database plus one registered organization — the world every
/// subscription call needs.
struct World {
    db: Db,
    pool: PgPool,
    organization_id: Uuid,
}

async fn world(url: &str) -> World {
    let pool = pool(url).await;
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let registration = db
        .register(NewUser {
            email: format!(
                "subs_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    World {
        db,
        pool,
        organization_id: registration.organization.id,
    }
}

fn fresh(name: &str) -> String {
    format!("{name}-{}", &Uuid::new_v4().simple().to_string()[..8])
}

/// A metered plan with the knobs a test cares about.
async fn plan(
    db: &Db,
    name: &str,
    quota: Option<i64>,
    reset: ResetPeriod,
    overflow: bool,
    sort_order: i32,
) -> SubscriptionPlan {
    db.set_subscription_plan(name, quota, reset, overflow, None, None, true, sort_order)
        .await
        .expect("the plan upserts")
}

/// The organization's active subscription, unwrapped — every test that binds
/// expects one to stand.
async fn active(db: &Db, organization_id: Uuid) -> OrgSubscription {
    db.active_subscription(organization_id)
        .await
        .expect("reads")
        .expect("an active subscription")
}

/// Force the subscription's window to have ended `ago` seconds in the past,
/// so the rollover paths have a due boundary to act on.
async fn end_period_ago(pool: &PgPool, subscription_id: Uuid, ago_secs: i64) {
    sqlx::query(
        "UPDATE oxsum.org_subscriptions \
         SET current_period_end = now() - ($2 || ' seconds')::interval \
         WHERE subscription_id = $1",
    )
    .bind(subscription_id)
    .bind(ago_secs.to_string())
    .execute(pool)
    .await
    .expect("the period end moves");
}

/// Force the open period row's end the same way — the draw path keys the
/// rollover on the subscription's own `current_period_end`, but the period
/// row's bounds are what the audit shows.
async fn end_open_period_row_ago(pool: &PgPool, subscription_id: Uuid, ago_secs: i64) {
    sqlx::query(
        "UPDATE oxsum.subscription_periods \
         SET period_end = now() - ($2 || ' seconds')::interval \
         WHERE subscription_id = $1 AND outcome = 'open'",
    )
    .bind(subscription_id)
    .bind(ago_secs.to_string())
    .execute(pool)
    .await
    .expect("the period row's end moves");
}

// ── the catalogue ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_plan_upserts_by_name_and_lists_in_sort_order() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let name = fresh("pro");
    let pro = plan(
        &world.db,
        &name,
        Some(10 * ONE),
        ResetPeriod::Monthly,
        true,
        2,
    )
    .await;
    assert_eq!(pro.name, name);
    assert_eq!(pro.quota_minor, Some(10 * ONE));
    assert_eq!(pro.reset_period, ResetPeriod::Monthly);

    // Upsert by the same name: the row changes, the id stays.
    let pro2 = plan(
        &world.db,
        &name,
        Some(20 * ONE),
        ResetPeriod::Monthly,
        true,
        2,
    )
    .await;
    assert_eq!(pro2.plan_id, pro.plan_id);
    assert_eq!(pro2.quota_minor, Some(20 * ONE));

    let starter = plan(
        &world.db,
        &fresh("starter"),
        Some(ONE),
        ResetPeriod::Daily,
        true,
        1,
    )
    .await;
    let plans = world.db.subscription_plans().await.expect("lists");
    let names: Vec<&str> = plans.iter().map(|p| p.name.as_str()).collect();
    let starter_at = names.iter().position(|n| *n == starter.name).unwrap();
    let pro_at = names.iter().position(|n| *n == name).unwrap();
    assert!(starter_at < pro_at, "sort_order orders the catalogue");
}

#[tokio::test]
async fn a_plan_refuses_a_nonpositive_quota() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let err = world
        .db
        .set_subscription_plan(
            &fresh("bad"),
            Some(0),
            ResetPeriod::Monthly,
            true,
            None,
            None,
            true,
            0,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)));
}

// ── bind / cancel ────────────────────────────────────────────────────────────

#[tokio::test]
async fn binding_writes_the_subscription_and_its_open_period() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("pro"),
        Some(10 * ONE),
        ResetPeriod::Monthly,
        true,
        1,
    )
    .await;
    let sub = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");
    assert_eq!(sub.state, "active");
    assert_eq!(sub.plan_id, plan.plan_id);
    assert_eq!(sub.consumed_minor, 0);
    assert!(
        sub.current_period_end.is_some(),
        "a monthly plan ends at the boundary"
    );
    assert!(sub.current_period_end.unwrap() > OffsetDateTime::now_utc());

    let periods = world
        .db
        .subscription_periods(sub.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].outcome, "open");
    assert_eq!(periods[0].quota_minor, Some(10 * ONE));
}

#[tokio::test]
async fn rebinding_the_same_plan_replays_without_a_new_period() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("pro"),
        Some(ONE),
        ResetPeriod::Monthly,
        true,
        1,
    )
    .await;
    let first = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");
    let second = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("replays");
    assert_eq!(first.subscription_id, second.subscription_id);

    let periods = world
        .db
        .subscription_periods(first.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(periods.len(), 1, "a replayed bind opens no second period");
}

#[tokio::test]
async fn an_upgrade_closes_the_old_period_and_opens_the_new_plan() {
    let url = db_or_skip!();
    let world = world(&url).await;

    // Lower sort_order is the downgrade direction.
    let starter = plan(
        &world.db,
        &fresh("starter"),
        Some(ONE),
        ResetPeriod::Monthly,
        true,
        1,
    )
    .await;
    let pro = plan(
        &world.db,
        &fresh("pro"),
        Some(10 * ONE),
        ResetPeriod::Monthly,
        true,
        2,
    )
    .await;
    let sub = world
        .db
        .bind_subscription(world.organization_id, starter.plan_id, "admin", None)
        .await
        .expect("binds");
    // Burn some quota so the closed period has something to report.
    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, "req-up:hold", ONE / 2)
        .await
        .expect("draws");
    assert_eq!(admission.drawn_minor, ONE / 2);

    let upgraded = world
        .db
        .bind_subscription(world.organization_id, pro.plan_id, "admin", None)
        .await
        .expect("upgrades");
    assert_ne!(upgraded.subscription_id, sub.subscription_id);
    assert_eq!(upgraded.plan_id, pro.plan_id);

    let periods = world
        .db
        .subscription_periods(sub.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(
        periods.len(),
        1,
        "the old subscription's period closed once"
    );
    assert_eq!(periods[0].outcome, "upgrade");
    assert_eq!(
        periods[0].consumed_minor,
        ONE / 2,
        "the closed period keeps its burn"
    );

    // A downgrade takes the other outcome.
    let downgraded = world
        .db
        .bind_subscription(world.organization_id, starter.plan_id, "admin", None)
        .await
        .expect("downgrades");
    let periods = world
        .db
        .subscription_periods(upgraded.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(periods[0].outcome, "downgrade");
    assert_eq!(downgraded.plan_id, starter.plan_id);
}

#[tokio::test]
async fn cancelling_closes_the_period_and_leaves_no_active_binding() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("pro"),
        Some(ONE),
        ResetPeriod::Monthly,
        true,
        1,
    )
    .await;
    let sub = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");

    assert!(
        world
            .db
            .cancel_subscription(world.organization_id)
            .await
            .expect("cancels")
    );
    assert!(
        world
            .db
            .active_subscription(world.organization_id)
            .await
            .expect("reads")
            .is_none()
    );
    let periods = world
        .db
        .subscription_periods(sub.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(periods[0].outcome, "cancelled");
    // Cancelling with nothing bound answers false, not an error.
    assert!(
        !world
            .db
            .cancel_subscription(world.organization_id)
            .await
            .expect("no-op")
    );
}

#[tokio::test]
async fn a_disabled_plan_refuses_to_bind() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = world
        .db
        .set_subscription_plan(
            &fresh("retired"),
            Some(ONE),
            ResetPeriod::Monthly,
            true,
            None,
            None,
            false,
            0,
        )
        .await
        .expect("the disabled plan upserts");
    let err = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::NotFound(_)));
}

// ── the draw ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_draw_covers_the_freeze_and_counts_against_the_period() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("pro"),
        Some(10 * ONE),
        ResetPeriod::Monthly,
        true,
        1,
    )
    .await;
    world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");

    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, "req-1:hold", 3 * ONE)
        .await
        .expect("draws");
    assert_eq!(admission.drawn_minor, 3 * ONE);

    let sub = active(&world.db, world.organization_id).await;
    assert_eq!(sub.consumed_minor, 3 * ONE);
    let periods = world
        .db
        .subscription_periods(sub.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(periods[0].consumed_minor, 3 * ONE);
}

#[tokio::test]
async fn a_replayed_draw_answers_its_own_row() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("pro"),
        Some(10 * ONE),
        ResetPeriod::Monthly,
        true,
        1,
    )
    .await;
    world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");

    let first = world
        .db
        .admit_subscription_draw(world.organization_id, "req-2:hold", ONE)
        .await
        .expect("draws");
    let replay = world
        .db
        .admit_subscription_draw(world.organization_id, "req-2:hold", ONE)
        .await
        .expect("replays");
    assert_eq!(first.draw_id, replay.draw_id);
    assert_eq!(replay.drawn_minor, ONE);

    let sub = active(&world.db, world.organization_id).await;
    assert_eq!(sub.consumed_minor, ONE, "a replayed draw debits once");
}

#[tokio::test]
async fn an_exhausted_quota_overflows_or_refuses_by_the_plan() {
    let url = db_or_skip!();
    let world = world(&url).await;

    // Overflow on: the remainder is the wallet's.
    let flex_plan = plan(
        &world.db,
        &fresh("flex"),
        Some(ONE),
        ResetPeriod::Monthly,
        true,
        1,
    )
    .await;
    world
        .db
        .bind_subscription(world.organization_id, flex_plan.plan_id, "admin", None)
        .await
        .expect("binds");
    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, "req-3:hold", 3 * ONE)
        .await
        .expect("overflows");
    assert_eq!(
        admission.drawn_minor, ONE,
        "only the quota draws; the rest is the wallet's"
    );

    // Overflow off on a second organization: the same exhaustion refuses.
    let registration = world
        .db
        .register(NewUser {
            email: format!(
                "subs_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    let rigid_plan = plan(
        &world.db,
        &fresh("rigid"),
        Some(ONE),
        ResetPeriod::Monthly,
        false,
        1,
    )
    .await;
    world
        .db
        .bind_subscription(
            registration.organization.id,
            rigid_plan.plan_id,
            "admin",
            None,
        )
        .await
        .expect("binds");
    let err = world
        .db
        .admit_subscription_draw(registration.organization.id, "req-4:hold", 2 * ONE)
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InsufficientFunds));
}

#[tokio::test]
async fn no_subscription_draws_nothing() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, "req-5:hold", ONE)
        .await
        .expect("no subscription is no draw");
    assert_eq!(admission.drawn_minor, 0);
}

// ── settle ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn settling_a_draw_returns_the_unused_share() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("pro"),
        Some(10 * ONE),
        ResetPeriod::Monthly,
        true,
        1,
    )
    .await;
    world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");
    world
        .db
        .admit_subscription_draw(world.organization_id, "req-6:hold", 3 * ONE)
        .await
        .expect("draws");

    // The turn billed 2 of the 3 drawn: one returns to the period.
    let settled = world
        .db
        .settle_subscription_draw("req-6:hold", 2 * ONE)
        .await
        .expect("settles")
        .expect("a draw stood");
    assert_eq!(settled, 2 * ONE);

    let sub = active(&world.db, world.organization_id).await;
    assert_eq!(sub.consumed_minor, 2 * ONE);
    let periods = world
        .db
        .subscription_periods(sub.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(periods[0].consumed_minor, 2 * ONE);

    // A replay answers the stored figure rather than settling twice.
    let replay = world
        .db
        .settle_subscription_draw("req-6:hold", 2 * ONE)
        .await
        .expect("replays")
        .expect("a draw stood");
    assert_eq!(replay, 2 * ONE);
}

#[tokio::test]
async fn settling_a_missing_draw_is_not_an_error() {
    let url = db_or_skip!();
    let world = world(&url).await;

    assert!(
        world
            .db
            .settle_subscription_draw("req-7:hold", 0)
            .await
            .expect("no draw, nothing to settle")
            .is_none()
    );
}

// ── rollover ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_lazy_rollover_closes_the_ended_window_on_the_next_draw() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("daily"),
        Some(ONE),
        ResetPeriod::Daily,
        true,
        1,
    )
    .await;
    let sub = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");
    world
        .db
        .admit_subscription_draw(world.organization_id, "req-8:hold", ONE / 2)
        .await
        .expect("draws");
    // Yesterday's window has ended.
    end_period_ago(&world.pool, sub.subscription_id, 3600).await;
    end_open_period_row_ago(&world.pool, sub.subscription_id, 3600).await;

    // The next draw rolls the boundary and counts inside the fresh window.
    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, "req-9:hold", ONE / 4)
        .await
        .expect("rolls and draws");
    assert_eq!(admission.drawn_minor, ONE / 4);

    let sub = active(&world.db, world.organization_id).await;
    assert_eq!(
        sub.consumed_minor,
        ONE / 4,
        "the fresh window counts, not the old burn"
    );
    let periods = world
        .db
        .subscription_periods(sub.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[1].outcome, "boundary");
    assert_eq!(periods[1].consumed_minor, ONE / 2);
    assert_eq!(periods[0].outcome, "open");
    assert_eq!(periods[0].consumed_minor, ONE / 4);
}

#[tokio::test]
async fn the_rollover_job_rolls_every_due_window() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("daily"),
        Some(ONE),
        ResetPeriod::Daily,
        true,
        1,
    )
    .await;
    let sub = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");
    end_period_ago(&world.pool, sub.subscription_id, 3600).await;
    end_open_period_row_ago(&world.pool, sub.subscription_id, 3600).await;

    let report = world
        .db
        .rollover_due_subscriptions()
        .await
        .expect("the pass runs");
    assert!(report.rolled >= 1);

    let sub = active(&world.db, world.organization_id).await;
    assert_eq!(sub.consumed_minor, 0);
    let periods = world
        .db
        .subscription_periods(sub.subscription_id)
        .await
        .expect("periods read");
    assert_eq!(periods.len(), 2);
    assert_eq!(periods[1].outcome, "boundary");
}

#[tokio::test]
async fn a_quiet_gap_rolls_every_boundary_it_spanned() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("daily"),
        Some(ONE),
        ResetPeriod::Daily,
        true,
        1,
    )
    .await;
    let sub = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");
    // Three days quiet: three windows must close, not one.
    end_period_ago(&world.pool, sub.subscription_id, 3 * 86400 + 3600).await;
    end_open_period_row_ago(&world.pool, sub.subscription_id, 3 * 86400 + 3600).await;

    let report = world
        .db
        .rollover_due_subscriptions()
        .await
        .expect("the pass runs");
    assert!(report.rolled >= 3, "every boundary the gap spanned rolled");

    let periods = world
        .db
        .subscription_periods(sub.subscription_id)
        .await
        .expect("periods read");
    // The bind window plus one row per spanned boundary, and one open.
    let closed = periods.iter().filter(|p| p.outcome == "boundary").count();
    assert!(closed >= 3);
    assert_eq!(periods[0].outcome, "open");
}

#[tokio::test]
async fn a_never_plan_holds_one_counter_until_cancelled() {
    let url = db_or_skip!();
    let world = world(&url).await;

    let plan = plan(
        &world.db,
        &fresh("lifetime"),
        Some(ONE),
        ResetPeriod::Never,
        true,
        1,
    )
    .await;
    let sub = world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds");
    assert!(
        sub.current_period_end.is_none(),
        "a never plan has no boundary"
    );

    // Time passing does not roll or expire it: the rollover job's scan needs
    // a non-NULL end, and the lazy path needs a due one — `never` has neither.
    let report = world
        .db
        .rollover_due_subscriptions()
        .await
        .expect("the pass runs");
    let sub_after = active(&world.db, world.organization_id).await;
    assert_eq!(sub_after.state, "active");
    assert_eq!(sub_after.subscription_id, sub.subscription_id);
    // The pass may have rolled other orgs' subscriptions in the shared suite;
    // it must not have touched this one.
    let _ = report;

    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, "req-10:hold", ONE / 2)
        .await
        .expect("still draws");
    assert_eq!(admission.drawn_minor, ONE / 2);
}

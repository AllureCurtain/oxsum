//! The transaction-aware settle (issue #602, roadmap T11, acceptance A12/A13):
//! the ledger entry, the operation's terminal row, the subscription draw's
//! consumption, the usage record and the watch-row clear commit as one unit —
//! and a fault mid-transaction leaves none of them behind — plus the typed
//! zero-cost usage receipt a fully entitlement-funded turn writes in place of
//! a fabricated 1-minor purchased hold.
//! Needs `DATABASE_URL`, skips without it.

use oxsum_core::{
    BillLine, CountsProvenance, Db, NewOperation, NewUser, OperationSettle, OperationState,
    ResetPeriod, Settlement, SettlementKind, Tenants, UsageRecord, UsageRow, WalletError,
    entry_id_for, settlement_key_for,
};
use sqlx::PgPool;
use time::Date;
use time::macros::date;
use uuid::Uuid;

const ONE: i64 = 1_000_000;
/// The posting date of the test writes, like the server's `today()`.
const D: Date = date!(2026 - 11 - 06);

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("settlement_tx")
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

/// A migrated database, one registered organization, its tenants handle.
struct World {
    db: Db,
    pool: PgPool,
    organization_id: Uuid,
    tenant_id: String,
    tenants: Tenants,
}

async fn world(url: &str) -> World {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let registration = db
        .register(NewUser {
            email: format!(
                "tx_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    World {
        db,
        tenants: Tenants::new(pool.clone()),
        pool,
        organization_id: registration.organization.id,
        tenant_id: registration.organization.tenant_id.clone(),
    }
}

/// The settlement description a settled turn would carry — the real shape,
/// `charged: 0` when the entitlement paid.
fn settle_description(request: &str, kind: SettlementKind, charged: i64, freeze: i64) -> String {
    let usage = UsageRecord::tokens(10, 2).unwrap();
    let lines = [
        BillLine {
            item: "input".into(),
            units: 0,
            price_per_m: 1_000_000,
        },
        BillLine {
            item: "output".into(),
            units: 0,
            price_per_m: 2_000_000,
        },
    ];
    Settlement {
        request,
        channel: "deepseek",
        model: "ds-chat",
        requested_model: "ds-chat",
        served_model: "ds-chat",
        upstream_model: None,
        price_version: 7,
        kind,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged,
        freeze,
        upstream_attempts: 1,
        mandate: None,
        subscription: None,
        cache: None,
        guardrail: Vec::new(),
        reason: None,
        experiment: None,
        sale: None,
    }
    .description()
    .expect("the settlement serializes")
}

fn new_operation(org: Uuid, fingerprint: &str, bound: i64) -> NewOperation {
    NewOperation {
        operation_id: Uuid::new_v4(),
        organization_id: org,
        key_id: None,
        surface: "chat".to_owned(),
        fingerprint: fingerprint.to_owned(),
        bound_minor: bound,
        model: "ds-chat".to_owned(),
        channel: Some("deepseek".to_owned()),
        customer_id: None,
        terms_version: Some(7),
    }
}

fn usage_row(
    request_id: &str,
    tenant_id: &str,
    hold_key: &str,
    charged: i64,
    freeze: i64,
) -> UsageRow {
    UsageRow {
        request_id: request_id.to_owned(),
        tenant_id: tenant_id.to_owned(),
        key_id: None,
        mandate_id: None,
        model: "ds-chat".to_owned(),
        channel: "deepseek".to_owned(),
        price_version: 7,
        kind: SettlementKind::Usage,
        entry_id: *entry_id_for(&settlement_key_for(hold_key)).as_uuid(),
        usage: UsageRecord::tokens(10, 2).unwrap(),
        status: "settled".to_owned(),
        failure_reason: None,
        counts: CountsProvenance::Provider {
            input: 10,
            output: 2,
        },
        charged_minor: charged,
        freeze_minor: freeze,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 1,
    }
}

/// How many entries the tenant ledger holds — the log size the tree head
/// reports.
async fn log_size(world: &World) -> u64 {
    let wallet = world
        .tenants
        .get(&world.tenant_id)
        .await
        .expect("opens the ledger");
    wallet.log_size().await.expect("reads")
}

/// A12's commit half: settle_in + settle_operation_in + the usage row + the
/// watch clear land in one commit — the operation is terminal under the same
/// transaction that moved the money.
#[tokio::test]
async fn a_settle_and_the_operation_close_commit_together() {
    let url = db_or_skip!();
    let world = world(&url).await;
    let wallet = world
        .tenants
        .get(&world.tenant_id)
        .await
        .expect("opens the ledger");
    wallet.top_up("top-1", 10 * ONE, D).await.expect("funds");

    let request_id = Uuid::new_v4().simple().to_string();
    let hold_key = format!("req-{request_id}:hold");
    let (operation, _) = world
        .db
        .insert_operation(&new_operation(
            world.organization_id,
            &format!("fp-{request_id}"),
            ONE,
        ))
        .await
        .expect("inserts the operation");
    assert!(
        world
            .db
            .bind_hold(operation.operation_id, &hold_key, "worker-1")
            .await
            .expect("binds")
    );
    wallet
        .hold(&hold_key, "a held turn", ONE / 2, D)
        .await
        .expect("takes the hold");

    let description = settle_description(
        &format!("req-{request_id}"),
        SettlementKind::Usage,
        100,
        ONE / 2,
    );
    let mut tx = world.pool.begin().await.expect("begins");
    wallet
        .settle_in(&mut tx, &hold_key, &description, 100, D)
        .await
        .expect("settles inside the transaction");
    assert_eq!(
        world
            .db
            .settle_operation_in(
                &mut tx,
                operation.operation_id,
                "worker-1",
                OperationState::Succeeded,
                100,
                "usage",
            )
            .await
            .expect("closes the operation"),
        OperationSettle::Settled,
    );
    world
        .db
        .record_usage_in(
            &mut tx,
            &usage_row(
                &format!("req-{request_id}"),
                &world.tenant_id,
                &hold_key,
                100,
                ONE / 2,
            ),
            None,
        )
        .await
        .expect("writes the usage row");
    world
        .db
        .clear_open_hold_in(&mut tx, &hold_key)
        .await
        .expect("clears the watch row");
    tx.commit().await.expect("commits");

    let closed = world
        .db
        .operation(world.organization_id, operation.operation_id)
        .await
        .expect("reads")
        .expect("the operation exists");
    assert_eq!(closed.state, OperationState::Succeeded);
    assert_eq!(closed.charged_minor, Some(100));
    // The ledger entry exists and proves — the charge and the close landed
    // in the same commit.
    assert!(
        wallet
            .settled_record(&hold_key)
            .await
            .expect("reads")
            .is_some(),
        "the settlement entry committed with the operation's close"
    );
}

/// A12's fault half: an injected failure after every write lands rolls the
/// whole unit back — no entry, no MMR node growth, no usage row, no operation
/// close, no subscription consumption.
#[tokio::test]
async fn a_fault_mid_settle_leaves_nothing_behind() {
    let url = db_or_skip!();
    let world = world(&url).await;
    let wallet = world
        .tenants
        .get(&world.tenant_id)
        .await
        .expect("opens the ledger");
    wallet.top_up("top-1", 10 * ONE, D).await.expect("funds");
    // A metered plan covering the whole freeze, so the subscription draw is
    // part of the unit that must roll back.
    let plan = world
        .db
        .set_subscription_plan(
            "plan-a",
            Some(10 * ONE),
            ResetPeriod::Monthly,
            true,
            None,
            None,
            true,
            1,
        )
        .await
        .expect("creates the plan");
    world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds the plan");

    let request_id = Uuid::new_v4().simple().to_string();
    let hold_key = format!("req-{request_id}:hold");
    let (operation, _) = world
        .db
        .insert_operation(&new_operation(
            world.organization_id,
            &format!("fp-{request_id}"),
            ONE,
        ))
        .await
        .expect("inserts the operation");
    assert!(
        world
            .db
            .bind_hold(operation.operation_id, &hold_key, "worker-1")
            .await
            .expect("binds")
    );
    wallet
        .hold(&hold_key, "a held turn", ONE / 4, D)
        .await
        .expect("takes the hold");
    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, &hold_key, ONE / 4)
        .await
        .expect("draws on the plan");
    assert_eq!(admission.drawn_minor, ONE / 4);

    let before = log_size(&world).await;
    let description = settle_description(
        &format!("req-{request_id}"),
        SettlementKind::Usage,
        50,
        ONE / 4,
    );

    let mut tx = world.pool.begin().await.expect("begins");
    world
        .db
        .settle_subscription_draw_in(&mut tx, &hold_key, 50)
        .await
        .expect("consumes the draw");
    wallet
        .settle_in(&mut tx, &hold_key, &description, 50, D)
        .await
        .expect("settles inside the transaction");
    world
        .db
        .record_usage_in(
            &mut tx,
            &usage_row(
                &format!("req-{request_id}"),
                &world.tenant_id,
                &hold_key,
                50,
                ONE / 4,
            ),
            None,
        )
        .await
        .expect("writes the usage row");
    world
        .db
        .settle_operation_in(
            &mut tx,
            operation.operation_id,
            "worker-1",
            OperationState::Succeeded,
            50,
            "usage",
        )
        .await
        .expect("closes the operation");
    // The injected fault: a statement that cannot succeed, so every write in
    // the transaction — entry, nodes, draw, usage, close — aborts with it.
    assert!(sqlx::query("SELECT 1/0").execute(&mut *tx).await.is_err());
    tx.rollback().await.expect("rolls back");

    // Nothing landed: the log did not grow (no entry, no proof leaf, no MMR
    // node), the operation is still open, the draw is unconsumed, the usage
    // row and the watch row are as the transaction found them.
    assert_eq!(
        log_size(&world).await,
        before,
        "a rolled-back settle leaves no ledger entry and no MMR node"
    );
    let still_open = world
        .db
        .operation(world.organization_id, operation.operation_id)
        .await
        .expect("reads")
        .expect("the operation exists");
    assert_eq!(
        still_open.state,
        OperationState::Leased,
        "the operation's close rolled back with the settle"
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT settled_minor FROM oxsum.subscription_draws WHERE hold_key = $1"
        )
        .bind(&hold_key)
        .fetch_one(&world.pool)
        .await
        .expect("reads"),
        None,
        "the draw's consumption rolled back with the settle"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM oxsum.usage_records WHERE request_id = $1"
        )
        .bind(format!("req-{request_id}"))
        .fetch_one(&world.pool)
        .await
        .expect("reads"),
        0,
        "the usage row rolled back with the settle"
    );
    // The hold is still outstanding — the sweeper owns the recovery, exactly
    // like a settle that never ran.
    assert!(
        wallet
            .settled_record(&hold_key)
            .await
            .expect("reads")
            .is_none(),
        "no settlement entry survived the rollback"
    );
    assert_eq!(wallet.reserved().await.expect("reads"), ONE / 4);
}

/// A13: the typed zero-cost receipt. A fully entitlement-funded turn writes a
/// balanced adjustments marker — the turn is recorded and provable, and no
/// balance, hold or revenue figure moves.
#[tokio::test]
async fn a_zero_cost_receipt_moves_no_money() {
    let url = db_or_skip!();
    let world = world(&url).await;
    let wallet = world
        .tenants
        .get(&world.tenant_id)
        .await
        .expect("opens the ledger");
    // No top-up at all: the receipt must land on a wallet with zero funds —
    // the whole point of A13.
    let request_id = Uuid::new_v4().simple().to_string();
    let hold_key = format!("req-{request_id}:hold");
    let description = settle_description(&format!("req-{request_id}"), SettlementKind::Usage, 0, 0);
    let actor = Uuid::new_v4().simple().to_string();

    let receipt = wallet
        .usage_receipt(&hold_key, &description, Some(&actor), None, D)
        .await
        .expect("the receipt lands with zero wallet funds");
    assert!(receipt.is_new);

    // Balances are untouched: nothing settled, nothing pending, no revenue.
    assert_eq!(wallet.settled().await.expect("reads"), 0);
    assert_eq!(wallet.reserved().await.expect("reads"), 0);
    assert_eq!(wallet.available().await.expect("reads"), 0);
    // The entry is a settlement record the verifier parses — the turn is
    // provable like any billed one.
    let record = wallet
        .settled_record(&hold_key)
        .await
        .expect("reads")
        .expect("the receipt is a settlement record");
    assert_eq!(record.kind, SettlementKind::Usage);
    assert_eq!(record.charged, 0);

    // A replay under the same key is a no-op; a different turn under it is a
    // conflict — the idempotency gate a wallet settle relies on holds for the
    // receipt too.
    let replay = wallet
        .usage_receipt(&hold_key, &description, Some(&actor), None, D)
        .await
        .expect("a replayed receipt replays");
    assert!(!replay.is_new);
    let other = settle_description("req-other", SettlementKind::Usage, 0, 0);
    assert!(
        matches!(
            wallet
                .usage_receipt(&hold_key, &other, Some(&actor), None, D)
                .await,
            Err(WalletError::Conflict(_))
        ),
        "a different record under the same key is the reuse conflict"
    );
}

/// A13's atomic half: the receipt rides the same transaction as the
/// operation's close — a fully-funded turn still leaves a terminal operation
/// and a provable record, with zero wallet funds throughout.
#[tokio::test]
async fn a_zero_cost_turn_settles_atomically() {
    let url = db_or_skip!();
    let world = world(&url).await;
    let wallet = world
        .tenants
        .get(&world.tenant_id)
        .await
        .expect("opens the ledger");
    let plan = world
        .db
        .set_subscription_plan(
            "plan-b",
            Some(10 * ONE),
            ResetPeriod::Monthly,
            true,
            None,
            None,
            true,
            1,
        )
        .await
        .expect("creates the plan");
    world
        .db
        .bind_subscription(world.organization_id, plan.plan_id, "admin", None)
        .await
        .expect("binds the plan");

    let request_id = Uuid::new_v4().simple().to_string();
    let hold_key = format!("req-{request_id}:hold");
    let (operation, _) = world
        .db
        .insert_operation(&new_operation(
            world.organization_id,
            &format!("fp-{request_id}"),
            ONE,
        ))
        .await
        .expect("inserts the operation");
    assert!(
        world
            .db
            .bind_hold(operation.operation_id, &hold_key, "worker-1")
            .await
            .expect("binds")
    );
    // The entitlement covers the whole freeze: no wallet hold is taken, the
    // draw is the reservation.
    let admission = world
        .db
        .admit_subscription_draw(world.organization_id, &hold_key, ONE)
        .await
        .expect("draws on the plan");
    assert_eq!(admission.drawn_minor, ONE);

    let description =
        settle_description(&format!("req-{request_id}"), SettlementKind::Usage, 0, ONE);
    let mut tx = world.pool.begin().await.expect("begins");
    world
        .db
        .settle_subscription_draw_in(&mut tx, &hold_key, 0)
        .await
        .expect("the draw settles at zero — the charge fit the quota");
    wallet
        .usage_receipt_in(&mut tx, &hold_key, &description, None, None, D)
        .await
        .expect("the receipt lands inside the transaction");
    assert_eq!(
        world
            .db
            .settle_operation_in(
                &mut tx,
                operation.operation_id,
                "worker-1",
                OperationState::Succeeded,
                0,
                "usage",
            )
            .await
            .expect("closes the operation"),
        OperationSettle::Settled,
    );
    world
        .db
        .record_usage_in(
            &mut tx,
            &usage_row(
                &format!("req-{request_id}"),
                &world.tenant_id,
                &hold_key,
                0,
                ONE,
            ),
            None,
        )
        .await
        .expect("writes the usage row");
    tx.commit().await.expect("commits");

    // The entitlement's counter: the draw consumed nothing, the released ONE
    // is back in the period.
    let subscription = world
        .db
        .active_subscription(world.organization_id)
        .await
        .expect("reads")
        .expect("the subscription stays active");
    assert_eq!(subscription.consumed_minor, 0);
    assert_eq!(wallet.settled().await.expect("reads"), 0);
    assert!(
        wallet
            .settled_record(&hold_key)
            .await
            .expect("reads")
            .is_some(),
        "the receipt committed with the operation's close"
    );
    let closed = world
        .db
        .operation(world.organization_id, operation.operation_id)
        .await
        .expect("reads")
        .expect("the operation exists");
    assert_eq!(closed.state, OperationState::Succeeded);
    assert_eq!(closed.charged_minor, Some(0));
}

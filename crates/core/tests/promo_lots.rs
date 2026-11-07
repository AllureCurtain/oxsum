//! Promotional lots and prepaid bundles (issue #609): grants land a lot plus
//! its ledger entry atomically, holds pin the exact lots that fund them in
//! expiry order, settlements debit the rows admission named, expiry burns the
//! unreserved remainder — and a release into a closed lot burns rather than
//! refunds. Needs DATABASE_URL; skips without it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;

use oxsum_core::*;
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

const D: time::Date = time::macros::date!(2026 - 10 - 01);
const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("promo_lots")
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

/// A migrated database, a registered organization and its wallet — the world
/// every lot lives in.
async fn world() -> Option<(Db, Uuid, String, sqlx::PgPool, Arc<Wallet>)> {
    let url = url()?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "promo lot fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = registration.organization.id;
    let tenant = registration.organization.tenant_id;
    let pool = db.pool().clone();
    let tenants = Tenants::new(pool.clone());
    let wallet = tenants.get(&tenant).await.unwrap();
    Some((db, org, tenant, pool, wallet))
}

/// The promo pool's settled balance — `Equity:Promo` net of debits and credits.
async fn promo_settled(pool: &sqlx::PgPool, tenant: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint \
         FROM ledger_{tenant}.postings p \
         JOIN ledger_{tenant}.entries e ON e.entry_id=p.entry_id \
         JOIN ledger_{tenant}.accounts a ON a.account_index=p.account_index \
         WHERE a.path='Equity:Promo' AND p.layer='settled' AND e.log_index IS NOT NULL"
    ))
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The promo-expiry sink's balance — what expired/cancelled lots burned.
async fn promo_expired_total(pool: &sqlx::PgPool, tenant: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint \
         FROM ledger_{tenant}.postings p \
         JOIN ledger_{tenant}.entries e ON e.entry_id=p.entry_id \
         JOIN ledger_{tenant}.accounts a ON a.account_index=p.account_index \
         WHERE a.path='Equity:PromoExpiry' AND p.layer='settled' AND e.log_index IS NOT NULL"
    ))
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The draws a hold pinned, in the order the allocation took them.
async fn draws(pool: &sqlx::PgPool, hold_key: &str) -> Vec<(Uuid, i64, Option<i64>, Option<i64>)> {
    sqlx::query(
        "SELECT lot_id, amount_minor, charged_minor, released_minor \
         FROM oxsum.promo_draws WHERE hold_key=$1 ORDER BY lot_id",
    )
    .bind(hold_key)
    .fetch_all(pool)
    .await
    .unwrap()
    .iter()
    .map(|r| {
        (
            r.get::<Uuid, _>("lot_id"),
            r.get::<i64, _>("amount_minor"),
            r.get::<Option<i64>, _>("charged_minor"),
            r.get::<Option<i64>, _>("released_minor"),
        )
    })
    .collect()
}

/// A grant lands the lot row and the `debit Adjustments / credit Promo` entry
/// atomically; replaying the source ref answers the standing lot.
#[tokio::test]
async fn a_grant_posts_the_lot_and_its_entry_once() {
    let Some((db, org, tenant, pool, wallet)) = world().await else {
        return;
    };
    let expires = OffsetDateTime::now_utc() + Duration::days(30);
    let lot = db
        .grant_promo_lot(
            &wallet,
            org,
            "admin",
            "welcome-1",
            5 * ONE,
            Some(expires),
            D,
        )
        .await
        .unwrap();
    assert_eq!(lot.amount_minor, 5 * ONE);
    assert_eq!(lot.remaining_minor, 5 * ONE);
    assert_eq!(lot.source, "admin");
    assert_eq!(lot.source_ref, "welcome-1");
    assert_eq!(promo_settled(&pool, &tenant).await, 5 * ONE);
    // The granted money is spendable — it sits inside `available`.
    assert_eq!(wallet.available().await.unwrap(), 5 * ONE);

    // Replay: same source ref answers the standing lot, grants nothing twice.
    let again = db
        .grant_promo_lot(
            &wallet,
            org,
            "admin",
            "welcome-1",
            5 * ONE,
            Some(expires),
            D,
        )
        .await
        .unwrap();
    assert_eq!(again.lot_id, lot.lot_id);
    assert_eq!(promo_settled(&pool, &tenant).await, 5 * ONE);
    assert_eq!(db.promo_lots(org, false).await.unwrap().len(), 1);
}

/// Holds pin lots in expiry order: the soonest-expiring open lot funds first,
/// a never-expiring one last — and `promo_draws` records the pick.
#[tokio::test]
async fn a_hold_pins_the_soonest_expiring_lots_first() {
    let Some((db, org, _tenant, pool, wallet)) = world().await else {
        return;
    };
    let later = db
        .grant_promo_lot(
            &wallet,
            org,
            "admin",
            "later",
            3 * ONE,
            Some(OffsetDateTime::now_utc() + Duration::days(60)),
            D,
        )
        .await
        .unwrap();
    let sooner = db
        .grant_promo_lot(
            &wallet,
            org,
            "admin",
            "sooner",
            2 * ONE,
            Some(OffsetDateTime::now_utc() + Duration::days(7)),
            D,
        )
        .await
        .unwrap();
    let never = db
        .grant_promo_lot(&wallet, org, "admin", "never", 4 * ONE, None, D)
        .await
        .unwrap();

    // A hold of 4 credits: `sooner` covers 2, `later` covers the other 2 —
    // `never`, the never-expiring lot, funds none of it.
    wallet.hold("h1", "hold", 4 * ONE, D).await.unwrap();
    let rows = draws(&pool, "h1").await;
    assert_eq!(rows.len(), 2);
    let pinned: HashMap<Uuid, i64> = rows.iter().map(|(l, a, _, _)| (*l, *a)).collect();
    assert_eq!(pinned.get(&sooner.lot_id), Some(&(2 * ONE)));
    assert_eq!(pinned.get(&later.lot_id), Some(&(2 * ONE)));
    assert!(!pinned.contains_key(&never.lot_id));

    // The lots' unreserved remainders dropped by what the hold pinned.
    let lots = db.promo_lots(org, false).await.unwrap();
    let by_id: HashMap<Uuid, PromoLot> = lots.into_iter().map(|l| (l.lot_id, l)).collect();
    assert_eq!(by_id[&sooner.lot_id].remaining_minor, 0);
    assert_eq!(by_id[&later.lot_id].remaining_minor, ONE);
    assert_eq!(by_id[&never.lot_id].remaining_minor, 4 * ONE);
    assert_eq!(by_id[&sooner.lot_id].reserved_minor, 2 * ONE);
}

/// A20: a lot expiring mid-hold never double-charges the turn — the pinned
/// draw's charged part debits the lot, its released share burns to
/// `Equity:PromoExpiry` because the lot already closed.
#[tokio::test]
async fn expiry_during_a_hold_burns_only_the_released_share() {
    let Some((db, org, tenant, pool, wallet)) = world().await else {
        return;
    };
    let lot = db
        .grant_promo_lot(
            &wallet,
            org,
            "admin",
            "burning",
            5 * ONE,
            // Already past: the expiry pass sees it due immediately.
            Some(OffsetDateTime::now_utc() - Duration::seconds(1)),
            D,
        )
        .await
        .unwrap();
    // The hold pins the lot's whole remainder before expiry runs: admission
    // locks the row and `pool_split` read the same available the pin decremented.
    wallet.hold("h-exp", "hold", 5 * ONE, D).await.unwrap();
    assert_eq!(draws(&pool, "h-exp").await.len(), 1);

    // Expiry now: the unreserved remainder is 0 — the hold pinned it all — so
    // the pass closes the lot without burning a minor.
    let tenants = Tenants::new(db.pool().clone());
    let expired = db.expire_due_promo_lots(&tenants, 100, D).await.unwrap();
    assert_eq!(expired, 1);
    assert_eq!(promo_expired_total(&pool, &tenant).await, 0);
    let lot = db.promo_lot(lot.lot_id).await.unwrap().unwrap();
    assert_eq!(lot.close_reason.as_deref(), Some("expired"));

    // Settle at 3 of the 5 held: the charged 3 debits the promo pool, the
    // released 2 returns to a closed lot — so it burns to PromoExpiry instead.
    wallet.settle("h-exp", "settle", 3 * ONE, D).await.unwrap();
    let lot = db.promo_lot(lot.lot_id).await.unwrap().unwrap();
    assert_eq!(lot.remaining_minor, 0, "released promo never refunds");
    assert_eq!(promo_expired_total(&pool, &tenant).await, 2 * ONE);
    // The draw is settled exactly once: charged + released = the reservation.
    let rows = draws(&pool, "h-exp").await;
    assert_eq!(
        rows,
        vec![(lot.lot_id, 5 * ONE, Some(3 * ONE), Some(2 * ONE))]
    );
    // The wallet now holds exactly what the charge left: 5 granted − 3 charged −
    // 2 burned = 0 spendable. The hold's full 5 released back, the charge took 3.
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// Concurrent expiry and settlement converge on the same books: whichever
/// order the pass and the settle run, the released share of a closed lot ends
/// in `Equity:PromoExpiry` and `remaining_minor` never revives it.
#[tokio::test]
async fn concurrent_expiry_and_settlement_still_converges() {
    let Some((db, org, tenant, pool, wallet)) = world().await else {
        return;
    };
    let lot = db
        .grant_promo_lot(
            &wallet,
            org,
            "admin",
            "race",
            4 * ONE,
            Some(OffsetDateTime::now_utc() - Duration::seconds(1)),
            D,
        )
        .await
        .unwrap();
    wallet.hold("h-race", "hold", 4 * ONE, D).await.unwrap();

    let tenants = Tenants::new(db.pool().clone());
    let (expired, settle) = tokio::join!(
        db.expire_due_promo_lots(&tenants, 100, D),
        wallet.settle("h-race", "settle", ONE, D),
    );
    expired.unwrap();
    settle.unwrap();

    // Whichever order they ran, the books close the same way: the lot closed
    // `expired`, the charged minor debited promo, the released rest burned.
    let lot = db.promo_lot(lot.lot_id).await.unwrap().unwrap();
    assert_eq!(lot.close_reason.as_deref(), Some("expired"));
    assert_eq!(lot.remaining_minor, 0);
    assert_eq!(promo_expired_total(&pool, &tenant).await, 3 * ONE);
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// A refunded bundle cancels the promo lot: the unspent remainder burns, the
/// purchased credit the same order posted stays in the wallet.
#[tokio::test]
async fn a_cancelled_bundle_lot_burns_but_the_purchase_stays() {
    let Some((db, org, tenant, pool, wallet)) = world().await else {
        return;
    };
    // The purchased half: wallet credit the bundle's `top_up` leg posted.
    wallet.top_up("bundle:1", 10 * ONE, D).await.unwrap();
    // The promo half: the `bundle_promo` lot the fulfillment granted.
    let lot = db
        .grant_promo_lot(&wallet, org, "bundle_promo", "order-1", 3 * ONE, None, D)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 13 * ONE);

    // The refund cancels the promo side only.
    let cancelled = db.cancel_promo_lot(&wallet, lot.lot_id, D).await.unwrap();
    assert_eq!(cancelled.close_reason.as_deref(), Some("cancelled"));
    assert_eq!(cancelled.remaining_minor, 0);
    assert_eq!(promo_expired_total(&pool, &tenant).await, 3 * ONE);
    // Purchased credit intact — promo was never refundable.
    assert_eq!(wallet.available().await.unwrap(), 10 * ONE);

    // Idempotent: a repeated cancel answers the standing closed lot.
    let again = db.cancel_promo_lot(&wallet, lot.lot_id, D).await.unwrap();
    assert_eq!(again.lot_id, lot.lot_id);
    assert_eq!(promo_expired_total(&pool, &tenant).await, 3 * ONE);
}

/// An organization with no purchased balance still serves: promo-only funding
/// pins a hold, settles its charge, and never invents wallet money.
#[tokio::test]
async fn a_zero_purchase_wallet_serves_on_promo_alone() {
    let Some((db, org, _tenant, pool, wallet)) = world().await else {
        return;
    };
    db.grant_promo_lot(&wallet, org, "admin", "only", 6 * ONE, None, D)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 6 * ONE);

    wallet.hold("h-only", "hold", 4 * ONE, D).await.unwrap();
    assert_eq!(draws(&pool, "h-only").await.len(), 1);
    wallet.settle("h-only", "settle", 3 * ONE, D).await.unwrap();

    // 6 granted − 3 charged = 3 spendable; the released 1 returned to the lot.
    assert_eq!(wallet.available().await.unwrap(), 3 * ONE);
    let lots = db.promo_lots(org, false).await.unwrap();
    assert_eq!(lots[0].remaining_minor, 3 * ONE);
}

/// The legacy migration: an aggregate bonus balance moves into a
/// `legacy_migration` lot that never expires — net balances unchanged, and a
/// reopen is a no-op.
#[tokio::test]
async fn the_legacy_bonus_migrates_once_and_reconciles() {
    let url = db_or_skip!();
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "legacy fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = registration.organization.id;
    let tenant = registration.organization.tenant_id;
    let tenants = Tenants::new(db.pool().clone());

    // Grant into the bonus pool the way pre-promo grants always did — `adjust`
    // credits `Equity:Bonus` — then reopen so `migrate_legacy_bonus` runs.
    let wallet = tenants.get(&tenant).await.unwrap();
    wallet
        .adjust("g1", "signup bonus", 7 * ONE, D)
        .await
        .unwrap();
    wallet
        .adjust("g2", "migration grant", 2 * ONE, D)
        .await
        .unwrap();
    let before = wallet.available().await.unwrap();
    assert_eq!(before, 9 * ONE);

    // Reopen: the aggregate bonus migrates into a never-expiring lot. The
    // migration runs inside `Wallet::open`, and `Tenants::get` caches the first
    // open — so the reopen has to be a fresh `Wallet::open`, not a cached get.
    let migrated = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let lots = db.promo_lots(org, false).await.unwrap();
    assert_eq!(lots.len(), 1);
    assert_eq!(lots[0].source, "legacy_migration");
    assert_eq!(lots[0].amount_minor, 9 * ONE);
    assert_eq!(lots[0].remaining_minor, 9 * ONE);
    assert!(lots[0].expires_at.is_none());
    // Net balances never moved: the money still spends, just under a new pool.
    assert_eq!(migrated.available().await.unwrap(), before);
    // A second open is a no-op — the marker entry and the lot anchor agree.
    let _ = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    assert_eq!(db.promo_lots(org, false).await.unwrap().len(), 1);
}

/// A hold spanning promo + wallet pins promo first and the rest falls through
/// to the purchased pool, so a settlement debits promo exactly what the charge
/// took from it.
#[tokio::test]
async fn a_hold_larger_than_promo_spills_into_the_wallet() {
    let Some((db, org, tenant, pool, wallet)) = world().await else {
        return;
    };
    wallet.top_up("t1", 10 * ONE, D).await.unwrap();
    let lot = db
        .grant_promo_lot(&wallet, org, "admin", "top-off", 2 * ONE, None, D)
        .await
        .unwrap();
    // A 5-credit hold: 2 promo + 3 purchased.
    wallet.hold("h-split", "hold", 5 * ONE, D).await.unwrap();
    assert_eq!(draws(&pool, "h-split").await.len(), 1);

    wallet
        .settle("h-split", "settle", 4 * ONE, D)
        .await
        .unwrap();
    // The charge took promo's whole pinned 2 plus 2 from the wallet; the
    // released 1 went back to the wallet — the lot's released share is 0.
    let lot = db.promo_lot(lot.lot_id).await.unwrap().unwrap();
    assert_eq!(lot.remaining_minor, 0);
    assert_eq!(wallet.available().await.unwrap(), 10 * ONE - 2 * ONE);
    // PromoExpiry saw nothing: the lot never closed, nothing burned.
    assert_eq!(promo_expired_total(&pool, &tenant).await, 0);
}

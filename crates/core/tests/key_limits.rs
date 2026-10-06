//! Per-key spend limits (issue #32), against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of
//! failing, so a bare `cargo test` still passes.
//!
//! A key's committed spend — settled charges plus outstanding holds attributed to it —
//! may not exceed its `spend_limit_minor`. The limit is checked inside `hold_for_key`,
//! serialized per key by an advisory lock held across the ledger append, so concurrent
//! holds cannot together exceed it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    ActingKey, BudgetDuration, Db, KeyConstraints, KeyScope, NewUser, Tenants, Wallet, WalletError,
};
use sqlx::PgPool;
use time::macros::date;
use uuid::Uuid;

const D: time::Date = date!(2026 - 10 - 01);
const ONE: i64 = 1_000_000;

/// Constraints carrying just a cumulative spend limit, the common case here.
fn limit(minor: i64) -> KeyConstraints {
    KeyConstraints {
        spend_limit_minor: Some(minor),
        ..Default::default()
    }
}

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
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

/// One pool with oxsum's own tables migrated, as the server has at startup.
async fn db(url: &str, max_connections: u32) -> Db {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    db
}

/// A user with a funded wallet, and the acting key of their first API key.
struct World {
    wallet: std::sync::Arc<Wallet>,
    key: ActingKey,
    secret: String,
    creator: Uuid,
    organization_id: Uuid,
}

async fn world(db: &Db, name: &str) -> World {
    let email = format!(
        "{}_{}@example.com",
        name,
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let registration = db
        .register(NewUser {
            email,
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .unwrap();
    let wallet = Tenants::new(db.pool().clone())
        .get(&registration.organization.tenant_id)
        .await
        .unwrap();
    wallet.top_up("topup", 1_000 * ONE, D).await.unwrap();
    let (organization, key) = db
        .authenticate(&registration.api_key.secret)
        .await
        .unwrap()
        .unwrap();
    World {
        wallet,
        key,
        secret: registration.api_key.secret,
        creator: registration.user.id,
        organization_id: organization.id,
    }
}

/// A hold that fits under the limit lands; one that would push the key past it is refused,
/// and the refusal names the numbers.
#[tokio::test]
async fn hold_for_key_enforces_the_limit() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "limit").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        limit(10 * ONE),
    )
    .await
    .unwrap();

    world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", 5 * ONE, D)
        .await
        .unwrap_err();
    let (limit, committed) = match err {
        WalletError::KeyLimitExceeded {
            limit_minor,
            committed_minor,
        } => (limit_minor, committed_minor),
        other => panic!("wrong error: {other:?}"),
    };
    assert_eq!(limit, 10 * ONE);
    assert_eq!(committed, 6 * ONE);
    assert_eq!(
        world.wallet.key_committed(&world.key.key_id).await.unwrap(),
        6 * ONE
    );
    // Exactly at the limit still fits.
    world
        .wallet
        .hold_for_key(&world.key, None, "h3", "", 4 * ONE, D)
        .await
        .unwrap();
}

/// Settled charges count toward the limit too: a hold released and partly charged leaves
/// the charge committed.
#[tokio::test]
async fn settled_spend_counts_toward_the_limit() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "settled").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        limit(10 * ONE),
    )
    .await
    .unwrap();

    world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    // Settles 2 of the 6: 4 return to the balance, 2 stay committed as the charge.
    world.wallet.settle("h1", "", 2 * ONE, D).await.unwrap();
    assert_eq!(
        world.wallet.key_committed(&world.key.key_id).await.unwrap(),
        2 * ONE
    );

    // 2 committed + 9 held would exceed 10.
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", 9 * ONE, D)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "wrong error: {err:?}"
    );
    // 2 committed + 8 held fits exactly.
    world
        .wallet
        .hold_for_key(&world.key, None, "h3", "", 8 * ONE, D)
        .await
        .unwrap();
}

/// The done-when of issue #32: racing holds against one key's limit cannot together
/// exceed it. Ten tasks each hold 300 of a 1000 limit; the serialization makes the
/// outcome deterministic — exactly three succeed, 900 committed.
#[tokio::test]
async fn concurrent_holds_cannot_exceed_the_limit() {
    let url = db_or_skip!();
    // Each in-flight hold_for_key transiently needs two pool connections: the outer
    // transaction holding the per-key advisory lock, and the engine's append inside it.
    let db = db(&url, 12).await;
    let world = world(&db, "racing").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        limit(1_000),
    )
    .await
    .unwrap();

    let mut tasks = Vec::new();
    for i in 0..10 {
        let wallet = world.wallet.clone();
        let key = world.key.clone();
        tasks.push(tokio::spawn(async move {
            wallet
                .hold_for_key(&key, None, &format!("race-{i}"), "", 300, D)
                .await
        }));
    }
    let mut ok = 0;
    let mut refused = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => ok += 1,
            Err(WalletError::KeyLimitExceeded { .. }) => refused += 1,
            Err(other) => panic!("wrong error: {other:?}"),
        }
    }
    assert_eq!(ok, 3, "exactly three holds of 300 fit under a 1000 limit");
    assert_eq!(refused, 7);
    assert_eq!(
        world.wallet.key_committed(&world.key.key_id).await.unwrap(),
        900
    );
}

/// The limit in force is the one on the row when the hold is taken, not the one the
/// request authenticated with: lowering it afterwards refuses the next hold.
#[tokio::test]
async fn limit_changes_apply_to_later_holds() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "change").await;
    let organization_id = world.organization_id;

    // No limit: the hold lands.
    world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    // Lower the limit below what is already committed: the next hold is refused.
    db.update_key_constraints(
        organization_id,
        world.key.key_id,
        KeyScope::Organization,
        limit(ONE),
    )
    .await
    .unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", ONE, D)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "wrong error: {err:?}"
    );
    // Clearing it back to unlimited lets the hold through.
    db.update_key_constraints(
        organization_id,
        world.key.key_id,
        KeyScope::Organization,
        KeyConstraints::default(),
    )
    .await
    .unwrap();
    world
        .wallet
        .hold_for_key(&world.key, None, "h3", "", ONE, D)
        .await
        .unwrap();
}

/// A key with no limit attributes its spend all the same, so a limit added later counts
/// history — and the hold itself is unaffected by the missing limit.
#[tokio::test]
async fn unlimited_key_holds_freely_but_attributes() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "unlimited").await;
    assert!(world.key.spend_limit_minor.is_none());

    world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    assert_eq!(
        world.wallet.key_committed(&world.key.key_id).await.unwrap(),
        6 * ONE
    );
}

/// The PATCH scope rules are the revoke's: a member may change only the keys they created.
#[tokio::test]
async fn update_key_limit_scope_rules() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "scope").await;
    let organization_id = world.organization_id;
    let other = Uuid::new_v4();

    // A member naming a key they did not create gets the same answer as a missing key.
    assert!(
        db.update_key_constraints(
            organization_id,
            world.key.key_id,
            KeyScope::Own(other),
            limit(ONE)
        )
        .await
        .unwrap()
        .is_none()
    );
    // The key is untouched.
    let (_, key) = db.authenticate(&world.secret).await.unwrap().unwrap();
    assert!(key.spend_limit_minor.is_none());

    // The creator may change their own.
    let updated = db
        .update_key_constraints(
            organization_id,
            world.key.key_id,
            KeyScope::Own(world.creator),
            limit(5 * ONE),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.spend_limit_minor, Some(5 * ONE));
}

/// A negative limit is refused at the API boundary, on create and on update.
#[tokio::test]
async fn negative_limit_is_refused() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "negative").await;
    let organization_id = world.organization_id;

    let err = db
        .create_key(
            organization_id,
            None,
            None,
            None,
            KeyConstraints {
                spend_limit_minor: Some(-1),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::InvalidInput(_)),
        "wrong error: {err:?}"
    );
    let err = db
        .update_key_constraints(
            organization_id,
            world.key.key_id,
            KeyScope::Organization,
            KeyConstraints {
                spend_limit_minor: Some(-1),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::InvalidInput(_)),
        "wrong error: {err:?}"
    );
}

/// An identical retry replays rather than spending again: the limit guards new spend, so a
/// retry that lands after the limit filled up answers with the original receipt, not 429.
#[tokio::test]
async fn identical_retry_replays_past_a_full_limit() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "replay").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        limit(10 * ONE),
    )
    .await
    .unwrap();

    let first = world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    assert!(first.is_new);
    // Fill the limit with a second hold.
    world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", 4 * ONE, D)
        .await
        .unwrap();
    // The identical retry of the first hold replays: same entry, nothing new written.
    let replay = world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    assert!(!replay.is_new);
    assert_eq!(replay.entry_id, first.entry_id);
    assert_eq!(
        world.wallet.key_committed(&world.key.key_id).await.unwrap(),
        10 * ONE
    );
    // A conflicting reuse of the key is still the engine's conflict, not a limit refusal.
    let err = world
        .wallet
        .hold_for_key(
            &world.key,
            None,
            "h1",
            "a different description",
            6 * ONE,
            D,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::Conflict(_)),
        "wrong error: {err:?}"
    );
}

/// Constraints carrying a periodic budget: the limit applies within the window.
fn budget(minor: i64, duration: BudgetDuration) -> KeyConstraints {
    KeyConstraints {
        spend_limit_minor: Some(minor),
        budget_duration: Some(duration),
        ..Default::default()
    }
}

/// Constraints carrying a model allowlist alone.
fn allowlist(models: &[&str]) -> KeyConstraints {
    KeyConstraints {
        model_allowlist: Some(models.iter().map(|m| m.to_string()).collect()),
        ..Default::default()
    }
}

/// A daily budget counts only the current UTC day's settled charges: yesterday's settled
/// charge no longer counts, but a hold still outstanding from yesterday does — it still
/// reserves money now.
#[tokio::test]
async fn a_daily_budget_counts_the_current_day() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "daily").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        budget(10 * ONE, BudgetDuration::Daily),
    )
    .await
    .unwrap();

    // Yesterday: a hold settled for 4, and a 2 hold left outstanding.
    world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    world.wallet.settle("h1", "", 4 * ONE, D).await.unwrap();
    world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", 2 * ONE, D)
        .await
        .unwrap();

    // Today: committed is just the outstanding 2; the settled 4 fell out of the window.
    let tomorrow = D + time::Duration::days(1);
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "h3", "", 9 * ONE, tomorrow)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { committed_minor, .. } if committed_minor == 2 * ONE),
        "wrong error: {err:?}"
    );
    world
        .wallet
        .hold_for_key(&world.key, None, "h4", "", 8 * ONE, tomorrow)
        .await
        .unwrap();
}

/// Weekly and monthly windows start on their calendar boundaries: an ISO-week Monday and
/// the first of the month. Spend settled inside the window counts; outside it does not.
#[tokio::test]
async fn budget_windows_reset_on_their_boundaries() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "windows").await;
    let sunday = date!(2026 - 10 - 04);
    let monday = date!(2026 - 10 - 05);
    let november = date!(2026 - 11 - 01);

    // Weekly: D (a Thursday) and Sunday share one week; Monday starts the next.
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        budget(10 * ONE, BudgetDuration::Weekly),
    )
    .await
    .unwrap();
    world
        .wallet
        .hold_for_key(&world.key, None, "w1", "", 6 * ONE, D)
        .await
        .unwrap();
    world.wallet.settle("w1", "", 6 * ONE, D).await.unwrap();
    // Same week: the settled 6 still counts.
    assert!(
        world
            .wallet
            .hold_for_key(&world.key, None, "w2", "", 5 * ONE, sunday)
            .await
            .is_err(),
        "a 5 hold over a 6 charge does not fit a weekly 10"
    );
    // Next week: the window reset.
    world
        .wallet
        .hold_for_key(&world.key, None, "w3", "", 9 * ONE, monday)
        .await
        .unwrap();
    world
        .wallet
        .settle("w3", "", 9 * ONE, monday)
        .await
        .unwrap();

    // Monthly: D and November are different months.
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        budget(10 * ONE, BudgetDuration::Monthly),
    )
    .await
    .unwrap();
    // October's 9 is committed within October still.
    assert!(
        world
            .wallet
            .hold_for_key(&world.key, None, "m1", "", 5 * ONE, sunday)
            .await
            .is_err(),
        "the October 5 hold saw the October charge"
    );
    world
        .wallet
        .hold_for_key(&world.key, None, "m2", "", 8 * ONE, november)
        .await
        .unwrap();
}

/// The window is read inside the same per-key lock as the limit, so racing holds cannot
/// together exceed a period budget either.
#[tokio::test]
async fn concurrent_holds_cannot_exceed_a_period_budget() {
    let url = db_or_skip!();
    let db = db(&url, 12).await;
    let world = world(&db, "race-budget").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        budget(1_000, BudgetDuration::Daily),
    )
    .await
    .unwrap();

    let mut tasks = Vec::new();
    for i in 0..10 {
        let wallet = world.wallet.clone();
        let key = world.key.clone();
        tasks.push(tokio::spawn(async move {
            wallet
                .hold_for_key(&key, None, &format!("race-{i}"), "", 300, D)
                .await
        }));
    }
    let mut ok = 0;
    let mut refused = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => ok += 1,
            Err(WalletError::KeyLimitExceeded { .. }) => refused += 1,
            Err(other) => panic!("wrong error: {other:?}"),
        }
    }
    assert_eq!(ok, 3);
    assert_eq!(refused, 7);
}

/// The allowlist gates by exact model name: listed models hold, unlisted are forbidden,
/// and a `None` model — the generic holds path — is never gated.
#[tokio::test]
async fn a_model_allowlist_gates_named_models() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "allowlist").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        allowlist(&["mock-a", "mock-b"]),
    )
    .await
    .unwrap();

    world
        .wallet
        .hold_for_key(&world.key, Some("mock-a"), "h1", "", ONE, D)
        .await
        .unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, Some("mock-c"), "h2", "", ONE, D)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::Forbidden(_)),
        "wrong error: {err:?}"
    );
    // No model named: the list does not apply.
    world
        .wallet
        .hold_for_key(&world.key, None, "h3", "", ONE, D)
        .await
        .unwrap();
}

/// A replayed hold answers with its own receipt even when the allowlist has since dropped
/// the model — the list guards new spend, like the limit does.
#[tokio::test]
async fn a_replayed_hold_survives_an_allowlist_change() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "replay-model").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        allowlist(&["mock-a", "mock-b"]),
    )
    .await
    .unwrap();

    let first = world
        .wallet
        .hold_for_key(&world.key, Some("mock-a"), "h1", "", ONE, D)
        .await
        .unwrap();
    // The list drops mock-a.
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        allowlist(&["mock-b"]),
    )
    .await
    .unwrap();
    let replay = world
        .wallet
        .hold_for_key(&world.key, Some("mock-a"), "h1", "", ONE, D)
        .await
        .unwrap();
    assert!(!replay.is_new);
    assert_eq!(replay.entry_id, first.entry_id);
}

/// Constraints carrying only the outstanding-holds cap.
fn holds_cap(n: i32) -> KeyConstraints {
    KeyConstraints {
        max_concurrent_holds: Some(n),
        ..Default::default()
    }
}

/// The cap on outstanding holds: a key may have at most `maxConcurrentHolds`
/// reservations open at once, the count comes from the ledger's own pending
/// entries, and a settlement frees the slot it took.
#[tokio::test]
async fn a_concurrent_holds_cap_counts_open_holds() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "cap").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        holds_cap(1),
    )
    .await
    .unwrap();

    let first = world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", ONE, D)
        .await
        .unwrap();
    assert!(first.is_new);

    // One hold open is the cap: a second is refused, and the refusal names the count.
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", ONE, D)
        .await
        .unwrap_err();
    let (limit, open) = match err {
        WalletError::TooManyHolds { limit, open } => (limit, open),
        other => panic!("wrong error: {other:?}"),
    };
    assert_eq!(limit, 1);
    assert_eq!(open, 1);

    // The identical retry is a replay, answered before the cap is counted.
    let replay = world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", ONE, D)
        .await
        .unwrap();
    assert!(!replay.is_new);
    assert_eq!(replay.entry_id, first.entry_id);

    // Settling frees the slot: the release leg cancels the hold's pending debit,
    // so the next hold fits under the same cap.
    world.wallet.settle("h1", "", ONE, D).await.unwrap();
    world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", ONE, D)
        .await
        .unwrap();
}

/// A key without the cap is unbounded: the cap column is null and nothing counts.
#[tokio::test]
async fn an_uncapped_key_holds_freely() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "nocap").await;
    for n in 1..=3 {
        world
            .wallet
            .hold_for_key(&world.key, None, &format!("h{n}"), "", ONE, D)
            .await
            .unwrap();
    }
}

/// The cap is part of the replace-all constraint set: a patch that leaves it out
/// clears it back to uncapped, and a zero is refused at the boundary.
#[tokio::test]
async fn the_holds_cap_patches_and_validates() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "cappatch").await;

    // Zero is not a cap: both rate fields require at least 1.
    for constraints in [
        KeyConstraints {
            max_concurrent_holds: Some(0),
            ..Default::default()
        },
        KeyConstraints {
            requests_per_minute: Some(0),
            ..Default::default()
        },
    ] {
        let err = db
            .update_key_constraints(
                world.organization_id,
                world.key.key_id,
                KeyScope::Organization,
                constraints,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, WalletError::InvalidInput(_)),
            "wrong error: {err:?}"
        );
    }

    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        holds_cap(1),
    )
    .await
    .unwrap();
    world
        .wallet
        .hold_for_key(&world.key, None, "h1", "", ONE, D)
        .await
        .unwrap();
    world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", ONE, D)
        .await
        .expect_err("the cap holds");

    // A patch naming only the spend limit clears the cap: replace-all semantics.
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        limit(10 * ONE),
    )
    .await
    .unwrap();
    world
        .wallet
        .hold_for_key(&world.key, None, "h2", "", ONE, D)
        .await
        .expect("the cap was cleared");
}

/// Two keys of one organization cap separately: the pending count is attributed
/// to the acting key, not to the wallet.
#[tokio::test]
async fn the_holds_cap_is_per_key() {
    let url = db_or_skip!();
    let db = db(&url, 5).await;
    let world = world(&db, "perkey").await;

    let second = db
        .create_key(
            world.organization_id,
            Some("second".to_owned()),
            None,
            Some(world.creator),
            KeyConstraints::default(),
        )
        .await
        .unwrap();
    let (_, second_key) = db.authenticate(&second.secret).await.unwrap().unwrap();

    db.update_key_constraints(
        world.organization_id,
        second_key.key_id,
        KeyScope::Organization,
        holds_cap(1),
    )
    .await
    .unwrap();

    // The first key's hold does not spend the second key's cap.
    world
        .wallet
        .hold_for_key(&world.key, None, "h-first", "", ONE, D)
        .await
        .unwrap();
    world
        .wallet
        .hold_for_key(&second_key, None, "h-second-1", "", ONE, D)
        .await
        .unwrap();
    world
        .wallet
        .hold_for_key(&second_key, None, "h-second-2", "", ONE, D)
        .await
        .expect_err("the second key's own cap holds");
}

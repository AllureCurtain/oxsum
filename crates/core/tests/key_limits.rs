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

use oxsum_core::{ActingKey, Db, KeyScope, NewUser, Tenants, Wallet, WalletError};
use sqlx::PgPool;
use time::macros::date;
use uuid::Uuid;

const D: time::Date = date!(2026 - 10 - 01);
const ONE: i64 = 1_000_000;

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
    db.update_key_limit(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        Some(10 * ONE),
    )
    .await
    .unwrap();

    world
        .wallet
        .hold_for_key(&world.key, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, "h2", "", 5 * ONE, D)
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
    assert_eq!(world.wallet.key_committed(&world.key.key_id).await.unwrap(), 6 * ONE);
    // Exactly at the limit still fits.
    world
        .wallet
        .hold_for_key(&world.key, "h3", "", 4 * ONE, D)
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
    db.update_key_limit(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        Some(10 * ONE),
    )
    .await
    .unwrap();

    world
        .wallet
        .hold_for_key(&world.key, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    // Settles 2 of the 6: 4 return to the balance, 2 stay committed as the charge.
    world.wallet.settle("h1", "", 2 * ONE, D).await.unwrap();
    assert_eq!(world.wallet.key_committed(&world.key.key_id).await.unwrap(), 2 * ONE);

    // 2 committed + 9 held would exceed 10.
    let err = world
        .wallet
        .hold_for_key(&world.key, "h2", "", 9 * ONE, D)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "wrong error: {err:?}"
    );
    // 2 committed + 8 held fits exactly.
    world
        .wallet
        .hold_for_key(&world.key, "h3", "", 8 * ONE, D)
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
    db.update_key_limit(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        Some(1_000),
    )
    .await
    .unwrap();

    let mut tasks = Vec::new();
    for i in 0..10 {
        let wallet = world.wallet.clone();
        let key = world.key.clone();
        tasks.push(tokio::spawn(async move {
            wallet
                .hold_for_key(&key, &format!("race-{i}"), "", 300, D)
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
        .hold_for_key(&world.key, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    // Lower the limit below what is already committed: the next hold is refused.
    db.update_key_limit(
        organization_id,
        world.key.key_id,
        KeyScope::Organization,
        Some(ONE),
    )
    .await
    .unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, "h2", "", ONE, D)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "wrong error: {err:?}"
    );
    // Clearing it back to unlimited lets the hold through.
    db.update_key_limit(organization_id, world.key.key_id, KeyScope::Organization, None)
        .await
        .unwrap();
    world
        .wallet
        .hold_for_key(&world.key, "h3", "", ONE, D)
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
        .hold_for_key(&world.key, "h1", "", 6 * ONE, D)
        .await
        .unwrap();
    assert_eq!(world.wallet.key_committed(&world.key.key_id).await.unwrap(), 6 * ONE);
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
        db.update_key_limit(organization_id, world.key.key_id, KeyScope::Own(other), Some(ONE))
            .await
            .unwrap()
            .is_none()
    );
    // The key is untouched.
    let (_, key) = db.authenticate(&world.secret).await.unwrap().unwrap();
    assert!(key.spend_limit_minor.is_none());

    // The creator may change their own.
    let updated = db
        .update_key_limit(
            organization_id,
            world.key.key_id,
            KeyScope::Own(world.creator),
            Some(5 * ONE),
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
        .create_key(organization_id, None, None, None, Some(-1))
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::InvalidInput(_)),
        "wrong error: {err:?}"
    );
    let err = db
        .update_key_limit(
            organization_id,
            world.key.key_id,
            KeyScope::Organization,
            Some(-1),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::InvalidInput(_)),
        "wrong error: {err:?}"
    );
}

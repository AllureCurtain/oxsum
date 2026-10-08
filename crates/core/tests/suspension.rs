//! Organization suspension and member budgets (issue #162), against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of
//! failing, so a bare `cargo test` still passes.
//!
//! A suspended organization's new holds refuse — session holds and key holds alike —
//! while holds already open still settle, because suspension gates admission, never the
//! ledger. A member's `budget_limit_minor` caps committed spend summed over every key
//! they minted, checked inside `hold_for_key`'s locked window.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    Db, KeyConstraints, MembershipActor, NewUser, ORG_SUSPENDED, Role, Tenants, Wallet, WalletError,
};
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

/// A migrated database, one registered organization with a funded wallet, its owner,
/// and the acting key of the first API key — the fixture every test here stands on.
struct World {
    db: Db,
    organization_id: Uuid,
    owner: Uuid,
    wallet: std::sync::Arc<Wallet>,
    key: oxsum_core::ActingKey,
}

async fn world(url: &str, name: &str) -> World {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
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
    let (_, key) = db
        .authenticate(&registration.api_key.secret)
        .await
        .unwrap()
        .unwrap();
    World {
        db,
        organization_id: registration.organization.id,
        owner: registration.user.id,
        wallet,
        key,
    }
}

/// The owner acts on memberships in these tests — the management role.
fn owner(world: &World) -> MembershipActor {
    MembershipActor {
        user_id: world.owner,
        role: Role::Owner,
    }
}

/// A fresh key minted by the same member as `world.key`.
async fn second_key(world: &World) -> oxsum_core::ActingKey {
    let minted = world
        .db
        .create_key(
            world.organization_id,
            Some("second".to_owned()),
            None,
            Some(world.owner),
            KeyConstraints::default(),
        )
        .await
        .unwrap();
    let (_, key) = world
        .db
        .authenticate(&minted.secret)
        .await
        .unwrap()
        .unwrap();
    key
}

/// Suspending stamps `suspended_at`, refuses new holds — a key's and a session's
/// alike — and reinstating restores admission.
#[tokio::test]
async fn suspension_gates_new_holds() {
    let url = db_or_skip!();
    let world = world(&url, "susp").await;
    let org = world.organization_id;

    assert!(world.db.set_suspended(org, true).await.unwrap());
    let admin = world.db.organization_by_id(org).await.unwrap();
    assert!(admin.suspended_at.is_some());

    let err = world
        .wallet
        .hold_for_key(&world.key, None, "h-1", "", ONE, D)
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::Forbidden(_)), "{err:?}");
    let err = world.wallet.hold("h-2", "", ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::Forbidden(_)), "{err:?}");

    // Reinstating clears the stamp and restores admission, key and session alike.
    assert!(world.db.set_suspended(org, false).await.unwrap());
    assert!(
        world
            .db
            .organization_by_id(org)
            .await
            .unwrap()
            .suspended_at
            .is_none()
    );
    world
        .wallet
        .hold_for_key(&world.key, None, "h-3", "", ONE, D)
        .await
        .unwrap();
    world.wallet.hold("h-4", "", ONE, D).await.unwrap();
}

/// A hold already open when the suspension lands still settles — suspension is an
/// admission gate, never a rewrite of what is reserved.
#[tokio::test]
async fn an_in_flight_hold_still_settles() {
    let url = db_or_skip!();
    let world = world(&url, "inflight").await;

    world
        .wallet
        .hold_for_key(&world.key, None, "open-hold", "", 10 * ONE, D)
        .await
        .unwrap();
    world
        .db
        .set_suspended(world.organization_id, true)
        .await
        .unwrap();

    world
        .wallet
        .settle("open-hold", "", 4 * ONE, D)
        .await
        .unwrap();
    assert_eq!(world.wallet.reserved().await.unwrap(), 0);
}

/// Replaying the same state writes nothing twice: `set_suspended` answers whether
/// the flag moved, so the webhook fans out on the transition, not on the call.
#[tokio::test]
async fn a_replayed_suspend_changes_nothing() {
    let url = db_or_skip!();
    let world = world(&url, "replay").await;
    let org = world.organization_id;

    assert!(world.db.set_suspended(org, true).await.unwrap());
    assert!(!world.db.set_suspended(org, true).await.unwrap());
    assert!(world.db.set_suspended(org, false).await.unwrap());
    assert!(!world.db.set_suspended(org, false).await.unwrap());
}

/// Suspending enqueues one `org.suspended` delivery per subscribed endpoint, inside
/// the flag's own transaction; a replayed suspend enqueues none.
#[tokio::test]
async fn suspension_webhooks_the_transition() {
    let url = db_or_skip!();
    let world = world(&url, "hook").await;
    let org = world.organization_id;

    let endpoint = world
        .db
        .create_webhook(
            org,
            "https://receiver.example/suspended",
            &[ORG_SUSPENDED.to_owned()],
            &oxsum_core::SecretKey::from_bytes([7; 32]),
        )
        .await
        .unwrap();

    world.db.set_suspended(org, true).await.unwrap();
    world.db.set_suspended(org, true).await.unwrap();
    // Reinstating announces nothing — this build subscribes to suspensions.
    world.db.set_suspended(org, false).await.unwrap();
    world.db.set_suspended(org, true).await.unwrap();

    let deliveries = world
        .db
        .webhook_deliveries(org, endpoint.endpoint.id)
        .await
        .unwrap()
        .expect("the endpoint is the organization's");
    assert_eq!(deliveries.len(), 2);
    assert!(deliveries.iter().all(|d| d.event_type == ORG_SUSPENDED));
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM oxsum.webhook_deliveries WHERE delivery_id = $1")
            .bind(deliveries[0].id)
            .fetch_one(world.db.pool())
            .await
            .unwrap();
    assert_eq!(payload["type"], "org.suspended");
    assert_eq!(
        payload["data"]["organizationId"].as_str().unwrap(),
        org.to_string()
    );
}

/// A member's budget counts the committed spend of every key they minted: one key's
/// holds reserve against the same cap the other's do.
#[tokio::test]
async fn a_member_budget_spans_the_member_keys() {
    let url = db_or_skip!();
    let world = world(&url, "budget").await;
    let second = second_key(&world).await;

    world
        .db
        .update_member(
            world.organization_id,
            owner(&world),
            world.owner,
            None,
            Some(Some(10 * ONE)),
        )
        .await
        .unwrap();

    world
        .wallet
        .hold_for_key(&world.key, None, "mb-1", "", 6 * ONE, D)
        .await
        .unwrap();
    // 6 held by the first key + 6 asked by the second is past the member's 10,
    // however each key alone would have fit.
    let err = world
        .wallet
        .hold_for_key(&second, None, "mb-2", "", 6 * ONE, D)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "{err:?}"
    );

    // Another member's key is its own budget: a key minted by no member —
    // `created_by` null — carries no member cap either.
    let unattributed = world
        .db
        .create_key(
            world.organization_id,
            None,
            None,
            None,
            KeyConstraints::default(),
        )
        .await
        .unwrap();
    let (_, key) = world
        .db
        .authenticate(&unattributed.secret)
        .await
        .unwrap()
        .unwrap();
    world
        .wallet
        .hold_for_key(&key, None, "mb-3", "", 6 * ONE, D)
        .await
        .unwrap();
}

/// Clearing the cap restores the member's spend; a negative cap and an empty PATCH
/// are validation errors.
#[tokio::test]
async fn member_budget_validates_and_clears() {
    let url = db_or_skip!();
    let world = world(&url, "bval").await;
    let org = world.organization_id;

    let err = world
        .db
        .update_member(org, owner(&world), world.owner, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "{err:?}");
    let err = world
        .db
        .update_member(org, owner(&world), world.owner, None, Some(Some(-5)))
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "{err:?}");

    let member = world
        .db
        .update_member(org, owner(&world), world.owner, None, Some(Some(ONE)))
        .await
        .unwrap();
    assert_eq!(member.budget_limit_minor, Some(ONE));
    let member = world
        .db
        .update_member(org, owner(&world), world.owner, None, Some(None))
        .await
        .unwrap();
    assert_eq!(member.budget_limit_minor, None);

    // A budget write alongside a role change applies both — on a second member,
    // since an owner's seat cannot demote while it is the only one.
    let other = world
        .db
        .register(NewUser {
            email: format!(
                "bval2_{}@example.com",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .unwrap();
    let other = world
        .db
        .add_member(org, owner(&world), &other.user.email)
        .await
        .unwrap();
    let member = world
        .db
        .update_member(
            org,
            owner(&world),
            other.user_id,
            Some(Role::Admin),
            Some(Some(2 * ONE)),
        )
        .await
        .unwrap();
    assert_eq!(member.role, Role::Admin);
    assert_eq!(member.budget_limit_minor, Some(2 * ONE));
}

/// The budget reads the ledger, so settled spend counts too — not just open holds.
#[tokio::test]
async fn a_member_budget_counts_settled_spend() {
    let url = db_or_skip!();
    let world = world(&url, "bset").await;

    world
        .wallet
        .hold_for_key(&world.key, None, "s-1", "", 8 * ONE, D)
        .await
        .unwrap();
    world.wallet.settle("s-1", "", 8 * ONE, D).await.unwrap();

    world
        .db
        .update_member(
            world.organization_id,
            owner(&world),
            world.owner,
            None,
            Some(Some(10 * ONE)),
        )
        .await
        .unwrap();
    // 8 settled + 3 asked is past 10 — the cap does not forget what already charged.
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "s-2", "", 3 * ONE, D)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "{err:?}"
    );
}

//! Signed spend mandates (issue #262), against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of
//! failing, so a bare `cargo test` still passes.
//!
//! A mandate is an Ed25519-signed delegation of a strictly narrower slice of a
//! key's authority: `POST /api/v1/org/keys/{keyId}/mandates` mints it, the
//! delegate presents it in `X-Oxsum-Mandate`, and `hold_for_key` enforces its
//! cap, window and model allowlist under the key's per-key advisory lock. The
//! signature checks against the key's public half — the seed is sealed under
//! `OXSUM_SECRET_KEY` and never leaves the database.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    Db, KeyConstraints, KeyScope, NewUser, SecretKey, Tenants, Wallet, WalletError, parse_presented,
};
use sqlx::PgPool;
use time::macros::date;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

const D: time::Date = date!(2026 - 10 - 01);
const ONE: i64 = 1_000_000;

/// The sealing key the suite seals mandate seeds under — the same fixed test
/// key the channel suite uses (`check_sealed_keys` scans the shared database).
fn secret() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("mandates")
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
async fn db(url: &str) -> Db {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
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
    key: oxsum_core::ActingKey,
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
        organization_id: organization.id,
    }
}

/// Mints a mandate on `world`'s key under the suite's sealing key.
async fn mint(
    db: &Db,
    world: &World,
    cap_minor: i64,
    models: Option<Vec<String>>,
    not_after: OffsetDateTime,
) -> oxsum_core::CreatedMandate {
    db.create_mandate(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        cap_minor,
        models,
        None,
        not_after,
        &secret(),
    )
    .await
    .unwrap()
    .expect("the key is found")
}

/// A mandate mints, lists and verifies: the blob `create_mandate` answers
/// parses back to a `PresentedMandate` whose signature checks against the key's
/// public half, and whose verified fields are what was minted.
#[tokio::test]
async fn a_mandate_mints_and_verifies() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let world = world(&db, "mint").await;

    let not_after = OffsetDateTime::now_utc() + Duration::hours(1);
    let created = mint(
        &db,
        &world,
        5 * ONE,
        Some(vec!["mock-a".to_owned()]),
        not_after,
    )
    .await;
    assert_eq!(created.mandate.key_id, world.key.key_id);
    assert_eq!(created.mandate.organization_id, world.organization_id);
    assert_eq!(created.mandate.cap_minor, 5 * ONE);
    assert_eq!(
        created.mandate.models.as_deref(),
        Some(["mock-a".to_owned()].as_slice())
    );

    // The mandate lists under the key's collection — metadata, no blob.
    let list = db
        .list_mandates(
            world.organization_id,
            world.key.key_id,
            KeyScope::Organization,
        )
        .await
        .unwrap();
    let listed = list
        .iter()
        .find(|m| m.mandate_id == created.mandate.mandate_id)
        .expect("the minted mandate lists");
    assert_eq!(listed.nonce, created.mandate.nonce);

    // The blob verifies against the public half the key's seed derives.
    let verifying = db
        .mandate_verifying_key(&world.key.key_id, &secret())
        .await
        .unwrap()
        .expect("a minted key carries a seed");
    let presented = parse_presented(&created.mandate_blob, &verifying).expect("the blob verifies");
    assert_eq!(presented.verified.mandate_id, created.mandate.mandate_id);
    assert_eq!(presented.verified.cap_minor, 5 * ONE);
    assert_eq!(
        presented.verified.not_after,
        created.mandate.not_after.unix_timestamp()
    );
}

/// Two racing mints cannot produce mandates signed under different seeds: the
/// second waits on the first's row lock and reads its seed — one public half
/// verifies both.
#[tokio::test]
async fn racing_mints_share_one_seed() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let world = world(&db, "race").await;

    let not_after = OffsetDateTime::now_utc() + Duration::hours(1);
    let (a, b) = tokio::join!(
        mint(&db, &world, 5 * ONE, None, not_after),
        mint(&db, &world, 7 * ONE, None, not_after),
    );
    let verifying = db
        .mandate_verifying_key(&world.key.key_id, &secret())
        .await
        .unwrap()
        .expect("a minted key carries a seed");
    for created in [&a, &b] {
        parse_presented(&created.mandate_blob, &verifying)
            .expect("both mandates verify under one seed");
    }
    assert_ne!(a.mandate.mandate_id, b.mandate.mandate_id);
}

/// A mandate may only narrow the key: a cap above `spend_limit_minor`, a model
/// outside `model_allowlist` or an inverted window is refused, not minted.
#[tokio::test]
async fn a_mint_may_only_narrow_the_key() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let world = world(&db, "narrow").await;
    db.update_key_constraints(
        world.organization_id,
        world.key.key_id,
        KeyScope::Organization,
        KeyConstraints {
            spend_limit_minor: Some(10 * ONE),
            model_allowlist: Some(vec!["mock-a".to_owned()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // A cap over the key's limit is refused.
    let err = db
        .create_mandate(
            world.organization_id,
            world.key.key_id,
            KeyScope::Organization,
            20 * ONE,
            None,
            None,
            OffsetDateTime::now_utc() + Duration::hours(1),
            &secret(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "{err:?}");

    // A model outside the key's allowlist is refused.
    let err = db
        .create_mandate(
            world.organization_id,
            world.key.key_id,
            KeyScope::Organization,
            5 * ONE,
            Some(vec!["mock-b".to_owned()]),
            None,
            OffsetDateTime::now_utc() + Duration::hours(1),
            &secret(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "{err:?}");

    // An inverted window is refused.
    let now = OffsetDateTime::now_utc();
    let err = db
        .create_mandate(
            world.organization_id,
            world.key.key_id,
            KeyScope::Organization,
            5 * ONE,
            None,
            Some(now),
            now - Duration::hours(1),
            &secret(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "{err:?}");

    // Nothing was minted: the list is empty.
    assert!(
        db.list_mandates(
            world.organization_id,
            world.key.key_id,
            KeyScope::Organization
        )
        .await
        .unwrap()
        .is_empty()
    );
}

/// A member's `Own` scope mints only on the keys they created — another
/// member's key id answers `None`, as if it were not there.
#[tokio::test]
async fn a_members_scope_limits_which_keys_mint() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let world = world(&db, "scope").await;

    // A key minted through the API records no creator — `created_by` is NULL,
    // so a member's `Own(user)` scope does not reach it.
    let other = db
        .create_key(
            world.organization_id,
            None,
            None,
            None,
            KeyConstraints::default(),
        )
        .await
        .unwrap();
    let minted = db
        .create_mandate(
            world.organization_id,
            other.key.id,
            // Any member id: the key's `created_by` is NULL, so `Own` never reaches it.
            KeyScope::Own(Uuid::new_v4()),
            5 * ONE,
            None,
            None,
            OffsetDateTime::now_utc() + Duration::hours(1),
            &secret(),
        )
        .await
        .unwrap();
    assert!(minted.is_none(), "a key outside the scope is not found");
}

/// Inside `hold_for_key`'s lock the mandate's bounds apply on top of the key's:
/// a hold over the cap is `MandateExceeded`, outside the window is
/// `MandateOutOfWindow`, off the allowlist is `Forbidden` — and a hold the
/// mandate admits is taken and settles against it.
#[tokio::test]
async fn the_mandate_bounds_a_hold() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let world = world(&db, "bound").await;

    let created = mint(
        &db,
        &world,
        3 * ONE,
        Some(vec!["mock-a".to_owned()]),
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await;
    let verifying = db
        .mandate_verifying_key(&world.key.key_id, &secret())
        .await
        .unwrap()
        .expect("a minted key carries a seed");
    let presented = parse_presented(&created.mandate_blob, &verifying).expect("the blob verifies");

    // A hold under the cap on the allowed model is admitted — and the open
    // watch row records the mandate the committed spend will key on.
    world
        .wallet
        .hold_for_key(
            &world.key,
            Some("mock-a"),
            "m-h1",
            "",
            2 * ONE,
            D,
            Some(&presented),
        )
        .await
        .expect("a hold under the cap is admitted");

    // One more credit than the cap has left is refused `MandateExceeded`.
    let err = world
        .wallet
        .hold_for_key(
            &world.key,
            Some("mock-a"),
            "m-h2",
            "",
            2 * ONE,
            D,
            Some(&presented),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::MandateExceeded { .. }),
        "{err:?}"
    );

    // A model the mandate does not delegate is refused, even one the key serves.
    let err = world
        .wallet
        .hold_for_key(
            &world.key,
            Some("mock-b"),
            "m-h3",
            "",
            ONE,
            D,
            Some(&presented),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::Forbidden(_)), "{err:?}");

    // Settle the first hold so the committed sum is the settled charge plus
    // the still-open reservation.
    world
        .wallet
        .settle("m-h1", "", 2 * ONE, D)
        .await
        .expect("the hold settles");
    let err = world
        .wallet
        .hold_for_key(
            &world.key,
            Some("mock-a"),
            "m-h4",
            "",
            2 * ONE,
            D,
            Some(&presented),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::MandateExceeded { .. }),
        "{err:?}"
    );
}

/// A mandate presented for a different key, or outside its window, is refused
/// before any money moves.
#[tokio::test]
async fn a_mandate_for_another_key_or_out_of_window_is_refused() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let world = world(&db, "other").await;
    let second = self::world(&db, "other2").await;

    // Minted on `world`'s key; presented under `second`'s — the signature
    // verifies under `world`'s public half, but the mandate's `keyId` is not
    // the acting key's, so `hold_for_key` refuses `InvalidMandate`.
    let created = mint(
        &db,
        &world,
        5 * ONE,
        None,
        OffsetDateTime::now_utc() + Duration::hours(1),
    )
    .await;
    let verifying = db
        .mandate_verifying_key(&world.key.key_id, &secret())
        .await
        .unwrap()
        .unwrap();
    let presented = parse_presented(&created.mandate_blob, &verifying).expect("the blob verifies");
    let err = second
        .wallet
        .hold_for_key(&second.key, None, "x-h1", "", ONE, D, Some(&presented))
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InvalidMandate), "{err:?}");

    // A mandate not yet valid refuses `MandateOutOfWindow { not_yet: true }`.
    let future = db
        .create_mandate(
            world.organization_id,
            world.key.key_id,
            KeyScope::Organization,
            5 * ONE,
            None,
            Some(OffsetDateTime::now_utc() + Duration::hours(1)),
            OffsetDateTime::now_utc() + Duration::hours(3),
            &secret(),
        )
        .await
        .unwrap()
        .expect("the key is found");
    let presented = parse_presented(&future.mandate_blob, &verifying).unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "x-h2", "", ONE, D, Some(&presented))
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::MandateOutOfWindow { not_yet: true }),
        "{err:?}"
    );

    // An expired one refuses with `not_yet: false`.
    let expired = db
        .create_mandate(
            world.organization_id,
            world.key.key_id,
            KeyScope::Organization,
            5 * ONE,
            None,
            Some(OffsetDateTime::now_utc() - Duration::hours(2)),
            OffsetDateTime::now_utc() - Duration::hours(1),
            &secret(),
        )
        .await
        .unwrap()
        .expect("the key is found");
    let presented = parse_presented(&expired.mandate_blob, &verifying).unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "x-h3", "", ONE, D, Some(&presented))
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::MandateOutOfWindow { not_yet: false }),
        "{err:?}"
    );
}

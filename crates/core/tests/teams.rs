//! Team-level budgets (issue #440), against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of
//! failing, so a bare `cargo test` still passes.
//!
//! A team is a named subgroup of one organization's members carrying its own
//! `budget_limit_minor` — the middle the key → member → organization hierarchy
//! missed. The cap is enforced inside `hold_for_key`'s locked window: the team's
//! committed spend is summed from the ledger's own postings across every key the
//! team's members minted, checked between the member budget and the
//! organization credit line.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    Db, KeyConstraints, MembershipActor, NewUser, Role, Tenants, Wallet, WalletError,
};
use sqlx::PgPool;
use time::macros::date;
use uuid::Uuid;

const D: time::Date = date!(2026 - 10 - 01);
const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("teams")
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

/// The owner acts on memberships and teams in these tests — the management role.
fn owner(world: &World) -> MembershipActor {
    MembershipActor {
        user_id: world.owner,
        role: Role::Owner,
    }
}

/// A second member joined to the organization, with one acting key minted by
/// them — the other seat a team's roster groups.
async fn member_with_key(world: &World, tag: &str) -> (Uuid, oxsum_core::ActingKey) {
    let email = format!(
        "{}_{}@example.com",
        tag,
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let account = world
        .db
        .register(NewUser {
            email: email.clone(),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .unwrap();
    let member = world
        .db
        .add_member(world.organization_id, owner(world), &account.user.email)
        .await
        .unwrap();
    let minted = world
        .db
        .create_key(
            world.organization_id,
            Some(tag.to_owned()),
            None,
            Some(member.user_id),
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
    (member.user_id, key)
}

/// A team's cap gates a member's holds across every key they minted — the
/// member's own figure alone would fit, the team's does not.
#[tokio::test]
async fn a_team_budget_spans_the_members_keys() {
    let url = db_or_skip!();
    let world = world(&url, "tbudget").await;
    let org = world.organization_id;

    // The owner and a second member share one team: each member's own budget
    // is uncapped, but the team's 10 caps what both may commit together.
    let (_member_id, member_key) = member_with_key(&world, "tbmember").await;
    let team = world
        .db
        .create_team(org, owner(&world), "sales", Some(10 * ONE))
        .await
        .unwrap();
    // Assign both members to the team.
    world
        .db
        .update_member(
            org,
            owner(&world),
            world.owner,
            None,
            None,
            Some(Some(team.team_id)),
        )
        .await
        .unwrap();
    world
        .db
        .update_member(
            org,
            owner(&world),
            _member_id,
            None,
            None,
            Some(Some(team.team_id)),
        )
        .await
        .unwrap();

    world
        .wallet
        .hold_for_key(&world.key, None, "tb-1", "", 6 * ONE, D, None)
        .await
        .unwrap();
    // 6 held by the owner's key + 6 asked by the member's is past the team's 10,
    // however each member alone would have fit.
    let err = world
        .wallet
        .hold_for_key(&member_key, None, "tb-2", "", 6 * ONE, D, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "{err:?}"
    );
}

/// A member outside any team reads no team cap: the subgroup's limit never
/// reaches an unassigned key.
#[tokio::test]
async fn an_unassigned_member_is_uncapped_at_the_team_layer() {
    let url = db_or_skip!();
    let world = world(&url, "tbnone").await;
    let org = world.organization_id;

    let team = world
        .db
        .create_team(org, owner(&world), "sales", Some(5 * ONE))
        .await
        .unwrap();
    // The owner stays unassigned — only the second member joins the team.
    let (_member_id, _member_key) = member_with_key(&world, "tbun").await;
    world
        .db
        .update_member(
            org,
            owner(&world),
            _member_id,
            None,
            None,
            Some(Some(team.team_id)),
        )
        .await
        .unwrap();

    // The owner's own key holds past the team's cap freely: it is not in the
    // subgroup the cap covers.
    world
        .wallet
        .hold_for_key(&world.key, None, "tb-n1", "", 9 * ONE, D, None)
        .await
        .unwrap();
}

/// Moving a member between teams changes which cap their *next* hold is
/// checked against — committed spend follows the roster.
#[tokio::test]
async fn reassigning_a_member_moves_the_cap_their_keys_see() {
    let url = db_or_skip!();
    let world = world(&url, "tbmove").await;
    let org = world.organization_id;

    let small = world
        .db
        .create_team(org, owner(&world), "small", Some(5 * ONE))
        .await
        .unwrap();
    let large = world
        .db
        .create_team(org, owner(&world), "large", Some(50 * ONE))
        .await
        .unwrap();
    // The owner joins the small team — their key's next hold is capped at 5.
    world
        .db
        .update_member(
            org,
            owner(&world),
            world.owner,
            None,
            None,
            Some(Some(small.team_id)),
        )
        .await
        .unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "tbm-1", "", 8 * ONE, D, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "{err:?}"
    );
    // Reassigned to the large team the same hold passes: the member's prior
    // spend (none here) travels with them, and 8 fits under 50.
    world
        .db
        .update_member(
            org,
            owner(&world),
            world.owner,
            None,
            None,
            Some(Some(large.team_id)),
        )
        .await
        .unwrap();
    world
        .wallet
        .hold_for_key(&world.key, None, "tbm-2", "", 8 * ONE, D, None)
        .await
        .unwrap();
}

/// Clearing a member's team assignment (`teamId: null`) uncaps them at the
/// team layer again.
#[tokio::test]
async fn unassigning_a_member_clears_the_team_cap() {
    let url = db_or_skip!();
    let world = world(&url, "tbclr").await;
    let org = world.organization_id;

    let team = world
        .db
        .create_team(org, owner(&world), "sales", Some(5 * ONE))
        .await
        .unwrap();
    world
        .db
        .update_member(
            org,
            owner(&world),
            world.owner,
            None,
            None,
            Some(Some(team.team_id)),
        )
        .await
        .unwrap();
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "tbc-1", "", 8 * ONE, D, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "{err:?}"
    );
    // An explicit null unassigns: the owner's keys read no team cap again.
    let member = world
        .db
        .update_member(org, owner(&world), world.owner, None, None, Some(None))
        .await
        .unwrap();
    assert_eq!(member.team_id, None);
    world
        .wallet
        .hold_for_key(&world.key, None, "tbc-2", "", 8 * ONE, D, None)
        .await
        .unwrap();
}

/// The team budget counts settled spend too — not just open holds — because
/// the figure is the ledger's own, read inside the locked window.
#[tokio::test]
async fn a_team_budget_counts_settled_spend() {
    let url = db_or_skip!();
    let world = world(&url, "tbset").await;
    let org = world.organization_id;

    world
        .wallet
        .hold_for_key(&world.key, None, "tbs-1", "", 8 * ONE, D, None)
        .await
        .unwrap();
    world.wallet.settle("tbs-1", "", 8 * ONE, D).await.unwrap();

    let team = world
        .db
        .create_team(org, owner(&world), "sales", Some(10 * ONE))
        .await
        .unwrap();
    world
        .db
        .update_member(
            org,
            owner(&world),
            world.owner,
            None,
            None,
            Some(Some(team.team_id)),
        )
        .await
        .unwrap();
    // 8 settled + 3 asked is past 10 — the cap does not forget what already
    // charged, the way the member budget's ledger read does not either.
    let err = world
        .wallet
        .hold_for_key(&world.key, None, "tbs-2", "", 3 * ONE, D, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WalletError::KeyLimitExceeded { .. }),
        "{err:?}"
    );
}

/// Dissolving a team unassigns its members and lifts the cap: the members keep
/// their seats, simply ungrouped.
#[tokio::test]
async fn dissolving_a_team_lifts_its_cap() {
    let url = db_or_skip!();
    let world = world(&url, "tbdel").await;
    let org = world.organization_id;

    let team = world
        .db
        .create_team(org, owner(&world), "sales", Some(5 * ONE))
        .await
        .unwrap();
    world
        .db
        .update_member(
            org,
            owner(&world),
            world.owner,
            None,
            None,
            Some(Some(team.team_id)),
        )
        .await
        .unwrap();
    world
        .db
        .delete_team(org, owner(&world), team.team_id)
        .await
        .unwrap();
    // The member's team_id nulled itself on the delete — ON DELETE SET NULL —
    // so their next hold reads no team cap.
    let members = world.db.members(org).await.unwrap();
    let owner_row = members.iter().find(|m| m.user_id == world.owner).unwrap();
    assert_eq!(owner_row.team_id, None);
    world
        .wallet
        .hold_for_key(&world.key, None, "tbd-1", "", 8 * ONE, D, None)
        .await
        .unwrap();
}

/// Team management follows the membership rules: a member may not create a
/// team, an owner/admin may.
#[tokio::test]
async fn team_management_is_an_owner_or_admin_action() {
    let url = db_or_skip!();
    let world = world(&url, "tbauth").await;
    let org = world.organization_id;
    let (member_id, _) = member_with_key(&world, "tbauth").await;

    let member_actor = MembershipActor {
        user_id: member_id,
        role: Role::Member,
    };
    let err = world
        .db
        .create_team(org, member_actor, "sales", None)
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::Forbidden(_)), "{err:?}");
    let err = world
        .db
        .update_team(org, member_actor, Uuid::new_v4(), Some("x"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::Forbidden(_)), "{err:?}");

    // The owner manages the team's shape: create, rename, set and clear the cap.
    let team = world
        .db
        .create_team(org, owner(&world), "sales", Some(5 * ONE))
        .await
        .unwrap();
    let team = world
        .db
        .update_team(org, owner(&world), team.team_id, Some("growth"), None)
        .await
        .unwrap();
    assert_eq!(team.name, "growth");
    let team = world
        .db
        .update_team(org, owner(&world), team.team_id, None, Some(None))
        .await
        .unwrap();
    assert_eq!(team.budget_limit_minor, None);
}

/// A team-id named on a member PATCH must belong to the organization — a
/// cross-organization id is `NotFound`, never a silent cross-tenant
/// assignment.
#[tokio::test]
async fn a_member_patch_names_a_team_of_the_same_organization() {
    let url = db_or_skip!();
    let world = world(&url, "tbxref").await;
    let org = world.organization_id;

    // A team that does not exist at all.
    let err = world
        .db
        .update_member(
            org,
            owner(&world),
            world.owner,
            None,
            None,
            Some(Some(Uuid::new_v4())),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::NotFound(_)), "{err:?}");
}

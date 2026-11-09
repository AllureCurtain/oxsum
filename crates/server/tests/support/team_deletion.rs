//! Controlled roster writers against monetary and quota-funded admission.

use std::time::Duration;

use oxsum_core::{MembershipActor, Role, WalletError};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Reservation, World, url, world};

#[derive(Clone, Copy, Debug)]
enum Funding {
    Legacy,
    Wallet,
    Quota,
}

impl Funding {
    fn wallet_minor(self) -> i64 {
        match self {
            Self::Legacy | Self::Wallet => 40,
            Self::Quota => 0,
        }
    }
}

fn owner(world: &World) -> MembershipActor {
    MembershipActor {
        user_id: world.user_id.parse().unwrap(),
        role: Role::Owner,
    }
}

async fn assigned_team(world: &World, name: &str, cap: i64) -> Uuid {
    let organization = world.organization_id.parse().unwrap();
    let team = world
        .db
        .create_team(organization, owner(world), name, Some(cap))
        .await
        .unwrap();
    world
        .db
        .update_member(
            organization,
            owner(world),
            world.user_id.parse().unwrap(),
            None,
            None,
            Some(Some(team.team_id)),
        )
        .await
        .unwrap();
    team.team_id
}

async fn monetary_history(world: &World) {
    let (_, key) = world.db.authenticate(&world.key).await.unwrap().unwrap();
    let wallet = world.wallet().await;
    let now = OffsetDateTime::now_utc().date();
    wallet
        .hold_for_key(&key, Some(&world.model), "old-team-charge", "", 40, now, None)
        .await
        .unwrap();
    wallet.settle("old-team-charge", "", 40, now).await.unwrap();
}

async fn take(
    db: &oxsum_core::Db,
    wallet: &oxsum_core::Wallet,
    input: &Reservation,
    funding: Funding,
) -> Result<i64, WalletError> {
    if matches!(funding, Funding::Legacy) {
        wallet
            .hold_for_key(
                &input.key,
                Some(&input.watch.model),
                &input.watch.hold_key,
                "",
                40,
                OffsetDateTime::now_utc().date(),
                None,
            )
            .await?;
        Ok(40)
    } else {
        Ok(super::admit(db, wallet, input).await?.wallet_minor)
    }
}

async fn release(world: &World, funding: Funding, holds: &[String]) {
    let wallet = world.wallet().await;
    let now = OffsetDateTime::now_utc();
    if matches!(funding, Funding::Legacy) {
        for hold in holds {
            wallet.settle(hold, "", 0, now.date()).await.unwrap();
        }
    } else {
        let report = oxsum_core::sweep_stale_holds(
            &world.db,
            &world.tenants,
            now + time::Duration::minutes(1),
            now.date(),
        )
        .await
        .unwrap();
        assert_eq!(report.resolved, holds.len());
    }
    super::no_reservations(world).await;
    if matches!(funding, Funding::Quota) {
        assert_eq!(super::remaining_draw(world).await, 0);
    }
}

async fn blocked_pid(world: &World, blocker: i32) -> i32 {
    super::wait_for_blocked(world, blocker, 1).await;
    sqlx::query_scalar(
        "SELECT pid FROM pg_stat_activity WHERE datname=current_database() \
         AND $1=ANY(pg_blocking_pids(pid))",
    )
    .bind(blocker)
    .fetch_one(&world.pool)
    .await
    .unwrap()
}

async fn locked_member(world: &World) -> (Transaction<'static, Postgres>, i32) {
    let mut tx = world.pool.begin().await.unwrap();
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "SELECT user_id FROM oxsum.memberships \
         WHERE organization_id=$1 AND user_id=$2 FOR UPDATE",
    )
    .bind(world.organization_id.parse::<Uuid>().unwrap())
    .bind(world.user_id.parse::<Uuid>().unwrap())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    (tx, pid)
}

/// Stop a roster writer after its parent/target locks are owned.
struct WriteGate {
    blocker: Transaction<'static, Postgres>,
    pid: i32,
    function: String,
    table: &'static str,
}

impl WriteGate {
    async fn create(world: &World, table: &'static str, event: &str, condition: &str) -> Self {
        let lock = Uuid::new_v4().as_u128() as i64;
        let function = format!("fixture_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(
            "CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN IF {condition} THEN PERFORM pg_advisory_xact_lock({lock}); END IF; \
             IF TG_OP='DELETE' THEN RETURN OLD; END IF; RETURN NEW; END $$"
        ))
        .execute(&world.pool)
        .await
        .unwrap();
        sqlx::query(&format!(
            "CREATE TRIGGER {function} BEFORE {event} ON oxsum.{table} \
             FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()"
        ))
        .execute(&world.pool)
        .await
        .unwrap();
        let mut blocker = world.pool.begin().await.unwrap();
        let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock)
            .execute(&mut *blocker)
            .await
            .unwrap();
        Self {
            blocker,
            pid,
            function,
            table,
        }
    }

    async fn release(self, world: &World) {
        self.blocker.commit().await.unwrap();
        sqlx::query(&format!("DROP TRIGGER {} ON oxsum.{}", self.function, self.table))
            .execute(&world.pool)
            .await
            .unwrap();
        sqlx::query(&format!("DROP FUNCTION oxsum.{}()", self.function))
            .execute(&world.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn deletion_first_detaches_before_any_admission_reads_the_roster() {
    for funding in [Funding::Legacy, Funding::Wallet, Funding::Quota] {
        let world = world_or_skip!(200);
        monetary_history(&world).await;
        if matches!(funding, Funding::Quota) {
            super::cover(&world).await;
        }
        let team = assigned_team(&world, "old team", 39).await;
        let input = super::reservation(&world, 40).await;
        let hold = input.watch.hold_key.clone();
        let wallet = world.wallet().await;
        assert!(matches!(
            take(&world.db, &wallet, &input, funding).await,
            Err(WalletError::KeyLimitExceeded { .. })
        ));
        let gate = WriteGate::create(
            &world,
            "teams",
            "DELETE",
            &format!("OLD.team_id='{team}'"),
        )
        .await;
        let db = world.db.clone();
        let organization = world.organization_id.parse().unwrap();
        let actor = owner(&world);
        let deletion = tokio::spawn(async move { db.delete_team(organization, actor, team).await });
        let writer_pid = blocked_pid(&world, gate.pid).await;
        let db = world.db.clone();
        let admission = tokio::spawn(async move { take(&db, &wallet, &input, funding).await });
        super::wait_for_blocked(&world, writer_pid, 1).await;
        gate.release(&world).await;
        let (deleted, admitted) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(deletion, admission)
        })
        .await
        .unwrap();
        assert_eq!(deleted.unwrap().unwrap().team_id, team);
        assert_eq!(admitted.unwrap().unwrap(), funding.wallet_minor());
        let members = world.db.members(organization).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].team_id, None);
        assert_eq!(world.wallet().await.reserved().await.unwrap(), funding.wallet_minor());
        release(&world, funding, &[hold]).await;
        assert_eq!(world.wallet().await.available().await.unwrap(), 160);
    }
}

#[tokio::test]
async fn admission_first_keeps_its_reservation_through_team_deletion() {
    for funding in [Funding::Legacy, Funding::Wallet, Funding::Quota] {
        let world = world_or_skip!(200);
        if matches!(funding, Funding::Quota) {
            super::cover(&world).await;
        }
        let team = assigned_team(&world, "old team", 40).await;
        let input = super::reservation(&world, 40).await;
        let first_hold = input.watch.hold_key.clone();
        let (blocker, pid) = locked_member(&world).await;
        let db = world.db.clone();
        let wallet = world.wallet().await;
        let admission = tokio::spawn(async move { take(&db, &wallet, &input, funding).await });
        let admission_pid = blocked_pid(&world, pid).await;
        let db = world.db.clone();
        let organization = world.organization_id.parse().unwrap();
        let actor = owner(&world);
        let deletion = tokio::spawn(async move { db.delete_team(organization, actor, team).await });
        // The writer waits for admission's parent share, before owning a team
        // row that the member-owning admission will need next.
        super::wait_for_blocked(&world, admission_pid, 1).await;
        blocker.commit().await.unwrap();
        let (admitted, deleted) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(admission, deletion)
        })
        .await
        .unwrap();
        assert_eq!(admitted.unwrap().unwrap(), funding.wallet_minor());
        assert_eq!(deleted.unwrap().unwrap().team_id, team);
        let next = super::reservation(&world, 40).await;
        let wallet = world.wallet().await;
        assert_eq!(take(&world.db, &wallet, &next, funding).await.unwrap(), funding.wallet_minor());
        assert_eq!(wallet.reserved().await.unwrap(), 2 * funding.wallet_minor());
        let members = world.db.members(organization).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].team_id, None);
        release(&world, funding, &[first_hold, next.watch.hold_key]).await;
        assert_eq!(world.wallet().await.available().await.unwrap(), 200);
    }
}

#[tokio::test]
async fn reassignment_uses_the_committed_roster_in_both_admission_orders() {
    for funding in [Funding::Legacy, Funding::Wallet, Funding::Quota] {
        for writer_first in [false, true] {
            let world = world_or_skip!(200);
            monetary_history(&world).await;
            if matches!(funding, Funding::Quota) {
                super::cover(&world).await;
            }
            assigned_team(&world, "original", 200).await;
            let organization = world.organization_id.parse().unwrap();
            let target = world
                .db
                .create_team(organization, owner(&world), "restricted", Some(39))
                .await
                .unwrap();
            let input = super::reservation(&world, 40).await;
            let hold = input.watch.hold_key.clone();
            let wallet = world.wallet().await;
            let db = world.db.clone();
            let admission = async move { take(&db, &wallet, &input, funding).await };
            let db = world.db.clone();
            let actor = owner(&world);
            let member = world.user_id.parse().unwrap();
            let team = target.team_id;
            let write = async move {
                db.update_member(organization, actor, member, None, None, Some(Some(team))).await
            };
            let (writer, admission) = if writer_first {
                let gate = WriteGate::create(
                    &world,
                    "memberships",
                    "UPDATE",
                    &format!("NEW.organization_id='{organization}' AND NEW.user_id='{member}' AND NEW.team_id='{team}'"),
                )
                .await;
                let writer = tokio::spawn(write);
                let writer_pid = blocked_pid(&world, gate.pid).await;
                let admission = tokio::spawn(admission);
                super::wait_for_blocked(&world, writer_pid, 1).await;
                gate.release(&world).await;
                (writer, admission)
            } else {
                let (blocker, pid) = locked_member(&world).await;
                let admission = tokio::spawn(admission);
                let admission_pid = blocked_pid(&world, pid).await;
                let writer = tokio::spawn(write);
                super::wait_for_blocked(&world, admission_pid, 1).await;
                blocker.commit().await.unwrap();
                (writer, admission)
            };
            let (written, admitted) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(writer, admission)
            })
            .await
            .unwrap();
            assert_eq!(written.unwrap().unwrap().team_id, Some(team));
            let admitted = admitted.unwrap();
            if writer_first {
                assert!(matches!(admitted, Err(WalletError::KeyLimitExceeded { .. })));
                super::no_reservations(&world).await;
            } else {
                assert_eq!(admitted.unwrap(), funding.wallet_minor());
                let next = super::reservation(&world, 40).await;
                let wallet = world.wallet().await;
                assert!(matches!(take(&world.db, &wallet, &next, funding).await,
                    Err(WalletError::KeyLimitExceeded { .. })));
                assert_eq!(wallet.reserved().await.unwrap(), funding.wallet_minor());
                release(&world, funding, &[hold]).await;
            }
            assert_eq!(world.wallet().await.available().await.unwrap(), 160);
            if matches!(funding, Funding::Quota) {
                assert_eq!(super::remaining_draw(&world).await, 0);
            }
        }
    }
}

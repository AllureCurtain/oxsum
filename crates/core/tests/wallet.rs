//! Wallet integration tests, running against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! Every tenant shares one connection pool, as it does in the server: tenants are told
//! apart by the schema each transaction resolves, not by owning connections.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use oxsum_core::{Tenants, Wallet, WalletError, verify_bundle};
use sqlx::PgPool;
use time::macros::date;

const D: time::Date = date!(2026 - 10 - 01);
const ONE: i64 = 1_000_000;

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

/// Each test uses a tenant name with a random suffix, so tests never interfere and reruns never collide.
fn fresh(name: &str) -> String {
    format!("{name}_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// The one pool a test works with, as the server holds one pool for the whole process.
async fn pool(url: &str) -> PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL")
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

#[tokio::test]
async fn tenants_are_isolated() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let a = Wallet::open(pool.clone(), &fresh("a")).await.unwrap();
    let b = Wallet::open(pool.clone(), &fresh("b")).await.unwrap();

    a.top_up("a-1", 10 * ONE, D).await.unwrap();
    a.top_up("a-2", 5 * ONE, D).await.unwrap();
    b.top_up("b-1", 3 * ONE, D).await.unwrap();

    assert_eq!(a.available().await.unwrap(), 15 * ONE);
    assert_eq!(b.available().await.unwrap(), 3 * ONE);
    // Each tenant's log counts from zero, so B's tree head reveals nothing about how many entries A has.
    assert_eq!(a.log_size().await.unwrap(), 2);
    assert_eq!(b.log_size().await.unwrap(), 1);
    // The same idempotency key in different tenants never collides.
    assert!(a.top_up("same-key", ONE, D).await.unwrap().is_new);
    assert!(b.top_up("same-key", ONE, D).await.unwrap().is_new);
}

/// Two tenants behind one pool, interleaved: neither the writes nor the reads of one are
/// visible to the other, and a proof still verifies from the shared pool.
#[tokio::test]
async fn tenants_on_one_pool_cannot_see_each_other() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let a = Wallet::open(pool.clone(), &fresh("shared_a"))
        .await
        .unwrap();
    let b = Wallet::open(pool.clone(), &fresh("shared_b"))
        .await
        .unwrap();

    let a_bill = a.top_up("a-1", 10 * ONE, D).await.unwrap();
    b.top_up("b-1", 3 * ONE, D).await.unwrap();
    a.hold("a-2:hold", "", 4 * ONE, D).await.unwrap();

    assert_eq!(a.available().await.unwrap(), 6 * ONE);
    assert_eq!(b.available().await.unwrap(), 3 * ONE);
    assert_eq!(a.log_size().await.unwrap(), 2);
    assert_eq!(b.log_size().await.unwrap(), 1);

    // A's entry does not exist in B's ledger — and B does not get to prove it either.
    assert!(b.receipt_proof(a_bill.entry_id).await.unwrap().is_none());

    // A's own proof still verifies, on the same shared pool.
    let bundle = a.receipt_proof(a_bill.entry_id).await.unwrap().unwrap();
    let json = serde_json::to_string(&bundle).unwrap();
    assert!(verify_bundle(&json, &a_bill.content_hash).unwrap());
}

/// A pool of exactly one connection, borrowed by tenant A, then B, then A again: the
/// schema must follow the tenant, not the connection.
#[tokio::test]
async fn one_connection_reused_across_tenants_does_not_leak() {
    let url = db_or_skip!();
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let tenants = Tenants::new(single);

    // A, then B, then A again — every statement after the first reuses one backend.
    let a = tenants.get(&fresh("reuse_a")).await.unwrap();
    a.top_up("a-1", 10 * ONE, D).await.unwrap();
    let b = tenants.get(&fresh("reuse_b")).await.unwrap();
    b.top_up("b-1", 3 * ONE, D).await.unwrap();
    a.top_up("a-2", ONE, D).await.unwrap();

    assert_eq!(a.available().await.unwrap(), 11 * ONE);
    assert_eq!(b.available().await.unwrap(), 3 * ONE);
    assert_eq!(a.log_size().await.unwrap(), 2);
    assert_eq!(b.log_size().await.unwrap(), 1);
}

/// A tenant costs a cached facade, not a pool. Five tenants are served by a pool of one
/// connection, and leave one backend behind; a pool per tenant would leave five.
#[tokio::test]
async fn tenants_do_not_multiply_connections() {
    const PROBE: &str = "oxsum-shared-pool-probe";

    let url = db_or_skip!();
    let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
    // Named, so the count below sees only this pool's backends: the other tests in this
    // binary are running against the same database.
    let shared = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.application_name(PROBE))
        .await
        .unwrap();
    let tenants = Tenants::new(shared);

    for i in 0..5 {
        let w = tenants.get(&fresh(&format!("conn{i}"))).await.unwrap();
        w.top_up(&format!("c{i}"), ONE, D).await.unwrap();
    }

    // Counted from a connection of its own, so the shared pool's backend is visible.
    let observer = pool(&url).await;
    let backends: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity \
         WHERE datname = current_database() AND application_name = $1 \
           AND pid <> pg_backend_pid()",
    )
    .bind(PROBE)
    .fetch_one(&observer)
    .await
    .unwrap();
    assert!(
        backends <= 1,
        "five tenants left {backends} backends behind; one pool per tenant would leave one each"
    );
}

#[tokio::test]
async fn concurrent_holds_cannot_overdraw() {
    let url = db_or_skip!();
    let w = Arc::new(
        Wallet::open(pool(&url).await, &fresh("race"))
            .await
            .unwrap(),
    );
    w.top_up("fund", 10 * ONE, D).await.unwrap();

    let tasks: Vec<_> = (0..20)
        .map(|i| {
            let w = w.clone();
            tokio::spawn(async move { w.hold(&format!("hold-{i}"), "", 3 * ONE, D).await })
        })
        .collect();
    let mut accepted = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => accepted += 1,
            Err(WalletError::InsufficientFunds) => {}
            Err(e) => panic!("unexpected: {e:?}"),
        }
    }
    // Balance 10, each hold 3: at most 3 can succeed.
    assert_eq!(accepted, 3);
    assert_eq!(w.available().await.unwrap(), ONE);
}

#[tokio::test]
async fn hold_then_partial_settle_refunds_the_rest() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("stream"))
        .await
        .unwrap();
    w.top_up("fund", 10 * ONE, D).await.unwrap();

    w.hold("req-1:hold", "", 4 * ONE, D).await.unwrap();
    assert_eq!(w.available().await.unwrap(), 6 * ONE);

    // The settlement names the hold it releases; its amount comes from the hold entry.
    w.settle("req-1:hold", "", 1_234_567, D).await.unwrap();
    assert_eq!(w.available().await.unwrap(), 10 * ONE - 1_234_567);

    // Retrying the same settlement is idempotent; nothing is charged twice.
    let again = w.settle("req-1:hold", "", 1_234_567, D).await.unwrap();
    assert!(!again.is_new);
    assert_eq!(w.available().await.unwrap(), 10 * ONE - 1_234_567);
}

#[tokio::test]
async fn settle_rejects_actual_above_hold() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("bounds"))
        .await
        .unwrap();
    w.top_up("fund", 10 * ONE, D).await.unwrap();
    w.hold("h", "", ONE, D).await.unwrap();

    // The actual may not exceed what the hold reserved: the bound comes from the ledger.
    let err = w.settle("h", "", 2 * ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)));
}

/// A settlement naming a hold that was never taken is refused, and invents nothing.
///
/// The settlement names the hold it releases, and the server reads the hold from the
/// ledger. No entry under the key means no hold, so there is nothing to release — refused
/// even though the wallet holds enough to cover it.
#[tokio::test]
async fn a_settlement_that_was_never_held_is_refused() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("unheld"))
        .await
        .unwrap();
    w.top_up("fund", 10 * ONE, D).await.unwrap();

    let err = w.settle("ghost", "", 0, D).await.unwrap_err();
    assert!(matches!(err, WalletError::HoldNotFound(_)), "{err:?}");

    // Not one minor unit moved, and nothing was written.
    assert_eq!(w.available().await.unwrap(), 10 * ONE);
    assert_eq!(w.log_size().await.unwrap(), 1);

    // A settlement that charges something names the same missing hold.
    let err = w.settle("ghost-charges", "", ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::HoldNotFound(_)), "{err:?}");
    assert_eq!(w.available().await.unwrap(), 10 * ONE);
}

/// Settling a hold that is already discharged is refused, even when other holds would
/// cover the release.
///
/// This is the pairing issue #10 adds over the aggregate rule: hold A is settled while
/// hold B still reserves 4, and settling A again must not spend B's reservation. The
/// refusal is the ledger's idempotency gate — the settlement entry's key is derived from
/// the hold's key — so the second, different settlement of A is a conflict.
#[tokio::test]
async fn settling_a_discharged_hold_is_refused() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("discharged"))
        .await
        .unwrap();
    w.top_up("fund", 10 * ONE, D).await.unwrap();
    w.hold("a", "", 4 * ONE, D).await.unwrap();
    w.hold("b", "", 4 * ONE, D).await.unwrap();
    assert_eq!(w.available().await.unwrap(), 2 * ONE);

    w.settle("a", "", 0, D).await.unwrap();
    assert_eq!(w.available().await.unwrap(), 6 * ONE);

    // A again, for a different charge: the hold is discharged, and B's reservation is
    // not A's to spend.
    let err = w.settle("a", "", ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::Conflict(_)), "{err:?}");
    assert_eq!(w.available().await.unwrap(), 6 * ONE);

    // B still settles exactly as before.
    w.settle("b", "", ONE, D).await.unwrap();
    assert_eq!(w.available().await.unwrap(), 9 * ONE);
}

/// Two settlements racing to release the same hold cannot both succeed.
///
/// The settlement entry's idempotency key is derived from the hold's key, so the two
/// appends collide on the ledger's idempotency gate: one wins, the other is refused as a
/// conflict. A check the caller did beforehand would let both through.
#[tokio::test]
async fn concurrent_settlements_of_one_hold_cannot_both_release_it() {
    let url = db_or_skip!();
    let w = Arc::new(
        Wallet::open(pool(&url).await, &fresh("settle_race"))
            .await
            .unwrap(),
    );
    w.top_up("fund", 10 * ONE, D).await.unwrap();
    w.hold("h", "", 4 * ONE, D).await.unwrap();

    // Different charges, so the loser is a conflict rather than an idempotent replay.
    let tasks: Vec<_> = (0..2)
        .map(|i| {
            let w = w.clone();
            tokio::spawn(async move { w.settle("h", "", i * ONE, D).await })
        })
        .collect();
    let mut released = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => released += 1,
            Err(WalletError::Conflict(_)) => {}
            Err(e) => panic!("unexpected: {e:?}"),
        }
    }
    assert_eq!(released, 1);
    // The winner released the whole hold; which of the two tasks won the race decides the
    // charge (0 or ONE), so the balance is 10*ONE or 9*ONE. What the test pins down is that
    // exactly one of them took effect — assuming the winner is always the first task spawned
    // flakes under load.
    let available = w.available().await.unwrap();
    assert!(
        available == 10 * ONE || available == 9 * ONE,
        "one settlement took effect, so the balance is 10 or 9: {available}"
    );
    assert_eq!(w.log_size().await.unwrap(), 3);
}

/// Opening a ledger written under the older rule tightens it.
///
/// The limit is master data the wallet owns, and `register_account` upserts it, so an
/// existing ledger is fixed on the next open rather than keeping the weaker rule — and the
/// hole with it — for the rest of its life.
#[tokio::test]
async fn opening_a_wallet_tightens_a_ledger_written_under_the_old_rule() {
    let url = db_or_skip!();
    let tenant = fresh("upgrade");
    let pool = pool(&url).await;
    let w = Wallet::open(pool.clone(), &tenant).await.unwrap();
    w.top_up("fund", 10 * ONE, D).await.unwrap();
    drop(w);

    // A ledger created before the reservation rule existed stored the weaker limit. The
    // schema is assembled from this test's own generated tenant id, never from input.
    let weakened = sqlx::query(&format!(
        "UPDATE ledger_{tenant}.accounts SET balance_limit = 'no_debit' \
         WHERE path = 'Liabilities:Wallet'"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(weakened.rows_affected(), 1);

    // Including the constraint that stored code was allowed by. `CREATE TABLE IF NOT EXISTS`
    // would leave it exactly as it is, so the migration has to widen it — otherwise the limit
    // below cannot be written at all.
    sqlx::query(&format!(
        "ALTER TABLE ledger_{tenant}.accounts DROP CONSTRAINT accounts_balance_limit"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(&format!(
        "ALTER TABLE ledger_{tenant}.accounts ADD CONSTRAINT accounts_balance_limit \
         CHECK (balance_limit IN ('unlimited', 'no_credit', 'no_debit'))"
    ))
    .execute(&pool)
    .await
    .unwrap();

    let w = Wallet::open(pool.clone(), &tenant).await.unwrap();
    let stored: String = sqlx::query_scalar(&format!(
        "SELECT balance_limit FROM ledger_{tenant}.accounts WHERE path = 'Liabilities:Wallet'"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, "funded_reservations");
    assert_eq!(w.available().await.unwrap(), 10 * ONE);

    // Which is the point: the rule is enforced again — a hold the balance cannot cover
    // is refused by the re-applied limit.
    let err = w.hold("over", "", 11 * ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::InsufficientFunds), "{err:?}");
    assert_eq!(w.available().await.unwrap(), 10 * ONE);
}

/// Reusing a key for a different request is the caller's mistake, and it says so.
///
/// The engine refuses it; what matters here is that the refusal reaches the domain layer as a
/// conflict rather than as a storage failure. `Storage` means the ledger is broken and answers
/// 500 with the details in the logs; this is a request the caller can fix by choosing another
/// key, and the API answers it 409 CONFLICT.
#[tokio::test]
async fn reusing_a_key_for_a_different_request_is_a_conflict() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("reuse"))
        .await
        .unwrap();
    let first = w.top_up("same-key", 5 * ONE, D).await.unwrap();
    assert!(first.is_new);

    let err = w.top_up("same-key", 6 * ONE, D).await.unwrap_err();
    match err {
        WalletError::Conflict(message) => assert!(message.contains("idempotency key"), "{message}"),
        other => panic!("expected a conflict, got {other:?}"),
    }

    // The refused attempt changed nothing: same balance, same log, and the key still belongs
    // to the entry that took it.
    assert_eq!(w.available().await.unwrap(), 5 * ONE);
    assert_eq!(w.log_size().await.unwrap(), 1);
    let replay = w.top_up("same-key", 5 * ONE, D).await.unwrap();
    assert!(!replay.is_new);
    assert_eq!(replay.entry_id, first.entry_id);

    // A different kind under a key another kind already holds is the same conflict.
    let err = w.hold("same-key", "", ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::Conflict(_)), "{err:?}");
    assert_eq!(w.available().await.unwrap(), 5 * ONE);
}

#[tokio::test]
async fn bill_proof_verifies_and_catches_tampering() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("proof"))
        .await
        .unwrap();
    w.top_up("fund", 10 * ONE, D).await.unwrap();
    let bill = w.hold("req-9:hold", "", 2 * ONE, D).await.unwrap();
    for i in 0..5 {
        w.top_up(&format!("noise-{i}"), ONE, D).await.unwrap();
    }

    let bundle = w.receipt_proof(bill.entry_id).await.unwrap().unwrap();
    let json = serde_json::to_string(&bundle).unwrap();
    assert!(verify_bundle(&json, &bill.content_hash).unwrap());

    // Simulate the server later rewriting this bill from 2.000000 to 0.500000.
    let tampered = json.replace("\"2.000000\"", "\"0.500000\"");
    assert_ne!(tampered, json, "fixture must contain the amount");
    assert!(!verify_bundle(&tampered, &bill.content_hash).unwrap());
}

#[tokio::test]
async fn reopening_a_tenant_keeps_its_books() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let name = fresh("restart");
    Wallet::open(pool.clone(), &name)
        .await
        .unwrap()
        .top_up("fund", 7 * ONE, D)
        .await
        .unwrap();
    let reopened = Wallet::open(pool.clone(), &name).await.unwrap();
    assert_eq!(reopened.available().await.unwrap(), 7 * ONE);
}

/// Several new tenants migrating an empty database for the first time must not hit
/// btree_gist's unique constraint. Already fixed in crates/doubleentry; see docs/decisions.md.
///
/// They migrate through one shared pool here, which is where the migrate lock now matters:
/// the pool is free to serve every one of them from the same connection.
#[tokio::test]
async fn concurrent_first_migrations_succeed() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let pool = pool.clone();
            tokio::spawn(async move { Wallet::open(pool, &fresh(&format!("mig{i}"))).await })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
}

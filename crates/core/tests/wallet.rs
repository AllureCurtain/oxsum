//! Wallet integration tests, running against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! Every tenant shares one connection pool, as it does in the server: tenants are told
//! apart by the schema each transaction resolves, not by owning connections.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use oxsum_core::{
    SCALE, Settlement, SettlementKind, Tenants, TransactionKind, UsageRecord, Wallet, WalletError,
    hold_description, verify_bundle,
};
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

    // A ledger created before the reservation rule existed stored the weaker limit
    // on every balance account — and for the credit pool and the facility, codes
    // the old constraint's domain knows too, so the narrowed check still fits.
    // The schema is assembled from this test's own generated tenant id, never
    // from input.
    let weakened = sqlx::query(&format!(
        "UPDATE ledger_{tenant}.accounts SET balance_limit = CASE \
             WHEN path = 'Assets:CreditFacility' THEN 'unlimited' ELSE 'no_debit' END \
         WHERE path IN ('Liabilities:Wallet', 'Equity:Bonus', 'Liabilities:CreditLine', \
                        'Assets:CreditFacility')"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(weakened.rows_affected(), 4);

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
    let stored: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT balance_limit FROM ledger_{tenant}.accounts \
         WHERE path IN ('Liabilities:Wallet', 'Equity:Bonus', 'Liabilities:CreditLine', \
                        'Assets:CreditFacility') ORDER BY path"
    ))
    .fetch_all(&pool)
    .await
    .unwrap();
    // Ordered by path: Assets:CreditFacility first, then the three pools —
    // Equity:Bonus, Liabilities:CreditLine, Liabilities:Wallet.
    assert_eq!(
        stored,
        [
            "no_credit",
            "funded_reservations",
            "funded_reservations",
            "funded_reservations"
        ]
    );
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

/// Two processes can open the same brand-new tenant at once — any second `AppState`
/// has its own `Tenants`, and admin endpoints open every organization's wallet. The
/// migrate's `CREATE SCHEMA IF NOT EXISTS` is not atomic in Postgres, so `open`
/// pre-creates the schema and treats a duplicate as the other opener winning; this
/// is that race, opened through separate pools so `Tenants` cannot serialize it.
#[tokio::test]
async fn one_tenants_first_open_converges_under_concurrency() {
    let url = db_or_skip!();
    for round in 0..12 {
        let tenant = fresh("race");
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let url = url.clone();
            let tenant = tenant.clone();
            tasks.push(tokio::spawn(async move {
                let pool = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(2)
                    .connect(&url)
                    .await
                    .expect("connects");
                Wallet::open(pool, &tenant).await
            }));
        }
        for (i, task) in tasks.into_iter().enumerate() {
            let r = task.await.unwrap();
            if let Err(e) = r {
                panic!("round {round} opener {i}: {e:?}");
            }
        }
    }
}

/// The bills page's transaction view lists top-ups, adjustments and settled requests
/// — each signed by what it moved on the wallet's settled balance, newest first — and
/// skips the holds that bill nothing yet (issue #90).
#[tokio::test]
async fn recent_transactions_lists_every_movement_but_holds() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("txview"))
        .await
        .unwrap();
    w.top_up("t1", 10 * ONE, D).await.unwrap();
    w.adjust("g1", "welcome", 2 * ONE, D).await.unwrap();
    w.hold("h1", "", 3 * ONE, D).await.unwrap();
    w.settle("h1", "", 1_234, D).await.unwrap();
    w.adjust("d1", "clawback", -ONE, D).await.unwrap();

    let txs = w.recent_transactions(100).await.unwrap();
    let kinds: Vec<TransactionKind> = txs.iter().map(|tx| tx.kind).collect();
    assert_eq!(
        kinds,
        [
            TransactionKind::Adjustment,
            TransactionKind::Settlement,
            TransactionKind::Adjustment,
            TransactionKind::TopUp,
        ],
        "newest first, the hold skipped: {txs:?}"
    );
    let amounts: Vec<i64> = txs.iter().map(|tx| tx.amount_minor).collect();
    assert_eq!(
        amounts,
        [-ONE, -1_234, 2 * ONE, 10 * ONE],
        "the signed effect on the settled balance"
    );
    assert_eq!(txs[1].description, "", "a hand-written settlement's words");
    assert!(
        txs.iter()
            .all(|tx| tx.record.is_none() && tx.key_id.is_none()),
        "nothing here is a gateway turn or key-attributed"
    );
    for tx in &txs {
        assert_eq!(tx.booked_on, D);
        assert!(!tx.content_hash.is_empty() && !tx.id.is_empty());
    }
}

/// A settled gateway turn attributes to the key that paid and carries the settlement
/// record its description holds, so the row shows exactly what the bill proves.
#[tokio::test]
async fn a_gateway_settlement_names_its_key_and_its_record() {
    let url = db_or_skip!();
    let pool = pool(&url).await;
    // `hold_for_key` re-reads the key row inside its limit check, so the key is a real
    // one: a registered organization's first key, authenticated the way the server does.
    let db = oxsum_core::Db::from_pool(pool.clone());
    db.migrate().await.unwrap();
    let registration = db
        .register(oxsum_core::NewUser {
            email: format!(
                "txgw_{}@example.com",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "correct horse battery staple".to_owned(),
            organization_name: None,
        })
        .await
        .unwrap();
    let (_organization, key) = db
        .authenticate(&registration.api_key.secret)
        .await
        .unwrap()
        .unwrap();
    let w = Wallet::open(pool, &registration.organization.tenant_id)
        .await
        .unwrap();
    w.top_up("t1", 10 * ONE, D).await.unwrap();
    w.hold_for_key(
        &key,
        None,
        "req-9:hold",
        &hold_description("req-9", "model-x", 5 * ONE).unwrap(),
        5 * ONE,
        D,
    )
    .await
    .unwrap();
    let usage = UsageRecord::tokens(11, 22).unwrap();
    let lines = [
        oxsum_core::BillLine {
            item: "input".into(),
            units: 11,
            price_per_m: 1_000,
        },
        oxsum_core::BillLine {
            item: "output".into(),
            units: 22,
            price_per_m: 2_000,
        },
    ];
    let record = Settlement {
        request: "req-9",
        channel: "chan",
        model: "model-x",
        price_version: 1,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        discount_percent: None,
        charged: 77,
        freeze: 5 * ONE,
    };
    w.settle("req-9:hold", &record.description().unwrap(), 77, D)
        .await
        .unwrap();

    let txs = w.recent_transactions(10).await.unwrap();
    assert_eq!(txs.len(), 2, "the settlement and the top-up: {txs:?}");
    let settlement = &txs[0];
    assert_eq!(settlement.kind, TransactionKind::Settlement);
    assert_eq!(settlement.amount_minor, -77);
    assert_eq!(
        settlement.key_id.as_deref(),
        Some(key.key_id.as_simple().to_string().as_str()),
        "the paying key, as the requests page's filter matches it"
    );
    let parsed = settlement.record.as_ref().expect("the record parses back");
    assert_eq!(parsed.request, "req-9");
    assert_eq!(parsed.charged, 77);
    assert_eq!(parsed.freeze, 5 * ONE);
}

/// A walk over the transaction pages returns the same rows as the first-page read
/// covers and then keeps going — newest first, every row exactly once, and the
/// cursor naming where each next page resumes until the log's start ends it
/// (issue #93).
#[tokio::test]
async fn transaction_pages_walk_the_log_without_gaps_or_repeats() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("txpage"))
        .await
        .unwrap();
    w.top_up("t1", 10 * ONE, D).await.unwrap();
    w.adjust("g1", "welcome", 2 * ONE, D).await.unwrap();
    w.hold("h1", "", 3 * ONE, D).await.unwrap();
    w.settle("h1", "", 1_234, D).await.unwrap();
    w.adjust("d1", "clawback", -ONE, D).await.unwrap();
    w.hold("h2", "", ONE, D).await.unwrap();
    w.settle("h2", "", ONE, D).await.unwrap();

    let all = w.recent_transactions(100).await.unwrap();
    assert_eq!(all.len(), 5, "the top-up, two adjustments, two settlements");

    let mut walked = Vec::new();
    let mut cursors = Vec::new();
    let mut before = None;
    loop {
        let page = w.transactions_page(before, 2).await.unwrap();
        assert!(page.rows.len() <= 2, "the page honors its limit");
        for tx in &page.rows {
            assert!(
                !walked.iter().any(|seen: &String| seen == &tx.id),
                "no row twice"
            );
            walked.push(tx.id.clone());
        }
        match page.next_cursor {
            Some(cursor) => {
                assert!(cursors.last().is_none_or(|&c| cursor < c), "strictly older");
                cursors.push(cursor);
            }
            None => break,
        }
        before = page.next_cursor;
    }
    let expect: Vec<String> = all.iter().map(|tx| tx.id.clone()).collect();
    assert_eq!(walked, expect, "the walk covers the whole list, in order");

    // A page resumed mid-walk starts strictly below its cursor.
    let tail = w
        .transactions_page(cursors.first().copied(), 100)
        .await
        .unwrap();
    assert_eq!(tail.rows.len(), expect.len() - 2);
    assert_eq!(
        tail.rows.first().map(|tx| tx.id.as_str()),
        expect.get(2).map(String::as_str)
    );
}

/// Entries page the same way: `before` bounds the page below it, and a walk with a
/// one-row page sees every index exactly once.
#[tokio::test]
async fn entry_pages_walk_the_log_without_gaps_or_repeats() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("logpage"))
        .await
        .unwrap();
    w.top_up("t1", ONE, D).await.unwrap();
    w.top_up("t2", ONE, D).await.unwrap();
    w.top_up("t3", ONE, D).await.unwrap();

    let mut indices = Vec::new();
    let mut before = None;
    loop {
        let page = w.entries_page(before, 1).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        indices.extend(page.rows.iter().map(|entry| entry.index));
        match page.next_cursor {
            Some(cursor) => before = Some(cursor),
            None => break,
        }
    }
    // Newest first, one index each, contiguous: [2, 1, 0].
    assert_eq!(
        indices,
        [2, 1, 0],
        "every index, once, newest first: {indices:?}"
    );

    // Appending while a cursor is held does not disturb the walk below it.
    let page = w.entries_page(None, 1).await.unwrap();
    let cursor = page.next_cursor.unwrap();
    w.top_up("t4", ONE, D).await.unwrap();
    let resumed = w.entries_page(Some(cursor), 100).await.unwrap();
    assert_eq!(
        resumed.rows.iter().map(|e| e.index).collect::<Vec<_>>(),
        [1, 0],
        "the held cursor still names the same boundary"
    );
}

/// The requests pages walk settled gateway turns the same way, and a hold that is
/// not a settlement record does not count against the page.
#[tokio::test]
async fn request_pages_walk_only_settled_turns() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("reqpage"))
        .await
        .unwrap();
    w.top_up("t1", 100 * ONE, D).await.unwrap();
    for i in 0..3 {
        let request = format!("req-{i}");
        w.hold(&format!("{request}:h"), &format!("{request}:hold"), ONE, D)
            .await
            .unwrap();
        let usage = UsageRecord::tokens(1, 2).unwrap();
        let lines = [
            oxsum_core::BillLine {
                item: "input".into(),
                units: 1,
                price_per_m: 1_000,
            },
            oxsum_core::BillLine {
                item: "output".into(),
                units: 2,
                price_per_m: 2_000,
            },
        ];
        let record = Settlement {
            request: &request,
            channel: "chan",
            model: "m",
            price_version: 1,
            kind: SettlementKind::Usage,
            usage: &usage,
            lines: &lines,
            matched_rule: None,
            discount_percent: None,
            charged: ONE,
            freeze: ONE,
        };
        w.settle(
            &format!("{request}:h"),
            &record.description().unwrap(),
            ONE,
            D,
        )
        .await
        .unwrap();
    }

    let mut requests = Vec::new();
    let mut before = None;
    loop {
        let page = w.requests_page(before, 2).await.unwrap();
        requests.extend(page.rows.iter().map(|r| r.request_id.clone()));
        match page.next_cursor {
            Some(cursor) => before = Some(cursor),
            None => break,
        }
    }
    assert_eq!(requests, ["req-2", "req-1", "req-0"]);
}

/// The settled-layer postings of one entry, as `(account index, direction, minor)`.
/// The account indexes are learned per tenant from entries whose shape is known —
/// a top-up credits the wallet, a grant credits the bonus pool — so the assertions
/// never name a handle the ledger assigned.
async fn settled_postings(w: &Wallet, entry: oxsum_core::EntryId) -> Vec<(u32, char, i64)> {
    use doubleentry::Direction;
    let bundle = w.receipt_proof(entry).await.unwrap().unwrap();
    bundle
        .entry
        .postings()
        .iter()
        .filter(|p| p.layer == doubleentry::Layer::Settled)
        .map(|p| {
            (
                p.account.index(),
                match p.direction {
                    Direction::Debit => 'D',
                    Direction::Credit => 'C',
                },
                p.amount.to_minor(),
            )
        })
        .collect()
}

async fn pending_debits(w: &Wallet, entry: oxsum_core::EntryId) -> Vec<(u32, i64)> {
    use doubleentry::Direction;
    let bundle = w.receipt_proof(entry).await.unwrap().unwrap();
    bundle
        .entry
        .postings()
        .iter()
        .filter(|p| p.layer == doubleentry::Layer::Pending && p.direction == Direction::Debit)
        .map(|p| (p.account.index(), p.amount.to_minor()))
        .collect()
}

/// The credit account of a settled entry: the wallet on a top-up, the bonus pool on
/// a grant, `Equity:Adjustments` never.
async fn credited_account(w: &Wallet, entry: oxsum_core::EntryId) -> u32 {
    settled_postings(w, entry)
        .await
        .into_iter()
        .find(|(_, d, _)| *d == 'C')
        .map(|(a, _, _)| a)
        .expect("the entry credits a pool account")
}

#[tokio::test]
async fn grants_and_charges_draw_the_bonus_pool_first() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("pools"))
        .await
        .unwrap();
    let fund = w.top_up("fund", 8 * ONE, D).await.unwrap();
    let grant = w.adjust("grant", "welcome", 2 * ONE, D).await.unwrap();
    let wallet = credited_account(&w, fund.entry_id).await;
    let bonus = credited_account(&w, grant.entry_id).await;
    assert_ne!(wallet, bonus, "top-ups and grants credit different pools");

    // A hold that spans both pools reserves each side separately: the bonus pool
    // funds what it can, the wallet carries the rest.
    let hold = w.hold("h", "", 6 * ONE, D).await.unwrap();
    let reserved = pending_debits(&w, hold.entry_id).await;
    let from = |account: u32| {
        reserved
            .iter()
            .filter(|(a, _)| *a == account)
            .map(|(_, m)| *m)
            .sum::<i64>()
    };
    assert_eq!(from(bonus), 2 * ONE, "the bonus pool funds what it holds");
    assert_eq!(from(wallet), 4 * ONE, "the wallet carries the rest");
    assert_eq!(w.reserved().await.unwrap(), 6 * ONE);
    assert_eq!(w.available().await.unwrap(), 4 * ONE);

    // The charge draws the bonus pool first, capped by what this hold reserved
    // there — actual 5 takes the 2 it reserved from bonus and 3 from the wallet.
    let settled = w.settle("h", "", 5 * ONE, D).await.unwrap();
    let postings = settled_postings(&w, settled.entry_id).await;
    let debit = |account: u32| {
        postings
            .iter()
            .filter(|(a, d, _)| *a == account && *d == 'D')
            .map(|(_, _, m)| *m)
            .sum::<i64>()
    };
    assert_eq!(debit(bonus), 2 * ONE);
    assert_eq!(debit(wallet), 3 * ONE);
    assert_eq!(w.available().await.unwrap(), 5 * ONE);
}

#[tokio::test]
async fn a_deduction_draws_bonus_first() {
    let url = db_or_skip!();
    let w = Wallet::open(pool(&url).await, &fresh("deduct"))
        .await
        .unwrap();
    let fund = w.top_up("fund", 6 * ONE, D).await.unwrap();
    let grant = w.adjust("grant", "welcome", 4 * ONE, D).await.unwrap();
    let wallet = credited_account(&w, fund.entry_id).await;
    let bonus = credited_account(&w, grant.entry_id).await;

    // The first deduction fits inside the bonus pool; the second spills over into
    // purchased credit only after the bonus side is empty.
    let first = w.adjust("d1", "clawback", -3 * ONE, D).await.unwrap();
    let postings = settled_postings(&w, first.entry_id).await;
    assert_eq!(postings.len(), 2, "one pool debit, one adjustments credit");
    assert_eq!(postings[0], (bonus, 'D', 3 * ONE));
    assert_eq!(w.available().await.unwrap(), 7 * ONE);

    let second = w.adjust("d2", "clawback", -3 * ONE, D).await.unwrap();
    let postings = settled_postings(&w, second.entry_id).await;
    let debit = |account: u32| {
        postings
            .iter()
            .filter(|(a, d, _)| *a == account && *d == 'D')
            .map(|(_, _, m)| *m)
            .sum::<i64>()
    };
    assert_eq!(debit(bonus), ONE, "the remainder of the bonus pool");
    assert_eq!(debit(wallet), 2 * ONE, "only what bonus could not cover");
    assert_eq!(w.available().await.unwrap(), 4 * ONE);
}

/// Appends a pre-pools entry — a grant (`Adjustments` → `Wallet`) or a charge
/// (`Wallet` → `Income:Usage`) in the single-pool shape — through the engine
/// directly, so the reclassification sees exactly the data it was written for.
async fn legacy_entry(
    pool: &PgPool,
    tenant: &str,
    key: &str,
    debit: &str,
    credit: &str,
    minor: i64,
) {
    use doubleentry::storage::postgres::PostgresStore;
    use doubleentry::{
        AccountRegistry, Amount, Currency, Draft, Entry, EntryBatch, IdempotencyKey, LedgerId,
        LedgerPolicy, LedgerStore, SealContext,
    };
    let store = PostgresStore::<SCALE>::new(
        pool.clone(),
        LedgerId::new(format!("tenant-{tenant}")).unwrap(),
    )
    .in_schema(&format!("ledger_{tenant}"));
    let registry = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
    let find = |path: &str| {
        registry
            .records()
            .iter()
            .find(|r| r.account.path.to_string() == path)
            .unwrap_or_else(|| panic!("account {path} exists"))
            .id
    };
    let amt = Amount::<SCALE>::from_minor(minor);
    let draft = Entry::<Draft, SCALE>::new(
        oxsum_core::entry_id_for(key),
        IdempotencyKey::new(key.as_bytes().to_vec()).unwrap(),
        D,
    )
    .debit(find(debit), amt, Currency::USD)
    .credit(find(credit), amt, Currency::USD);
    let sealed = draft
        .seal(&SealContext {
            accounts: &registry,
            calendar: &store.calendar().await.unwrap(),
            policy: &LedgerPolicy::default(),
        })
        .unwrap();
    store.append(&EntryBatch::single(sealed)).await.unwrap();
}

#[tokio::test]
async fn reclassification_moves_unspent_grants_into_bonus_once() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let tenant = fresh("reclass");
    let w = Wallet::open(p.clone(), &tenant).await.unwrap();

    // What a pre-pools ledger looks like: purchased and granted credit mixed in
    // the wallet account, with spend already charged against it.
    w.top_up("fund", 5 * ONE, D).await.unwrap();
    legacy_entry(
        &p,
        &tenant,
        "g1",
        "Equity:Adjustments",
        "Liabilities:Wallet",
        4 * ONE,
    )
    .await;
    legacy_entry(&p, &tenant, "c1", "Liabilities:Wallet", "Income:Usage", ONE).await;

    // Granted 4, spent 1: the draw-bonus-first convention leaves 3 of the grant
    // unspent, inside a wallet balance of 8.
    assert!(w.reclassify_grants().await.unwrap());
    assert!(
        !w.reclassify_grants().await.unwrap(),
        "self-marking: a rerun writes nothing"
    );
    assert_eq!(w.settled().await.unwrap(), 8 * ONE, "the move nets to zero");

    // The reclass debit is not spend.
    assert_eq!(w.settled_spend_since(D).await.unwrap(), ONE);

    // The bills page never lists it — it classifies as no transaction kind.
    let listed: Vec<String> = w
        .recent_transactions(20)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.description)
        .collect();
    assert!(
        !listed.iter().any(|d| d.contains("reclassification")),
        "the reclassification is invisible to bills: {listed:?}"
    );

    // And the migrated wallet charges bonus first like any other.
    let fund = w.top_up("fund2", ONE, D).await.unwrap();
    let grant = w.adjust("grant", "welcome", ONE, D).await.unwrap();
    let wallet = credited_account(&w, fund.entry_id).await;
    let bonus = credited_account(&w, grant.entry_id).await;
    let hold = w.hold("h", "", 2 * ONE, D).await.unwrap();
    let reserved = pending_debits(&w, hold.entry_id).await;
    let from = |account: u32| {
        reserved
            .iter()
            .filter(|(a, _)| *a == account)
            .map(|(_, m)| *m)
            .sum::<i64>()
    };
    // Bonus after migration: 3 reclassified + 1 new grant = 4, so the whole 2 fits.
    assert_eq!(from(bonus), 2 * ONE);
    assert_eq!(from(wallet), 0);
}

#[tokio::test]
async fn reclassification_leaves_a_grant_fully_spent_ledger_alone() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let tenant = fresh("reclass_spent");
    let w = Wallet::open(p.clone(), &tenant).await.unwrap();

    w.top_up("fund", 5 * ONE, D).await.unwrap();
    legacy_entry(
        &p,
        &tenant,
        "g1",
        "Equity:Adjustments",
        "Liabilities:Wallet",
        4 * ONE,
    )
    .await;
    legacy_entry(
        &p,
        &tenant,
        "c1",
        "Liabilities:Wallet",
        "Income:Usage",
        6 * ONE,
    )
    .await;

    // Grants (4) are fully spent (6 ≥ 4): nothing moves and no marker entry lands.
    assert!(!w.reclassify_grants().await.unwrap());
    assert_eq!(w.settled().await.unwrap(), 3 * ONE);
}

// ---- organization credit limit (issue #122) ----

#[tokio::test]
async fn credit_limit_defaults_to_zero_and_counts_in_available() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let w = Wallet::open(p.clone(), &fresh("cl_default")).await.unwrap();

    assert_eq!(w.credit_limit().await.unwrap(), 0);
    assert_eq!(w.credit_used().await.unwrap(), 0);

    w.set_credit_limit("cl-1", 10 * ONE, D).await.unwrap();
    assert_eq!(w.credit_limit().await.unwrap(), 10 * ONE);
    assert_eq!(w.credit_used().await.unwrap(), 0);

    // The undrawn line is spendable: own funds plus the line's headroom.
    w.top_up("t1", 3 * ONE, D).await.unwrap();
    assert_eq!(w.available().await.unwrap(), 13 * ONE);
    assert_eq!(w.credit_used().await.unwrap(), 0);
}

#[tokio::test]
async fn holds_draw_the_credit_line_after_own_funds() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let w = Wallet::open(p.clone(), &fresh("cl_draw")).await.unwrap();

    w.top_up("t1", 5 * ONE, D).await.unwrap();
    w.set_credit_limit("cl-1", 10 * ONE, D).await.unwrap();

    // A hold within own funds + the line passes; beyond it is refused by the
    // engine, not by a read-then-write check.
    w.hold("h1", "", 12 * ONE, D).await.unwrap();
    let err = w.hold("h2", "", 4 * ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::InsufficientFunds));

    // The drawn share is the reserved part: 12 held, 5 of it own funds.
    assert_eq!(w.credit_used().await.unwrap(), 7 * ONE);
    assert_eq!(w.available().await.unwrap(), 3 * ONE);
}

#[tokio::test]
async fn settling_a_credit_hold_charges_the_line_and_releases_the_rest() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let w = Wallet::open(p.clone(), &fresh("cl_settle")).await.unwrap();

    w.set_credit_limit("cl-1", 10 * ONE, D).await.unwrap();
    w.hold("h1", "", 6 * ONE, D).await.unwrap();
    assert_eq!(w.credit_used().await.unwrap(), 6 * ONE);

    // Settle less than held: the charge stays on the line, the rest releases.
    w.settle("h1", "", 4 * ONE, D).await.unwrap();
    assert_eq!(w.credit_used().await.unwrap(), 4 * ONE);
    assert_eq!(w.available().await.unwrap(), 6 * ONE);
    assert_eq!(w.settled_spend_since(D).await.unwrap(), 4 * ONE);

    // A full release gives the whole reservation back to the line.
    w.hold("h2", "", 3 * ONE, D).await.unwrap();
    w.settle("h2", "", 0, D).await.unwrap();
    assert_eq!(w.credit_used().await.unwrap(), 4 * ONE);
    assert_eq!(w.available().await.unwrap(), 6 * ONE);
}

#[tokio::test]
async fn a_topup_repays_the_drawn_line_before_the_wallet() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let w = Wallet::open(p.clone(), &fresh("cl_repay")).await.unwrap();

    w.set_credit_limit("cl-1", 10 * ONE, D).await.unwrap();
    w.hold("h1", "", 6 * ONE, D).await.unwrap();
    w.settle("h1", "", 6 * ONE, D).await.unwrap();
    assert_eq!(w.credit_used().await.unwrap(), 6 * ONE);

    // Repaying deposit: 4 toward the drawn 6, the rest into the wallet.
    w.top_up("pay", 7 * ONE, D).await.unwrap();
    assert_eq!(w.credit_used().await.unwrap(), 0);
    // Spendable = wallet remainder (1) + restored line (10).
    assert_eq!(w.available().await.unwrap(), 11 * ONE);

    // Retrying the same key replays the entry it committed — the repayment
    // shape is not recomputed against the now-restored line.
    let replay = w.top_up("pay", 7 * ONE, D).await.unwrap();
    assert!(!replay.is_new);
    assert_eq!(w.credit_used().await.unwrap(), 0);
    assert_eq!(w.available().await.unwrap(), 11 * ONE);
}

#[tokio::test]
async fn the_limit_cannot_shrink_below_the_outstanding_draw() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let w = Wallet::open(p.clone(), &fresh("cl_shrink")).await.unwrap();

    w.set_credit_limit("cl-1", 10 * ONE, D).await.unwrap();
    w.hold("h1", "", 6 * ONE, D).await.unwrap();
    w.settle("h1", "", 6 * ONE, D).await.unwrap();

    // 6 is still owed: a limit of 5 would leave the debt larger than the line.
    let err = w.set_credit_limit("cl-2", 5 * ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)));

    // A limit equal to the debt stands: the line is fully drawn and blocks
    // every further hold.
    w.set_credit_limit("cl-3", 6 * ONE, D).await.unwrap();
    assert_eq!(w.credit_limit().await.unwrap(), 6 * ONE);
    assert_eq!(w.credit_used().await.unwrap(), 6 * ONE);
    let err = w.hold("h2", "", ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::InsufficientFunds));
}

#[tokio::test]
async fn a_credit_limit_replay_and_a_noop_write_nothing_new() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let w = Wallet::open(p.clone(), &fresh("cl_idem")).await.unwrap();

    let first = w
        .set_credit_limit("cl-1", 10 * ONE, D)
        .await
        .unwrap()
        .expect("a grant writes an entry");
    assert!(first.is_new);
    let size = w.log_size().await.unwrap();

    // The same key answers the same entry; a fresh key at the same limit is a
    // no-op — the delta is zero, so there is nothing to write.
    let again = w
        .set_credit_limit("cl-1", 10 * ONE, D)
        .await
        .unwrap()
        .expect("the replayed entry");
    assert!(!again.is_new);
    assert_eq!(first.entry_id, again.entry_id);
    assert!(
        w.set_credit_limit("cl-2", 10 * ONE, D)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(w.log_size().await.unwrap(), size);

    // A reused key carrying a different limit is a conflict, not a replay.
    let err = w.set_credit_limit("cl-1", 5 * ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::Conflict(_)));

    let err = w.set_credit_limit("cl-3", -ONE, D).await.unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)));
}

#[tokio::test]
async fn concurrent_holds_cannot_overdraw_the_line() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let tenant = fresh("cl_race");
    let w = Wallet::open(p.clone(), &tenant).await.unwrap();
    w.set_credit_limit("cl-1", 10 * ONE, D).await.unwrap();

    // Twelve holds of 1 each against a 10 line: ten win, two are refused, and
    // the committed draw never exceeds the grant.
    let mut joins = Vec::new();
    for i in 0..12 {
        let w = Wallet::open(p.clone(), &tenant).await.unwrap();
        joins.push(tokio::spawn(async move {
            w.hold(&format!("h-{i}"), "", ONE, D).await
        }));
    }
    let mut held = 0;
    for j in joins {
        if j.await.unwrap().is_ok() {
            held += 1;
        }
    }
    assert_eq!(held, 10);
    let w = Wallet::open(p.clone(), &tenant).await.unwrap();
    assert_eq!(w.credit_used().await.unwrap(), 10 * ONE);
}

#[tokio::test]
async fn credit_entries_classify_but_never_count_as_spend() {
    let url = db_or_skip!();
    let p = pool(&url).await;
    let w = Wallet::open(p.clone(), &fresh("cl_spend")).await.unwrap();

    w.top_up("t1", 5 * ONE, D).await.unwrap();
    w.set_credit_limit("cl-1", 10 * ONE, D).await.unwrap();
    w.hold("h1", "", 8 * ONE, D).await.unwrap();
    w.settle("h1", "", 8 * ONE, D).await.unwrap();
    // Shrink the undrawn headroom back: a facility write, not a charge.
    w.set_credit_limit("cl-2", 3 * ONE, D).await.unwrap();

    // Spend counts the settled draw only — the grant and the shrink move the
    // line but bill nothing.
    assert_eq!(w.settled_spend_since(D).await.unwrap(), 8 * ONE);
    assert_eq!(w.credit_used().await.unwrap(), 3 * ONE);

    let kinds: Vec<TransactionKind> = w
        .recent_transactions(20)
        .await
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect();
    // Two operator movements show as adjustments; the grant reads +10, the
    // shrink reads -7 of spendable change.
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == TransactionKind::Adjustment)
            .count(),
        2
    );
    assert!(kinds.contains(&TransactionKind::TopUp));
    assert!(kinds.contains(&TransactionKind::Settlement));
}

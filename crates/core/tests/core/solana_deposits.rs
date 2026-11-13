//! The Solana rail's intent bookkeeping against the scratch database
//! (issue #1117): `open_solana_deposit` inserts `pending` with the declared
//! terms in `meta`, `expire_solana_deposit` walks `pending`/`confirmed` to
//! `expired`, and neither misbehaves on a replayed or mis-priced open.
//! Skips without DATABASE_URL.
use oxsum_core::{Db, NewUser, SolanaTerms, WalletError};
use sqlx::Row;
use uuid::Uuid;

async fn database(slug: &str) -> Option<Db> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url(slug)?;
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}

/// The deployment's terms every test here opens under.
fn terms() -> SolanaTerms {
    SolanaTerms {
        pay_to: "oxsumDepositAddr111111111111111111111111111".to_owned(),
        spl_token_mint: None,
        rate_micro: 200_000_000,
        intent_ttl_secs: 600,
    }
}

/// A fresh organization's id; `deposits.organization_id` references it.
async fn organization(db: &Db, slug: &str) -> Uuid {
    let registration = db
        .register(NewUser {
            email: format!(
                "solana-{slug}-{}@example.test",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "solana-private-password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    registration.organization.id
}

#[tokio::test]
async fn open_solana_deposit_inserts_pending_with_locked_terms() {
    let Some(db) = database("solana_open").await else {
        return;
    };
    let org = organization(&db, "open").await;
    let deposit_id = Uuid::new_v4();
    let (reference, lamports, ttl) = db
        .open_solana_deposit(org, deposit_id, 1000, &terms())
        .await
        .unwrap()
        .expect("a fresh deposit inserts");
    // 1000 minor-credits at 200_000_000 minor-credits/SOL = 0.000005 SOL
    // = 5_000 lamports (lamports = credits_minor * 1e9 / rate_micro).
    assert_eq!(lamports, 5_000);
    assert_eq!(ttl, 600);
    assert!(!reference.is_empty());
    let row = sqlx::query(
        "SELECT rail, status, amount_minor, payment_ref, meta FROM oxsum.deposits \
         WHERE deposit_id = $1",
    )
    .bind(deposit_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("rail"), "solana");
    assert_eq!(row.get::<String, _>("status"), "pending");
    assert_eq!(row.get::<i64, _>("amount_minor"), 1000);
    assert_eq!(row.get::<String, _>("payment_ref"), reference);
    let meta = row.get::<serde_json::Value, _>("meta");
    assert_eq!(meta["solLamports"].as_i64().unwrap(), 5_000);
    assert_eq!(meta["rateMicro"].as_i64().unwrap(), 200_000_000);
    assert_eq!(meta["rateSource"].as_str().unwrap(), "config");
    assert!(meta["expiresAt"].is_string());
}

#[tokio::test]
async fn open_solana_deposit_refuses_a_nonpositive_amount() {
    let Some(db) = database("solana_open_refuse").await else {
        return;
    };
    let org = organization(&db, "open-refuse").await;
    for amount in [0_i64, -5] {
        let result = db
            .open_solana_deposit(org, Uuid::new_v4(), amount, &terms())
            .await;
        assert!(
            matches!(result, Err(WalletError::InvalidInput(_))),
            "{result:?}"
        );
    }
    // Nothing was inserted.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.deposits WHERE organization_id = $1 AND rail = 'solana'",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn open_solana_deposit_replays_the_unique_anchor() {
    let Some(db) = database("solana_open_replay").await else {
        return;
    };
    let org = organization(&db, "open-replay").await;
    let deposit_id = Uuid::new_v4();
    let first = db
        .open_solana_deposit(org, deposit_id, 100, &terms())
        .await
        .unwrap()
        .expect("the first open inserts");
    // A second open mints a different reference — the (rail, org, payment_ref)
    // anchor is the reference, so a replay only collides when the marker is
    // reused, which a fresh mint cannot do. Prove the anchor by forcing a
    // duplicate payment_ref the way a raced retry of the same intent would.
    let inserted = sqlx::query(
        "INSERT INTO oxsum.deposits \
             (deposit_id, organization_id, rail, payment_ref, amount_minor, status, meta) \
         VALUES ($1, $2, 'solana', $3, 100, 'pending', '{}'::jsonb) \
         ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(org)
    .bind(&first.0)
    .execute(db.pool())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(inserted, 0, "the UNIQUE anchor refused the replay");
}

#[tokio::test]
async fn expire_solana_deposit_walks_pending_and_confirmed_only() {
    let Some(db) = database("solana_expire").await else {
        return;
    };
    let org = organization(&db, "expire").await;
    let pending = Uuid::new_v4();
    db.open_solana_deposit(org, pending, 100, &terms())
        .await
        .unwrap()
        .expect("inserts");
    db.expire_solana_deposit(pending).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(pending)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "expired");
    // A second expire is a no-op: the row stays expired, never walks back.
    db.expire_solana_deposit(pending).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(pending)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "expired");
    // A `confirmed` row expires too; a `credited` one is untouched.
    let confirmed = Uuid::new_v4();
    let credited = Uuid::new_v4();
    for (id, st) in [(confirmed, "confirmed"), (credited, "credited")] {
        sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, status, meta) \
             VALUES ($1, $2, 'solana', $3, 100, $4, '{}'::jsonb)",
        )
        .bind(id)
        .bind(org)
        .bind(format!("ref-{id}"))
        .bind(st)
        .execute(db.pool())
        .await
        .unwrap();
        db.expire_solana_deposit(id).await.unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
                .bind(id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        let expected = if st == "confirmed" {
            "expired"
        } else {
            "credited"
        };
        assert_eq!(status, expected, "{st} row after expire");
    }
}

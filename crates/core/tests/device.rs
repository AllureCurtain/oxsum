//! Device authorization (issue #156): mint, lookup, verdict, poll — the storage
//! contract the grant stands on. The tests need DATABASE_URL and skip without
//! it, like the other core suites.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, DevicePoll, KeyScope, NewUser, PollError, WalletError};
use sha2::{Digest, Sha256};
use uuid::Uuid;

fn url() -> Option<String> {
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

/// A migrated database and one registered user, returned as (user, org) ids.
async fn world(url: &str) -> (Db, Uuid, Uuid, sqlx::PgPool) {
    let pool: sqlx::PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let email = format!(
        "device_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let registration = db
        .register(NewUser {
            email,
            password: "a long enough password".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    (db, registration.user.id, registration.organization.id, pool)
}

#[tokio::test]
async fn a_minted_request_reads_back_pending() {
    let (db, _user, _org, _pool) = world(&db_or_skip!()).await;

    let grant = db.mint_device_request().await.unwrap();
    assert!(grant.device_code.starts_with("oxd-"));
    assert_eq!(grant.user_code.len(), 9, "XXXX-XXXX");
    assert_eq!(&grant.user_code[4..5], "-");

    // The lookup takes the code as typed — lowercase, no dashes — and answers
    // the formatted code back.
    let read = db
        .device_request(&grant.user_code.to_lowercase().replace('-', ""))
        .await
        .unwrap();
    assert_eq!(read.user_code, grant.user_code);
}

#[tokio::test]
async fn approve_then_poll_delivers_the_key_once() {
    let (db, user, org, _pool) = world(&db_or_skip!()).await;
    let grant = db.mint_device_request().await.unwrap();

    // A poll while pending waits; the interval applies to the second visit.
    let first = db.poll_device(&grant.device_code).await.unwrap();
    assert!(matches!(first, DevicePoll::Pending));
    let second = db.poll_device(&grant.device_code).await;
    assert!(matches!(second, Err(PollError::TooFast { .. })));

    db.decide_device_request(&grant.user_code, true, user, org)
        .await
        .unwrap();

    // The interval guards every poll — rewind the last visit rather than
    // sleeping five seconds.
    cool_down(&db, &grant.device_code).await;
    let poll = db.poll_device(&grant.device_code).await.unwrap();
    let DevicePoll::Delivered(key) = poll else {
        panic!("expected delivery: {poll:?}");
    };
    assert!(key.secret.starts_with("oxs-"), "a key secret");
    assert_eq!(
        key.key.name.as_deref(),
        Some(format!("device {}", grant.user_code).as_str())
    );

    // The delivered key authenticates for the approver's organization.
    let (key_org, _acting) = db
        .authenticate(&key.secret)
        .await
        .unwrap()
        .expect("the minted key authenticates");
    assert_eq!(key_org.id, org);

    // A second delivery answers consumed — the secret is never re-served.
    cool_down(&db, &grant.device_code).await;
    let again = db.poll_device(&grant.device_code).await.unwrap();
    assert!(matches!(again, DevicePoll::Consumed));
}

/// Rewinds `last_poll_at` so the next poll clears the interval.
async fn cool_down(db: &Db, device_code: &str) {
    sqlx::query(
        "UPDATE oxsum.device_codes SET last_poll_at = now() - interval '1 minute' \
         WHERE device_code_hash = $1",
    )
    .bind(Sha256::digest(device_code.as_bytes()).as_slice())
    .execute(db.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn a_denied_request_polls_denied() {
    let (db, user, org, pool) = world(&db_or_skip!()).await;
    let grant = db.mint_device_request().await.unwrap();

    db.decide_device_request(&grant.user_code, false, user, org)
        .await
        .unwrap();

    let poll = db.poll_device(&grant.device_code).await.unwrap();
    assert!(matches!(poll, DevicePoll::Denied));

    // And no key was ever minted for it.
    let keys = db.list_keys(org, KeyScope::All).await.unwrap();
    assert!(
        keys.iter()
            .all(|k| k.name.as_deref() != Some(format!("device {}", grant.user_code).as_str()))
    );
    drop(pool);
}

#[tokio::test]
async fn a_second_verdict_and_a_guessed_code_are_not_found() {
    let (db, user, org, _pool) = world(&db_or_skip!()).await;
    let grant = db.mint_device_request().await.unwrap();

    db.decide_device_request(&grant.user_code, true, user, org)
        .await
        .unwrap();
    // Decided once, the request is gone to every later verdict.
    let again = db
        .decide_device_request(&grant.user_code, true, user, org)
        .await;
    assert!(matches!(again, Err(WalletError::NotFound(_))));

    // Malformed, unknown and decided codes read identically.
    for code in ["notacode", "XXXX-XXXX", &grant.user_code.clone()] {
        let res = db.device_request(code).await;
        assert!(matches!(res, Err(WalletError::NotFound(_))), "{code}");
    }

    // Unknown and expired device codes poll identically too.
    let res = db.poll_device("oxd-nope").await;
    assert!(matches!(res, Err(PollError::NotFound)));
}

#[tokio::test]
async fn an_expired_request_is_gone_everywhere() {
    let (db, user, org, _pool) = world(&db_or_skip!()).await;
    let grant = db.mint_device_request().await.unwrap();

    // Scope the expiry to this request's row only — the suite shares a database.
    sqlx::query(
        "UPDATE oxsum.device_codes SET expires_at = now() - interval '1 second' \
         WHERE user_code = $1",
    )
    .bind(&grant.user_code)
    .execute(db.pool())
    .await
    .unwrap();

    let res = db.device_request(&grant.user_code).await;
    assert!(matches!(res, Err(WalletError::NotFound(_))));
    let res = db
        .decide_device_request(&grant.user_code, true, user, org)
        .await;
    assert!(matches!(res, Err(WalletError::NotFound(_))));
    let res = db.poll_device(&grant.device_code).await;
    assert!(matches!(res, Err(PollError::NotFound)));
}

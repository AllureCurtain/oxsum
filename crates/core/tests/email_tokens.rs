//! Email tokens (issue #150): mint, consume, cooldown, expiry and purpose
//! isolation — the storage contract both mails stand on. The tests need
//! DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, EmailPurpose, NewUser, WalletError};
use sqlx::PgPool;
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

/// A migrated database and a registered user, returned as the user's id.
async fn world(url: &str) -> (Db, Uuid, PgPool) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    let email = format!(
        "mail_{}@example.com",
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
    (db, registration.user.id, pool)
}

#[tokio::test]
async fn a_minted_token_verifies_its_user() {
    let (db, user_id, _pool) = world(&db_or_skip!()).await;

    let minted = db
        .mint_email_token(user_id, EmailPurpose::Verify)
        .await
        .unwrap()
        .expect("the first mint is not cooling down");
    assert!(minted.token.starts_with("oxt-"));
    assert!(minted.email.contains("@example.com"));

    let consumed = db
        .consume_email_token(&minted.token, EmailPurpose::Verify)
        .await
        .unwrap();
    assert_eq!(consumed, Some(user_id));
}

#[tokio::test]
async fn a_token_redeems_once_ever() {
    let (db, user_id, _pool) = world(&db_or_skip!()).await;
    let minted = db
        .mint_email_token(user_id, EmailPurpose::Verify)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        db.consume_email_token(&minted.token, EmailPurpose::Verify)
            .await
            .unwrap(),
        Some(user_id)
    );
    // A raced double click, a refresh, a replayed link: the second read finds
    // the row spent and answers the same None an unknown token does.
    assert_eq!(
        db.consume_email_token(&minted.token, EmailPurpose::Verify)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn an_expired_token_does_not_consume() {
    let (db, user_id, pool) = world(&db_or_skip!()).await;
    let minted = db
        .mint_email_token(user_id, EmailPurpose::Verify)
        .await
        .unwrap()
        .unwrap();
    // Scoped to this test's user: sibling tests share the database, and an
    // unscoped update would age their tokens out from under them.
    sqlx::query(
        "UPDATE oxsum.email_tokens SET expires_at = now() - interval '1 second' \
         WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(
        db.consume_email_token(&minted.token, EmailPurpose::Verify)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_token_minted_for_one_purpose_serves_no_other() {
    let (db, user_id, _pool) = world(&db_or_skip!()).await;
    let minted = db
        .mint_email_token(user_id, EmailPurpose::Verify)
        .await
        .unwrap()
        .unwrap();

    // A verify link must not reset a password, and vice versa.
    assert_eq!(
        db.consume_email_token(&minted.token, EmailPurpose::Reset)
            .await
            .unwrap(),
        None
    );
    // The failed wrong-purpose attempt did not spend it either.
    assert_eq!(
        db.consume_email_token(&minted.token, EmailPurpose::Verify)
            .await
            .unwrap(),
        Some(user_id)
    );
}

#[tokio::test]
async fn a_resend_inside_the_cooldown_mints_nothing() {
    let (db, user_id, _pool) = world(&db_or_skip!()).await;

    assert!(
        db.mint_email_token(user_id, EmailPurpose::Verify)
            .await
            .unwrap()
            .is_some()
    );
    // Sixty seconds later the next mint works; inside the window it does not —
    // the resend button cannot flood an inbox.
    assert!(
        db.mint_email_token(user_id, EmailPurpose::Verify)
            .await
            .unwrap()
            .is_none()
    );
    // The cooldown is per purpose: asking for a reset right after a verify is
    // a different mail, not a resend.
    assert!(
        db.mint_email_token(user_id, EmailPurpose::Reset)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_spent_token_frees_the_cooldown() {
    let (db, user_id, _pool) = world(&db_or_skip!()).await;
    let minted = db
        .mint_email_token(user_id, EmailPurpose::Verify)
        .await
        .unwrap()
        .unwrap();
    db.consume_email_token(&minted.token, EmailPurpose::Verify)
        .await
        .unwrap();

    // The cooldown counts live tokens only: once the mailed link was clicked,
    // asking again mints a fresh one — otherwise a used link would mute resend.
    assert!(
        db.mint_email_token(user_id, EmailPurpose::Verify)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn minting_for_an_address_finds_the_user_case_insensitively() {
    let (db, user_id, _pool) = world(&db_or_skip!()).await;
    let minted = db
        .mint_email_token(user_id, EmailPurpose::Reset)
        .await
        .unwrap()
        .unwrap();
    let email = minted.email.clone();

    let by_address = db
        .mint_email_token_for_address(&email.to_uppercase(), EmailPurpose::Verify)
        .await
        .unwrap()
        .expect("the address resolves to the user");
    assert_eq!(by_address.email, email);

    // An address with no account is the same None the cooldown returns — the
    // caller cannot tell them apart, which is the point.
    assert!(
        db.mint_email_token_for_address("nobody@example.com", EmailPurpose::Reset)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn minting_for_an_unknown_user_is_not_found() {
    let (db, _user_id, _pool) = world(&db_or_skip!()).await;
    let err = db
        .mint_email_token(Uuid::new_v4(), EmailPurpose::Verify)
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::NotFound(_)));
}

#[tokio::test]
async fn the_verified_flag_marks_and_reads() {
    let (db, user_id, _pool) = world(&db_or_skip!()).await;
    assert!(!db.email_verified(user_id).await.unwrap());
    db.mark_email_verified(user_id).await.unwrap();
    assert!(db.email_verified(user_id).await.unwrap());
    // Marking twice is a rewrite of the same fact, not an error.
    db.mark_email_verified(user_id).await.unwrap();

    let err = db.email_verified(Uuid::new_v4()).await.unwrap_err();
    assert!(matches!(err, WalletError::NotFound(_)));
}

#[tokio::test]
async fn an_unknown_or_malformed_token_consumes_to_none() {
    let (db, _user_id, _pool) = world(&db_or_skip!()).await;
    for token in ["oxt-0000", "", "not a token", "oxi-not-an-email-token"] {
        assert_eq!(
            db.consume_email_token(token, EmailPurpose::Verify)
                .await
                .unwrap(),
            None,
            "{token:?}"
        );
    }
}

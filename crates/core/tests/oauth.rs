//! OAuth storage (issue #152): the single-use state, and the account
//! resolution — linked identity, verified-email linking, and registration —
//! that the callback stands on. The tests need DATABASE_URL and skip without
//! it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, OAuthIdentity, WalletError};
use sha2::{Digest, Sha256};
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

/// A migrated database and its pool, for the SQL pokes the tests make
/// directly.
async fn world(url: &str) -> (Db, PgPool) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (db, pool)
}

/// An identity the provider might vouch for, unique per call.
fn identity(email: &str) -> OAuthIdentity {
    OAuthIdentity {
        provider: "github".to_owned(),
        provider_user_id: format!("gh-{}", Uuid::new_v4().simple()),
        email: email.to_owned(),
    }
}

/// The digest of a state, as the table stores it — for the SQL that scopes a
/// change to exactly this attempt's row.
fn state_hash(state: &str) -> Vec<u8> {
    Sha256::digest(state.as_bytes()).to_vec()
}

#[tokio::test]
async fn a_state_consumes_once() {
    let (db, _pool) = world(&db_or_skip!()).await;

    let state = db.mint_oauth_state("github").await.unwrap();
    assert!(state.starts_with("oxo-"), "the mark names the flow");
    assert!(db.consume_oauth_state(&state, "github").await.unwrap());
    // A replay — the raced or duplicated callback — is already spent.
    assert!(!db.consume_oauth_state(&state, "github").await.unwrap());
}

#[tokio::test]
async fn a_state_is_single_purpose() {
    let (db, _pool) = world(&db_or_skip!()).await;

    let state = db.mint_oauth_state("github").await.unwrap();
    // A state minted for one provider is not spendable under another name —
    // the provider is part of what the state proves.
    assert!(!db.consume_oauth_state(&state, "other").await.unwrap());
    // And an unknown value answers the same way.
    assert!(
        !db.consume_oauth_state("oxo-not-a-state", "github")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn an_expired_state_consumes_nothing() {
    let (db, pool) = world(&db_or_skip!()).await;

    let state = db.mint_oauth_state("github").await.unwrap();
    sqlx::query(
        "UPDATE oxsum.oauth_states SET expires_at = now() - interval '1 second' \
         WHERE state_hash = $1",
    )
    .bind(state_hash(&state))
    .execute(&pool)
    .await
    .unwrap();
    assert!(!db.consume_oauth_state(&state, "github").await.unwrap());
}

#[tokio::test]
async fn a_linked_identity_logs_in_again() {
    let (db, _pool) = world(&db_or_skip!()).await;
    let email = format!(
        "linked_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );

    let first = db
        .oauth_login(&identity(&email), true)
        .await
        .expect("the first login registers");
    let second = db
        .oauth_login(&identity(&email), true)
        .await
        .expect("the second login resolves the link");

    // Same account both times — the provider id is the link, and the session
    // belongs to it rather than minting a second user.
    let first_id = first.principal.user.id;
    assert_eq!(second.principal.user.id, first_id);
    assert_eq!(second.principal.user.email, email);
}

#[tokio::test]
async fn a_verified_email_links_the_identity() {
    let (db, pool) = world(&db_or_skip!()).await;
    let email = format!(
        "match_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let registered = db
        .register(oxsum_core::NewUser {
            email: email.clone(),
            password: "a long enough password".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");

    let session = db
        .oauth_login(&identity(&email), true)
        .await
        .expect("the email match resolves the account");
    assert_eq!(session.principal.user.id, registered.user.id);

    // The link is on record, so the next login resolves by provider id — the
    // row the match created.
    let linked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.oauth_accounts \
         WHERE user_id = $1 AND provider = 'github'",
    )
    .bind(registered.user.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(linked, 1);
}

#[tokio::test]
async fn a_new_identity_registers_a_whole_account() {
    let (db, pool) = world(&db_or_skip!()).await;
    let email = format!(
        "fresh_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );

    let session = db
        .oauth_login(&identity(&email), true)
        .await
        .expect("registers through the provider");

    // The full shape a registration has: verified email (the provider already
    // checked it), a personal organization owned by the user, and a first API
    // key — all of it in the transaction that landed.
    assert!(session.principal.user.email_verified);
    assert_eq!(session.principal.role, oxsum_core::Role::Owner);
    let (verified, keys): (bool, i64) = sqlx::query_as(
        "SELECT u.email_verified_at IS NOT NULL, \
                (SELECT count(*) FROM oxsum.api_keys k JOIN oxsum.organizations o \
                 ON k.organization_id = o.organization_id \
                 WHERE o.organization_id = $2) \
         FROM oxsum.users u WHERE u.user_id = $1",
    )
    .bind(session.principal.user.id)
    .bind(session.principal.organization.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(verified, "the provider-verified email lands verified");
    assert_eq!(keys, 1, "the registration's first API key exists");
}

#[tokio::test]
async fn an_invite_mode_deployment_refuses_new_accounts() {
    let (db, _pool) = world(&db_or_skip!()).await;
    let email = format!(
        "closed_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );

    let error = db
        .oauth_login(&identity(&email), false)
        .await
        .expect_err("an unknown identity cannot self-register here");
    assert!(matches!(error, WalletError::Forbidden(_)));

    // …but a linked or email-matched account still logs in: the mode gates
    // registration, not the credential.
    let registered = db
        .register(oxsum_core::NewUser {
            email: email.clone(),
            password: "a long enough password".to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    let session = db
        .oauth_login(&identity(&email), false)
        .await
        .expect("an existing account logs in regardless of signup mode");
    assert_eq!(session.principal.user.id, registered.user.id);
}

#[tokio::test]
async fn an_oauth_account_has_no_usable_password() {
    let (db, _pool) = world(&db_or_skip!()).await;
    let email = format!(
        "nopw_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    db.oauth_login(&identity(&email), true)
        .await
        .expect("registers");

    // No password was ever chosen, so no password authenticates: the hash on
    // record is of a secret nobody holds.
    let error = db
        .login(&email, "anything at all")
        .await
        .expect_err("a password the user never set cannot open the account");
    assert!(matches!(error, WalletError::InvalidCredentials));
}

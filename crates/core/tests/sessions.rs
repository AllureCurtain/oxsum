//! Session integration tests: login, cookie authentication, logout and role scopes,
//! against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of
//! failing, so a bare `cargo test` still passes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    ActingKey, CreatedSession, Db, KeyPrincipal, KeyScope, NewUser, Principal, WalletError,
};
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery staple";

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

/// A name that no other run of this suite has used.
fn fresh(name: &str) -> String {
    format!("{name}_{}", &Uuid::new_v4().simple().to_string()[..8])
}

fn signup(name: &str) -> NewUser {
    NewUser {
        email: format!("{}@example.com", fresh(name)),
        password: PASSWORD.to_owned(),
        organization_name: None,
    }
}

async fn register(db: &Db, name: &str) -> oxsum_core::Registration {
    db.register(signup(name)).await.unwrap()
}

#[tokio::test]
async fn login_mints_a_session_that_authenticates() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = register(&db, "login").await;

    let CreatedSession { principal, token } =
        db.login(&registration.user.email, PASSWORD).await.unwrap();
    assert!(token.starts_with("oxsess-"), "token is {token}");
    assert_eq!(principal.user.id, registration.user.id);
    assert_eq!(principal.organization.id, registration.organization.id);
    assert_eq!(principal.role, oxsum_core::Role::Owner);
    // Thirty days of absolute expiry, no sliding renewal.
    let lifetime = principal.session.expires_at - principal.session.created_at;
    assert!(
        lifetime.whole_seconds() >= 30 * 24 * 3600 - 60
            && lifetime.whole_seconds() <= 30 * 24 * 3600 + 60,
        "session lifetime is {lifetime}"
    );

    let resolved = db
        .authenticate_session(&token)
        .await
        .unwrap()
        .expect("the session authenticates");
    assert_eq!(resolved.session.id, principal.session.id);
    assert_eq!(resolved.user.id, registration.user.id);
    assert_eq!(resolved.organization.id, registration.organization.id);
    assert_eq!(resolved.role, oxsum_core::Role::Owner);

    // A key principal keeps acting as the whole organization: no user, full scope.
    let key_principal = Principal::Key(KeyPrincipal {
        organization: registration.organization.clone(),
        key: ActingKey {
            key_id: Uuid::new_v4(),
            spend_limit_minor: None,
        },
    });
    assert_eq!(key_principal.user_id(), None);
    assert!(matches!(key_principal.key_scope(), KeyScope::Organization));
}

#[tokio::test]
async fn login_failures_are_indistinguishable() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = register(&db, "loginfail").await;

    let wrong_password = db
        .login(&registration.user.email, "wrong password 123")
        .await;
    let unknown_email = db
        .login("nobody-has-this@example.com", "wrong password 123")
        .await;

    for result in [wrong_password, unknown_email] {
        let error = result.expect_err("login must fail");
        assert!(
            matches!(error, WalletError::InvalidCredentials),
            "unexpected error: {error:?}"
        );
        assert_eq!(error.to_string(), "invalid email or password");
    }

    // Email matching is case-insensitive and trims, like registration's uniqueness rule.
    let upper = registration.user.email.to_uppercase();
    assert!(
        db.login(&format!("  {upper}  "), PASSWORD).await.is_ok(),
        "login normalizes the email the way signup does"
    );
}

#[tokio::test]
async fn the_plaintext_token_is_nowhere_in_the_database() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = register(&db, "tokenhash").await;
    let created = db.login(&registration.user.email, PASSWORD).await.unwrap();
    let token = &created.token;

    // Every column of the row, as text: a stored token would show up in one of them.
    let row = sqlx::query(
        "SELECT session_id::text AS a, user_id::text AS b, organization_id::text AS c, \
                token_hash::text AS d, created_at::text AS e, expires_at::text AS f, \
                last_used_at::text AS g, revoked_at::text AS h \
         FROM oxsum.sessions WHERE session_id = $1",
    )
    .bind(created.principal.session.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    for column in ["a", "b", "c", "d", "e", "f", "g", "h"] {
        let value: Option<String> = row.try_get(column).unwrap();
        assert!(
            !value.is_some_and(|value| value.contains(token)),
            "column {column} holds the plaintext token"
        );
    }

    // The hash is a SHA-256 of the token, not the token: 32 bytes, unlike a 71-character
    // token, and the token cannot be read back out of it.
    let hash: Vec<u8> =
        sqlx::query_scalar("SELECT token_hash FROM oxsum.sessions WHERE session_id = $1")
            .bind(created.principal.session.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(hash.len(), 32);
    assert_ne!(hash, token.as_bytes());

    // A raw dump of the table authenticates nothing: the hash, presented as a token,
    // resolves to no session.
    let dumped: String = sqlx::query_scalar(
        "SELECT encode(token_hash, 'hex') FROM oxsum.sessions WHERE session_id = $1",
    )
    .bind(created.principal.session.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        db.authenticate_session(&format!("oxsess-{dumped}"))
            .await
            .unwrap()
            .is_none(),
        "the stored hash must not authenticate"
    );
}

#[tokio::test]
async fn logout_revokes_and_is_idempotent() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = register(&db, "logout").await;
    let created = db.login(&registration.user.email, PASSWORD).await.unwrap();

    db.logout(&created.token).await.unwrap();
    assert!(
        db.authenticate_session(&created.token)
            .await
            .unwrap()
            .is_none(),
        "a logged-out session authenticates nothing"
    );

    // Logging out twice is not an error, and does not move the timestamp.
    let first: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT revoked_at FROM oxsum.sessions WHERE session_id = $1")
            .bind(created.principal.session.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    db.logout(&created.token).await.unwrap();
    let second: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT revoked_at FROM oxsum.sessions WHERE session_id = $1")
            .bind(created.principal.session.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(first, second);

    // A token that was never minted is not an error either.
    db.logout("oxsess-deadbeef").await.unwrap();

    // Garbage authenticates nothing, without a database probe that can tell it apart.
    assert!(
        db.authenticate_session("not-a-token")
            .await
            .unwrap()
            .is_none()
    );
    assert!(db.authenticate_session("").await.unwrap().is_none());
}

#[tokio::test]
async fn an_expired_session_stops_authenticating() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = register(&db, "expiry").await;
    let created = db.login(&registration.user.email, PASSWORD).await.unwrap();

    sqlx::query(
        "UPDATE oxsum.sessions SET expires_at = now() - interval '1 minute' \
         WHERE session_id = $1",
    )
    .bind(created.principal.session.id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        db.authenticate_session(&created.token)
            .await
            .unwrap()
            .is_none(),
        "an expired session authenticates nothing"
    );
}

#[tokio::test]
async fn a_session_dies_with_its_membership() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = register(&db, "membership").await;
    let created = db.login(&registration.user.email, PASSWORD).await.unwrap();
    assert!(
        db.authenticate_session(&created.token)
            .await
            .unwrap()
            .is_some()
    );

    // The membership is removed directly: invitation flows that do this through the API
    // are a later item, and the session layer only cares about the row.
    sqlx::query("DELETE FROM oxsum.memberships WHERE organization_id = $1 AND user_id = $2")
        .bind(registration.organization.id)
        .bind(registration.user.id)
        .execute(db.pool())
        .await
        .unwrap();
    assert!(
        db.authenticate_session(&created.token)
            .await
            .unwrap()
            .is_none(),
        "a session without its membership authenticates nothing"
    );
}

#[tokio::test]
async fn login_acts_as_the_oldest_membership() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let first = register(&db, "oldest").await;
    let second = register(&db, "newest").await;

    // A user with two memberships; the rows go in directly because invitation flows are a
    // later item. The second membership is backdated so it is the oldest.
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role, created_at) \
         VALUES ($1, $2, 'member', now() - interval '1 hour')",
    )
    .bind(first.organization.id)
    .bind(second.user.id)
    .execute(db.pool())
    .await
    .unwrap();

    let created = db.login(&second.user.email, PASSWORD).await.unwrap();
    assert_eq!(created.principal.organization.id, first.organization.id);
    assert_eq!(created.principal.role, oxsum_core::Role::Member);
}

#[tokio::test]
async fn role_scopes_constrain_key_listing_and_revocation() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let owner = register(&db, "scope_owner").await;
    let member_user = register(&db, "scope_member").await;
    let org = owner.organization.id;

    // The member's oldest membership is in the owner's organization, so login acts as it.
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role, created_at) \
         VALUES ($1, $2, 'member', now() - interval '1 hour')",
    )
    .bind(org)
    .bind(member_user.user.id)
    .execute(db.pool())
    .await
    .unwrap();
    let member = Principal::Session(
        db.login(&member_user.user.email, PASSWORD)
            .await
            .unwrap()
            .principal,
    );

    let owner_key = db
        .create_key(
            org,
            Some("owner-key".into()),
            None,
            Some(owner.user.id),
            None,
        )
        .await
        .unwrap();
    let member_key = db
        .create_key(
            org,
            Some("member-key".into()),
            None,
            Some(member_user.user.id),
            None,
        )
        .await
        .unwrap();

    // A member lists only the keys they created: the owner's key and the signup key
    // (created by the owner) are not in the list.
    let mine = db.list_keys(org, member.key_scope()).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].id, member_key.key.id);
    assert_eq!(mine[0].created_by, Some(member_user.user.id));

    // The owner sees every key of the organization.
    let owner_principal = Principal::Session(
        db.login(&owner.user.email, PASSWORD)
            .await
            .unwrap()
            .principal,
    );
    assert!(matches!(owner_principal.key_scope(), KeyScope::All));
    let all = db
        .list_keys(org, owner_principal.key_scope())
        .await
        .unwrap();
    assert_eq!(all.len(), 3);

    // A member revoking a key they did not create gets a 404-shaped answer — None — and
    // the key stays live.
    assert!(
        db.revoke_key(org, owner_key.key.id, member.key_scope())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        db.authenticate(&owner_key.secret).await.unwrap().is_some(),
        "the key a member could not revoke still works"
    );

    // The owner revoking the member's key works.
    let revoked = db
        .revoke_key(org, member_key.key.id, owner_principal.key_scope())
        .await
        .unwrap()
        .expect("the owner may revoke any key");
    assert!(revoked.revoked_at.is_some());

    // A key minted with an API key records no creator: it is invisible to a member's
    // listing, and a member cannot revoke it. (The revoked member key is still listed,
    // with its revocation timestamp — listing never hides revoked keys.)
    let machine_key = db.create_key(org, None, None, None, None).await.unwrap();
    assert_eq!(machine_key.key.created_by, None);
    let mine = db.list_keys(org, member.key_scope()).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].id, member_key.key.id);
    assert!(mine[0].revoked_at.is_some());
    assert!(
        db.revoke_key(org, machine_key.key.id, member.key_scope())
            .await
            .unwrap()
            .is_none()
    );
}

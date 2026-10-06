//! Identity integration tests: signup, organizations and API keys, against real PostgreSQL.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip instead of
//! failing, so a bare `cargo test` still passes.
//!
//! oxsum's own tables live in one shared `oxsum` schema (unlike the per-organization ledger
//! schemas), so every test works on its own freshly named email and organization.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{Db, KeyConstraints, KeyScope, NewUser, Tenants, WalletError};
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

const ONE: i64 = 1_000_000;
const PASSWORD: &str = "correct horse battery staple";
const D: time::Date = time::macros::date!(2026 - 10 - 01);

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

#[tokio::test]
async fn registering_creates_a_user_with_a_personal_organization_and_one_key() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let new = signup("signup");

    let registration = db.register(new.clone()).await.unwrap();

    assert_eq!(registration.user.email, new.email);
    assert_eq!(registration.organization.kind, oxsum_core::Kind::Personal);
    // The default organization name is the email's local part.
    assert_eq!(
        registration.organization.name,
        new.email.split('@').next().unwrap()
    );
    // The tenant id is the organization's UUID without dashes: 32 characters the ledger's
    // tenant-id rule accepts unchanged.
    assert_eq!(registration.organization.tenant_id.len(), 32);
    assert!(
        registration
            .organization
            .tenant_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );

    let secret = &registration.api_key.secret;
    assert!(secret.starts_with("oxs-"), "secret is {secret}");
    assert!(
        secret.starts_with(&registration.api_key.key.prefix),
        "the display prefix is the start of the secret"
    );
    assert!(registration.api_key.key.revoked_at.is_none());
    assert!(registration.api_key.key.expires_at.is_none());

    // The key resolves to the organization it was minted for, and nothing else.
    let (resolved, _) = db.authenticate(secret).await.unwrap().expect("key works");
    assert_eq!(resolved.id, registration.organization.id);
    assert_eq!(resolved.tenant_id, registration.organization.tenant_id);

    // And the ledger that tenant id names is usable: signup itself does not open one.
    let wallet = Tenants::new(db.pool().clone())
        .get(&resolved.tenant_id)
        .await
        .unwrap();
    wallet.top_up("first", 5 * ONE, D).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 5 * ONE);
}

#[tokio::test]
async fn the_plaintext_key_is_nowhere_in_the_database() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = db.register(signup("hash")).await.unwrap();
    let secret = &registration.api_key.secret;

    // Every column of the row, as text, including the hash: a stored secret would show up
    // in one of them.
    let row = sqlx::query(
        "SELECT key_id::text AS a, organization_id::text AS b, name AS c, prefix AS d, \
                secret_hash::text AS e, created_by::text AS f, created_at::text AS g, \
                expires_at::text AS h, revoked_at::text AS i \
         FROM oxsum.api_keys WHERE key_id = $1",
    )
    .bind(registration.api_key.key.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    for column in ["a", "b", "c", "d", "e", "f", "g", "h", "i"] {
        let value: Option<String> = row.try_get(column).unwrap();
        assert!(
            !value.is_some_and(|value| value.contains(secret)),
            "column {column} holds the plaintext secret"
        );
    }

    // The hash is a SHA-256 of the secret, not the secret: 32 bytes, unlike a 68-character
    // secret, and the secret cannot be read back out of it.
    let hash: Vec<u8> =
        sqlx::query_scalar("SELECT secret_hash FROM oxsum.api_keys WHERE key_id = $1")
            .bind(registration.api_key.key.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(hash.len(), 32);
    assert_ne!(hash, secret.as_bytes());
}

#[tokio::test]
async fn two_organizations_under_one_user_have_isolated_balances() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = db.register(signup("isolation")).await.unwrap();
    let user = registration.user.id;
    let a = registration.organization.clone();

    // A second organization the same user belongs to. Creating organizations beyond the
    // personal one is a later flow with its own contract; the schema and the ledger are
    // what this test is about, so the rows go in directly.
    let b_id = Uuid::new_v4();
    let b_tenant = b_id.simple().to_string();
    sqlx::query(
        "INSERT INTO oxsum.organizations (organization_id, name, tenant_id, kind) \
         VALUES ($1, $2, $3, 'personal')",
    )
    .bind(b_id)
    .bind(fresh("second"))
    .bind(&b_tenant)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(b_id)
    .bind(user)
    .execute(db.pool())
    .await
    .unwrap();
    let b_key = db
        .create_key(
            b_id,
            Some("second".into()),
            None,
            Some(user),
            KeyConstraints::default(),
        )
        .await
        .unwrap();

    // Reading the keys back proves the credential, not the caller's wishes, picks the
    // organization: two keys of one user, two organizations.
    let (from_a, _) = db
        .authenticate(&registration.api_key.secret)
        .await
        .unwrap()
        .unwrap();
    let (from_b, _) = db.authenticate(&b_key.secret).await.unwrap().unwrap();
    assert_eq!(from_a.id, a.id);
    assert_eq!(from_b.id, b_id);

    let tenants = Tenants::new(db.pool().clone());
    let wallet_a = tenants.get(&a.tenant_id).await.unwrap();
    let wallet_b = tenants.get(&b_tenant).await.unwrap();
    let receipt = wallet_a.top_up("a-funds", 10 * ONE, D).await.unwrap();

    assert_eq!(wallet_a.available().await.unwrap(), 10 * ONE);
    assert_eq!(wallet_b.available().await.unwrap(), 0, "B sees A's money");
    assert_eq!(wallet_a.log_size().await.unwrap(), 1);
    assert_eq!(wallet_b.log_size().await.unwrap(), 0);

    // A's bill exists in A's ledger and nowhere else, proof and all.
    assert!(
        wallet_b
            .receipt_proof(receipt.entry_id)
            .await
            .unwrap()
            .is_none(),
        "B can prove an entry of A"
    );
    assert!(
        wallet_a
            .receipt_proof(receipt.entry_id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_revoked_key_stops_authenticating() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = db.register(signup("revoke")).await.unwrap();
    let organization = registration.organization.id;
    let secret = registration.api_key.secret.clone();

    assert!(db.authenticate(&secret).await.unwrap().is_some());

    let revoked = db
        .revoke_key(
            organization,
            registration.api_key.key.id,
            KeyScope::Organization,
        )
        .await
        .unwrap()
        .expect("the key exists");
    assert!(revoked.revoked_at.is_some());
    assert!(db.authenticate(&secret).await.unwrap().is_none());

    // Revoking again is not an error, and does not move the timestamp.
    let again = db
        .revoke_key(
            organization,
            registration.api_key.key.id,
            KeyScope::Organization,
        )
        .await
        .unwrap()
        .expect("the key still exists");
    assert_eq!(again.revoked_at, revoked.revoked_at);

    // Another organization's key id is simply not there.
    let other = db.register(signup("revoke_other")).await.unwrap();
    assert!(
        db.revoke_key(
            other.organization.id,
            registration.api_key.key.id,
            KeyScope::Organization
        )
        .await
        .unwrap()
        .is_none()
    );
}

#[tokio::test]
async fn an_expired_key_stops_authenticating() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let registration = db.register(signup("expiry")).await.unwrap();
    let organization = registration.organization.id;

    let key = db
        .create_key(
            organization,
            Some("short-lived".into()),
            Some(time::OffsetDateTime::now_utc() + time::Duration::seconds(60)),
            None,
            KeyConstraints::default(),
        )
        .await
        .unwrap();
    assert!(db.authenticate(&key.secret).await.unwrap().is_some());

    // Move the expiry into the past rather than waiting a minute for it.
    sqlx::query(
        "UPDATE oxsum.api_keys SET expires_at = now() - interval '1 second' WHERE key_id = $1",
    )
    .bind(key.key.id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        db.authenticate(&key.secret).await.unwrap().is_none(),
        "an expired key authenticated"
    );

    // A key that is already expired at creation is refused outright.
    let err = db
        .create_key(
            organization,
            None,
            Some(time::OffsetDateTime::now_utc() - time::Duration::seconds(1)),
            None,
            KeyConstraints::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "got {err:?}");
}

#[tokio::test]
async fn registering_one_email_twice_is_a_conflict() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let new = signup("dupe");
    db.register(new.clone()).await.unwrap();

    let err = db.register(new.clone()).await.unwrap_err();
    assert!(matches!(err, WalletError::Conflict(_)), "got {err:?}");

    // Case does not create a second account: uniqueness is on the normalized address.
    let shouting = NewUser {
        email: new.email.to_uppercase(),
        ..new
    };
    let err = db.register(shouting).await.unwrap_err();
    assert!(matches!(err, WalletError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn concurrent_registrations_of_one_email_leave_exactly_one_account() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let new = signup("race");

    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            let new = new.clone();
            tokio::spawn(async move { db.register(new).await })
        })
        .collect();

    let mut created = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => created += 1,
            Err(WalletError::Conflict(_)) => {}
            Err(e) => panic!("unexpected: {e:?}"),
        }
    }
    assert_eq!(created, 1, "the unique index must settle this, not luck");
}

#[tokio::test]
async fn signup_validates_its_input() {
    let url = db_or_skip!();
    let db = db(&url).await;

    let cases = [
        NewUser {
            email: "not-an-address".into(),
            ..signup("bad_email")
        },
        NewUser {
            email: "two@@example.com".into(),
            ..signup("bad_email")
        },
        NewUser {
            email: format!("{} example.com", fresh("space")),
            ..signup("bad_email")
        },
        NewUser {
            password: "too short".into(),
            ..signup("bad_password")
        },
        NewUser {
            organization_name: Some("x".repeat(81)),
            ..signup("long_org")
        },
    ];
    for new in cases {
        let err = db.register(new.clone()).await.unwrap_err();
        assert!(
            matches!(err, WalletError::InvalidInput(_)),
            "{new:?} was accepted: {err:?}"
        );
    }
}

#[tokio::test]
async fn key_management_is_scoped_to_its_organization() {
    let url = db_or_skip!();
    let db = db(&url).await;
    let first = db.register(signup("keys")).await.unwrap();
    let organization = first.organization.id;

    let second = db
        .create_key(
            organization,
            Some("  staging  ".into()),
            None,
            None,
            KeyConstraints::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        second.key.name.as_deref(),
        Some("staging"),
        "name is trimmed"
    );
    assert!(second.key.id != first.api_key.key.id);
    assert_ne!(second.secret, first.api_key.secret);

    // Newest first, metadata only: the list type has no secret field at all.
    let keys = db
        .list_keys(organization, KeyScope::Organization)
        .await
        .unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].id, second.key.id);
    assert_eq!(keys[1].id, first.api_key.key.id);
    assert!(keys.iter().all(|key| key.revoked_at.is_none()));

    // A different organization sees its own key and none of these.
    let other = db.register(signup("keys_other")).await.unwrap();
    let other_keys = db
        .list_keys(other.organization.id, KeyScope::Organization)
        .await
        .unwrap();
    assert_eq!(other_keys.len(), 1);
    assert_eq!(other_keys[0].id, other.api_key.key.id);

    let too_long = db
        .create_key(
            organization,
            Some("x".repeat(81)),
            None,
            None,
            KeyConstraints::default(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(too_long, WalletError::InvalidInput(_)),
        "got {too_long:?}"
    );
}

#[tokio::test]
async fn migrating_twice_and_concurrently_is_safe() {
    let url = db_or_skip!();
    let db = db(&url).await;
    db.migrate().await.expect("a second migration is a no-op");

    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move { db.migrate().await })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().expect("concurrent migrations");
    }

    // The tables, once each, in oxsum's own schema — never in public.
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = 'oxsum' ORDER BY table_name",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        tables,
        vec![
            "_migrations".to_owned(),
            "api_keys".to_owned(),
            "channel_prices".to_owned(),
            "channels".to_owned(),
            "deposits".to_owned(),
            "invitations".to_owned(),
            "memberships".to_owned(),
            "open_holds".to_owned(),
            "organizations".to_owned(),
            "redemption_codes".to_owned(),
            "sessions".to_owned(),
            "statement_lines".to_owned(),
            "statements".to_owned(),
            "usage_records".to_owned(),
            "users".to_owned(),
        ]
    );

    // And nothing landed outside it. The runner clears `search_path` so an unqualified name
    // fails instead of quietly creating tables in `public`; this is the assertion for the
    // version of the migration that did not.
    let stray: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_schema = 'public' \
           AND table_name IN ('users', 'organizations', 'memberships', 'api_keys')",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(stray, 0, "oxsum's tables exist outside the oxsum schema");
}

/// The organizations list pages by `(created_at, organization_id)` (issue #93): a
/// walk sees every organization exactly once, ordered, and the cursor is the
/// keyset bound — two organizations that share a timestamp still sort by id.
#[tokio::test]
async fn organization_pages_walk_the_table_without_gaps_or_repeats() {
    let url = db_or_skip!();
    let db = db(&url).await;
    // Three organizations whose pages this test can name, on a table the shared
    // database may already fill.
    for name in ["a", "b", "c"] {
        db.register(signup(&format!("orgpage_{name}")))
            .await
            .unwrap();
    }

    let mut seen: Vec<uuid::Uuid> = Vec::new();
    let mut last_key: Option<(time::OffsetDateTime, uuid::Uuid)> = None;
    let mut before = None;
    loop {
        let page = db.organizations_page(before, 2).await.unwrap();
        assert!(page.rows.len() <= 2, "the page honors its limit");
        for organization in &page.rows {
            let key = (organization.created_at, organization.id);
            assert!(
                last_key.is_none_or(|past| key > past),
                "oldest first, strictly ordered"
            );
            last_key = Some(key);
            assert!(!seen.contains(&organization.id), "no organization twice");
            seen.push(organization.id);
        }
        match page.next_cursor {
            Some(cursor) => before = Some(cursor),
            None => break,
        }
    }
    // The three registered here are somewhere in the walk.
    assert!(seen.len() >= 3);
}

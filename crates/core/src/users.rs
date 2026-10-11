//! Users and signup.
//!
//! Passwords are argon2 hashes from `password-auth`, one function each way and no knobs to
//! get wrong (docs/decisions.md, "users and login"). Signup is one transaction: the user,
//! the personal organization, the owner membership and the first API key. A user without an
//! organization, or an organization without its owner, cannot exist.

use password_auth::{generate_hash, verify_password};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::keys::{self, CreatedApiKey};
use crate::orgs::{self, Kind, Organization, Role};

/// The shortest password signup accepts. Length is the only rule that survives contact with
/// real users: no composition classes, no expiry.
const MIN_PASSWORD: usize = 12;
const MAX_PASSWORD: usize = 256;
/// RFC 5321's maximum path length.
const MAX_EMAIL: usize = 320;
const MAX_ORG_NAME: usize = 80;
/// The name of the key signup mints, so the list has something readable in it.
pub(crate) const FIRST_KEY_NAME: &str = "default";

/// A user as the API presents it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: Uuid,
    pub email: String,
    /// Whether the address has been verified through a mailed token (issue
    /// #150): `false` for an account that registered before or without a mailer.
    pub email_verified: bool,
}

/// What a caller must supply to register.
#[derive(Debug, Clone)]
pub struct NewUser {
    pub email: String,
    pub password: String,
    /// Name of the personal organization; defaults to the local part of the email.
    pub organization_name: Option<String>,
}

/// What an account deletion removed (issue #202). The ledger is untouched, so
/// no count covers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDeletion {
    /// Sessions the account held, now dead.
    pub sessions_revoked: u64,
    /// API keys the account minted, now dead.
    pub keys_revoked: u64,
    /// Memberships the account left; the organizations stay.
    pub memberships_removed: u64,
}

/// What registration produced. The key's secret is never seen again after this response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub user: User,
    pub organization: Organization,
    pub api_key: CreatedApiKey,
    /// Whether a verification mail went out — false until the route layer mails
    /// one, which a deployment without a mailer never does (issue #150).
    pub verification_sent: bool,
}

impl Db {
    /// Registers a user with a personal organization and the first API key.
    ///
    /// The ledger is deliberately not part of this transaction: it is created on first use
    /// by `Tenants::get`, idempotently, and a ledger whose schema already exists is reused.
    /// Registering therefore cannot fail half way through a ledger migration, and a retry
    /// after a failed registration is safe.
    pub async fn register(&self, new: NewUser) -> Result<Registration, WalletError> {
        let email = validate_email(&new.email)?;
        let email_normalized = email.to_lowercase();
        validate_password(&new.password)?;
        let organization_name = organization_name(new.organization_name.as_deref(), &email)?;

        // Argon2 is deliberately slow (~100ms), and it runs before the transaction opens,
        // so hashing never holds a database connection. `generate_hash` is infallible.
        let password_hash = generate_hash(&new.password);

        let user_id = Uuid::new_v4();
        let organization_id = Uuid::new_v4();
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO oxsum.users (user_id, email, email_normalized, password_hash) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(user_id)
        .bind(&email)
        .bind(&email_normalized)
        .bind(&password_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            keys::conflict_or_storage(e, "users_email_normalized_key", "email already registered")
        })?;
        let organization =
            orgs::insert(&mut tx, organization_id, &organization_name, Kind::Personal).await?;
        orgs::add_member(&mut tx, organization_id, user_id, Role::Owner).await?;
        let api_key = keys::insert(
            &mut tx,
            organization_id,
            Some(FIRST_KEY_NAME.to_owned()),
            None,
            Some(user_id),
            keys::KeyConstraints::default(),
        )
        .await?;
        tx.commit().await?;

        Ok(Registration {
            user: User {
                id: user_id,
                email,
                email_verified: false,
            },
            organization,
            api_key,
            verification_sent: false,
        })
    }

    /// Resets a user's password and revokes every session they hold, in one
    /// transaction. Both recovery paths call this — the platform admin's
    /// endpoint and the self-service reset through a mailed token (issue #150);
    /// an unknown user is `NotFound`. Returns the sessions the reset revoked,
    /// so the caller can report what died with it.
    pub async fn reset_password(&self, user_id: Uuid, password: &str) -> Result<u64, WalletError> {
        validate_password(password)?;
        // Hashing runs before the transaction opens, same as register: argon2 is
        // deliberately slow and must not hold a connection.
        let password_hash = generate_hash(password);

        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query("UPDATE oxsum.users SET password_hash = $1 WHERE user_id = $2")
            .bind(&password_hash)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        if updated.rows_affected() == 0 {
            return Err(WalletError::NotFound("no such user".into()));
        }
        let revoked = sqlx::query(
            "UPDATE oxsum.sessions SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(revoked.rows_affected())
    }

    /// Deletes an account by pseudonymization (issue #202): the identity goes,
    /// the books keep their history.
    ///
    /// `password` is the act's confirmation and is verified before anything
    /// moves — a wrong one is [`WalletError::InvalidCredentials`], the same
    /// answer login gives, so the refusal leaks nothing about the account.
    /// An account created through OAuth holds a hash nobody knows, so it can
    /// never confirm this call until the reset flow sets a real password.
    ///
    /// One transaction then does the whole walk:
    ///
    /// - the last-owner wall: an organization the user is the only owner of
    ///   *and that has other members* refuses the delete
    ///   [`WalletError::Conflict`] — the seat has to be transferred first, the
    ///   same rule `remove_member` enforces. An organization whose only member
    ///   is this user does not trip it: its seat goes empty in the same
    ///   transaction that tombstones the organization;
    /// - every membership of the user is removed; an organization whose only
    ///   member was this user — the personal org every signup makes, or a team
    ///   org that shrank to one seat — is tombstoned (renamed `deleted-<id>`,
    ///   suspended) rather than deleted — an append-only ledger cannot be
    ///   orphaned, and a memberless organization acts for nobody;
    /// - the user row is tombstoned: `email`/`email_normalized` become
    ///   `deleted-<id>@deleted.invalid` (the unique index stays satisfied —
    ///   the id is the row's own), `password_hash` is replaced by a marker
    ///   that no password verifies against, `email_verified_at` cleared;
    /// - every session the user holds is revoked, every API key they minted
    ///   (`created_by`) is revoked — the rows stay for provenance;
    /// - the attribution the user's keys wrote is pseudonymized:
    ///   `usage_records.end_user`/`tags` and `open_holds.end_user`/`tags` on
    ///   rows their minted keys paid are nulled — mutable-store data, which is
    ///   exactly why it lives outside the hashed ledger entries;
    /// - `oauth_accounts` and `email_tokens` go with the account; invitations
    ///   the user sent stay — an invitation is the receiving organization's
    ///   fact, and only its hash is stored anyway.
    ///
    /// The ledger is untouched by design: settlement descriptions carry no
    /// identity, so a deleted user's bills verify exactly as before.
    ///
    /// Returns what the walk removed, for the caller to report.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidCredentials`] on a wrong password;
    /// [`WalletError::NotFound`] when the user does not exist — deleting twice
    /// lands here, since a tombstoned account holds no membership the session
    /// could still act through; [`WalletError::Conflict`] while the user is an
    /// organization's last owner; storage failures surface as `WalletError`.
    pub async fn delete_account(
        &self,
        user_id: Uuid,
        password: &str,
    ) -> Result<AccountDeletion, WalletError> {
        // Verify before the transaction opens: the argon2 check is deliberately
        // slow (~100ms) and must not hold a connection, same as login.
        let row = sqlx::query("SELECT password_hash FROM oxsum.users WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or_else(|| WalletError::NotFound("no such user".into()))?;
        let password_hash: String = row.try_get("password_hash")?;
        if verify_password(password, &password_hash).is_err() {
            return Err(WalletError::InvalidCredentials);
        }

        let mut tx = self.pool().begin().await?;

        // The last-owner wall, read inside the transaction against locked
        // membership rows: a delete that removes the seat must be refused
        // before anything is blanked. The lock is taken on the user's own
        // membership rows first — `FOR UPDATE` cannot ride an aggregate — so
        // a concurrent ownership transfer or role change on the same rows
        // serializes behind it; then the count runs under that lock.
        sqlx::query(
            "SELECT organization_id FROM oxsum.memberships WHERE user_id = $1 \
             FOR UPDATE",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        // The wall applies to organizations that outlive the account — ones
        // with other members. A personal organization whose only member is
        // this user is tombstoned in the same transaction, so its empty seat
        // is no orphan: the organization dies with the account.
        let last_owner = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM oxsum.memberships m \
             WHERE m.user_id = $1 AND m.role = 'owner' \
               AND EXISTS ( \
                     SELECT 1 FROM oxsum.memberships o \
                     WHERE o.organization_id = m.organization_id \
                       AND o.user_id <> $1) \
               AND NOT EXISTS ( \
                     SELECT 1 FROM oxsum.memberships o \
                     WHERE o.organization_id = m.organization_id \
                       AND o.role = 'owner' AND o.user_id <> $1)",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
        if last_owner > 0 {
            return Err(WalletError::Conflict(
                "you are an organization's only owner: transfer ownership first".into(),
            ));
        }

        // Organizations whose only member was this user: tombstone the row —
        // renamed and suspended — because the ledger under its tenant id may
        // hold history an append-only store cannot orphan, and a memberless
        // organization acts for nobody. (This is where the personal
        // organization every signup creates dies; a team organization that
        // shrank to one seat meets the same end.)
        sqlx::query(
            "UPDATE oxsum.organizations SET \
                 name = 'deleted-' || organization_id::text, \
                 suspended_at = COALESCE(suspended_at, now()) \
             WHERE organization_id IN ( \
                   SELECT organization_id FROM oxsum.memberships WHERE user_id = $1) \
               AND NOT EXISTS ( \
                   SELECT 1 FROM oxsum.memberships o \
                   WHERE o.organization_id = oxsum.organizations.organization_id \
                     AND o.user_id <> $1)",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        let memberships_removed = sqlx::query("DELETE FROM oxsum.memberships WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();

        let sessions_revoked = sqlx::query(
            "UPDATE oxsum.sessions SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        let keys_revoked = sqlx::query(
            "UPDATE oxsum.api_keys SET revoked_at = COALESCE(revoked_at, now()) \
             WHERE created_by = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        // Pseudonymize the attribution the account's keys wrote — the turns
        // already settled (usage_records) and the ones still in flight
        // (open_holds, whose watch rows carry the same fields so a swept
        // settlement still says whose turn it was). The entry is untouched;
        // that is what the mutable store is for.
        sqlx::query(
            "UPDATE oxsum.usage_records SET end_user = NULL, tags = '{}' \
             WHERE key_id IN (SELECT key_id FROM oxsum.api_keys WHERE created_by = $1)",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE oxsum.open_holds SET end_user = NULL, tags = '{}' \
             WHERE key_id IN (SELECT key_id FROM oxsum.api_keys WHERE created_by = $1)",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        // Account-access plumbing goes with the account; the user row stays
        // as the tombstone the foreign keys (invitations.invited_by among
        // them) point at.
        sqlx::query("DELETE FROM oxsum.oauth_accounts WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM oxsum.email_tokens WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

        // The tombstone: a marker no password verifies (argon2 parse fails on
        // it), an address that can never be mailed or collided with
        // (`deleted.invalid` is a guaranteed-invalid TLD), and no verified
        // flag. `email_normalized` keeps UNIQUE satisfied — the id is the
        // row's own.
        sqlx::query(
            "UPDATE oxsum.users SET \
                 email = 'deleted-' || user_id::text || '@deleted.invalid', \
                 email_normalized = 'deleted-' || user_id::text || '@deleted.invalid', \
                 password_hash = '!deleted', \
                 email_verified_at = NULL \
             WHERE user_id = $1",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(AccountDeletion {
            sessions_revoked,
            keys_revoked,
            memberships_removed,
        })
    }
}

/// Trims and sanity-checks an address: one `@`, something on each side, no whitespace.
///
/// v1 sends no email, so this only has to stop obvious mistakes and keep the unique index
/// meaningful; it is not an address validator. Shared with membership management, where the
/// same address is looked up in `users.email_normalized`.
pub(crate) fn validate_email(raw: &str) -> Result<String, WalletError> {
    let email = raw.trim();
    if email.is_empty() || email.len() > MAX_EMAIL || email.chars().any(char::is_whitespace) {
        return Err(WalletError::InvalidInput(
            "email must be one address of at most 320 bytes".into(),
        ));
    }
    let (local, domain) = email
        .split_once('@')
        .ok_or_else(|| WalletError::InvalidInput("email must look like name@example.com".into()))?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return Err(WalletError::InvalidInput(
            "email must look like name@example.com".into(),
        ));
    }
    Ok(email.to_owned())
}

pub(crate) fn validate_password(password: &str) -> Result<(), WalletError> {
    // Characters on both ends, matching the limits openapi.yaml states: a user counting
    // characters should not be told they pass and then be refused.
    let length = password.chars().count();
    if length < MIN_PASSWORD {
        return Err(WalletError::InvalidInput(format!(
            "password must be at least {MIN_PASSWORD} characters"
        )));
    }
    if length > MAX_PASSWORD {
        return Err(WalletError::InvalidInput(format!(
            "password must be at most {MAX_PASSWORD} characters"
        )));
    }
    Ok(())
}

/// The personal organization's name: the caller's choice, or the email's local part.
pub(crate) fn organization_name(given: Option<&str>, email: &str) -> Result<String, WalletError> {
    if let Some(given) = given.map(str::trim).filter(|name| !name.is_empty()) {
        if given.chars().count() > MAX_ORG_NAME {
            return Err(WalletError::InvalidInput(format!(
                "organizationName must be at most {MAX_ORG_NAME} characters"
            )));
        }
        return Ok(given.to_owned());
    }
    let local = email.split('@').next().unwrap_or("personal");
    Ok(local.chars().take(MAX_ORG_NAME).collect())
}

//! OAuth login (issue #152, roadmap P6-2): the browser-facing flow's two halves
//! live here — the CSRF `state` that ties a callback to the attempt that minted
//! it, and the account resolution the callback runs once the provider has
//! answered.
//!
//! `oxsum.oauth_states` is the single-use table the authorize redirect mints
//! into and the callback consumes from — ten minutes to live, spent in the same
//! statement that checks it live, so a raced or replayed callback cannot mint a
//! second session off one state. `oxsum.oauth_accounts` links a provider
//! identity to a user by the provider's own id — not by email, which the
//! provider may reassign — keeping the verified email it arrived with as
//! provenance.
//!
//! An account an OAuth login creates has no known password: the hash stored is
//! argon2 over a random secret nobody holds, so the password path can never
//! authenticate it — the mail's reset (or the admin's) is the way to set one.
//! The address arrives verified, because the provider already did that work.

use std::fmt::Write;

use rand::Rng;
use sha2::{Digest, Sha256};
use time::Duration;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::sessions::CreatedSession;
use crate::{keys, orgs, users};

/// Every OAuth state starts with this, so the value is recognizable in logs.
const STATE_MARK: &str = "oxo-";
const STATE_BYTES: usize = 32;
/// A state lives ten minutes: long enough for the provider round-trip, short
/// enough that a leaked one is stale by the time it matters.
const STATE_LIFETIME: Duration = Duration::minutes(10);

/// Who the provider vouched for: the stable id it issued and the email it
/// verified. The route layer fills this from the provider's userinfo; core
/// trusts it only because the route required the provider to say `verified`.
#[derive(Debug, Clone)]
pub struct OAuthIdentity {
    pub provider: String,
    pub provider_user_id: String,
    pub email: String,
}

impl Db {
    /// Mints a CSRF state for an authorize redirect. The value goes to the
    /// provider and back in the query string; only its SHA-256 is stored.
    ///
    /// # Errors
    ///
    /// Storage failures surface as `WalletError`.
    pub async fn mint_oauth_state(&self, provider: &str) -> Result<String, WalletError> {
        let state = generate_state();
        sqlx::query(
            "INSERT INTO oxsum.oauth_states (state_hash, provider, expires_at) \
             VALUES ($1, $2, now() + $3)",
        )
        .bind(hash_state(&state).as_slice())
        .bind(provider)
        .bind(STATE_LIFETIME)
        .execute(self.pool())
        .await?;
        Ok(state)
    }

    /// Consumes a state: stamps `used_at` and answers whether it was live —
    /// unknown, spent, expired or wrong-provider all answer `false`,
    /// indistinguishably, in one statement.
    ///
    /// # Errors
    ///
    /// Storage failures surface as `WalletError`.
    pub async fn consume_oauth_state(
        &self,
        state: &str,
        provider: &str,
    ) -> Result<bool, WalletError> {
        let n = sqlx::query(
            "UPDATE oxsum.oauth_states SET used_at = now() \
             WHERE state_hash = $1 AND provider = $2 \
             AND used_at IS NULL AND expires_at > now()",
        )
        .bind(hash_state(state).as_slice())
        .bind(provider)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    /// Resolves an OAuth identity to a session. The order is deliberate: a link
    /// the account already has wins, then a verified-email match links the
    /// identity to that account for next time, and only then — when the caller
    /// allows registration — a new account.
    ///
    /// # Errors
    ///
    /// `WalletError::Forbidden` when the identity names no account and `create`
    /// is false (an `invite`-mode deployment); `WalletError::InvalidCredentials`
    /// when the resolved user holds no membership to act through (the same
    /// corner `login` cannot produce either). Storage failures surface as
    /// `WalletError`.
    pub async fn oauth_login(
        &self,
        identity: &OAuthIdentity,
        create: bool,
    ) -> Result<CreatedSession, WalletError> {
        let linked: Option<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM oxsum.oauth_accounts \
             WHERE provider = $1 AND provider_user_id = $2",
        )
        .bind(&identity.provider)
        .bind(&identity.provider_user_id)
        .fetch_optional(self.pool())
        .await?;
        if let Some(user_id) = linked {
            return self.session_for(user_id).await;
        }

        let normalized = identity.email.trim().to_lowercase();
        let existing: Option<Uuid> =
            sqlx::query_scalar("SELECT user_id FROM oxsum.users WHERE email_normalized = $1")
                .bind(&normalized)
                .fetch_optional(self.pool())
                .await?;
        if let Some(user_id) = existing {
            self.link_oauth_account(user_id, identity).await?;
            return self.session_for(user_id).await;
        }

        if !create {
            return Err(WalletError::Forbidden(
                "this deployment registers by invitation only".into(),
            ));
        }
        let user_id = self.register_oauth(identity).await?;
        self.session_for(user_id).await
    }

    /// Links a provider identity to an existing user. `ON CONFLICT` makes the
    /// race between two callbacks resolving the same account a no-op for the
    /// loser — the link is a fact, not an event.
    async fn link_oauth_account(
        &self,
        user_id: Uuid,
        identity: &OAuthIdentity,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.oauth_accounts (provider, provider_user_id, user_id, email) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(&identity.provider)
        .bind(&identity.provider_user_id)
        .bind(user_id)
        .bind(&identity.email)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Registers a user whose credential is the provider, not a password: the
    /// stored hash is argon2 over a random secret nobody holds, so no password
    /// ever matches — the account can still be recovered through a mailed or
    /// admin reset, which sets a real one. The email arrives verified; the
    /// account, personal organization, owner membership and first API key land
    /// in one transaction beside the OAuth link.
    async fn register_oauth(&self, identity: &OAuthIdentity) -> Result<Uuid, WalletError> {
        let email = users::validate_email(&identity.email)?;
        let email_normalized = email.to_lowercase();
        let organization_name = users::organization_name(None, &email)?;

        // The unguessable password: a random secret is hashed, the secret is
        // dropped, and only the hash is stored — argon2's cost still applies to
        // anyone probing the password path.
        let mut secret = [0u8; 32];
        rand::rng().fill_bytes(&mut secret);
        let password_hash = password_auth::generate_hash(secret);

        let user_id = Uuid::new_v4();
        let organization_id = Uuid::new_v4();
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO oxsum.users \
                 (user_id, email, email_normalized, password_hash, email_verified_at) \
             VALUES ($1, $2, $3, $4, now())",
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
        orgs::insert(
            &mut tx,
            organization_id,
            &organization_name,
            crate::orgs::Kind::Personal,
        )
        .await?;
        orgs::add_member(&mut tx, organization_id, user_id, crate::orgs::Role::Owner).await?;
        keys::insert(
            &mut tx,
            organization_id,
            Some(users::FIRST_KEY_NAME.to_owned()),
            None,
            Some(user_id),
            keys::KeyConstraints::default(),
        )
        .await?;
        sqlx::query(
            "INSERT INTO oxsum.oauth_accounts (provider, provider_user_id, user_id, email) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&identity.provider)
        .bind(&identity.provider_user_id)
        .bind(user_id)
        .bind(&email)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(user_id)
    }
}

/// A fresh state: the mark plus 32 random bytes from the operating system's
/// CSPRNG, hex-encoded like every token the project mints.
fn generate_state() -> String {
    let mut bytes = [0u8; STATE_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut state = String::with_capacity(STATE_MARK.len() + STATE_BYTES * 2);
    state.push_str(STATE_MARK);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(state, "{byte:02x}");
    }
    state
}

/// SHA-256 of the state, the only form the database ever holds.
fn hash_state(state: &str) -> Vec<u8> {
    Sha256::digest(state.as_bytes()).to_vec()
}

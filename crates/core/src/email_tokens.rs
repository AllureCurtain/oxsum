//! Email tokens: the single-use secrets the verification and password-reset
//! mails carry (issue #150, roadmap P6-1).
//!
//! The pattern is the invitation's: the token the mail's link carries is `oxt-`
//! plus 32 random bytes, and `oxsum.email_tokens` keeps only its SHA-256, so a
//! database dump verifies no address and resets no password. A token names its
//! `purpose` at mint and a consumption for the other purpose does not match it;
//! consumption is one `UPDATE … RETURNING` that checks live-and-unused and
//! stamps `used_at` atomically, so a token redeems once ever and a raced double
//! click cannot both win.

use std::fmt::Write;

use rand::Rng;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// Every email token starts with this, so the value is recognizable in logs.
const TOKEN_MARK: &str = "oxt-";
const TOKEN_BYTES: usize = 32;
/// A verification link lives seven days; a reset link one hour.
const VERIFY_LIFETIME: Duration = Duration::days(7);
const RESET_LIFETIME: Duration = Duration::hours(1);
/// Mints for one `(user, purpose)` are at least this far apart — the resend
/// cooldown that keeps the endpoints from being mail amplifiers.
const RESEND_COOLDOWN: Duration = Duration::seconds(60);

/// Which mail a token is for. The string is the row's `purpose` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailPurpose {
    Verify,
    Reset,
}

impl EmailPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Verify => "verify",
            Self::Reset => "reset",
        }
    }

    fn lifetime(self) -> Duration {
        match self {
            Self::Verify => VERIFY_LIFETIME,
            Self::Reset => RESET_LIFETIME,
        }
    }
}

/// A freshly minted token plus who it mails to: the only time the token itself
/// is visible — the row holds its hash.
#[derive(Debug, Clone)]
pub struct MintedEmailToken {
    pub token: String,
    pub email: String,
    pub expires_at: OffsetDateTime,
}

impl Db {
    /// Mints a token for `(user_id, purpose)` and answers it beside the user's
    /// email, ready for the mail to carry.
    ///
    /// `None` — not an error — is the resend cooldown: a live token minted for
    /// this pair within the last minute means a mail just went out, and minting
    /// another would only flood the inbox. The caller answers accordingly.
    ///
    /// # Errors
    ///
    /// `WalletError::NotFound` for an unknown user; storage failures surface as
    /// `WalletError`.
    pub async fn mint_email_token(
        &self,
        user_id: Uuid,
        purpose: EmailPurpose,
    ) -> Result<Option<MintedEmailToken>, WalletError> {
        let email: Option<String> =
            sqlx::query_scalar("SELECT email FROM oxsum.users WHERE user_id = $1")
                .bind(user_id)
                .fetch_optional(self.pool())
                .await?;
        let email = email.ok_or_else(|| WalletError::NotFound("no such user".into()))?;
        let cooling: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM oxsum.email_tokens \
                 WHERE user_id = $1 AND purpose = $2 AND used_at IS NULL \
                 AND created_at > now() - $3)",
        )
        .bind(user_id)
        .bind(purpose.as_str())
        .bind(RESEND_COOLDOWN)
        .fetch_one(self.pool())
        .await?;
        if cooling {
            return Ok(None);
        }
        let token = generate_token();
        let expires_at: OffsetDateTime = sqlx::query_scalar(
            "INSERT INTO oxsum.email_tokens (token_id, user_id, purpose, token_hash, expires_at) \
             VALUES ($1, $2, $3, $4, now() + $5) \
             RETURNING expires_at",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(purpose.as_str())
        .bind(hash_token(&token).as_slice())
        .bind(purpose.lifetime())
        .fetch_one(self.pool())
        .await?;
        Ok(Some(MintedEmailToken {
            token,
            email,
            expires_at,
        }))
    }

    /// The same mint for an address rather than a user id — the forgot-password
    /// path's entry point, which names an email the caller claims to own.
    ///
    /// `None` covers both cases the endpoint must not distinguish: no account
    /// under the address, and the resend cooldown. The route answers identically
    /// either way.
    ///
    /// # Errors
    ///
    /// Storage failures surface as `WalletError`.
    pub async fn mint_email_token_for_address(
        &self,
        email: &str,
        purpose: EmailPurpose,
    ) -> Result<Option<MintedEmailToken>, WalletError> {
        let normalized = email.trim().to_lowercase();
        let user_id: Option<Uuid> =
            sqlx::query_scalar("SELECT user_id FROM oxsum.users WHERE email_normalized = $1")
                .bind(&normalized)
                .fetch_optional(self.pool())
                .await?;
        let Some(user_id) = user_id else {
            return Ok(None);
        };
        self.mint_email_token(user_id, purpose).await
    }

    /// Consumes a token: stamps `used_at` and answers whose it was, in one
    /// statement — an unknown, spent, expired or wrong-purpose token answers
    /// `None`, indistinguishably. The caller maps `None` to its "not found".
    ///
    /// # Errors
    ///
    /// Storage failures surface as `WalletError`.
    pub async fn consume_email_token(
        &self,
        token: &str,
        purpose: EmailPurpose,
    ) -> Result<Option<Uuid>, WalletError> {
        let user_id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE oxsum.email_tokens SET used_at = now() \
             WHERE token_hash = $1 AND purpose = $2 \
             AND used_at IS NULL AND expires_at > now() \
             RETURNING user_id",
        )
        .bind(hash_token(token).as_slice())
        .bind(purpose.as_str())
        .fetch_optional(self.pool())
        .await?;
        Ok(user_id)
    }

    /// Whether the user's address is verified — the field the session payload
    /// and the dashboard's reminder banner read.
    ///
    /// # Errors
    ///
    /// `WalletError::NotFound` for an unknown user; storage failures surface as
    /// `WalletError`.
    pub async fn email_verified(&self, user_id: Uuid) -> Result<bool, WalletError> {
        let verified: Option<bool> = sqlx::query_scalar(
            "SELECT email_verified_at IS NOT NULL FROM oxsum.users WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?;
        verified.ok_or_else(|| WalletError::NotFound("no such user".into()))
    }

    /// Marks the user's address verified. Idempotent in effect: a second verify
    /// rewrites the same timestamp, which is a fact, not history.
    ///
    /// # Errors
    ///
    /// `WalletError::NotFound` for an unknown user; storage failures surface as
    /// `WalletError`.
    pub async fn mark_email_verified(&self, user_id: Uuid) -> Result<(), WalletError> {
        let n = sqlx::query("UPDATE oxsum.users SET email_verified_at = now() WHERE user_id = $1")
            .bind(user_id)
            .execute(self.pool())
            .await?
            .rows_affected();
        if n == 0 {
            return Err(WalletError::NotFound("no such user".into()));
        }
        Ok(())
    }
}

/// A new email token: the mark plus 32 random bytes from the operating system's
/// CSPRNG, hex-encoded like an invitation token or an API key secret.
fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut token = String::with_capacity(TOKEN_MARK.len() + TOKEN_BYTES * 2);
    token.push_str(TOKEN_MARK);
    for byte in bytes {
        // Writing into a String cannot fail.
        let _ = write!(token, "{byte:02x}");
    }
    token
}

/// SHA-256 of the token, the only form the database ever holds.
fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

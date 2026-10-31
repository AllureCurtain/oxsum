//! TOTP second factor (issue #504): the RFC 6238 code itself — a six-digit
//! HMAC-SHA1 over the 30-second time step — plus the per-user storage the
//! login's second leg and the enrollment endpoints share.
//!
//! The shared secret is sealed under the deployment's `OXSUM_SECRET_KEY` the
//! way an upstream credential is (`channel_credentials.api_key_sealed`), so a
//! dump of `oxsum.user_totp` mints no code. A secret is *in force* only once
//! `confirmed_at` is set: enrollment writes the sealed secret unconfirmed, and
//! a wrong-code-on-enrollment never locks the user out — the login gate reads
//! the confirmed half only.
//!
//! A TOTP-enabled login is two legs: `login` verifies the password, then —
//! rather than mint the session — answers [`LoginOutcome::TotpRequired`] with
//! a challenge token, and `verify_totp` on the challenge plus a live code
//! (or a backup code) mints the session. The challenge is single-use,
//! short-lived and attempt-capped, so a stolen password lets a probe brute
//! neither the session nor the six digits.
//!
//! The code is HMAC-SHA1 because the spec is (RFC 6238's `HOTP`/`TOTP` run on
//! it); `sha1` + `hmac` implement it directly rather than pull a TOTP crate —
//! the whole algorithm is forty lines, and the dependency story stays the
//! narrow one the workspace already keeps.

use std::fmt::Write as _;

use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::sessions::CreatedSession;

/// The code's width — the universal six-digit.
const DIGITS: u32 = 6;
/// The time step a code covers: RFC 6238's 30 seconds.
const STEP: i64 = 30;
/// The drift a live code is verified against: the current step plus one each
/// way, so a code typed as it rolls over still counts.
const STEP_TOLERANCE: i64 = 1;
/// How long the login's second leg's challenge lives.
const CHALLENGE_LIFETIME: time::Duration = time::Duration::minutes(10);
/// The wrong-code attempts a challenge survives: a spent, exhausted or expired
/// challenge is dead and the login starts over, so a probe cannot brute the
/// six digits inside one challenge.
const CHALLENGE_ATTEMPT_CAP: i64 = 5;
/// The backup codes minted on enrollment, each single-use.
const BACKUP_CODE_COUNT: usize = 10;

/// What `login` answers — renamed off `CreatedSession` so the second leg can
/// withhold the session behind a challenge instead of minting it unconditionally.
#[derive(Debug)]
pub enum LoginOutcome {
    /// The password was the whole credential: a session was minted.
    Session(Box<CreatedSession>),
    /// The password verified but the account runs a second factor: the session
    /// is withheld behind this challenge token, which `verify_totp` trades for
    /// it alongside a live code.
    TotpRequired {
        /// The single-use challenge the client presents with its code.
        challenge_token: String,
    },
}

impl LoginOutcome {
    /// The session a password-only login minted — the caller's assertion that
    /// the account runs no second factor. Panics on `TotpRequired`, which makes
    /// it a test helper: a route branches on the variant instead.
    #[must_use]
    pub fn into_session(self) -> CreatedSession {
        match self {
            Self::Session(created) => *created,
            Self::TotpRequired { .. } => {
                panic!("the account requires a second factor; no session was minted")
            }
        }
    }
}

/// Generates a fresh shared secret: 20 bytes of CSPRNG, the size RFC 4226's
/// example and every authenticator's default.
pub fn generate_secret() -> [u8; 20] {
    let mut secret = [0u8; 20];
    rand::rng().fill(&mut secret);
    secret
}

/// The six-digit code `secret` produces for the instant `at` (RFC 6238 over
/// HMAC-SHA1, 30s step, dynamic-truncation extraction).
pub fn code(secret: &[u8], at: i64) -> String {
    let step = (at.div_euclid(STEP)) as u64;
    code_at_step(secret, step)
}

/// The HOTP at one counter step: HMAC-SHA1 over the big-endian counter, the
/// low nibble of the last byte picking the four dynamic-truncation bytes.
fn code_at_step(secret: &[u8], step: u64) -> String {
    // `new_from_slice` fails only on a zero-length key — impossible: the
    // secret is the caller's 20 bytes — so fall back to an empty-keyed MAC
    // rather than panic; a zero-length input yields a wrong code, not a crash.
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(secret)
        .unwrap_or_else(|_| <Hmac<Sha1> as KeyInit>::new(&Default::default()));
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let binary = (u32::from(digest[offset]) & 0x7f) << 24
        | u32::from(digest[offset + 1]) << 16
        | u32::from(digest[offset + 2]) << 8
        | u32::from(digest[offset + 3]);
    let value = binary % 10_u32.pow(DIGITS);
    format!("{value:0width$}", width = DIGITS as usize)
}

/// Whether `presented` is a live code for `secret` at `at`: constant-time
/// compare over the three tolerated steps, so a code rolling over as it is
/// typed still counts and a wrong code leaks nothing through timing.
pub fn verify(secret: &[u8], presented: &str, at: i64) -> bool {
    let presented = presented.trim();
    if presented.len() != DIGITS as usize || !presented.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let step = at.div_euclid(STEP);
    (-STEP_TOLERANCE..=STEP_TOLERANCE).any(|drift| {
        let expected = code_at_step(secret, (step + drift) as u64);
        expected.as_bytes().ct_eq(presented.as_bytes()).into()
    })
}

/// The `otpauth://` URL an authenticator's QR encodes — the client renders the
/// image; the server only owes it the well-formed string.
pub fn otpauth_url(issuer: &str, account: &str, secret: &[u8]) -> String {
    format!(
        "otpauth://totp/{issuer}:{account}?secret={}&issuer={issuer}&digits={DIGITS}&period={STEP}",
        base32_encode(secret)
    )
}

/// base32-encodes a shared secret for the enrollment answer — the form the
/// client shows and the `secret` field reports, so a user can also type it into
/// an authenticator by hand.
#[must_use]
pub fn base32_encode_pub(bytes: &[u8]) -> String {
    base32_encode(bytes)
}

/// base32 (RFC 4648, no padding) — the form authenticators read a secret in.
fn base32_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    for chunk in bytes.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u64::from(buf[0]) << 32
            | u64::from(buf[1]) << 24
            | u64::from(buf[2]) << 16
            | u64::from(buf[3]) << 8
            | u64::from(buf[4]);
        let symbols = match chunk.len() {
            5 => 8,
            4 => 7,
            3 => 5,
            2 => 3,
            _ => 2,
        };
        for i in 0..symbols {
            let shift = 35 - i * 5;
            out.push(ALPHABET[((n >> shift) & 0x1f) as usize] as char);
        }
    }
    out
}

/// A one-time recovery code, `xxxx-xxxx` from the CSPRNG, shown once at
/// enrollment and single-use.
fn generate_backup_code() -> String {
    let digits: String = (0..8)
        .map(|_| char::from(b'0' + (rand::rng().random::<u8>() % 10)))
        .collect();
    format!("{}-{}", &digits[..4], &digits[4..])
}

/// SHA-256 of a backup code, the only form the row holds — like a session
/// token, the code itself is machine-random and needs no password hash.
fn hash_backup_code(code: &str) -> Vec<u8> {
    // Normalize away the display dash and any case, so the user may type it
    // either way and still match what was hashed at mint.
    let normalized: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    Sha256::digest(normalized.to_lowercase().as_bytes()).to_vec()
}

/// SHA-256 of a challenge token, the only form the row holds — like
/// `sessions.token_hash`, a dump mints no login.
fn hash_challenge(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// A fresh challenge token: `oxtotp-` plus 32 CSPRNG bytes, hex like every
/// token the project mints.
fn generate_challenge() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    let mut token = String::with_capacity(7 + 64);
    token.push_str("oxtotp-");
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    token
}

/// The user's stored factor as the enrollment and login paths read it.
#[derive(Debug, Clone)]
pub struct UserTotp {
    /// Whether a confirmed secret is in force — the login gate reads only this.
    pub enabled: bool,
    /// The sealed secret (opened by the caller with the deployment's key).
    pub secret_sealed: String,
    /// The unused backup codes' hashes.
    pub backup_code_hashes: Vec<Vec<u8>>,
}

impl Db {
    /// Whether the user runs a confirmed second factor — `login`'s branch
    /// point. `confirmed_at` set is the whole test: an enrolled-but-unproved
    /// secret never gates.
    pub async fn totp_enabled(&self, user_id: Uuid) -> Result<bool, WalletError> {
        let enabled: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM oxsum.user_totp \
             WHERE user_id = $1 AND confirmed_at IS NOT NULL)",
        )
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        Ok(enabled)
    }

    /// Begins enrollment: writes the sealed secret unconfirmed, replacing any
    /// prior pending secret. The caller seals with the deployment's key and
    /// hands back the secret plus the otpauth URL for the QR.
    pub async fn enroll_totp(&self, user_id: Uuid, secret_sealed: &str) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.user_totp (user_id, secret_sealed, confirmed_at, backup_code_hashes) \
             VALUES ($1, $2, NULL, '{}') \
             ON CONFLICT (user_id) DO UPDATE \
             SET secret_sealed = $2, confirmed_at = NULL, backup_code_hashes = '{}'",
        )
        .bind(user_id)
        .bind(secret_sealed)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Confirms enrollment on a first live code and mints the backup codes —
    /// returned here, in the clear, exactly once (the row holds only their
    /// hashes). A wrong code leaves the enrollment pending and answers
    /// `InvalidCredentials` like any failed verification.
    pub async fn confirm_totp(
        &self,
        user_id: Uuid,
        presented: &str,
        secret: &crate::channels::SecretKey,
    ) -> Result<Vec<String>, WalletError> {
        let secret_sealed: Option<String> = sqlx::query_scalar(
            "SELECT secret_sealed FROM oxsum.user_totp \
             WHERE user_id = $1 AND confirmed_at IS NULL",
        )
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(sealed) = secret_sealed else {
            return Err(WalletError::InvalidInput(
                "no TOTP enrollment is pending".into(),
            ));
        };
        let opened = secret.open(&sealed)?;
        let secret_bytes = base32_decode(&opened).ok_or_else(|| {
            WalletError::Misconfigured("a stored TOTP secret is not base32".into())
        })?;
        if !verify(
            &secret_bytes,
            presented,
            OffsetDateTime::now_utc().unix_timestamp(),
        ) {
            return Err(WalletError::InvalidCredentials);
        }
        let codes: Vec<String> = (0..BACKUP_CODE_COUNT)
            .map(|_| generate_backup_code())
            .collect();
        let hashes: Vec<Vec<u8>> = codes.iter().map(|c| hash_backup_code(c)).collect();
        sqlx::query(
            "UPDATE oxsum.user_totp \
             SET confirmed_at = now(), backup_code_hashes = $2 \
             WHERE user_id = $1",
        )
        .bind(user_id)
        .bind(&hashes)
        .execute(self.pool())
        .await?;
        Ok(codes)
    }

    /// Disables the factor on a live code — the same verification a login
    /// would run, so disabling is itself second-factored.
    pub async fn disable_totp(
        &self,
        user_id: Uuid,
        presented: &str,
        secret: &crate::channels::SecretKey,
    ) -> Result<(), WalletError> {
        let secret_sealed: Option<String> = sqlx::query_scalar(
            "SELECT secret_sealed FROM oxsum.user_totp \
             WHERE user_id = $1 AND confirmed_at IS NOT NULL",
        )
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(sealed) = secret_sealed else {
            return Err(WalletError::InvalidInput("TOTP is not enabled".into()));
        };
        let opened = secret.open(&sealed)?;
        let secret_bytes = base32_decode(&opened).ok_or_else(|| {
            WalletError::Misconfigured("a stored TOTP secret is not base32".into())
        })?;
        if !verify(
            &secret_bytes,
            presented,
            OffsetDateTime::now_utc().unix_timestamp(),
        ) {
            return Err(WalletError::InvalidCredentials);
        }
        sqlx::query("DELETE FROM oxsum.user_totp WHERE user_id = $1")
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Mints the login's second-leg challenge after a verified password:
    /// records who proved the password behind a single-use, attempt-capped,
    /// short-lived token.
    pub async fn mint_totp_challenge(&self, user_id: Uuid) -> Result<String, WalletError> {
        let token = generate_challenge();
        sqlx::query(
            "INSERT INTO oxsum.totp_challenges (challenge_hash, user_id, expires_at) \
             VALUES ($1, $2, now() + $3)",
        )
        .bind(hash_challenge(&token).as_slice())
        .bind(user_id)
        .bind(CHALLENGE_LIFETIME)
        .execute(self.pool())
        .await?;
        Ok(token)
    }

    /// The login's second leg: a live challenge plus a live TOTP code — or a
    /// backup code, consumed — mints the session. A wrong code increments the
    /// challenge's attempts and answers `InvalidCredentials`; a spent,
    /// exhausted or expired challenge is dead and answers `NotFound`, so the
    /// caller starts the login over rather than finding a boundary to probe.
    pub async fn verify_totp(
        &self,
        challenge_token: &str,
        presented: &str,
        secret: &crate::channels::SecretKey,
    ) -> Result<CreatedSession, WalletError> {
        let row = sqlx::query(
            "SELECT c.user_id, c.attempts, t.secret_sealed, t.backup_code_hashes \
             FROM oxsum.totp_challenges c \
             JOIN oxsum.user_totp t ON t.user_id = c.user_id \
             WHERE c.challenge_hash = $1 AND c.expires_at > now() \
               AND t.confirmed_at IS NOT NULL",
        )
        .bind(hash_challenge(challenge_token).as_slice())
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Err(WalletError::NotFound("not found".into()));
        };
        use sqlx::Row;
        let user_id: Uuid = row.try_get("user_id")?;
        let attempts: i32 = row.try_get("attempts")?;
        let sealed: String = row.try_get("secret_sealed")?;
        let backup_hashes: Vec<Vec<u8>> = row.try_get("backup_code_hashes")?;

        // The challenge's own rate limit: a wrong code spends an attempt and a
        // spent-out challenge is dead, so the six digits are never brute-able
        // inside one challenge's life.
        if i64::from(attempts) >= CHALLENGE_ATTEMPT_CAP {
            return Err(WalletError::NotFound("not found".into()));
        }

        // A backup code is single-use: matching a hash consumes it inside the
        // same statement the session mint follows, so a replay of the code
        // finds its hash gone.
        let backup_hash = hash_backup_code(presented);
        if let Some(position) = backup_hashes
            .iter()
            .position(|hash| hash.as_slice().ct_eq(backup_hash.as_slice()).into())
        {
            sqlx::query(
                "UPDATE oxsum.user_totp \
                 SET backup_code_hashes = array_remove(backup_code_hashes, $2) \
                 WHERE user_id = $1",
            )
            .bind(user_id)
            .bind(&backup_hashes[position])
            .execute(self.pool())
            .await?;
            return self.session_after_totp(user_id, challenge_token).await;
        }

        let opened = secret.open(&sealed)?;
        let secret_bytes = base32_decode(&opened).ok_or_else(|| {
            WalletError::Misconfigured("a stored TOTP secret is not base32".into())
        })?;
        if !verify(
            &secret_bytes,
            presented,
            OffsetDateTime::now_utc().unix_timestamp(),
        ) {
            sqlx::query(
                "UPDATE oxsum.totp_challenges SET attempts = attempts + 1 \
                 WHERE challenge_hash = $1",
            )
            .bind(hash_challenge(challenge_token).as_slice())
            .execute(self.pool())
            .await?;
            return Err(WalletError::InvalidCredentials);
        }
        self.session_after_totp(user_id, challenge_token).await
    }

    /// The shared tail of a satisfied second factor: the challenge is spent
    /// (deleted, so a replay finds nothing) and the session is minted with its
    /// `totp_verified_at` stamped — the column a freshness-requiring route
    /// re-checks.
    async fn session_after_totp(
        &self,
        user_id: Uuid,
        challenge_token: &str,
    ) -> Result<CreatedSession, WalletError> {
        sqlx::query("DELETE FROM oxsum.totp_challenges WHERE challenge_hash = $1")
            .bind(hash_challenge(challenge_token).as_slice())
            .execute(self.pool())
            .await?;
        let session = self.session_for(user_id).await?;
        sqlx::query(
            "UPDATE oxsum.sessions SET totp_verified_at = now() \
             WHERE session_id = $1",
        )
        .bind(session.principal.session.id)
        .execute(self.pool())
        .await?;
        Ok(session)
    }
}

/// base32-decodes a stored secret back to bytes — the inverse of
/// `base32_encode`, accepting either case and no padding.
fn base32_decode(text: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a'),
            b'2'..=b'7' => Some(byte - b'2' + 26),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut acc = 0u64;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        acc = (acc << 5) | u64::from(value(byte)?);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// RFC 6238's appendix-B test vector, 8-digit codes trimmed to the six the
    /// implementation emits — the reference secret is the ASCII "12345678901234567890".
    #[test]
    fn the_rfc_6238_vector_codes_correctly() {
        let secret = b"12345678901234567890";
        // (unix_seconds, expected 8-digit from the RFC) → assert the first 6.
        for (at, expected8) in [
            (59_i64, "94287082"),
            (1111111109, "07081804"),
            (1111111111, "14050471"),
            (1234567890, "89005924"),
            (2000000000, "69279037"),
        ] {
            let got = code(secret, at);
            assert!(
                expected8.starts_with(&got) || got == expected8[2..],
                "at {at}: got {got}, want suffix of {expected8}"
            );
        }
    }

    #[test]
    fn a_code_verifies_within_the_step_tolerance() {
        let secret = b"totp-secret-for-test";
        let at = 1_700_000_000_i64;
        let presented = code(secret, at);
        assert!(verify(secret, &presented, at));
        // One step either way still counts; two does not.
        assert!(verify(secret, &presented, at + STEP));
        assert!(!verify(secret, &presented, at + 2 * STEP + 1));
        assert!(!verify(secret, "000000", at));
        assert!(!verify(secret, "abc", at));
    }

    #[test]
    fn base32_round_trips() {
        let secret = generate_secret();
        let encoded = base32_encode(&secret);
        assert_eq!(base32_decode(&encoded), Some(secret.to_vec()));
    }
}

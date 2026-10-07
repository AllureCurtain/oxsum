//! Device authorization (issue #156, roadmap P6-4): the RFC 8628-flavored grant
//! a CLI or any input-constrained tool uses to receive an API key through a
//! signed-in browser session, without a secret ever crossing a terminal.
//!
//! Two codes share one row in `oxsum.device_codes` (migration 0021). The tool
//! holds the `oxd-` device code and polls; the user types the short user code
//! at the approval page. Both are SHA-256 at rest, like every token the project
//! mints, and both lapse fifteen minutes after minting.
//!
//! The API key is minted inside the successful poll's transaction — an approval
//! records only who approved for which organization, so a secret is never at
//! rest waiting for a poll that may never come, and a code nobody polls grants
//! nothing.

use std::fmt::Write;

use rand::{Rng, RngExt};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::keys::{self, CreatedApiKey, KeyConstraints};

/// Every device code starts with this, so the value is recognizable in logs.
const DEVICE_MARK: &str = "oxd-";
const DEVICE_BYTES: usize = 32;

/// The grant's whole lifetime: long enough to open a browser and type eight
/// letters, short enough that a leaked code is stale before it is useful.
const CODE_LIFETIME: Duration = Duration::minutes(15);

/// The minimum gap between token polls — the `interval` the mint response
/// publishes and the poll leg enforces.
pub const POLL_INTERVAL: Duration = Duration::seconds(5);

/// The user code's alphabet: no 0/O/1/I/L, so nothing a user types is
/// ambiguous.
const USER_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const USER_CODE_LEN: usize = 8;

/// What `POST /api/v1/device/code` mints: both halves the grant runs on.
#[derive(Debug)]
pub struct DeviceGrant {
    /// The tool's polling credential — answered once, here.
    pub device_code: String,
    /// What the user types at the approval page, `XXXX-XXXX`.
    pub user_code: String,
    /// Both codes lapse at this instant.
    pub expires_at: OffsetDateTime,
}

/// The pending request the approval page reads back: when it was minted and
/// when it lapses. Approved/denied state is not exposed — the lookup only ever
/// sees pending rows, everything else is `NotFound`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRequest {
    /// The code as the tool displayed it, `XXXX-XXXX`.
    pub user_code: String,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

/// A poll's answer.
#[derive(Debug)]
pub enum DevicePoll {
    /// The user has not decided yet; poll again after `interval`.
    Pending,
    /// The user refused the request — terminal.
    Denied,
    /// The key was already delivered to an earlier poll — terminal.
    Consumed,
    /// Approved, and the key minted in this poll's transaction: its secret is
    /// answered once, here, and is never stored.
    Delivered(Box<CreatedApiKey>),
}

/// Why a poll failed, kept separate from [`WalletError`] because the two
/// expected failures are part of the grant's contract, not wallet failures:
/// `TooFast` maps to the API's rate-limit answer with its own `Retry-After`.
#[derive(Debug)]
pub enum PollError {
    /// Unknown, expired or already-gone — one answer for all three.
    NotFound,
    /// Polled inside `interval`; the field is seconds until the next legal poll.
    TooFast {
        retry_after_secs: u64,
    },
    Wallet(WalletError),
}

impl From<WalletError> for PollError {
    fn from(e: WalletError) -> Self {
        Self::Wallet(e)
    }
}

impl From<sqlx::Error> for PollError {
    fn from(e: sqlx::Error) -> Self {
        Self::Wallet(e.into())
    }
}

impl Db {
    /// Mints a device request: a fresh device code and user code, both hashed at
    /// rest, expiring together fifteen minutes out. The user code's unique index
    /// can collide — the alphabet is large but finite — so the mint retries it
    /// once; a second collision is a real error, not a guessable state.
    pub async fn mint_device_request(&self) -> Result<DeviceGrant, WalletError> {
        for _ in 0..2 {
            let device_code = generate_device_code();
            let user_code = generate_user_code();
            let expires_at = OffsetDateTime::now_utc() + CODE_LIFETIME;
            let res = sqlx::query(
                "INSERT INTO oxsum.device_codes \
                     (device_code_hash, user_code_hash, user_code, expires_at) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(hash_token(&device_code))
            .bind(hash_user_code(&user_code))
            .bind(&user_code)
            .bind(expires_at)
            .execute(self.pool())
            .await;
            match res {
                Ok(_) => {
                    return Ok(DeviceGrant {
                        device_code,
                        user_code,
                        expires_at,
                    });
                }
                Err(e) if is_unique_violation(&e) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(WalletError::Misconfigured(
            "device code minting collided twice".into(),
        ))
    }

    /// The pending request a user code names, for the approval page's confirm
    /// step. Expired, decided and unknown codes all answer `NotFound` — the
    /// page cannot tell them apart and neither can a guesser.
    pub async fn device_request(&self, user_code: &str) -> Result<DeviceRequest, WalletError> {
        let Some(normalized) = normalize_user_code(user_code) else {
            return Err(WalletError::NotFound("device request not found".into()));
        };
        let row = sqlx::query(
            "SELECT user_code, created_at, expires_at FROM oxsum.device_codes \
             WHERE user_code_hash = $1 AND status = 'pending' AND expires_at > now()",
        )
        .bind(hash_user_code_normalized(&normalized))
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| device_request_from_row(&row))
            .transpose()?
            .ok_or_else(|| WalletError::NotFound("device request not found".into()))
    }

    /// The signed-in user's verdict on the request `user_code` names:
    /// approved stamps the approver and the organization the key will mint
    /// into, denied is terminal. The stamp is one statement guarded on
    /// pending-and-live, so a raced second verdict — like a spent code — is
    /// `NotFound`.
    pub async fn decide_device_request(
        &self,
        user_code: &str,
        approve: bool,
        approved_by: Uuid,
        organization_id: Uuid,
    ) -> Result<DeviceRequest, WalletError> {
        let Some(normalized) = normalize_user_code(user_code) else {
            return Err(WalletError::NotFound("device request not found".into()));
        };
        let status = if approve { "approved" } else { "denied" };
        let row = sqlx::query(
            "UPDATE oxsum.device_codes \
             SET status = $2, approved_by = $3, organization_id = $4 \
             WHERE user_code_hash = $1 AND status = 'pending' AND expires_at > now() \
             RETURNING user_code, created_at, expires_at",
        )
        .bind(hash_user_code_normalized(&normalized))
        .bind(status)
        .bind(approved_by)
        .bind(organization_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| device_request_from_row(&row))
            .transpose()?
            .ok_or_else(|| WalletError::NotFound("device request not found".into()))
    }

    /// The tool's poll. The row is locked `FOR UPDATE` so a verdict and a poll
    /// can never interleave, and an approved request mints its key inside this
    /// transaction — the secret exists in memory, is answered once, and only
    /// its SHA-256 survives in `api_keys`. A repeated poll on a delivered code
    /// is `Consumed`; polling inside `interval` is `TooFast` and does not count
    /// as a visit.
    pub async fn poll_device(&self, device_code: &str) -> Result<DevicePoll, PollError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT status, expires_at, last_poll_at, approved_by, organization_id, user_code \
             FROM oxsum.device_codes WHERE device_code_hash = $1 FOR UPDATE",
        )
        .bind(hash_token(device_code))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Err(PollError::NotFound);
        };
        use sqlx::Row;
        let expires_at: OffsetDateTime = row.try_get("expires_at")?;
        let status: String = row.try_get("status")?;
        let now = OffsetDateTime::now_utc();
        if expires_at <= now {
            // The row stays — nothing deletes history — but it answers like a
            // code that never was.
            return Err(PollError::NotFound);
        }
        if let Some(last) = row.try_get::<Option<OffsetDateTime>, _>("last_poll_at")?
            && now - last < POLL_INTERVAL
        {
            let remaining = (POLL_INTERVAL - (now - last)).whole_seconds().max(1) as u64;
            return Err(PollError::TooFast {
                retry_after_secs: remaining,
            });
        }
        sqlx::query(
            "UPDATE oxsum.device_codes SET last_poll_at = now() \
             WHERE device_code_hash = $1",
        )
        .bind(hash_token(device_code))
        .execute(&mut *tx)
        .await?;
        let outcome = match status.as_str() {
            "pending" => DevicePoll::Pending,
            "denied" => DevicePoll::Denied,
            "delivered" => DevicePoll::Consumed,
            "approved" => {
                let approved_by: Uuid =
                    row.try_get::<Option<Uuid>, _>("approved_by")?
                        .ok_or_else(|| {
                            WalletError::Misconfigured("approved without approver".into())
                        })?;
                let organization_id: Uuid = row
                    .try_get::<Option<Uuid>, _>("organization_id")?
                    .ok_or_else(|| {
                        WalletError::Misconfigured("approved without organization".into())
                    })?;
                let user_code: String = row.try_get("user_code")?;
                let key = keys::insert(
                    &mut tx,
                    organization_id,
                    Some(format!("device {user_code}")),
                    None,
                    Some(approved_by),
                    KeyConstraints::default(),
                )
                .await?;
                sqlx::query(
                    "UPDATE oxsum.device_codes SET status = 'delivered' \
                     WHERE device_code_hash = $1",
                )
                .bind(hash_token(device_code))
                .execute(&mut *tx)
                .await?;
                DevicePoll::Delivered(Box::new(key))
            }
            other => {
                return Err(
                    WalletError::Misconfigured(format!("unknown device status {other}")).into(),
                );
            }
        };
        tx.commit().await?;
        Ok(outcome)
    }
}

fn device_request_from_row(row: &sqlx::postgres::PgRow) -> Result<DeviceRequest, WalletError> {
    use sqlx::Row;
    Ok(DeviceRequest {
        user_code: row.try_get("user_code")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
    })
}

/// `oxd-` plus 32 random bytes from the operating system's CSPRNG, hex-encoded
/// like every token the project mints.
fn generate_device_code() -> String {
    let mut bytes = [0u8; DEVICE_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let mut code = String::with_capacity(DEVICE_MARK.len() + DEVICE_BYTES * 2);
    code.push_str(DEVICE_MARK);
    for byte in bytes {
        let _ = write!(code, "{byte:02x}");
    }
    code
}

/// Eight letters from the unambiguous alphabet, `XXXX-XXXX`.
fn generate_user_code() -> String {
    let mut rng = rand::rng();
    let mut raw = String::with_capacity(USER_CODE_LEN);
    for _ in 0..USER_CODE_LEN {
        raw.push(USER_CODE_ALPHABET[rng.random_range(0..USER_CODE_ALPHABET.len())] as char);
    }
    format_user_code(&raw)
}

/// `XXXX-XXXX` — the raw eight letters with the dash the user sees.
fn format_user_code(raw: &str) -> String {
    if raw.len() == USER_CODE_LEN {
        format!("{}-{}", &raw[..4], &raw[4..])
    } else {
        raw.to_owned()
    }
}

/// Uppercase alphanumeric, dashes and spaces stripped — what the user typed
/// and what the mint generated agree on before hashing. `None` when nothing
/// eight-letter survives.
fn normalize_user_code(code: &str) -> Option<String> {
    let normalized: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (normalized.len() == USER_CODE_LEN).then_some(normalized)
}

/// SHA-256 of the normalized user code.
fn hash_user_code(user_code: &str) -> Vec<u8> {
    let normalized = normalize_user_code(user_code).unwrap_or_else(|| user_code.to_owned());
    hash_user_code_normalized(&normalized)
}

fn hash_user_code_normalized(normalized: &str) -> Vec<u8> {
    Sha256::digest(normalized.as_bytes()).to_vec()
}

/// SHA-256 of the device code, the only form the database ever holds.
fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(|d| d.code().as_deref() == Some("23505"))
}

//! Request-level idempotency on the gateway (issue #132, roadmap P3-5).
//!
//! A client that times out and retries must never produce a second hold or charge. The
//! gateway's `/api/v1` sibling endpoints already carry an `idempotencyKey` through the
//! ledger's own gate; `POST /v1/chat/completions` cannot, because the request id and the
//! hold key are minted inside the handler. The `Idempotency-Key` header closes that gap
//! with the Stripe shape: the first request claims `(organization, key)`, a retry with
//! the same request fingerprint replays the stored answer, a different fingerprint is
//! refused 422, and a retry that lands while the first turn is still running is refused
//! 409. A completed streamed turn cannot replay its bytes, so it answers the settled
//! receipt instead — the record is completed where the usage row lands, which is also
//! where the sweeper's settlement lands, so a crashed process still resolves the claim.
//!
//! A refusal that never reached the wallet releases the claim rather than recording it:
//! nothing was billed, so a corrected retry should run, not replay an old refusal.

use doubleentry::Hash;
use serde_json::Value;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;

/// What makes two requests under one key "the same": the body's bytes, hashed.
/// Byte-exactness is the safe direction — a re-encoded body is a different
/// request rather than a silently deduplicated one.
#[must_use]
pub fn fingerprint(body: &[u8]) -> String {
    Hash::digest(b"oxsum/idempotency/v1", body).to_hex()
}

/// How long a claimed key is kept, matching the retention the decisions record fixes.
const CLAIM_TTL: &str = "24 hours";

/// The longest key the surface accepts, matching the contract's `maxLength`.
const MAX_KEY_LEN: usize = 255;

/// What claiming an idempotency key decided.
#[derive(Debug)]
pub enum Claim {
    /// The claim is fresh: run the turn under this request id.
    Fresh { request_id: String },
    /// The first request is still running.
    InFlight,
    /// The key was claimed under a different request body.
    Mismatch,
    /// The first request finished: replay its stored answer under its own request id.
    Replay {
        request_id: String,
        status: i32,
        body: Value,
    },
}

struct Record {
    request_fingerprint: String,
    request_id: String,
    status: String,
    response_status: Option<i32>,
    response_body: Option<Value>,
    expired: bool,
}

impl Db {
    /// Claims `key` for `organization`, or answers what a retry should do.
    ///
    /// An expired record is released lazily and the claim retried, so a key is usable
    /// again after its 24-hour life without waiting on a cleanup job.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] on an empty or over-long key; storage failures
    /// surface as [`WalletError`].
    pub async fn claim_request(
        &self,
        organization: Uuid,
        key: &str,
        fingerprint: &str,
        request_id: &str,
    ) -> Result<Claim, WalletError> {
        if key.is_empty() || key.len() > MAX_KEY_LEN {
            return Err(WalletError::InvalidInput(format!(
                "an idempotency key is 1..={MAX_KEY_LEN} characters"
            )));
        }
        for _ in 0..2 {
            let claimed = sqlx::query_scalar::<_, String>(
                "INSERT INTO oxsum.idempotency_records \
                 (organization_id, idempotency_key, request_fingerprint, request_id, \
                  status, expires_at) \
                 VALUES ($1, $2, $3, $4, 'in_flight', now() + $5::interval) \
                 ON CONFLICT (organization_id, idempotency_key) DO NOTHING \
                 RETURNING request_id",
            )
            .bind(organization)
            .bind(key)
            .bind(fingerprint)
            .bind(request_id)
            .bind(CLAIM_TTL)
            .fetch_optional(self.pool())
            .await?;
            if claimed.is_some() {
                return Ok(Claim::Fresh {
                    request_id: request_id.to_owned(),
                });
            }
            let Some(record) = self.record(organization, key).await? else {
                // The row disappeared between the insert's conflict and the read:
                // a released claim. Trying again once answers it.
                continue;
            };
            if record.expired {
                sqlx::query(
                    "DELETE FROM oxsum.idempotency_records \
                     WHERE organization_id = $1 AND idempotency_key = $2 \
                       AND expires_at < now()",
                )
                .bind(organization)
                .bind(key)
                .execute(self.pool())
                .await?;
                continue;
            }
            if record.request_fingerprint != fingerprint {
                return Ok(Claim::Mismatch);
            }
            if record.status == "in_flight" {
                return Ok(Claim::InFlight);
            }
            return Ok(Claim::Replay {
                request_id: record.request_id,
                status: record.response_status.unwrap_or(200),
                body: record.response_body.unwrap_or(Value::Null),
            });
        }
        // Two conflicts in a row without landing the claim is a caller racing itself;
        // in-flight is the honest answer either way.
        Ok(Claim::InFlight)
    }

    /// Reads the record a claim collided with.
    async fn record(&self, organization: Uuid, key: &str) -> Result<Option<Record>, WalletError> {
        let row = sqlx::query_as::<_, (String, String, String, Option<i32>, Option<Value>, bool)>(
            "SELECT request_fingerprint, request_id, status, response_status, \
                    response_body, expires_at < now() \
             FROM oxsum.idempotency_records \
             WHERE organization_id = $1 AND idempotency_key = $2",
        )
        .bind(organization)
        .bind(key)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(
            |(request_fingerprint, request_id, status, response_status, response_body, expired)| {
                Record {
                    request_fingerprint,
                    request_id,
                    status,
                    response_status,
                    response_body,
                    expired,
                }
            },
        ))
    }

    /// Stores the answer a retry replays. Called once the turn is over; a stored
    /// receipt is overwritten by the real response when there is one to store.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn complete_request(
        &self,
        organization: Uuid,
        key: &str,
        status: i32,
        body: &Value,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.idempotency_records \
             SET status = 'completed', response_status = $3, response_body = $4 \
             WHERE organization_id = $1 AND idempotency_key = $2",
        )
        .bind(organization)
        .bind(key)
        .bind(status)
        .bind(body)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Releases a claim whose request was refused before money moved: the key
    /// is free to be claimed by the corrected retry.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn release_request(&self, organization: Uuid, key: &str) -> Result<(), WalletError> {
        sqlx::query(
            "DELETE FROM oxsum.idempotency_records \
             WHERE organization_id = $1 AND idempotency_key = $2",
        )
        .bind(organization)
        .bind(key)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

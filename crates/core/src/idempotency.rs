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

/// Bind replay to the submitting key and protocol/resource. Legacy unbound
/// fingerprints mismatch rather than disclose another credential's response.
#[must_use]
pub fn credential_fingerprint(
    key: Uuid,
    surface: &str,
    resource: Option<&str>,
    body: &[u8],
) -> String {
    let value = serde_json::json!([key, surface, resource, fingerprint(body)]);
    Hash::digest(
        b"oxsum/idempotency/credential/v1",
        value.to_string().as_bytes(),
    )
    .to_hex()
}

/// How long a claimed key is kept, matching the retention the decisions record fixes.
const CLAIM_TTL: &str = "24 hours";

/// The longest key the surface accepts, matching the contract's `maxLength`.
const MAX_KEY_LEN: usize = 255;

/// What claiming an idempotency key decided.
#[derive(Debug)]
pub enum Claim {
    /// The claim is fresh: run the turn under this request id.
    Fresh {
        /// The request id the fresh turn runs under.
        request_id: String,
    },
    /// The first request is still running.
    InFlight,
    /// The key was claimed under a different request body.
    Mismatch,
    /// The first request finished: replay its stored answer under its own request id.
    Replay {
        /// The request id the original turn ran under.
        request_id: String,
        /// The stored HTTP status to replay.
        status: i32,
        /// The stored response body to replay.
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
                // A live `in_flight` record means the first turn is still
                // running — refuse the retry 409. But the row can outlive its
                // turn: a crash between claim and hold, or a settled hold whose
                // usage write never landed, leaves `in_flight` until the 24h
                // `expires_at`. The live arbiter is the hold, not the clock —
                // `oxsum.open_holds.request_id` names the same request id, so a
                // record with no open hold under it is safe to take over: the
                // claim is re-pointed at the new request and the retry runs.
                if self.hold_alive(&record.request_id).await? {
                    return Ok(Claim::InFlight);
                }
                if self
                    .retake(organization, key, &record.request_id, request_id)
                    .await?
                {
                    return Ok(Claim::Fresh {
                        request_id: request_id.to_owned(),
                    });
                }
                // A racing retry won the retake — in-flight is the honest answer.
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

    /// Whether the turn an `in_flight` record belongs to still has an open
    /// hold — the live arbiter that separates "still running" from "crashed
    /// mid-flight". `oxsum.open_holds.request_id` carries the same request id.
    async fn hold_alive(&self, request_id: &str) -> Result<bool, WalletError> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM oxsum.open_holds WHERE request_id = $1)",
        )
        .bind(request_id)
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    /// Takes over a stale `in_flight` record whose hold is gone: re-points the
    /// claim at the retry's request id and clears the stale replay payload.
    /// Returns `false` when a racing claim already moved the row.
    ///
    /// The `claimed_at` grace bounds the claim→hold window: a record younger
    /// than it is left alone, because its hold may simply not be written yet —
    /// only a claim that has sat open past the window with no hold is treated
    /// as dead. This keeps a slow-but-live admission from being double-run.
    async fn retake(
        &self,
        organization: Uuid,
        key: &str,
        stale_request_id: &str,
        new_request_id: &str,
    ) -> Result<bool, WalletError> {
        let retaken = sqlx::query_scalar::<_, String>(
            "UPDATE oxsum.idempotency_records \
             SET request_id = $4, status = 'in_flight', \
                 response_status = NULL, response_body = NULL, \
                 claimed_at = now(), expires_at = now() + $5::interval \
             WHERE organization_id = $1 AND idempotency_key = $2 \
               AND status = 'in_flight' AND request_id = $3 \
               AND claimed_at < now() - interval '10 seconds' \
             RETURNING request_id",
        )
        .bind(organization)
        .bind(key)
        .bind(stale_request_id)
        .bind(new_request_id)
        .bind(CLAIM_TTL)
        .fetch_optional(self.pool())
        .await?;
        Ok(retaken.is_some())
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

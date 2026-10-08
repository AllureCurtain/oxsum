//! Outbound webhooks (issue #144, roadmap P5-2).
//!
//! An organization registers an HTTPS endpoint and oxsum posts it a signed event
//! when its money moves. The queue is durable and transactional: a settled turn
//! enqueues one delivery per enabled subscribed endpoint inside `record_usage`'s
//! transaction, so the ledger write and the notification can never diverge — and
//! the usage row's `ON CONFLICT` guard means a replay enqueues nothing twice.
//!
//! Delivery belongs to the server, which polls [`Db::due_deliveries`], signs the
//! stored payload under the endpoint's secret and POSTs it; this layer owns what
//! a webhook *is* — the endpoint records, the queue, the attempt bookkeeping. A
//! delivery retries on a backoff and is marked `failed` once
//! [`MAX_DELIVERY_ATTEMPTS`] run out, mirroring the hold dead-letter's contract:
//! failure is recorded, never silent.

use rand::RngExt;
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::channels::SecretKey;
use crate::db::Db;
use crate::error::WalletError;

/// The one event this build sends: a request's settlement landed — the
/// `request.settled` of the event catalog in docs/decisions.md, opt-in per
/// endpoint because per-request traffic is noisy by nature.
pub const REQUEST_SETTLED: &str = "request.settled";
/// The platform suspended an organization's spend (issue #162).
pub const ORG_SUSPENDED: &str = "org.suspended";

/// Every event name a subscription may carry.
const KNOWN_EVENTS: &[&str] = &[REQUEST_SETTLED, ORG_SUSPENDED];

/// How many times a delivery is attempted before it is marked `failed` — the
/// same budget the hold sweeper gives a settlement before dead-lettering.
pub const MAX_DELIVERY_ATTEMPTS: i32 = 10;

/// How many endpoints one organization may register.
const MAX_ENDPOINTS: i64 = 20;

/// The most recent deliveries the listing answers.
const DELIVERY_PAGE: i64 = 50;

/// The longest endpoint URL the surface accepts.
const MAX_URL_LEN: usize = 500;

/// How many due deliveries the worker takes per pass.
const DUE_BATCH: i64 = 100;

/// How long a `sending` claim lives before another worker may reclaim the row —
/// comfortably longer than the sender's POST timeout, so a slow receiver never
/// overlaps two deliveries.
const CLAIM_LEASE_SECS: i64 = 120;

/// An endpoint as the API presents it: the secret exists only at creation, so a
/// listing shows its last four characters for recognition.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub url: String,
    pub events: Vec<String>,
    pub enabled: bool,
    pub secret_last4: String,
    pub created_at: OffsetDateTime,
}

/// The create response: the endpoint plus its signing secret, shown once.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWebhook {
    #[serde(flatten)]
    pub endpoint: WebhookEndpoint,
    pub secret: String,
}

/// One delivery's record as the API lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub event_type: String,
    pub status: String,
    pub attempts: i32,
    pub next_attempt_at: OffsetDateTime,
    pub response_status: Option<i32>,
    pub last_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub delivered_at: Option<OffsetDateTime>,
    /// The lease fencing token: `record_delivery` accepts the outcome only from
    /// the worker holding the claim this timestamp proves. Internal to the
    /// worker — it is not an API field.
    #[serde(skip)]
    pub claimed_at: Option<OffsetDateTime>,
}

/// A due delivery joined to what the sender needs: the receiver's URL and the
/// sealed secret it signs under. Never serialized — the sealed secret is not an
/// API field.
#[derive(Debug)]
pub struct DueDelivery {
    pub delivery: WebhookDelivery,
    pub url: String,
    /// The sealed signing secret; the sender opens it with `OXSUM_SECRET_KEY`.
    pub secret_sealed: String,
    /// The exact bytes the signature covers and the POST sends.
    pub body: String,
}

/// What one delivery attempt ended in.
#[derive(Debug)]
pub enum DeliveryOutcome {
    /// The receiver answered 2xx; `status` is what it answered.
    Delivered { status: i32 },
    /// The receiver answered, or the attempt failed before an answer. `attempt`
    /// is the number of the attempt just made, so the record knows both the
    /// backoff that follows it and whether the budget ran out.
    AttemptFailed {
        status: Option<i32>,
        error: String,
        attempt: i32,
    },
}

/// A minted signing secret: `whsec-` plus 48 lowercase hex characters, the same
/// spelling distance from an API key's `oxs-` that Stripe's `whsec_` keeps.
fn mint_secret() -> String {
    let mut rng = rand::rng();
    let hex: String = (0..24)
        .map(|_| format!("{:02x}", rng.random::<u8>()))
        .collect();
    format!("whsec-{hex}")
}

/// The payload stored on a delivery and posted to the endpoint — the catalog
/// envelope `{id, type, created_at, org_id, data}` from docs/decisions.md. The
/// envelope is what the signature covers, byte for byte.
#[must_use]
pub fn event_envelope(
    delivery_id: Uuid,
    event_type: &str,
    organization: Uuid,
    data: Value,
) -> Value {
    serde_json::json!({
        "id": delivery_id,
        "type": event_type,
        "created_at": OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Iso8601::DEFAULT)
            .unwrap_or_default(),
        "org_id": organization,
        "data": data,
    })
}

/// The `x-oxsum-signature` header's value: `t=<unix>,v1=<hex>` where `v1` is
/// HMAC-SHA256 of `"{t}.{body}"` under the endpoint's secret — the Stripe
/// convention, so a receiver can verify with a library it already has.
#[must_use]
pub fn signature(secret: &str, timestamp: i64, body: &[u8]) -> String {
    use hmac::{KeyInit, Mac, SimpleHmac};
    use sha2::Sha256;

    // `new_from_slice` only fails for a zero-length key; the `whsec-` prefix
    // rules that out, so the fallback arm is unreachable in practice.
    let mut mac = <SimpleHmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| <SimpleHmac<Sha256> as KeyInit>::new(&Default::default()));
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    let hex: String = tag.iter().map(|b| format!("{b:02x}")).collect();
    format!("t={timestamp},v1={hex}")
}

/// The delay in seconds after attempt number `attempt`: 10s, then 1m, 5m, 15m,
/// 30m, and hourly from there — early retries are quick because a receiver blip
/// is usually short, later ones spaced because a real outage will not be fixed
/// by hammering.
#[must_use]
pub fn retry_delay_secs(attempt: i32) -> i64 {
    match attempt {
        0 | 1 => 10,
        2 => 60,
        3 => 300,
        4 => 900,
        5 => 1800,
        _ => 3600,
    }
}

/// Validates a receiver URL: `https` anywhere, `http` only to localhost — the
/// one place a plaintext webhook is legitimate is the developer's own machine.
fn validate_url(raw: &str) -> Result<String, WalletError> {
    if raw.len() > MAX_URL_LEN {
        return Err(WalletError::InvalidInput(
            "url is at most 500 characters".to_owned(),
        ));
    }
    let url = Url::parse(raw)
        .map_err(|_| WalletError::InvalidInput("url must be an http or https URL".to_owned()))?;
    let localhost = url
        .host_str()
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]"));
    match url.scheme() {
        "https" => Ok(raw.to_owned()),
        "http" if localhost => Ok(raw.to_owned()),
        "http" => Err(WalletError::InvalidInput(
            "url must be https; http is allowed only for localhost".to_owned(),
        )),
        _ => Err(WalletError::InvalidInput(
            "url must be an http or https URL".to_owned(),
        )),
    }
}

fn validate_events(events: &[String]) -> Result<(), WalletError> {
    if events.is_empty() {
        return Err(WalletError::InvalidInput(
            "events must name at least one event type".to_owned(),
        ));
    }
    if let Some(unknown) = events
        .iter()
        .find(|event| !KNOWN_EVENTS.contains(&event.as_str()))
    {
        return Err(WalletError::InvalidInput(format!(
            "unknown event type: {unknown}"
        )));
    }
    Ok(())
}

const ENDPOINT_COLS: &str = "endpoint_id, url, events, enabled, secret_last4, created_at";

const DELIVERY_COLS: &str = "delivery_id, endpoint_id, event_type, status, attempts, \
     next_attempt_at, response_status, last_error, created_at, delivered_at, claimed_at";

fn endpoint_from_row(row: &sqlx::postgres::PgRow) -> Result<WebhookEndpoint, WalletError> {
    use sqlx::Row;
    Ok(WebhookEndpoint {
        id: row.try_get("endpoint_id")?,
        url: row.try_get("url")?,
        events: row.try_get("events")?,
        enabled: row.try_get("enabled")?,
        secret_last4: row.try_get("secret_last4")?,
        created_at: row.try_get("created_at")?,
    })
}

fn delivery_from_row(row: &sqlx::postgres::PgRow) -> Result<WebhookDelivery, WalletError> {
    use sqlx::Row;
    Ok(WebhookDelivery {
        id: row.try_get("delivery_id")?,
        endpoint_id: row.try_get("endpoint_id")?,
        event_type: row.try_get("event_type")?,
        status: row.try_get("status")?,
        attempts: row.try_get("attempts")?,
        next_attempt_at: row.try_get("next_attempt_at")?,
        response_status: row.try_get("response_status")?,
        last_error: row.try_get("last_error")?,
        created_at: row.try_get("created_at")?,
        delivered_at: row.try_get("delivered_at")?,
        claimed_at: row.try_get("claimed_at")?,
    })
}

impl Db {
    /// Registers an endpoint for the organization and answers it with its signing
    /// secret — the only time the secret is readable; afterwards only
    /// `secretLast4` is, exactly like a channel's `apiKeyLast4`.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] on a malformed URL, a non-HTTPS host, an
    /// empty or unknown event list, or the organization already carrying twenty
    /// endpoints; storage failures surface as [`WalletError`].
    pub async fn create_webhook(
        &self,
        organization: Uuid,
        url: &str,
        events: &[String],
        secret_key: &SecretKey,
    ) -> Result<CreatedWebhook, WalletError> {
        let url = validate_url(url)?;
        validate_events(events)?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.webhook_endpoints WHERE organization_id = $1",
        )
        .bind(organization)
        .fetch_one(self.pool())
        .await?;
        if count >= MAX_ENDPOINTS {
            return Err(WalletError::InvalidInput(
                "an organization may register at most 20 webhook endpoints".to_owned(),
            ));
        }
        let secret = mint_secret();
        let mut dedup: Vec<String> = events.to_vec();
        dedup.sort();
        dedup.dedup();
        let row = sqlx::query(&format!(
            "INSERT INTO oxsum.webhook_endpoints \
             (endpoint_id, organization_id, url, secret_sealed, secret_last4, events) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING {ENDPOINT_COLS}"
        ))
        .bind(Uuid::new_v4())
        .bind(organization)
        .bind(&url)
        .bind(secret_key.seal(&secret)?)
        .bind(&secret[secret.len() - 4..])
        .bind(&dedup)
        .fetch_one(self.pool())
        .await?;
        Ok(CreatedWebhook {
            endpoint: endpoint_from_row(&row)?,
            secret,
        })
    }

    /// The organization's endpoints, newest first — secrets never leave storage.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn list_webhooks(
        &self,
        organization: Uuid,
    ) -> Result<Vec<WebhookEndpoint>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT {ENDPOINT_COLS} FROM oxsum.webhook_endpoints \
             WHERE organization_id = $1 ORDER BY created_at DESC"
        ))
        .bind(organization)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(endpoint_from_row).collect()
    }

    /// Deletes the endpoint if it belongs to the organization, answering what was
    /// deleted — `Ok(None)` means no such endpoint under this organization, which
    /// the route maps to 404. Pending deliveries go with the row (the FK
    /// cascades), so a deleted endpoint stops being called.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn delete_webhook(
        &self,
        organization: Uuid,
        endpoint: Uuid,
    ) -> Result<Option<WebhookEndpoint>, WalletError> {
        let row = sqlx::query(&format!(
            "DELETE FROM oxsum.webhook_endpoints \
             WHERE endpoint_id = $1 AND organization_id = $2 RETURNING {ENDPOINT_COLS}"
        ))
        .bind(endpoint)
        .bind(organization)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| endpoint_from_row(&row)).transpose()
    }

    /// The endpoint's most recent deliveries, newest first — `Ok(None)` when the
    /// endpoint is not the organization's.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn webhook_deliveries(
        &self,
        organization: Uuid,
        endpoint: Uuid,
    ) -> Result<Option<Vec<WebhookDelivery>>, WalletError> {
        let owns: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.webhook_endpoints \
             WHERE endpoint_id = $1 AND organization_id = $2)",
        )
        .bind(endpoint)
        .bind(organization)
        .fetch_one(self.pool())
        .await?;
        if !owns {
            return Ok(None);
        }
        let rows = sqlx::query(&format!(
            "SELECT {DELIVERY_COLS} FROM oxsum.webhook_deliveries \
             WHERE endpoint_id = $1 ORDER BY created_at DESC LIMIT {DELIVERY_PAGE}"
        ))
        .bind(endpoint)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(delivery_from_row)
            .collect::<Result<_, _>>()
            .map(Some)
    }

    /// Enqueues one `request.settled` delivery per enabled subscribed endpoint of the
    /// tenant's organization — called inside `record_usage`'s transaction, so the
    /// settlement, the usage row and the notification commit or roll back
    /// together.
    ///
    /// A synthetic tenant id — the margin suite seeds usage rows for tenants with
    /// no organization row — parses to no UUID and enqueues nothing.
    pub(super) async fn enqueue_request_settled(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        tenant_id: &str,
        data: Value,
    ) -> Result<(), WalletError> {
        let Ok(organization) = Uuid::parse_str(tenant_id) else {
            return Ok(());
        };
        Self::enqueue_event(tx, organization, REQUEST_SETTLED, data).await
    }

    /// Enqueues one delivery of `event_type` per enabled subscribed endpoint of the
    /// organization, on the caller's transaction — the event and the change it
    /// announces commit or roll back together.
    pub(crate) async fn enqueue_event(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization: Uuid,
        event_type: &str,
        data: Value,
    ) -> Result<(), WalletError> {
        let endpoints = sqlx::query(
            "SELECT endpoint_id FROM oxsum.webhook_endpoints \
             WHERE organization_id = $1 AND enabled AND $2 = ANY(events)",
        )
        .bind(organization)
        .bind(event_type)
        .fetch_all(&mut **tx)
        .await?;
        for endpoint in endpoints {
            use sqlx::Row;
            let endpoint_id: Uuid = endpoint.try_get("endpoint_id")?;
            let delivery_id = Uuid::new_v4();
            let payload = event_envelope(delivery_id, event_type, organization, data.clone());
            sqlx::query(
                "INSERT INTO oxsum.webhook_deliveries \
                 (delivery_id, endpoint_id, organization_id, event_type, payload) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(delivery_id)
            .bind(endpoint_id)
            .bind(organization)
            .bind(event_type)
            .bind(payload)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// Claims the next batch of due deliveries for the calling worker: each row
    /// moves `pending` → `sending` atomically, so two workers never hand the
    /// same delivery to a receiver twice in one pass — `SKIP LOCKED` means a
    /// claim in flight is invisible rather than contended. A row left `sending`
    /// past [`CLAIM_LEASE_SECS`] belonged to a worker that died mid-attempt and
    /// is claimed again.
    ///
    /// A disabled endpoint's queued deliveries still send — `enabled` gates
    /// what enqueues, not what was already promised. `organization` scopes the
    /// claim to one organization (tests use it to isolate their deliveries;
    /// the worker passes `None` and claims the whole queue).
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn claim_due_deliveries(
        &self,
        organization: Option<Uuid>,
    ) -> Result<Vec<DueDelivery>, WalletError> {
        let rows = sqlx::query(&format!(
            "WITH due AS ( \
                 SELECT d.delivery_id FROM oxsum.webhook_deliveries d \
                 WHERE ((d.status = 'pending' AND d.next_attempt_at <= now()) \
                    OR (d.status = 'sending' AND d.claimed_at <= now() - interval '{CLAIM_LEASE_SECS} seconds')) \
                    AND ($1::uuid IS NULL OR d.organization_id = $1) \
                 ORDER BY d.next_attempt_at LIMIT {DUE_BATCH} \
                 FOR UPDATE SKIP LOCKED \
             ) \
             UPDATE oxsum.webhook_deliveries d \
             SET status = 'sending', claimed_at = now() \
             FROM due WHERE d.delivery_id = due.delivery_id \
             RETURNING d.delivery_id, d.endpoint_id, d.event_type, d.status, d.attempts, \
                       d.next_attempt_at, d.response_status, d.last_error, d.created_at, \
                       d.delivered_at, d.claimed_at, d.payload, \
                       (SELECT e.url FROM oxsum.webhook_endpoints e WHERE e.endpoint_id = d.endpoint_id) AS url, \
                       (SELECT e.secret_sealed FROM oxsum.webhook_endpoints e WHERE e.endpoint_id = d.endpoint_id) AS secret_sealed"
        ))
        .bind(organization)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                use sqlx::Row;
                Ok(DueDelivery {
                    delivery: delivery_from_row(row)?,
                    url: row.try_get("url")?,
                    secret_sealed: row.try_get("secret_sealed")?,
                    body: row.try_get::<Value, _>("payload")?.to_string(),
                })
            })
            .collect()
    }

    /// Records what one delivery attempt ended in: `delivered` on a 2xx, a
    /// rescheduled `pending` inside the attempt budget, or `failed` past it.
    /// The `claimed_at` guard fences the write to the claim that sent it — a
    /// worker whose lease lapsed mid-flight cannot overwrite the next
    /// claimant's state.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn record_delivery(
        &self,
        delivery: &WebhookDelivery,
        outcome: &DeliveryOutcome,
    ) -> Result<(), WalletError> {
        match outcome {
            DeliveryOutcome::Delivered { status } => {
                sqlx::query(
                    "UPDATE oxsum.webhook_deliveries \
                     SET status = 'delivered', attempts = attempts + 1, \
                         response_status = $3, last_error = NULL, delivered_at = now() \
                     WHERE delivery_id = $1 AND status = 'sending' AND claimed_at = $2",
                )
                .bind(delivery.id)
                .bind(delivery.claimed_at)
                .bind(status)
                .execute(self.pool())
                .await?;
            }
            DeliveryOutcome::AttemptFailed {
                status,
                error,
                attempt,
            } => {
                sqlx::query(
                    "UPDATE oxsum.webhook_deliveries \
                     SET attempts = attempts + 1, response_status = $3, last_error = $4, \
                         status = CASE WHEN $5 >= $6 THEN 'failed' ELSE 'pending' END, \
                         next_attempt_at = now() + ($7 || ' seconds')::interval, \
                         claimed_at = NULL \
                     WHERE delivery_id = $1 AND status = 'sending' AND claimed_at = $2",
                )
                .bind(delivery.id)
                .bind(delivery.claimed_at)
                .bind(status)
                .bind(error)
                .bind(attempt)
                .bind(MAX_DELIVERY_ATTEMPTS)
                .bind(retry_delay_secs(*attempt))
                .execute(self.pool())
                .await?;
            }
        }
        Ok(())
    }
}

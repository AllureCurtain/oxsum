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

/// A request's settlement landed — the `request.settled` of the event catalog
/// in docs/decisions.md, opt-in per endpoint because per-request traffic is
/// noisy by nature.
pub const REQUEST_SETTLED: &str = "request.settled";
/// The platform suspended an organization's spend (issue #162).
pub const ORG_SUSPENDED: &str = "org.suspended";
/// A deposit reached `credited` — money landed in the purchased pool.
pub const DEPOSIT_CREDITED: &str = "deposit.credited";
/// A deposit's rail reported it failed.
pub const DEPOSIT_FAILED: &str = "deposit.failed";
/// A deposit's rail reversed a credit it had already confirmed.
pub const DEPOSIT_REVERSED: &str = "deposit.reversed";
/// A settlement took the organization's available balance below the endpoint's
/// `balanceLowMinor` threshold. Fires on the crossing only; the latch resets
/// when a deposit credits back above the line.
pub const BALANCE_LOW: &str = "balance.low";
/// A statement was finalized — issued to the organization.
pub const STATEMENT_FINALIZED: &str = "statement.finalized";
/// A finalized statement's due date passed unpaid.
pub const STATEMENT_OVERDUE: &str = "statement.payment_overdue";
/// A statement's payment standing changed (to `paid` on a settlement, or
/// through `suspended`).
pub const STATEMENT_STATUS_UPDATED: &str = "statement.payment_status_updated";
/// A hold was refused for insufficient funds while the organization carries a
/// configured credit limit — the organization is living on the line and has
/// hit it.
pub const ORG_CREDIT_LIMIT_EXCEEDED: &str = "org.credit_limit_exceeded";
/// A settled turn closed anomalously — `unpriced`, `capped`, `estimated`,
/// `client_cancelled`, `swept`, or a hold that dead-lettered.
pub const ANOMALY_DETECTED: &str = "anomaly.detected";
/// A key's current `budgetDuration` window is projected to exhaust its
/// `spendLimitMinor` before the window ends — the early warning the refused
/// hold would otherwise be the first sign of. Fires once per key per window;
/// the latch is the `budget_forecast_sent_for` date on the key row (issue #220).
pub const BUDGET_EXHAUSTION_FORECAST: &str = "budget.exhaustion_forecast";
/// A scoped budget crossed an armed consumption percentage (issue #232): the
/// endpoint's `budgetThresholdsPercent` names the lines it watches, and each
/// scope a settle re-evaluates — the key's `spendLimitMinor`, the minting
/// member's `budgetLimitMinor`, and the organization's credit line — fires at
/// most once per window per threshold, latched by `budget_threshold_fires`
/// inside the settle's own transaction. `data` carries `scope`, the scope's
/// own id (`keyId`, `memberId` or `organizationId`), `limitMinor`,
/// `committedMinor`, `consumedPercent`, `thresholdPercent`, and `windowStart`
/// for the windowed scopes. A constraint PATCH re-arms the scope's latches.
pub const BUDGET_THRESHOLD_CROSSED: &str = "budget.threshold_crossed";

/// Every event name a subscription may carry.
const KNOWN_EVENTS: &[&str] = &[
    REQUEST_SETTLED,
    ORG_SUSPENDED,
    DEPOSIT_CREDITED,
    DEPOSIT_FAILED,
    DEPOSIT_REVERSED,
    BALANCE_LOW,
    STATEMENT_FINALIZED,
    STATEMENT_OVERDUE,
    STATEMENT_STATUS_UPDATED,
    ORG_CREDIT_LIMIT_EXCEEDED,
    ANOMALY_DETECTED,
    BUDGET_EXHAUSTION_FORECAST,
    BUDGET_THRESHOLD_CROSSED,
];

/// How many times a delivery is attempted before it is marked `failed` — the
/// same budget the hold sweeper gives a settlement before dead-lettering.
pub const MAX_DELIVERY_ATTEMPTS: i32 = 10;

/// The projection a settle computes for one periodically-limited key: the date
/// the current window's committed spend, carried forward at the rate observed
/// so far in the window, would cross the key's `spendLimitMinor`. Produced by
/// [`forecast_exhaustion`]; consumed by [`Db::enqueue_budget_forecast`] and by
/// the keys dashboard, which renders the same projection as a line per key.
///
/// `days_remaining` is `projected_exhaustion_date - today`, floored at zero —
/// a `daily` window always projects to today when it crosses, so `0` means
/// "the window's own close exhausts it", not that the limit is already hit.
#[derive(Debug, Clone)]
pub struct BudgetForecast {
    /// The key the projection is for.
    pub key_id: Uuid,
    /// The limit being projected against, in minor units.
    pub spend_limit_minor: i64,
    /// The window's duration, as the contract spells it.
    pub budget_duration: &'static str,
    /// The first day of the window the projection ran in.
    pub window_start: time::Date,
    /// The committed spend in the window when the projection ran.
    pub committed_minor: i64,
    /// The date the spend rate crosses the limit.
    pub projected_exhaustion_date: time::Date,
    /// Days from today to the projected date, floored at zero.
    pub days_remaining: i64,
}

/// Projects when a periodically-limited key exhausts its `spendLimitMinor`,
/// from the window's elapsed fraction and committed spend.
///
/// The rate is `committed_minor / elapsed-days` carried over the whole window:
/// `projected_total = committed * window_days / elapsed_days`, and exhaustion
/// lands at `window_start + (limit / rate)` — the date the line crosses the
/// cap. `Some(date)` is returned only when that date falls inside the window
/// (a slower burn never exhausts this period, so nothing to warn about); the
/// returned date is the projected crossing, which may be today for a `daily`
/// window judged partway through.
///
/// `elapsed_days` counts `window_start..=today` inclusive — a settle at 09:00
/// on a `daily` window has burned a partial day, and projecting it as a full
/// one would over-read the rate; treating the first day as fully elapsed is
/// the conservative floor that fires the warning earliest. The boundary cases:
/// `committed <= 0` or a zero/negative rate projects nothing (no spend, or
/// credit-only); `committed >= limit` is already over and projects to today.
#[must_use]
pub fn forecast_exhaustion(
    duration: crate::keys::BudgetDuration,
    window_start: time::Date,
    today: time::Date,
    committed_minor: i64,
    spend_limit_minor: i64,
) -> Option<BudgetForecast> {
    if committed_minor <= 0 || spend_limit_minor <= 0 {
        return None;
    }
    let window_days = match duration {
        crate::keys::BudgetDuration::Daily => 1,
        crate::keys::BudgetDuration::Weekly => 7,
        crate::keys::BudgetDuration::Monthly => {
            // The month's real length — a 31-day month burns slower per day.
            days_in_month(today)
        }
    };
    // Days elapsed in the window, inclusive of today; the first day is 1, so a
    // window that just opened cannot divide by zero — and a `daily` window's
    // elapsed is always 1, meaning its projection is "as fast as today".
    let elapsed_days = (today - window_start).whole_days().max(0) + 1;
    // Already over the limit: exhaustion is now, not a projection.
    if committed_minor >= spend_limit_minor {
        return Some(BudgetForecast {
            key_id: Uuid::nil(),
            spend_limit_minor,
            budget_duration: duration.as_str(),
            window_start,
            committed_minor,
            projected_exhaustion_date: today,
            days_remaining: 0,
        });
    }
    // Project the window's total at the current rate, then walk back to the
    // date the running line crosses the limit:
    //   rate          = committed / elapsed_days            (minor per day)
    //   days_to_cross = ceil(limit / rate) - 1              (window-start offset)
    // computed in integer minor-days to avoid floating point on money.
    //
    // exhaustion_offset = floor((limit * elapsed_days - 1) / committed) — the
    // number of window days, minus one, until committed * (d+1)/elapsed >= limit.
    // Equivalently the first d where committed * (d+1) / elapsed >= limit.
    // Solve d = ceil(limit * elapsed / committed) - 1.
    let numerator = (spend_limit_minor as i128) * (elapsed_days as i128);
    let days_to_cross =
        ((numerator + (committed_minor as i128) - 1) / (committed_minor as i128)) as i64 - 1;
    let projected = window_start + time::Duration::days(days_to_cross);
    let window_end = window_start + time::Duration::days(window_days - 1);
    if projected > window_end {
        return None;
    }
    Some(BudgetForecast {
        key_id: Uuid::nil(),
        spend_limit_minor,
        budget_duration: duration.as_str(),
        window_start,
        committed_minor,
        projected_exhaustion_date: projected,
        days_remaining: (projected - today).whole_days().max(0),
    })
}

/// The number of days in the month `on` falls in — `monthly` windows are not a
/// fixed width, and February's short month is the case the width exists for.
fn days_in_month(on: time::Date) -> i64 {
    let next_month = if on.month() == time::Month::December {
        time::Date::from_calendar_date(on.year() + 1, time::Month::January, 1)
    } else {
        time::Date::from_calendar_date(on.year(), on.month().next(), 1)
    };
    let first = time::Date::from_calendar_date(on.year(), on.month(), 1);
    match (next_month, first) {
        (Ok(next), Ok(first)) => (next - first).whole_days(),
        // Unreachable for any real date; a 30-day fallback projects mid-range.
        _ => 30,
    }
}

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
    /// The available-balance line `balance.low` fires below; `None` is off.
    pub balance_low_minor: Option<i64>,
    /// The consumption percentages `budget.threshold_crossed` fires at; empty
    /// is off (issue #232).
    #[serde(rename = "budgetThresholdsPercent")]
    pub budget_thresholds: Vec<i32>,
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

const ENDPOINT_COLS: &str = "endpoint_id, url, events, enabled, balance_low_minor, \
     budget_thresholds, secret_last4, created_at";

const DELIVERY_COLS: &str = "delivery_id, endpoint_id, event_type, status, attempts, \
     next_attempt_at, response_status, last_error, created_at, delivered_at, claimed_at";

fn endpoint_from_row(row: &sqlx::postgres::PgRow) -> Result<WebhookEndpoint, WalletError> {
    use sqlx::Row;
    Ok(WebhookEndpoint {
        id: row.try_get("endpoint_id")?,
        url: row.try_get("url")?,
        events: row.try_get("events")?,
        enabled: row.try_get("enabled")?,
        balance_low_minor: row.try_get("balance_low_minor")?,
        budget_thresholds: row
            .try_get::<Vec<i16>, _>("budget_thresholds")?
            .into_iter()
            .map(i32::from)
            .collect(),
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
    /// Registers an endpoint with no `balance.low` threshold armed — the form
    /// every caller before the event catalog took (issue #200 adds the field).
    ///
    /// # Errors
    ///
    /// As [`create_webhook_with_threshold`](Self::create_webhook_with_threshold).
    pub async fn create_webhook(
        &self,
        organization: Uuid,
        url: &str,
        events: &[String],
        secret_key: &SecretKey,
    ) -> Result<CreatedWebhook, WalletError> {
        self.create_webhook_with_threshold(organization, url, events, None, &[], secret_key)
            .await
    }

    /// Registers an endpoint, optionally armed with the `balance.low` threshold
    /// `balance_low_minor` — the available-balance line the event fires below —
    /// and the consumption percentages `budget_thresholds` arms
    /// `budget.threshold_crossed` at (issue #232). `None` and an empty list
    /// leave the endpoint deaf to either.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] on a malformed URL, a non-HTTPS host, an
    /// empty or unknown event list, a negative threshold, a percentage outside
    /// 1–100, or the organization already carrying twenty endpoints; storage
    /// failures surface as [`WalletError`].
    pub async fn create_webhook_with_threshold(
        &self,
        organization: Uuid,
        url: &str,
        events: &[String],
        balance_low_minor: Option<i64>,
        budget_thresholds: &[i32],
        secret_key: &SecretKey,
    ) -> Result<CreatedWebhook, WalletError> {
        let url = validate_url(url)?;
        validate_events(events)?;
        if let Some(threshold) = balance_low_minor
            && threshold < 0
        {
            return Err(WalletError::InvalidInput(
                "balanceLowMinor must be zero or positive".to_owned(),
            ));
        }
        let mut thresholds: Vec<i16> = Vec::with_capacity(budget_thresholds.len());
        for percent in budget_thresholds {
            if !(1..=100).contains(percent) {
                return Err(WalletError::InvalidInput(
                    "budgetThresholdsPercent entries must be between 1 and 100".to_owned(),
                ));
            }
            thresholds.push(i16::try_from(*percent).unwrap_or(100));
        }
        thresholds.sort_unstable();
        thresholds.dedup();
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
             (endpoint_id, organization_id, url, secret_sealed, secret_last4, events, \
              balance_low_minor, budget_thresholds) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {ENDPOINT_COLS}"
        ))
        .bind(Uuid::new_v4())
        .bind(organization)
        .bind(&url)
        .bind(secret_key.seal(&secret)?)
        .bind(&secret[secret.len() - 4..])
        .bind(&dedup)
        .bind(balance_low_minor)
        .bind(&thresholds)
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

    /// Enqueues one delivery per endpoint subscribed to `event_type` — like
    /// [`enqueue_event`](Self::enqueue_event), but on the pool rather than a
    /// transaction: for the notifications that have no transaction of their own
    /// (`org.credit_limit_exceeded`, fired at the dispatch layer after a hold
    /// the engine's append refused).
    ///
    /// The enqueue is at-least-once: a crash between the refusal and this call
    /// loses the event, and a retried refusal can enqueue twice. Delivery is
    /// durable once written, and a receiver that cannot absorb a duplicate
    /// `org.credit_limit_exceeded` is in worse shape than the missed one.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn notify_event(
        &self,
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
        .fetch_all(self.pool())
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
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }

    /// Enqueues one delivery of `event_type` per enabled subscribed endpoint
    /// across *every* organization — for platform events that belong to no
    /// tenant, such as `anomaly.detected` on upstream price drift (issue #222).
    /// Each delivery carries its own endpoint's `organization_id`, which the
    /// NOT NULL column requires; the event is safe to fan out because it names
    /// rates the public catalog already serves, never organization data.
    ///
    /// At-least-once like [`notify_event`](Self::notify_event): durable once a
    /// row is written, and a crash before it is recorded as a lost event rather
    /// than a partial fan-out.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn broadcast_event(&self, event_type: &str, data: Value) -> Result<(), WalletError> {
        let endpoints = sqlx::query(
            "SELECT endpoint_id, organization_id FROM oxsum.webhook_endpoints \
             WHERE enabled AND $1 = ANY(events)",
        )
        .bind(event_type)
        .fetch_all(self.pool())
        .await?;
        for endpoint in endpoints {
            use sqlx::Row;
            let endpoint_id: Uuid = endpoint.try_get("endpoint_id")?;
            let organization_id: Uuid = endpoint.try_get("organization_id")?;
            let delivery_id = Uuid::new_v4();
            let payload = event_envelope(delivery_id, event_type, organization_id, data.clone());
            sqlx::query(
                "INSERT INTO oxsum.webhook_deliveries \
                 (delivery_id, endpoint_id, organization_id, event_type, payload) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(delivery_id)
            .bind(endpoint_id)
            .bind(organization_id)
            .bind(event_type)
            .bind(payload)
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }

    /// `balance.low`: enqueues the event once per endpoint whose threshold the
    /// organization's available balance has dropped under, and latches the row
    /// so a balance that stays low does not re-fire on every settle. The latch
    /// clears on the next deposit that credits the wallet back over the line.
    ///
    /// Called inside `record_usage`'s transaction with the post-settle balance
    /// the settlement left — the caller reads it off the ledger entries the
    /// same tx already sees, so the number the threshold is judged against is
    /// the books', not a second opinion.
    ///
    /// `available` may be negative (a credit line takes it below zero); the
    /// comparison is `<`, so a threshold of 0 still fires on debt.
    pub(crate) async fn enqueue_balance_low(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization: Uuid,
        available: i64,
    ) -> Result<(), WalletError> {
        // Endpoints subscribed and armed: threshold set, latch clear, and the
        // balance under it. `FOR UPDATE` serializes the latch against a
        // concurrent settle, so two settles cannot both pass the threshold and
        // enqueue the event twice.
        let endpoints = sqlx::query(
            "SELECT endpoint_id, balance_low_minor FROM oxsum.webhook_endpoints \
             WHERE organization_id = $1 AND enabled \
               AND balance_low_minor IS NOT NULL \
               AND NOT balance_low_sent \
               AND $2 < balance_low_minor \
               AND 'balance.low' = ANY(events) \
             FOR UPDATE",
        )
        .bind(organization)
        .bind(available)
        .fetch_all(&mut **tx)
        .await?;
        for endpoint in endpoints {
            use sqlx::Row;
            let endpoint_id: Uuid = endpoint.try_get("endpoint_id")?;
            let threshold: i64 = endpoint.try_get("balance_low_minor")?;
            let delivery_id = Uuid::new_v4();
            let payload = event_envelope(
                delivery_id,
                BALANCE_LOW,
                organization,
                serde_json::json!({
                    "availableMinor": available,
                    "thresholdMinor": threshold,
                }),
            );
            sqlx::query(
                "INSERT INTO oxsum.webhook_deliveries \
                 (delivery_id, endpoint_id, organization_id, event_type, payload) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(delivery_id)
            .bind(endpoint_id)
            .bind(organization)
            .bind(BALANCE_LOW)
            .bind(payload)
            .execute(&mut **tx)
            .await?;
            // The latch is set with the delivery inside the same transaction:
            // commit and they land together, roll back and neither does.
            sqlx::query(
                "UPDATE oxsum.webhook_endpoints SET balance_low_sent = TRUE \
                 WHERE endpoint_id = $1",
            )
            .bind(endpoint_id)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// Clears the `balance.low` latch on every endpoint whose threshold the
    /// organization's available balance has come back above — called where a
    /// deposit credits the wallet, so the next drop below the line fires again.
    ///
    /// The event stays armed while the balance sits over a threshold it once
    /// crossed: only a settle that takes it under again enqueues.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn clear_balance_low(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization: Uuid,
        available: i64,
    ) -> Result<(), WalletError> {
        sqlx::query(
            "UPDATE oxsum.webhook_endpoints SET balance_low_sent = FALSE \
             WHERE organization_id = $1 AND balance_low_sent \
               AND balance_low_minor IS NOT NULL AND $2 >= balance_low_minor",
        )
        .bind(organization)
        .bind(available)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// `budget.exhaustion_forecast`: enqueue the event for the key whose
    /// forecast this settle computed, inside the settle's transaction so the
    /// projection, the usage row and the delivery commit or roll back together.
    /// The caller (the settle path, which holds the wallet) has already judged
    /// the projection and read the key's constraint; this only writes the
    /// delivery and latches the window on the key row. `key_id` is `None` on an
    /// unattributed settle — nothing to forecast.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub(crate) async fn enqueue_budget_forecast(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        tenant_id: &str,
        forecast: &BudgetForecast,
    ) -> Result<(), WalletError> {
        let Ok(organization) = Uuid::parse_str(tenant_id) else {
            return Ok(());
        };
        Self::enqueue_event(
            tx,
            organization,
            BUDGET_EXHAUSTION_FORECAST,
            serde_json::json!({
                "keyId": forecast.key_id,
                "spendLimitMinor": forecast.spend_limit_minor,
                "budgetDuration": forecast.budget_duration,
                "windowStart": forecast.window_start,
                "committedMinor": forecast.committed_minor,
                "projectedExhaustionDate": forecast.projected_exhaustion_date,
                "daysRemaining": forecast.days_remaining,
            }),
        )
        .await?;
        sqlx::query(
            "UPDATE oxsum.api_keys SET budget_forecast_sent_for = $2 \
             WHERE key_id = $1",
        )
        .bind(forecast.key_id)
        .bind(forecast.window_start)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// `org.credit_limit_exceeded`: announces a hold refused for insufficient
    /// funds on an organization that carries a configured credit line — it is
    /// living on the line and has hit it. No transaction exists to enqueue
    /// inside (the refusal surfaces from the engine's append), so this is the
    /// dispatch layer's at-least-once notification: a crash between the refusal
    /// and the call loses the event, and a client retrying the refused request
    /// re-raises it.
    ///
    /// Enqueues only when the organization actually has a credit limit — a
    /// self-funded wallet's 402 is ordinary insufficient funds, not a credit
    /// event. Errors are logged, not propagated: the 402 the caller is about to
    /// answer is the fact that matters.
    pub async fn notify_credit_limit_exceeded(
        &self,
        organization: Uuid,
        wallet: &crate::wallet::Wallet,
    ) {
        match wallet.credit_limit().await {
            Ok(limit) if limit > 0 => {
                if let Err(error) = self
                    .notify_event(
                        organization,
                        ORG_CREDIT_LIMIT_EXCEEDED,
                        serde_json::json!({
                            "organizationId": organization,
                            "creditLimitMinor": limit,
                        }),
                    )
                    .await
                {
                    tracing::error!(%error, %organization,
                        "enqueueing org.credit_limit_exceeded failed");
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, %organization,
                    "the credit limit could not be read for a refused hold");
            }
        }
    }

    /// `anomaly.detected`: enqueues the event for a settled turn whose kind is
    /// one the anomalies page reviews — `unpriced`, `capped`, `estimated`,
    /// `client_cancelled`, `swept`, `released`. Called inside `record_usage`'s
    /// transaction beside the `request.settled` enqueue; a clean `usage` or
    /// zero-charge refusal kind (`upstream_error`, `upstream_unreachable`)
    /// enqueues nothing.
    pub(crate) async fn enqueue_anomaly(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        tenant_id: &str,
        kind: crate::billing::SettlementKind,
        data: Value,
    ) -> Result<(), WalletError> {
        use crate::billing::SettlementKind;
        let anomalous = matches!(
            kind,
            SettlementKind::Unpriced
                | SettlementKind::Capped
                | SettlementKind::Estimated
                | SettlementKind::ClientCancelled
                | SettlementKind::Swept
                | SettlementKind::Released
        );
        if !anomalous {
            return Ok(());
        }
        let Ok(organization) = Uuid::parse_str(tenant_id) else {
            return Ok(());
        };
        Self::enqueue_event(tx, organization, ANOMALY_DETECTED, data).await
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

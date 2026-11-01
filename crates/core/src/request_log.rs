//! Request analytics (issue #505): the per-request operational record a
//! billing dispute or an SRE query actually reads — "every request this key
//! made to this model in this window, with token counts and latency".
//!
//! `usage_records` is the settlement's accounting fact; `request_log` is the
//! search surface over it. The row writes *after* the turn settles, off the
//! settle path's success — a logging failure is traced and dropped, never a
//! failed request. That is why it is a separate post-settlement insert, not a
//! column on `record_usage_in`'s transaction: the write must be free to fail.

use sqlx::Row as _;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Db, WalletError};

/// One gateway request's operational record — the row `oxsum.request_log`
/// holds and `GET /api/v1/requests` answers. `entry_id` joins the row to the
/// settlement entry whose proof `GET /api/v1/entries/{entryId}/proof` serves;
/// `None` on a refusal that never reached a hold.
#[derive(Debug, Clone)]
pub struct RequestLogRow {
    /// The caller's request id; the settlement entry is `req-<id>:settle`.
    pub request_id: String,
    /// The organization's ledger tenant id — the partition `usage_records`
    /// shares, kept beside `organization_id` so both reads stay index-only.
    pub tenant_id: String,
    /// The org uuid the member endpoint scopes to.
    pub organization_id: Uuid,
    /// The settlement entry's id, when the turn reached one.
    pub entry_id: Option<Uuid>,
    /// The key that paid, where the hold attributed one.
    pub key_id: Option<Uuid>,
    /// The model the caller asked for and the model that answered: equal
    /// until a cross-model failover rewrote the served name (issue #500).
    pub model_requested: String,
    pub model_served: String,
    /// The channel that served; empty on a refusal.
    pub channel: String,
    /// `admitted` when the turn reached a hold, `refused` when admission
    /// turned it away — a refused turn still logs so the surface answers
    /// "which requests were turned away".
    pub admission_outcome: String,
    /// How the turn ended, mirroring `usage_records.status`:
    /// `settled`/`errored`/`refused`.
    pub status: String,
    /// Milliseconds to the first upstream byte; `None` on a whole-body read
    /// or a refusal.
    pub ttft_ms: Option<i64>,
    /// Milliseconds admission→settle — the turn's whole wall clock.
    pub total_ms: Option<i64>,
    /// The tokens the settle billed.
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// The input served from the cache.
    pub cached_tokens: i64,
    /// What the turn charged, in minor units; 0 on a refusal.
    pub charge_minor: i64,
    /// How many upstream calls the turn made — above 1 means it failed over
    /// or fell back between models.
    pub upstream_attempts: i64,
    /// The settlement kind or failure reason on a non-settled turn — the
    /// terminal cause the row shows without a second table.
    pub error_kind: Option<String>,
    /// The gateway's own answer status when it served one.
    pub http_status: Option<i64>,
    /// When the row was written — just after the turn settled.
    pub created_at: OffsetDateTime,
}

/// The filters `request_log_page` applies — every field optional, all
/// conjunctive. `organization_id` is the scope: `Some` narrows a member's
/// read to their org, `None` is the operator's all-orgs read.
#[derive(Debug, Clone, Default)]
pub struct RequestLogFilter {
    /// The org uuid a member read is locked to; `None` reads every org (the
    /// admin endpoint, which passes its own `organization_id` filter through
    /// `filter_organization_id` below).
    pub scope_organization_id: Option<Uuid>,
    /// An explicit org filter the admin endpoint applies on top of the
    /// unscoped read.
    pub filter_organization_id: Option<Uuid>,
    pub key_id: Option<Uuid>,
    pub model_served: Option<String>,
    pub channel: Option<String>,
    pub from: Option<OffsetDateTime>,
    pub to: Option<OffsetDateTime>,
    pub status: Option<String>,
    pub min_charge: Option<i64>,
}

/// One page of the log plus the cursor the next page asks with — `None` at
/// the log's start.
#[derive(Debug)]
pub struct RequestLogPage {
    pub rows: Vec<RequestLogRow>,
    /// `(created_at, request_id)` of the page's last row; echoed back as the
    /// keyset position the next page resumes above.
    pub next_cursor: Option<(OffsetDateTime, String)>,
}

impl Db {
    /// Writes one request's operational record. Best-effort by contract: the
    /// caller invokes it after the settle commits and treats any error as
    /// trace-only — it is deliberately *not* part of `record_usage_in`'s
    /// transaction, so a logging failure can never turn a settled request
    /// into a failed one. `ON CONFLICT DO NOTHING` keeps a replay (the
    /// sweeper's settle, a retried write) from standing a second row.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]; the caller drops them to
    /// a warning.
    pub async fn record_request_log(&self, row: &RequestLogRow) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.request_log \
             (request_id, tenant_id, organization_id, entry_id, key_id, \
              model_requested, model_served, channel, admission_outcome, status, \
              ttft_ms, total_ms, input_tokens, output_tokens, cached_tokens, \
              charge_minor, upstream_attempts, error_kind, http_status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, \
                     $16, $17, $18, $19) \
             ON CONFLICT (request_id, tenant_id) DO NOTHING",
        )
        .bind(&row.request_id)
        .bind(&row.tenant_id)
        .bind(row.organization_id)
        .bind(row.entry_id)
        .bind(row.key_id)
        .bind(&row.model_requested)
        .bind(&row.model_served)
        .bind(&row.channel)
        .bind(&row.admission_outcome)
        .bind(&row.status)
        .bind(row.ttft_ms)
        .bind(row.total_ms)
        .bind(row.input_tokens)
        .bind(row.output_tokens)
        .bind(row.cached_tokens)
        .bind(row.charge_minor)
        .bind(row.upstream_attempts)
        .bind(&row.error_kind)
        .bind(row.http_status)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// One page of the log, newest first — `after` is the `(created_at,
    /// request_id)` position the previous page ended at, and `filter` narrows
    /// to the rows the caller is allowed and asked to see.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn request_log_page(
        &self,
        filter: &RequestLogFilter,
        after: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<RequestLogPage, WalletError> {
        // One row over the page size is the "there is more" signal — the
        // cursor names the page's last row, not the peeked one.
        let limit = limit.clamp(1, 200) as i64;
        let (after_at, after_id) = after
            .map(|(at, id)| (Some(at), Some(id)))
            .unwrap_or((None, None));
        let rows = sqlx::query(
            "SELECT request_id, tenant_id, organization_id, entry_id, key_id, \
                    model_requested, model_served, channel, admission_outcome, status, \
                    ttft_ms, total_ms, input_tokens, output_tokens, cached_tokens, \
                    charge_minor, upstream_attempts, error_kind, http_status, created_at \
             FROM oxsum.request_log \
             WHERE ($1::uuid IS NULL OR organization_id = $1) \
               AND ($2::uuid IS NULL OR organization_id = $2) \
               AND ($3::uuid IS NULL OR key_id = $3) \
               AND ($4::text IS NULL OR model_served = $4) \
               AND ($5::text IS NULL OR channel = $5) \
               AND ($6::timestamptz IS NULL OR created_at >= $6) \
               AND ($7::timestamptz IS NULL OR created_at < $7) \
               AND ($8::text IS NULL OR status = $8) \
               AND ($9::bigint IS NULL OR charge_minor >= $9) \
               AND ($10::timestamptz IS NULL OR (created_at, request_id) < ($10, $11)) \
             ORDER BY created_at DESC, request_id DESC \
             LIMIT $12",
        )
        .bind(filter.scope_organization_id)
        .bind(filter.filter_organization_id)
        .bind(filter.key_id)
        .bind(&filter.model_served)
        .bind(&filter.channel)
        .bind(filter.from)
        .bind(filter.to)
        .bind(&filter.status)
        .bind(filter.min_charge)
        .bind(after_at)
        .bind(after_id)
        .bind(limit + 1)
        .fetch_all(self.pool())
        .await?;
        let more = rows.len() as i64 > limit;
        let entries: Vec<RequestLogRow> = rows
            .iter()
            .take(limit as usize)
            .map(row_to_request_log)
            .collect::<Result<_, _>>()?;
        let next_cursor = more
            .then(|| entries.last())
            .flatten()
            .map(|row| (row.created_at, row.request_id.clone()));
        Ok(RequestLogPage {
            rows: entries,
            next_cursor,
        })
    }

    /// Deletes request-log rows older than `days`, the retention sweep the
    /// periodic `retention` job runs (issue #505). Answers the count removed.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn sweep_request_log(&self, days: i64) -> Result<u64, WalletError> {
        let deleted = sqlx::query(
            "DELETE FROM oxsum.request_log WHERE created_at < now() - make_interval(days => $1)",
        )
        .bind(days.max(0) as i32)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(deleted)
    }
}

/// One `request_log` row read back — the integer columns come off the wire as
/// `i32` and widen to the struct's `i64` so the page never surprises a caller
/// with a narrower type than the contract declares.
fn row_to_request_log(row: &sqlx::postgres::PgRow) -> Result<RequestLogRow, WalletError> {
    Ok(RequestLogRow {
        request_id: row.try_get("request_id")?,
        tenant_id: row.try_get("tenant_id")?,
        organization_id: row.try_get("organization_id")?,
        entry_id: row.try_get("entry_id")?,
        key_id: row.try_get("key_id")?,
        model_requested: row.try_get("model_requested")?,
        model_served: row.try_get("model_served")?,
        channel: row.try_get("channel")?,
        admission_outcome: row.try_get("admission_outcome")?,
        status: row.try_get("status")?,
        ttft_ms: row.try_get::<Option<i32>, _>("ttft_ms")?.map(i64::from),
        total_ms: row.try_get::<Option<i32>, _>("total_ms")?.map(i64::from),
        input_tokens: i64::from(row.try_get::<i32, _>("input_tokens")?),
        output_tokens: i64::from(row.try_get::<i32, _>("output_tokens")?),
        cached_tokens: i64::from(row.try_get::<i32, _>("cached_tokens")?),
        charge_minor: row.try_get("charge_minor")?,
        upstream_attempts: i64::from(row.try_get::<i32, _>("upstream_attempts")?),
        error_kind: row.try_get("error_kind")?,
        http_status: row.try_get::<Option<i32>, _>("http_status")?.map(i64::from),
        created_at: row.try_get("created_at")?,
    })
}

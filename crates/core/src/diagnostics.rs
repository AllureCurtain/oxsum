//! Read-only deployment aggregates without identities, payloads or external I/O.

use serde::Serialize;
use sqlx::Row;

use crate::error::WalletError;
use crate::{CLAIM_LEASE_SECS, Db, LEASE_SECS, METERING_SURFACE};

/// Fixed operational counts from one database snapshot; no customer identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCounts {
    /// Configured provider channel rows; not a connectivity result.
    pub provider_channels: i64,
    /// Enabled credentials outside cooldown.
    pub provider_credentials_active: i64,
    /// Enabled credentials with a future cooldown.
    pub provider_credentials_cooling: i64,
    /// Explicitly disabled credentials.
    pub provider_credentials_disabled: i64,
    /// Enabled merchant accounts without identities.
    pub merchants_enabled: i64,
    /// Disabled merchant accounts.
    pub merchants_disabled: i64,
    /// Pending jobs scheduled in the future.
    pub jobs_scheduled: i64,
    /// Pending jobs whose scheduled time has arrived.
    pub jobs_due: i64,
    /// Running jobs including expired claims.
    pub jobs_running: i64,
    /// Running jobs past the five-minute claim lease or missing a claim stamp.
    pub job_claims_expired: i64,
    /// Retained dead job history, not necessarily a current outage.
    pub jobs_dead: i64,
    /// Created, leased or running request operations.
    pub operations_open: i64,
    /// Operations eligible for the gateway lease sweeper; metering is excluded.
    pub operation_leases_expired: i64,
    /// Open hold watch rows without scanning tenant ledgers.
    pub holds_open: i64,
    /// Hold watch rows marked for hourly retry and operator attention.
    pub holds_dead: i64,
    /// Paid orders awaiting fulfillment.
    pub paid_orders_unfulfilled: i64,
    /// Payment orders explicitly marked for human attention.
    pub payment_orders_need_attention: i64,
    /// Pending merchant event inbox rows.
    pub merchant_events_pending: i64,
    /// Accepted merchant events awaiting application.
    pub merchant_events_accepted: i64,
    /// Provider-pending refunds whose next retry has arrived.
    pub refunds_retry_due: i64,
    /// Provider-pending refunds without a next retry.
    pub refunds_need_review: i64,
    /// Unknown or pending legacy refunds whose next attempt is due.
    pub legacy_refunds_retry_due: i64,
    /// Unknown or pending legacy refunds without a next attempt.
    pub legacy_refunds_need_review: i64,
}

/// Storage readiness from a consistent read-only snapshot.
#[derive(Debug, Clone)]
pub enum DiagnosticSnapshot {
    /// No migration registry exists.
    Unmigrated,
    /// Applied version differs; do not query tables this binary expects.
    SchemaMismatch(Option<String>),
    /// Schema matches and all fixed aggregates were read successfully.
    Ready(DiagnosticCounts),
}

impl Db {
    /// Read schema and aggregates without changing rows or calling providers.
    ///
    /// Uses repeatable-read, read-only isolation and a five-second query timeout.
    /// Stored strings are never selected except the migration version.
    ///
    /// # Errors
    /// Returns storage/timeout failures for the HTTP layer to map to unavailable.
    pub async fn diagnostic_snapshot(&self) -> Result<DiagnosticSnapshot, WalletError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout = '5s'")
            .execute(&mut *tx)
            .await?;
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('oxsum._migrations') IS NOT NULL")
                .fetch_one(&mut *tx)
                .await?;
        if !exists {
            tx.commit().await?;
            return Ok(DiagnosticSnapshot::Unmigrated);
        }
        let applied: Option<String> =
            sqlx::query_scalar("SELECT max(version) FROM oxsum._migrations")
                .fetch_one(&mut *tx)
                .await?;
        if applied.as_deref() != Some(Self::latest_schema_version()) {
            tx.commit().await?;
            return Ok(DiagnosticSnapshot::SchemaMismatch(applied));
        }
        let row = sqlx::query(
            "SELECT \
             (SELECT count(*) FROM oxsum.channels WHERE true) AS provider_channels, \
             (SELECT count(*) FROM oxsum.channel_credentials WHERE status='active' AND (cooldown_until IS NULL OR cooldown_until<=now())) AS provider_credentials_active, \
             (SELECT count(*) FROM oxsum.channel_credentials WHERE status='active' AND cooldown_until>now()) AS provider_credentials_cooling, \
             (SELECT count(*) FROM oxsum.channel_credentials WHERE status='disabled') AS provider_credentials_disabled, \
             (SELECT count(*) FROM oxsum.merchant_accounts WHERE enabled) AS merchants_enabled, \
             (SELECT count(*) FROM oxsum.merchant_accounts WHERE NOT enabled) AS merchants_disabled, \
             (SELECT count(*) FROM oxsum.jobs WHERE status='pending' AND run_at>now()) AS jobs_scheduled, \
             (SELECT count(*) FROM oxsum.jobs WHERE status='pending' AND run_at<=now()) AS jobs_due, \
             (SELECT count(*) FROM oxsum.jobs WHERE status='running') AS jobs_running, \
             (SELECT count(*) FROM oxsum.jobs WHERE status='running' AND (claimed_at IS NULL OR claimed_at<=now()-($1*interval '1 second'))) AS job_claims_expired, \
             (SELECT count(*) FROM oxsum.jobs WHERE status='dead') AS jobs_dead, \
             (SELECT count(*) FROM oxsum.request_operations WHERE state IN ('created','leased','running')) AS operations_open, \
             (SELECT count(*) FROM oxsum.request_operations WHERE surface<>$3 AND ((state IN ('leased','running') AND leased_until<now()) OR (state='created' AND created_at<now()-($2*interval '1 second')))) AS operation_leases_expired, \
             (SELECT count(*) FROM oxsum.open_holds WHERE true) AS holds_open, \
             (SELECT count(*) FROM oxsum.open_holds WHERE dead_at IS NOT NULL) AS holds_dead, \
             (SELECT count(*) FROM oxsum.payment_orders WHERE status='paid') AS paid_orders_unfulfilled, \
             (SELECT count(*) FROM oxsum.payment_orders WHERE status='needs_attention') AS payment_orders_need_attention, \
             (SELECT count(*) FROM oxsum.merchant_event_inbox WHERE state='pending') AS merchant_events_pending, \
             (SELECT count(*) FROM oxsum.merchant_event_inbox WHERE state='accepted') AS merchant_events_accepted, \
             (SELECT count(*) FROM oxsum.refunds WHERE state='provider_pending' AND next_retry_at<=now()) AS refunds_retry_due, \
             (SELECT count(*) FROM oxsum.refunds WHERE state='provider_pending' AND next_retry_at IS NULL) AS refunds_need_review, \
             (SELECT count(*) FROM oxsum.deposit_refunds WHERE state IN ('unknown','pending') AND next_attempt_at<=now()) AS legacy_refunds_retry_due, \
             (SELECT count(*) FROM oxsum.deposit_refunds WHERE state IN ('unknown','pending') AND next_attempt_at IS NULL) AS legacy_refunds_need_review",
        )
        .bind(CLAIM_LEASE_SECS)
        .bind(LEASE_SECS)
        .bind(METERING_SURFACE)
        .fetch_one(&mut *tx)
        .await?;
        let counts = DiagnosticCounts {
            provider_channels: row.try_get("provider_channels")?,
            provider_credentials_active: row.try_get("provider_credentials_active")?,
            provider_credentials_cooling: row.try_get("provider_credentials_cooling")?,
            provider_credentials_disabled: row.try_get("provider_credentials_disabled")?,
            merchants_enabled: row.try_get("merchants_enabled")?,
            merchants_disabled: row.try_get("merchants_disabled")?,
            jobs_scheduled: row.try_get("jobs_scheduled")?,
            jobs_due: row.try_get("jobs_due")?,
            jobs_running: row.try_get("jobs_running")?,
            job_claims_expired: row.try_get("job_claims_expired")?,
            jobs_dead: row.try_get("jobs_dead")?,
            operations_open: row.try_get("operations_open")?,
            operation_leases_expired: row.try_get("operation_leases_expired")?,
            holds_open: row.try_get("holds_open")?,
            holds_dead: row.try_get("holds_dead")?,
            paid_orders_unfulfilled: row.try_get("paid_orders_unfulfilled")?,
            payment_orders_need_attention: row.try_get("payment_orders_need_attention")?,
            merchant_events_pending: row.try_get("merchant_events_pending")?,
            merchant_events_accepted: row.try_get("merchant_events_accepted")?,
            refunds_retry_due: row.try_get("refunds_retry_due")?,
            refunds_need_review: row.try_get("refunds_need_review")?,
            legacy_refunds_retry_due: row.try_get("legacy_refunds_retry_due")?,
            legacy_refunds_need_review: row.try_get("legacy_refunds_need_review")?,
        };
        tx.commit().await?;
        Ok(DiagnosticSnapshot::Ready(counts))
    }
}

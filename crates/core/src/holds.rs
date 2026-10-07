//! The hold sweeper's watch list: which gateway holds are open, and since when.
//!
//! The ledger stays the source of truth — a hold's amount and whether it is settled are read from
//! the hold entry, and the settlement entry's idempotency key is derived from the hold's key, so a
//! late settlement and the sweeper cannot both take effect. This table is only how the sweeper
//! *finds* stale holds: the pending layer says how much is held, not by which request or since
//! when, and paging every tenant's log on every pass would cost O(the log) each time. A row whose
//! hold is gone is deleted without a ledger write, so a disagreement always resolves toward the
//! ledger (docs/decisions.md, "a small watch table for the hold sweeper").

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use sqlx::Row;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::billing::BillLine;
use crate::db::Db;
use crate::error::WalletError;
use crate::tenants::Tenants;
use crate::usage::{UsageRecord, UsageRow};
use crate::wallet::{entry_id_for, settlement_key_for};
use crate::{Settlement, SettlementKind};

/// One gateway hold the sweeper is watching: everything the swept settlement's record needs, so
/// the description is built from the row alone and a retried sweep reproduces it exactly.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenHold {
    /// `req-<request id>:hold`: the idempotency key the hold was taken under.
    pub hold_key: String,
    /// The organization whose ledger holds the freeze, as its ledger tenant id.
    pub tenant_id: String,
    /// From the `x-oxsum-request-id` header.
    pub request_id: String,
    pub model: String,
    pub channel: String,
    /// The price version in force when the turn started.
    pub price_version: i64,
    /// Minor units per million tokens at that version.
    pub input_price: i64,
    pub output_price: i64,
    /// What was frozen, in minor units.
    pub freeze_minor: i64,
    /// The key that took the hold, where one was attributed — what the settled
    /// turn's usage row records as its payer.
    pub key_id: Option<Uuid>,
    /// The caller's attribution on the turn, carried so a swept settlement's usage
    /// row still says whose turn it was.
    pub end_user: Option<String>,
    pub service_tier: Option<String>,
    pub tags: BTreeMap<String, String>,
    /// How many sweep attempts have failed on this hold.
    pub sweep_attempts: i32,
    /// The last failed attempt's error — what an operator needs to see the
    /// failure mode, truncated at the column's size.
    pub last_error: Option<String>,
    /// Set once, when `sweep_attempts` reached [`DEAD_AFTER_SWEEP_ATTEMPTS`]:
    /// the dead-letter marker. A dead hold still retries, hourly instead of
    /// every pass, so a fixed cause still self-heals.
    #[serde(with = "time::serde::rfc3339::option")]
    pub dead_at: Option<OffsetDateTime>,
}

impl Db {
    /// Starts watching a hold. Called before the hold is taken: a row without a hold heals
    /// itself (the sweeper deletes it when the hold is not there), while a hold without a row
    /// would be invisible to the sweeper if this process died.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]; the gateway refuses the request rather
    /// than taking a hold nobody watches.
    pub async fn note_open_hold(&self, hold: &OpenHold) -> Result<(), WalletError> {
        sqlx::query(
            "INSERT INTO oxsum.open_holds \
             (hold_key, tenant_id, request_id, model, channel, price_version, \
              input_price, output_price, freeze_minor, key_id, end_user, service_tier, tags) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(&hold.hold_key)
        .bind(&hold.tenant_id)
        .bind(&hold.request_id)
        .bind(&hold.model)
        .bind(&hold.channel)
        .bind(hold.price_version)
        .bind(hold.input_price)
        .bind(hold.output_price)
        .bind(hold.freeze_minor)
        .bind(hold.key_id)
        .bind(&hold.end_user)
        .bind(&hold.service_tier)
        .bind(serde_json::to_value(&hold.tags).unwrap_or_default())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// How many hold watch rows are open right now — the `/metrics` gauge, refreshed per
    /// scrape rather than counted up and down, so a missed decrement can never drift it.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn open_hold_count(&self) -> Result<i64, WalletError> {
        sqlx::query_scalar("SELECT count(*)::bigint FROM oxsum.open_holds")
            .fetch_one(self.pool())
            .await
            .map_err(Into::into)
    }

    /// How many watched holds are dead-lettered right now — the `/metrics`
    /// gauge, refreshed per scrape like [`Self::open_hold_count`].
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn dead_hold_count(&self) -> Result<i64, WalletError> {
        sqlx::query_scalar(
            "SELECT count(*)::bigint FROM oxsum.open_holds WHERE dead_at IS NOT NULL",
        )
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    /// Stops watching a hold. Idempotent: deleting a row that is already gone is not an error,
    /// because the sweeper and the settling turn both clear it.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn clear_open_hold(&self, hold_key: &str) -> Result<(), WalletError> {
        sqlx::query("DELETE FROM oxsum.open_holds WHERE hold_key = $1")
            .bind(hold_key)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Records one failed sweep attempt against a watched hold: bumps
    /// `sweep_attempts`, stores the error and, the first time the count reaches
    /// [`DEAD_AFTER_SWEEP_ATTEMPTS`], sets `dead_at` — the dead-letter marker.
    /// Returns the new attempt count; the caller reads `DEAD_AFTER_SWEEP_ATTEMPTS`
    /// off it to tell the transition into dead from every other failure.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]. A hold_key nobody watches is
    /// not an error — it answers 0, like a row nobody ever wrote.
    pub async fn note_sweep_failure(
        &self,
        hold_key: &str,
        error: &str,
    ) -> Result<i32, WalletError> {
        sqlx::query_scalar(
            "UPDATE oxsum.open_holds \
             SET sweep_attempts = sweep_attempts + 1, \
                 last_error = left($2, 500), \
                 last_attempt_at = now(), \
                 dead_at = CASE WHEN dead_at IS NULL \
                                AND sweep_attempts + 1 >= $3 THEN now() \
                                ELSE dead_at END \
             WHERE hold_key = $1 \
             RETURNING sweep_attempts",
        )
        .bind(hold_key)
        .bind(error)
        .bind(DEAD_AFTER_SWEEP_ATTEMPTS)
        .fetch_optional(self.pool())
        .await
        .map(|attempts: Option<i32>| attempts.unwrap_or_default())
        .map_err(Into::into)
    }

    /// The watched holds older than `older_than`: what the sweeper may release.
    ///
    /// A dead-lettered row is skipped unless its last attempt is an hour old:
    /// the sweeper keeps a dead hold on an hourly retry instead of tripping
    /// over it every pass, so a fixed cause still self-heals without the
    /// minute cadence's noise.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn stale_open_holds(
        &self,
        older_than: OffsetDateTime,
    ) -> Result<Vec<OpenHold>, WalletError> {
        let rows = sqlx::query(
            "SELECT hold_key, tenant_id, request_id, model, channel, \
             price_version, input_price, output_price, freeze_minor, key_id, \
             end_user, service_tier, tags, sweep_attempts, last_error, dead_at \
             FROM oxsum.open_holds \
             WHERE opened_at < $1 \
               AND (dead_at IS NULL OR last_attempt_at < now() - interval '1 hour') \
             ORDER BY opened_at",
        )
        .bind(older_than)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(open_hold_from_row).collect()
    }

    /// The holds one organization currently has in flight, newest first.
    ///
    /// What the dashboard's holds section lists and the billing WebSocket keeps live.
    /// The ledger stays the source of truth — a row whose hold is gone is deleted, not
    /// shown — this only finds the rows.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn open_holds_for_tenant(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<OpenHold>, WalletError> {
        let rows = sqlx::query(
            "SELECT hold_key, tenant_id, request_id, model, channel, \
             price_version, input_price, output_price, freeze_minor, key_id, \
             end_user, service_tier, tags, sweep_attempts, last_error, dead_at \
             FROM oxsum.open_holds WHERE tenant_id = $1 ORDER BY opened_at DESC",
        )
        .bind(tenant_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(open_hold_from_row).collect()
    }

    /// Every watched hold, across all organizations, newest first: the platform admin's
    /// in-flight list.
    ///
    /// The ledger stays the source of truth — the sweeper deletes a row whose hold is
    /// gone — so a hold listed here is one the ledger still reserves. An organization
    /// that was deleted is impossible (the membership schema forbids it), but a watch
    /// row predating its organization join survives as an unnamed organization rather
    /// than vanishing.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn open_holds(&self) -> Result<Vec<InFlightHold>, WalletError> {
        let rows = sqlx::query(
            "SELECT h.request_id, h.model, h.channel, h.price_version, h.freeze_minor, \
             h.opened_at, h.sweep_attempts, h.last_error, h.dead_at, \
             coalesce(o.name, h.tenant_id) AS organization \
             FROM oxsum.open_holds h \
             LEFT JOIN oxsum.organizations o ON o.tenant_id = h.tenant_id \
             ORDER BY h.opened_at DESC",
        )
        .fetch_all(self.pool())
        .await?;
        let mut holds = Vec::with_capacity(rows.len());
        for row in &rows {
            holds.push(InFlightHold {
                organization: row.try_get("organization")?,
                request_id: row.try_get("request_id")?,
                model: row.try_get("model")?,
                channel: row.try_get("channel")?,
                price_version: row.try_get("price_version")?,
                freeze_minor: row.try_get("freeze_minor")?,
                opened_at: row.try_get("opened_at")?,
                sweep_attempts: row.try_get("sweep_attempts")?,
                last_error: row.try_get("last_error")?,
                dead_at: row.try_get("dead_at")?,
            });
        }
        Ok(holds)
    }
}

/// A watched hold as the platform admin lists it: the hold row joined to the
/// organization it belongs to, with when it opened — the one timestamp the sweeper's
/// own [`OpenHold`] does not carry, because a retried sweep rebuilds a settlement from
/// the row alone and needs no clock.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InFlightHold {
    /// The organization's name, resolved through its `tenant_id`.
    pub organization: String,
    /// From the `x-oxsum-request-id` header.
    pub request_id: String,
    pub model: String,
    pub channel: String,
    /// The price version in force when the turn started.
    pub price_version: i64,
    /// What was frozen, in minor units.
    pub freeze_minor: i64,
    /// When the hold was taken; how stale it is is the admin's first question.
    #[serde(with = "time::serde::rfc3339")]
    pub opened_at: OffsetDateTime,
    /// How many sweep attempts have failed on it.
    pub sweep_attempts: i32,
    /// The last failed attempt's error.
    pub last_error: Option<String>,
    /// When it was dead-lettered — `None` while the sweeper retries every pass.
    #[serde(with = "time::serde::rfc3339::option")]
    pub dead_at: Option<OffsetDateTime>,
}

/// Maps one `oxsum.open_holds` row to [`OpenHold`].
fn open_hold_from_row(row: &sqlx::postgres::PgRow) -> Result<OpenHold, WalletError> {
    let tags: serde_json::Value = row.try_get("tags")?;
    Ok(OpenHold {
        hold_key: row.try_get("hold_key")?,
        tenant_id: row.try_get("tenant_id")?,
        request_id: row.try_get("request_id")?,
        model: row.try_get("model")?,
        channel: row.try_get("channel")?,
        price_version: row.try_get("price_version")?,
        input_price: row.try_get("input_price")?,
        output_price: row.try_get("output_price")?,
        freeze_minor: row.try_get("freeze_minor")?,
        key_id: row.try_get("key_id")?,
        end_user: row.try_get("end_user")?,
        service_tier: row.try_get("service_tier")?,
        tags: serde_json::from_value(tags).unwrap_or_default(),
        sweep_attempts: row.try_get("sweep_attempts")?,
        last_error: row.try_get("last_error")?,
        dead_at: row.try_get("dead_at")?,
    })
}

/// What one sweep pass did: the stale holds it resolved and the ones whose
/// failure count crossed into the dead-letter state this pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct SweepReport {
    /// Holds released — settled, found already settled, or never taken.
    pub resolved: usize,
    /// Holds whose `sweep_attempts` just reached [`DEAD_AFTER_SWEEP_ATTEMPTS`]:
    /// the pass that dead-letters a hold counts it once, so the number is
    /// transitions, not attempts.
    pub dead_lettered: usize,
}

/// Settles every watched hold older than `older_than` at 0 with kind [`SettlementKind::Swept`],
/// releasing the whole freeze, and reports what the pass did.
///
/// A hold that is old but already settled — the turn's own settlement landed first — is not
/// settled twice: the sweeper names the same settlement entry the turn would have (the
/// idempotency key is [`crate::settlement_key_for`] of the hold's key), so the ledger's
/// idempotency gate refuses the second write and the row is just cleaned up. A hold that was
/// never taken leaves no write behind either. A hold whose settlement fails for any other
/// reason stays watched: [`Db::note_sweep_failure`] counts the attempt, keeps the error and,
/// at [`DEAD_AFTER_SWEEP_ATTEMPTS`] failures, dead-letters the row — it then retries hourly
/// instead of every pass and shows up in the admin holds list and the reconciliation report.
///
/// `on` is the posting date of the sweep entries: the server's current UTC date, like every
/// other write (crates/server/AGENTS.md).
///
/// # Errors
///
/// Reading the watch list surfaces as [`WalletError`]; a single hold that fails to sweep is
/// logged and left for the next pass rather than failing the whole run.
pub async fn sweep_stale_holds(
    db: &Db,
    tenants: &Tenants,
    older_than: OffsetDateTime,
    on: Date,
) -> Result<SweepReport, WalletError> {
    let mut report = SweepReport::default();
    for hold in db.stale_open_holds(older_than).await? {
        match sweep_one(db, tenants, &hold, on).await {
            Ok(()) => report.resolved += 1,
            Err(error) => {
                match db
                    .note_sweep_failure(&hold.hold_key, &error.to_string())
                    .await
                {
                    // `attempts == DEAD_AFTER_SWEEP_ATTEMPTS` happens exactly
                    // once per hold — the update that set `dead_at`.
                    Ok(DEAD_AFTER_SWEEP_ATTEMPTS) => {
                        report.dead_lettered += 1;
                        tracing::error!(%error, hold_key = %hold.hold_key,
                            "a stale hold's settlement failed the tenth sweep; it is \
                             dead-lettered — hourly retries from here, and an operator's problem");
                    }
                    Ok(attempts) => {
                        tracing::error!(%error, attempts, hold_key = %hold.hold_key,
                            "sweeping a stale hold failed; it stays watched for the next pass");
                    }
                    Err(note_error) => {
                        tracing::error!(%error, %note_error, hold_key = %hold.hold_key,
                            "sweeping a stale hold failed and the failure could not be recorded");
                    }
                }
            }
        }
    }
    Ok(report)
}

/// Releases one stale hold and stops watching it.
async fn sweep_one(
    db: &Db,
    tenants: &Tenants,
    hold: &OpenHold,
    on: Date,
) -> Result<(), WalletError> {
    let wallet = tenants.get(&hold.tenant_id).await?;
    // The swept record reuses the settlement shape, with zero counts and zero charge: the
    // description is built from the row alone, so a retried sweep reproduces it exactly and
    // replays instead of conflicting.
    let usage = UsageRecord::default();
    // A swept turn ran nothing and owes nothing: the lines price zero units at
    // the rates the hold's version carried.
    let lines = [
        BillLine {
            item: "input".into(),
            units: 0,
            price_per_m: hold.input_price,
        },
        BillLine {
            item: "output".into(),
            units: 0,
            price_per_m: hold.output_price,
        },
    ];
    let description = Settlement {
        request: &hold.request_id,
        channel: &hold.channel,
        model: &hold.model,
        price_version: hold.price_version,
        kind: SettlementKind::Swept,
        usage: &usage,
        lines: &lines,
        matched_rule: None,
        // A swept turn charges zero — a discount would change nothing.
        discount_percent: None,
        charged: 0,
        freeze: hold.freeze_minor,
    }
    .description()?;
    match wallet.settle(&hold.hold_key, &description, 0, on).await {
        // Swept: the whole freeze is released and the `swept` kind marks the anomaly for the
        // admin page. The other two arms are the race the derived key decides: the turn's own
        // settlement landed first (`Conflict`: "hold already settled"), or the hold was never
        // taken (`HoldNotFound`) — either way there is nothing left to release.
        Ok(_) => {
            tracing::info!(
                request_id = %hold.request_id,
                "swept a timed-out hold: the freeze is released and the turn is recorded as an anomaly"
            );
            // The usage row only when this sweep's settlement landed: on a conflict the
            // turn's own settlement wrote its record, and a hold that was never taken
            // billed nothing and records nothing.
            if let Err(error) = record_swept_usage(db, hold).await {
                tracing::error!(%error, request_id = %hold.request_id,
                    "recording the swept turn's usage row failed");
            }
            db.clear_open_hold(&hold.hold_key).await?;
            Ok(())
        }
        Err(WalletError::HoldNotFound(_)) | Err(WalletError::Conflict(_)) => {
            db.clear_open_hold(&hold.hold_key).await?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// The swept turn's usage row: zero counts and zero charge, with the attribution the
/// watch row kept, so the row says whose turn timed out.
async fn record_swept_usage(db: &Db, hold: &OpenHold) -> Result<(), WalletError> {
    db.record_usage(&UsageRow {
        request_id: hold.request_id.clone(),
        tenant_id: hold.tenant_id.clone(),
        key_id: hold.key_id,
        model: hold.model.clone(),
        channel: hold.channel.clone(),
        price_version: hold.price_version,
        kind: SettlementKind::Swept,
        entry_id: *entry_id_for(&settlement_key_for(&hold.hold_key)).as_uuid(),
        usage: UsageRecord {
            end_user: hold.end_user.clone(),
            service_tier: hold.service_tier.clone(),
            tags: hold.tags.clone(),
            ..UsageRecord::default()
        },
        charged_minor: 0,
        freeze_minor: hold.freeze_minor,
        // The watch row carries no price, so a swept turn's upstream cost is
        // untracked rather than zero — `upstream_cost_minor` stays NULL.
        upstream_cost_minor: None,
    })
    .await
}

/// The default hold timeout: 30 minutes (docs/product.md). It must exceed the longest possible
/// single request: anything older is abandoned by definition, so a hold that is old but whose
/// request is still streaming cannot exist under a correct configuration.
pub const DEFAULT_HOLD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The sweeper's pass interval: how often it looks for stale holds.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// The number of failed sweeps that dead-letters a hold. Below it a hold is a
/// transient failure worth an immediate retry; at it the failure is persistent,
/// the row is marked dead, and the cadence drops to hourly.
pub const DEAD_AFTER_SWEEP_ATTEMPTS: i32 = 10;

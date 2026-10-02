//! The hold sweeper's watch list: which gateway holds are open, and since when.
//!
//! The ledger stays the source of truth — a hold's amount and whether it is settled are read from
//! the hold entry, and the settlement entry's idempotency key is derived from the hold's key, so a
//! late settlement and the sweeper cannot both take effect. This table is only how the sweeper
//! *finds* stale holds: the pending layer says how much is held, not by which request or since
//! when, and paging every tenant's log on every pass would cost O(the log) each time. A row whose
//! hold is gone is deleted without a ledger write, so a disagreement always resolves toward the
//! ledger (docs/decisions.md, "a small watch table for the hold sweeper").

use std::time::Duration;

use serde::Serialize;
use sqlx::Row;
use time::{Date, OffsetDateTime};

use crate::db::Db;
use crate::error::WalletError;
use crate::tenants::Tenants;
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
              input_price, output_price, freeze_minor) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
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
        .execute(self.pool())
        .await?;
        Ok(())
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

    /// The watched holds older than `older_than`: what the sweeper may release.
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
             price_version, input_price, output_price, freeze_minor \
             FROM oxsum.open_holds WHERE opened_at < $1 ORDER BY opened_at",
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
             price_version, input_price, output_price, freeze_minor \
             FROM oxsum.open_holds WHERE tenant_id = $1 ORDER BY opened_at DESC",
        )
        .bind(tenant_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(open_hold_from_row).collect()
    }
}

/// Maps one `oxsum.open_holds` row to [`OpenHold`].
fn open_hold_from_row(row: &sqlx::postgres::PgRow) -> Result<OpenHold, WalletError> {
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
    })
}

/// Settles every watched hold older than `older_than` at 0 with kind [`SettlementKind::Swept`],
/// releasing the whole freeze, and returns how many rows were resolved.
///
/// A hold that is old but already settled — the turn's own settlement landed first — is not
/// settled twice: the sweeper names the same settlement entry the turn would have (the
/// idempotency key is [`crate::settlement_key_for`] of the hold's key), so the ledger's
/// idempotency gate refuses the second write and the row is just cleaned up. A hold that was
/// never taken leaves no write behind either. A hold whose settlement fails for any other reason
/// stays watched for the next pass.
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
) -> Result<usize, WalletError> {
    let mut resolved = 0;
    for hold in db.stale_open_holds(older_than).await? {
        match sweep_one(db, tenants, &hold, on).await {
            Ok(()) => resolved += 1,
            Err(error) => {
                tracing::error!(%error, hold_key = %hold.hold_key,
                    "sweeping a stale hold failed; it stays watched for the next pass");
            }
        }
    }
    Ok(resolved)
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
    let description = Settlement {
        request: &hold.request_id,
        channel: &hold.channel,
        model: &hold.model,
        price_version: hold.price_version,
        kind: SettlementKind::Swept,
        input_tokens: 0,
        output_tokens: 0,
        input_price: hold.input_price,
        output_price: hold.output_price,
        charged: 0,
        freeze: hold.freeze_minor,
    }
    .description()?;
    match wallet.settle(&hold.hold_key, &description, 0, on).await {
        // Swept: the whole freeze is released and the `swept` kind marks the anomaly for the
        // admin page. The other two arms are the race the derived key decides: the turn's own
        // settlement landed first (`Conflict`: "hold already settled"), or the hold was never
        // taken (`HoldNotFound`) — either way there is nothing left to release.
        Ok(_) | Err(WalletError::HoldNotFound(_)) | Err(WalletError::Conflict(_)) => {
            tracing::info!(
                request_id = %hold.request_id,
                "swept a timed-out hold: the freeze is released and the turn is recorded as an anomaly"
            );
            db.clear_open_hold(&hold.hold_key).await?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// The default hold timeout: 30 minutes (docs/product.md). It must exceed the longest possible
/// single request: anything older is abandoned by definition, so a hold that is old but whose
/// request is still streaming cannot exist under a correct configuration.
pub const DEFAULT_HOLD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The sweeper's pass interval: how often it looks for stale holds.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

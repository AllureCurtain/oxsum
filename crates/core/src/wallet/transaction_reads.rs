//! Ledger reads on a borrowed transaction's connection; never wait for a second pool slot.

use doubleentry::storage::postgres::PostgresError;
use doubleentry::{
    Balanced, Direction, Draft, Entry, EntryId, Layer, Period, PeriodCalendar, PeriodId,
    PeriodState, SealContext, StoredEntry,
};
use sqlx::{PgConnection, Row};
use time::Date;
use uuid::Uuid;

use super::{Receipt, SCALE, Wallet};
use crate::{WalletError, ledger_source};

/// Reserved amounts in promo, bonus, wallet and credit order, then actor/correlation.
pub(super) type HoldParts = (i64, i64, i64, i64, Option<String>, Option<String>);

impl Wallet {
    /// Check the receipt's wallet charge against its actual settled revenue legs.
    pub(crate) fn receipt_charge_matches(
        &self,
        entry: &Entry<Balanced, SCALE>,
        charged: i64,
    ) -> bool {
        let posted: i128 = entry
            .postings()
            .iter()
            .filter(|posting| posting.layer == Layer::Settled && posting.account == self.revenue)
            .map(|posting| match posting.direction {
                Direction::Credit => i128::from(posting.amount.to_minor()),
                Direction::Debit => -i128::from(posting.amount.to_minor()),
            })
            .sum();
        posted == i128::from(charged)
    }

    /// Read the full hash-verified entry on the caller's connection.
    pub(super) async fn entry_in(
        &self,
        connection: &mut PgConnection,
        entry: EntryId,
    ) -> Result<Option<StoredEntry<SCALE>>, WalletError> {
        ledger_source::entry_in(connection, &self.tenant_id, entry)
            .await
            .map_err(Into::into)
    }

    /// The receipt projection used by transaction-owned replay checks.
    /// Hash failures propagate exactly like the standalone engine read.
    pub(super) async fn receipt_in(
        &self,
        connection: &mut PgConnection,
        entry: EntryId,
    ) -> Result<Option<Receipt>, WalletError> {
        Ok(self
            .entry_in(connection, entry)
            .await?
            .as_ref()
            .map(super::stored_receipt))
    }

    /// Verify a source's complete canonical bytes without acquiring a second connection.
    /// Missing or corrupt sources cannot justify an effect; operational failures propagate.
    pub(crate) async fn verified_source_hash_in(
        &self,
        connection: &mut PgConnection,
        id: Uuid,
    ) -> Result<Option<String>, WalletError> {
        match ledger_source::entry_in(connection, &self.tenant_id, EntryId::from_uuid(id)).await {
            Ok(Some(stored)) => Ok(Some(stored.content_hash.to_hex())),
            Ok(None) | Err(PostgresError::Integrity(_) | PostgresError::Malformed(_)) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Resolve promo ownership without a nested checkout while the append lock is held.
    pub(super) async fn organization_id_hint_in(
        &self,
        connection: &mut PgConnection,
    ) -> Result<Option<Uuid>, WalletError> {
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('oxsum.organizations') IS NOT NULL")
                .fetch_one(&mut *connection)
                .await?;
        if !exists {
            return Ok(None);
        }
        Ok(
            sqlx::query_scalar(
                "SELECT organization_id FROM oxsum.organizations WHERE tenant_id=$1",
            )
            .bind(&self.tenant_id)
            .fetch_optional(connection)
            .await?,
        )
    }

    /// Check the day seal through the caller's snapshot.
    pub(super) async fn day_is_anchored_in(
        &self,
        connection: &mut PgConnection,
        day: Date,
    ) -> Result<bool, WalletError> {
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('oxsum.usage_anchors') IS NOT NULL")
                .fetch_one(&mut *connection)
                .await?;
        if !exists {
            return Ok(false);
        }
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oxsum.usage_anchors a \
             JOIN oxsum.organizations o USING(organization_id) WHERE o.tenant_id=$1 AND a.day=$2)",
        )
        .bind(&self.tenant_id)
        .bind(day)
        .fetch_one(connection)
        .await?)
    }

    /// Validate against the calendar visible on this connection.
    pub(super) async fn seal_in(
        &self,
        connection: &mut PgConnection,
        draft: Entry<Draft, SCALE>,
    ) -> Result<Entry<Balanced, SCALE>, WalletError> {
        let rows = sqlx::query(&format!(
            "SELECT period_id,starts_on,ends_on,state FROM ledger_{}.periods ORDER BY starts_on",
            self.tenant_id,
        ))
        .fetch_all(connection)
        .await?;
        let mut periods = Vec::with_capacity(rows.len());
        for row in rows {
            let mut period = Period::new(
                PeriodId::new(row.try_get::<String, _>("period_id")?)
                    .map_err(|error| PostgresError::Malformed(error.to_string()))?,
                row.try_get("starts_on")?,
                row.try_get("ends_on")?,
            )
            .map_err(|error| PostgresError::Malformed(error.to_string()))?;
            period.state = match row.try_get::<&str, _>("state")? {
                "open" => PeriodState::Open,
                "closing" => PeriodState::Closing,
                "sealed" => PeriodState::Sealed,
                _ => {
                    return Err(PostgresError::Malformed(
                        "unrecognized ledger period state".into(),
                    )
                    .into());
                }
            };
            periods.push(period);
        }
        let calendar = PeriodCalendar::from_periods(periods).map_err(PostgresError::from)?;
        draft
            .seal(&SealContext {
                accounts: &self.registry,
                calendar: &calendar,
                policy: &self.policy,
            })
            .map_err(super::invalid)
    }

    /// Match the immutable hold read used by standalone settlement. Reading
    /// through this transaction also avoids starving a pool whose other slots
    /// belong to admissions waiting on this ledger's append lock.
    pub(super) async fn outstanding_hold_in(
        &self,
        connection: &mut PgConnection,
        hold_key: &str,
    ) -> Result<HoldParts, WalletError> {
        let stored = self
            .entry_in(connection, super::entry_id_for(hold_key))
            .await?
            .ok_or_else(|| WalletError::HoldNotFound(format!("no hold under key {hold_key:?}")))?;
        self.hold_parts(&stored, hold_key)
    }

    /// Both read paths derive reservation amounts and attribution from verified bytes.
    pub(super) fn hold_parts(
        &self,
        stored: &StoredEntry<SCALE>,
        hold_key: &str,
    ) -> Result<HoldParts, WalletError> {
        if stored.entry.idempotency_key().as_bytes() != hold_key.as_bytes() {
            return Err(WalletError::HoldNotFound(format!(
                "key {hold_key:?} does not name this hold"
            )));
        }
        let mut amounts = [0i64; 4];
        for posting in stored.entry.postings() {
            if posting.layer != Layer::Pending {
                continue;
            }
            let signed = match posting.direction {
                Direction::Debit => posting.amount.to_minor(),
                Direction::Credit => -posting.amount.to_minor(),
            };
            for (index, account) in [self.promo, self.bonus, self.wallet, self.credit_line]
                .into_iter()
                .enumerate()
            {
                if posting.account == account {
                    amounts[index] = amounts[index]
                        .checked_add(signed)
                        .ok_or_else(|| super::invalid("hold amount overflows"))?;
                }
            }
        }
        let total = amounts
            .iter()
            .try_fold(0i64, |total, amount| total.checked_add(*amount))
            .ok_or_else(|| super::invalid("hold amount overflows"))?;
        if total <= 0 {
            return Err(WalletError::HoldNotFound(format!(
                "key {hold_key:?} does not name a hold"
            )));
        }
        let provenance = stored.entry.provenance();
        Ok((
            amounts[0],
            amounts[1],
            amounts[2],
            amounts[3],
            provenance
                .actor
                .as_ref()
                .map(|label| label.as_str().to_owned()),
            provenance
                .correlation
                .as_ref()
                .map(|label| label.as_str().to_owned()),
        ))
    }
}

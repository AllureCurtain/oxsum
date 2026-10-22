//! The daily usage anchor pass (issue #338, docs/decisions.md 2026-10-22).
//!
//! One ledger entry per organization and closed UTC day commits a sorted map
//! from every customer with a day log to that customer's log head — the root
//! over their day leaves and the number of days it covers. The anchor is what a
//! completeness proof hangs on: a report shows its customer's day leaves, the
//! verifier rebuilds the head, and the anchor's `map_root` proves that head is
//! the one fixed before the report was minted.
//!
//! The pass is idempotent per `(organization, day)`: the table row and the
//! ledger entry are keyed by the day, so a retried pass replays what stands. A
//! day with no usage-log rows leaves the organization unanchored — there is no
//! customer head to commit, and a report over such a span does not exist.

use std::collections::BTreeMap;

use sqlx::Row;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::{Hash, MerkleAccumulator, Tenants};

/// What one pass did.
#[derive(Debug, Default, Clone, Copy)]
pub struct UsageAnchorReport {
    /// Organizations the pass visited.
    pub organizations: i64,
    /// Anchors committed (a replay of an already-anchored day is `skipped`).
    pub anchors: i64,
    /// Organizations already anchored for the day.
    pub skipped: i64,
    /// Organizations with no day-log rows up to the day — nothing to commit.
    pub without_usage: i64,
}

impl Db {
    /// Commits `day`'s anchor for every organization with a day log up to it.
    ///
    /// The caller passes a closed day (the job passes yesterday): a day that is
    /// still open would anchor a map the settle path can still change.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`]; one failing
    /// organization aborts the pass, and the next run resumes from the skipped
    /// rows because every organization already anchored is skipped.
    pub async fn anchor_usage_days(
        &self,
        day: time::Date,
    ) -> Result<UsageAnchorReport, WalletError> {
        let mut report = UsageAnchorReport::default();
        let organizations: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT organization_id, tenant_id FROM oxsum.organizations \
             ORDER BY organization_id",
        )
        .fetch_all(self.pool())
        .await?;
        let tenants = Tenants::new(self.pool().clone());
        for (organization_id, tenant_id) in organizations {
            report.organizations += 1;
            let anchored: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM oxsum.usage_anchors \
                 WHERE organization_id = $1 AND day = $2)",
            )
            .bind(organization_id)
            .bind(day)
            .fetch_one(self.pool())
            .await?;
            if anchored {
                report.skipped += 1;
                continue;
            }
            // Each customer's day leaves up to and including the anchor day, in
            // day order; `BTreeMap` gives the map's sorted key order.
            let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
                "SELECT end_user, leaf_hash FROM oxsum.usage_day_log \
                 WHERE tenant_id = $1 AND day <= $2 \
                 ORDER BY end_user, day",
            )
            .bind(&tenant_id)
            .bind(day)
            .fetch_all(self.pool())
            .await?;
            if rows.is_empty() {
                report.without_usage += 1;
                continue;
            }
            let mut by_customer: BTreeMap<String, Vec<Vec<u8>>> = BTreeMap::new();
            for (end_user, leaf) in rows {
                by_customer.entry(end_user).or_default().push(leaf);
            }
            let mut pairs: Vec<(String, Hash, u64)> = Vec::with_capacity(by_customer.len());
            for (end_user, leaves) in by_customer {
                let mut accumulator = MerkleAccumulator::new();
                for leaf in leaves {
                    let array: [u8; 32] = leaf.as_slice().try_into().map_err(|_| {
                        WalletError::InvalidInput("a stored day leaf is not 32 bytes".into())
                    })?;
                    accumulator.push(Hash::from_bytes(array));
                }
                pairs.push((end_user, accumulator.root(), accumulator.size()));
            }
            let refs: Vec<(&str, Hash, u64)> = pairs
                .iter()
                .map(|(end_user, root, size)| (end_user.as_str(), *root, *size))
                .collect();
            let map_root = oxsum_verify::usage_anchor_map_root(&refs);
            let description = serde_json::json!({
                "v": 1,
                "day": day.to_string(),
                "mapRoot": map_root.to_hex(),
                "enduserCount": pairs.len(),
            })
            .to_string();
            let wallet = tenants.get(&tenant_id).await?;
            let (entry_id, _) = wallet.anchor_usage_day(day, &description, map_root).await?;
            sqlx::query(
                "INSERT INTO oxsum.usage_anchors \
                     (organization_id, day, entry_id, map_root, enduser_count) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (organization_id, day) DO NOTHING",
            )
            .bind(organization_id)
            .bind(day)
            .bind(entry_id.as_uuid())
            .bind(map_root.as_bytes().to_vec())
            .bind(pairs.len() as i64)
            .execute(self.pool())
            .await?;
            report.anchors += 1;
        }
        Ok(report)
    }

    /// The anchor a report's span ends on: the organization's row for `day`.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn usage_anchor(
        &self,
        organization: Uuid,
        day: time::Date,
    ) -> Result<Option<(Uuid, Hash, i64)>, WalletError> {
        let row = sqlx::query(
            "SELECT entry_id, map_root, enduser_count FROM oxsum.usage_anchors \
             WHERE organization_id = $1 AND day = $2",
        )
        .bind(organization)
        .bind(day)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let root: Vec<u8> = row.try_get("map_root")?;
        let array: [u8; 32] = root.as_slice().try_into().map_err(|_| {
            WalletError::InvalidInput("a stored anchor root is not 32 bytes".into())
        })?;
        Ok(Some((
            row.try_get("entry_id")?,
            Hash::from_bytes(array),
            row.try_get("enduser_count")?,
        )))
    }
}

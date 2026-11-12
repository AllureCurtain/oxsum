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
//!
//! One organization's anchor lands inside a single transaction (issue #1088):
//! the settle path rewrites a `(tenant, endUser, day)` leaf under a per-key
//! `pg_advisory_xact_lock`, and the anchor transaction takes that same lock for
//! every customer the day touches before it reads a leaf — so the root it
//! computes covers either the leaf a settle already committed or no leaf at
//! all, never one that lands between the read and the entry. The `DaySealed`
//! refusal on the settle side closes the last window: a settle that observes
//! the committed anchor row is refused rather than booking past the seal.

use std::collections::BTreeMap;

use sqlx::Row;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::{Hash, MerkleAccumulator, Tenants};

/// What one organization's anchor attempt resolved to — the per-organization
/// outcome the pass's report counters tally.
enum AnchorOutcome {
    /// The anchor committed: entry, map root and bookkeeping row landed.
    Anchored,
    /// An already-committed anchor answered the probe — nothing to write.
    Skipped,
    /// Open `request_operations` still own the day; the next pass re-checks.
    Deferred,
    /// No `usage_day_log` rows up to the day — nothing to commit.
    WithoutUsage,
}

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
    /// Organizations deferred on the operation watermark (issue #603): they
    /// still own open `request_operations` rows that could settle into `day`,
    /// so their anchor waits — the map a sealed day commits must be final.
    pub deferred: i64,
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
            // The operation watermark (issue #603, D05): a day-close seals the
            // map only once no open operation can still settle into it. An
            // operation created on or before the day's end and still
            // `created`/`leased`/`running` may yet write a settle (or be
            // swept) into this day's leaves — the pass defers the
            // organization rather than commit an anchor a late settle would
            // contradict. The next pass re-reads: a lapsed lease the sweeper
            // has since reclaimed no longer blocks.
            let day_end = day.next_day().unwrap_or(day).midnight().assume_utc();
            match self
                .anchor_one_organization(&tenants, organization_id, &tenant_id, day, day_end)
                .await?
            {
                AnchorOutcome::Anchored => report.anchors += 1,
                AnchorOutcome::Deferred => report.deferred += 1,
                AnchorOutcome::WithoutUsage => report.without_usage += 1,
                AnchorOutcome::Skipped => report.skipped += 1,
            }
        }
        Ok(report)
    }

    /// One organization's `(org, day)` anchor inside a single transaction — the
    /// leaf read, the map's ledger entry and the bookkeeping row commit
    /// together (issue #1088).
    ///
    /// Two interlocks close the read-to-commit window the earlier pass left
    /// open. First, the operation watermark is re-read *inside* the
    /// transaction: every settle path that can still write a leaf goes
    /// terminal on its `request_operations` row only after the leaf commits —
    /// the gateway and sweeper write both in one transaction, the metered and
    /// task paths write the leaf first — so `open = 0` here means no leaf can
    /// still land for the day. Second, the transaction takes the same
    /// per-customer `pg_advisory_xact_lock` `record_usage_day` holds while it
    /// recomputes a leaf, so a settle that did slip between the two checks
    /// blocks on the lock until the anchor commits — where its own
    /// `day_is_anchored_in` re-check then refuses it.
    ///
    /// Returns `true` when the organization still owns an open operation and
    /// the pass must defer; `false` when the anchor committed.
    async fn anchor_one_organization(
        &self,
        tenants: &Tenants,
        organization_id: Uuid,
        tenant_id: &str,
        day: time::Date,
        day_end: time::OffsetDateTime,
    ) -> Result<AnchorOutcome, WalletError> {
        let mut tx = self.pool().begin().await?;
        // A concurrent pass that already committed this organization's anchor
        // is replayed rather than duplicated — the check repeats inside the
        // transaction so a row landing between the caller's probe and this
        // begin cannot slip past it.
        let anchored: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM oxsum.usage_anchors \
             WHERE organization_id = $1 AND day = $2)",
        )
        .bind(organization_id)
        .bind(day)
        .fetch_one(&mut *tx)
        .await?;
        if anchored {
            tx.rollback().await?;
            return Ok(AnchorOutcome::Skipped);
        }
        // The watermark, again under the transaction: `open > 0` defers the
        // organization, `open = 0` is now the proof that no leaf writer is
        // still in flight — every settle path commits its leaf no later than
        // the operation's terminal row, so a closed operation's leaf is
        // already visible to this read.
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM oxsum.request_operations \
             WHERE organization_id = $1 AND created_at <= $2 \
               AND state IN ('created','leased','running')",
        )
        .bind(organization_id)
        .bind(day_end)
        .fetch_one(&mut *tx)
        .await?;
        if open > 0 {
            tx.rollback().await?;
            return Ok(AnchorOutcome::Deferred);
        }
        // Every customer whose leaf the day holds — the lock set the settle
        // path serializes on. `record_usage_day` takes this same lock before
        // recomputing a leaf, so a writer that started between the watermark
        // probe and here waits out this transaction instead of racing it.
        let customers: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT end_user FROM oxsum.usage_day_log \
             WHERE tenant_id = $1 AND day <= $2 \
             ORDER BY end_user",
        )
        .bind(tenant_id)
        .bind(day)
        .fetch_all(&mut *tx)
        .await?;
        if customers.is_empty() {
            tx.rollback().await?;
            return Ok(AnchorOutcome::WithoutUsage);
        }
        for customer in &customers {
            sqlx::query(
                "SELECT pg_advisory_xact_lock(hashtextextended($1 || ':' || $2 || ':' || $3, 0))",
            )
            .bind(tenant_id)
            .bind(customer)
            .bind(day.to_string())
            .execute(&mut *tx)
            .await?;
        }
        // Each customer's day leaves up to and including the anchor day, in
        // day order; `BTreeMap` gives the map's sorted key order. Under the
        // per-customer locks this read is the day's final state.
        let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT end_user, leaf_hash FROM oxsum.usage_day_log \
             WHERE tenant_id = $1 AND day <= $2 \
             ORDER BY end_user, day",
        )
        .bind(tenant_id)
        .bind(day)
        .fetch_all(&mut *tx)
        .await?;
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
        let wallet = tenants.get(tenant_id).await?;
        let (entry_id, _) = wallet
            .anchor_usage_day_in(&mut tx, day, &description, map_root)
            .await?;
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
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(AnchorOutcome::Anchored)
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

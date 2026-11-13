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
//! One organization's anchor lands inside a single transaction (issue #1088).
//! The leaf writers serialize with it twice over: a per-`(tenant, day)` lock
//! pairs with one `record_usage_day` takes before it touches any leaf, so a
//! customer whose first leaf the day has not seen yet is still covered, and
//! the per-`(tenant, endUser, day)` locks the anchor then takes are the same
//! ones the leaf rewrite holds while it recomputes — which re-checks the seal
//! under its locks and refuses a sealed day instead of rewriting past it.

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
    /// Organizations whose anchor attempt failed (issue #1088): one bad
    /// organization no longer aborts the pass — the error is logged, counted
    /// here, and the next organization's anchor still runs. The failed
    /// organization leaves no row, so the next pass retries it.
    pub failed: i64,
}

impl Db {
    /// Commits `day`'s anchor for every organization with a day log up to it.
    ///
    /// The caller passes a closed day (the job passes yesterday): a day that is
    /// still open would anchor a map the settle path can still change.
    ///
    /// # Errors
    ///
    /// The organization listing's own storage failure surfaces as
    /// [`WalletError`]. One organization's anchor failing is captured instead:
    /// the error is logged, the report counts it in [`UsageAnchorReport::failed`],
    /// and the pass moves on — one bad organization must not starve the rest,
    /// and the failed one leaves no row, so the next pass retries it.
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
            let anchored: bool = match sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM oxsum.usage_anchors \
                 WHERE organization_id = $1 AND day = $2)",
            )
            .bind(organization_id)
            .bind(day)
            .fetch_one(self.pool())
            .await
            {
                Ok(anchored) => anchored,
                Err(error) => {
                    tracing::error!(%error, %organization_id, day = %day,
                        "checking the organization's anchor failed");
                    report.failed += 1;
                    continue;
                }
            };
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
                .await
            {
                Ok(AnchorOutcome::Anchored) => report.anchors += 1,
                Ok(AnchorOutcome::Deferred) => report.deferred += 1,
                Ok(AnchorOutcome::WithoutUsage) => report.without_usage += 1,
                Ok(AnchorOutcome::Skipped) => report.skipped += 1,
                Err(error) => {
                    tracing::error!(%error, %organization_id, day = %day,
                        "the organization's anchor failed");
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }

    /// One organization's `(org, day)` anchor inside a single transaction — the
    /// leaf read, the map's ledger entry and the bookkeeping row commit
    /// together (issue #1088).
    ///
    /// Three interlocks close the read-to-commit window the earlier pass left
    /// open. First, the operation watermark is re-read *inside* the
    /// transaction: the settle paths that track their write on a
    /// `request_operations` row go terminal only with (gateway, sweeper) or
    /// before (task) the leaf commit, so `open = 0` means none of them can
    /// still land a leaf for the day. Second, the transaction takes the
    /// per-`(tenant, day)` lock before any customer lock — the same lock
    /// `record_usage_day` takes first — so a leaf writer already in flight,
    /// for a customer the day has not seen yet included, is either finished
    /// and visible to the leaf read or waits out the anchor. Third, the
    /// per-customer locks the leaf rewrite holds while it recomputes — taken
    /// on the anchored day only, in ledger-append order after the entry, so
    /// every writer's lock order is append-then-customer — keep a writer that
    /// slipped between the checks serialized until the anchor commits, where
    /// its own seal re-check under the lock refuses it with `DaySealed`.
    ///
    /// Answers the attempt's outcome: the anchor committed, a committed anchor
    /// replayed (`Skipped`), open operations deferred the day, or the day held
    /// no leaves to commit.
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
        // The per-(tenant, day) lock `record_usage_day` takes before it
        // touches any leaf (issue #1088). Held from here to the commit, it
        // covers customers the day has not seen yet: a writer whose first
        // leaf is still unwritten either finished before this lock landed —
        // and the leaf read below sees it — or waits out the anchor and then
        // refuses the sealed day under its own seal check.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('oxsum/usage-anchor/v1' || $1 || ':' || $2, 0))",
        )
        .bind(tenant_id)
        .bind(day.to_string())
        .execute(&mut *tx)
        .await?;
        // Every customer with a leaf on the anchored day — the lock set the
        // leaf rewrite serializes on. `record_usage_day` takes this same lock
        // before recomputing a leaf, so a writer that started between the
        // watermark probe and here waits out this transaction instead of
        // racing it. The set is the anchored day's own customers, not the
        // log's whole history: a leaf for an earlier day is already fixed in
        // the map its own day's anchor committed.
        let customers: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT end_user FROM oxsum.usage_day_log \
             WHERE tenant_id = $1 AND day = $2 \
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
        // Each customer's day leaves up to and including the anchor day, in
        // day order; `BTreeMap` gives the map's sorted key order. The read
        // precedes the customer locks because the per-day lock above already
        // bars every leaf writer for the day: the rows it returns are final.
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
        // The per-customer locks, taken after the ledger entry — append lock
        // first, then customer locks, the order every leaf writer already
        // keeps (`record_usage_day`'s lock follows its ledger append), so the
        // anchor cannot hold a customer lock while waiting on the append lock
        // a blocked writer holds. They run last so the entry's log position
        // stops waiting on a slow writer: a customer the writer still holds
        // shows up in the read above only once its leaf committed, and a
        // writer still racing re-checks the seal under this lock after the
        // commit and refuses with `DaySealed` rather than rewriting the leaf
        // the map just fixed.
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

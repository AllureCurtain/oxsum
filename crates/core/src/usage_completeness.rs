//! The anchored-day completeness proof (issue #339, the design in
//! docs/decisions.md 2026-10-22, stages 1 and 2 in issues #337/#338).
//!
//! A report's marker attests the subset and the customer checks the figures —
//! but the bundle cannot, on its own, prove that no row or day was dropped. The
//! daily anchor fixes the customer's whole day-log head inside the ledger; this
//! module assembles the proof that the head was folded from exactly the day
//! leaves the bundle shows, so a dropped row (changing its day's leaf) or a
//! dropped day (a gap in the head) makes the verifier fail instead of reading
//! as an empty day.
//!
//! The proof is the customer's **whole** log up to the anchor day, not just the
//! span: the head is the fold of every leaf, so showing all of them is the
//! simplest honest proof that none is missing. The bundle grows with the
//! customer's history — a range proof would shrink it and is a later
//! optimization, not a correctness change.

use std::collections::BTreeMap;
use std::collections::HashMap;

use sqlx::Row;
use uuid::Uuid;

use doubleentry::MerkleLog;

use crate::db::Db;
use crate::error::WalletError;
use crate::usage_reports::UsageReportRow;
use crate::{EntryId, Hash, Tenants};

/// The bundle's completeness half, serialized into the report's wire shape.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletenessBundle {
    /// The proof's version — `1` for the anchored-day proof.
    pub v: i64,
    /// The anchor entry that fixed the map root, with its inclusion proof.
    pub anchor: CompletenessAnchor,
    /// The customer's day-log head at the anchor day.
    pub head: CompletenessHead,
    /// The head's inclusion proof in the anchor map (the proof carries the
    /// customer's position).
    pub map_proof: doubleentry::InclusionProof,
    /// Every day leaf the head was folded from, in index order.
    pub days: Vec<CompletenessDay>,
}

/// The anchor half: the ledger entry, the day it anchored, and the figures its
/// description must repeat.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletenessAnchor {
    /// The anchor entry, its recorded hash and its inclusion in the bundle head.
    pub entry: crate::usage_reports::ReportEntryBundle,
    /// The anchored UTC day, ISO `YYYY-MM-DD`.
    pub day: String,
    /// The sorted-map root over every customer's head, hex.
    pub map_root: String,
    /// How many customers the map covered.
    pub enduser_count: u64,
}

/// The customer's day-log head.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletenessHead {
    /// The head's Merkle root, hex.
    pub root: String,
    /// How many day leaves it covers.
    pub size: u64,
}

/// One day leaf in the customer's log.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletenessDay {
    /// The UTC day, ISO `YYYY-MM-DD`.
    pub day: String,
    /// The leaf's index in the customer's log, zero-based.
    pub index: u64,
    /// The day leaf, hex.
    pub leaf: String,
    /// The leaf's inclusion proof into the head.
    pub proof: doubleentry::InclusionProof,
    /// Positions (in the report's `entries`) of the rows this leaf covers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<usize>,
}

impl Db {
    /// Assembles the completeness proof for a report, when the span is covered
    /// by a daily anchor and every shown row has a day leaf.
    ///
    /// `head` is the bundle's ledger head — the anchor entry's proof is pinned
    /// to it, so the anchor is shown to be in the same ledger the report's
    /// marker and rows are. Returns `None` (claim-only) when no anchor covers
    /// the span's end, or when a shown row predates the day log (history from
    /// before issue #337): the proof cannot honestly cover those rows.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]; a day log that disagrees
    /// with its anchor's map root is a data-integrity error, not a silent
    /// `None`.
    pub async fn usage_completeness(
        &self,
        organization: Uuid,
        tenant_id: &str,
        end_user: &str,
        to: time::Date,
        rows: &[UsageReportRow],
        head: doubleentry::TreeHead,
    ) -> Result<Option<CompletenessBundle>, WalletError> {
        let anchor = sqlx::query(
            "SELECT day, entry_id, map_root, enduser_count FROM oxsum.usage_anchors \
             WHERE organization_id = $1 AND day >= $2 \
             ORDER BY day ASC LIMIT 1",
        )
        .bind(organization)
        .bind(to)
        .fetch_optional(self.pool())
        .await?;
        let Some(anchor) = anchor else {
            return Ok(None);
        };
        let anchor_day: time::Date = anchor.try_get("day")?;
        let anchor_entry_id: Uuid = anchor.try_get("entry_id")?;
        let map_root_bytes: Vec<u8> = anchor.try_get("map_root")?;
        let stored_root = Hash::from_bytes(map_root_bytes.as_slice().try_into().map_err(|_| {
            WalletError::InvalidInput("the anchor's map root is not 32 bytes".into())
        })?);
        let enduser_count: i64 = anchor.try_get("enduser_count")?;

        // Every customer's day leaves up to the anchor day, in day order —
        // the same rows the anchor pass folded.
        let leaves: Vec<(String, time::Date, Vec<u8>)> = sqlx::query_as(
            "SELECT end_user, day, leaf_hash FROM oxsum.usage_day_log \
             WHERE tenant_id = $1 AND day <= $2 \
             ORDER BY end_user, day",
        )
        .bind(tenant_id)
        .bind(anchor_day)
        .fetch_all(self.pool())
        .await?;
        if leaves.is_empty() {
            return Ok(None);
        }
        let mut by_customer: BTreeMap<String, Vec<(time::Date, Hash)>> = BTreeMap::new();
        for (end_user, day, leaf) in leaves {
            let hash = Hash::from_bytes(leaf.as_slice().try_into().map_err(|_| {
                WalletError::InvalidInput("a stored day leaf is not 32 bytes".into())
            })?);
            by_customer.entry(end_user).or_default().push((day, hash));
        }

        // Rebuild the anchor map exactly as the anchor pass did; a mismatch is
        // drift, and serving a proof over it would be a false claim.
        let mut pairs: Vec<(String, Hash, u64)> = Vec::with_capacity(by_customer.len());
        for (end_user, days) in &by_customer {
            let leaves: Vec<Hash> = days.iter().map(|(_, leaf)| *leaf).collect();
            let head = MerkleLog::from_leaves(leaves).head();
            pairs.push((end_user.clone(), head.root, head.size));
        }
        let refs: Vec<(&str, Hash, u64)> = pairs
            .iter()
            .map(|(end_user, root, size)| (end_user.as_str(), *root, *size))
            .collect();
        if oxsum_verify::usage_anchor_map_root(&refs) != stored_root {
            return Err(WalletError::InvalidInput(
                "the anchor's map root disagrees with the day log".into(),
            ));
        }

        let Some(map_index) = pairs.iter().position(|(name, _, _)| name == end_user) else {
            return Ok(None);
        };
        let mut map_leaves = Vec::with_capacity(pairs.len());
        for (end_user, root, size) in &pairs {
            map_leaves.push(oxsum_verify::usage_anchor_pair_leaf(end_user, root, *size));
        }
        let map_log = MerkleLog::from_leaves(map_leaves);
        let map_proof = map_log
            .inclusion_proof_at(map_index as u64, pairs.len() as u64)
            .map_err(|e| WalletError::InvalidInput(e.to_string()))?;

        // The customer's head and every leaf's proof into it.
        let Some(customer_days) = by_customer.get(end_user) else {
            return Ok(None);
        };
        let day_leaves: Vec<Hash> = customer_days.iter().map(|(_, leaf)| *leaf).collect();
        let head_log = MerkleLog::from_leaves(day_leaves);
        let customer_head = head_log.head();
        let size = customer_head.size;
        let mut day_proofs = Vec::with_capacity(customer_days.len());
        for index in 0..size {
            day_proofs.push(
                head_log
                    .inclusion_proof_at(index, size)
                    .map_err(|e| WalletError::InvalidInput(e.to_string()))?,
            );
        }

        // Where each shown row belongs: its settlement entry's booking day.
        let ids: Vec<Uuid> = rows.iter().map(|row| row.entry_id).collect();
        let ledger = format!("ledger_{}", tenant_id.replace('"', "\"\""));
        let dated: Vec<(Uuid, time::Date)> = sqlx::query_as(&format!(
            "SELECT entry_id, booking_date FROM \"{ledger}\".entries \
             WHERE entry_id = ANY($1)"
        ))
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        let date_by_id: HashMap<Uuid, time::Date> = dated.into_iter().collect();
        let mut positions_by_day: BTreeMap<time::Date, Vec<usize>> = BTreeMap::new();
        for (position, row) in rows.iter().enumerate() {
            let Some(day) = date_by_id.get(&row.entry_id) else {
                return Err(WalletError::InvalidInput(
                    "a usage-report row's entry cannot be dated".into(),
                ));
            };
            positions_by_day.entry(*day).or_default().push(position);
        }
        // A row whose day has no leaf (history before the day log existed)
        // cannot be covered honestly; the report stays claim-only.
        let covered: usize = positions_by_day
            .keys()
            .filter(|day| customer_days.iter().any(|(d, _)| d == *day))
            .map(|day| positions_by_day.get(day).map_or(0, Vec::len))
            .sum();
        if covered != rows.len() {
            return Ok(None);
        }

        let wallet = Tenants::new(self.pool().clone()).get(tenant_id).await?;
        let anchor_entry = wallet
            .entry_bundle(EntryId::from_uuid(anchor_entry_id), head)
            .await?;

        let mut days = Vec::with_capacity(customer_days.len());
        for (index, (day, leaf)) in customer_days.iter().enumerate() {
            days.push(CompletenessDay {
                day: day.to_string(),
                index: index as u64,
                leaf: leaf.to_hex(),
                proof: day_proofs[index].clone(),
                rows: positions_by_day.get(day).cloned().unwrap_or_default(),
            });
        }

        Ok(Some(CompletenessBundle {
            v: 1,
            anchor: CompletenessAnchor {
                entry: anchor_entry,
                day: anchor_day.to_string(),
                map_root: stored_root.to_hex(),
                enduser_count: enduser_count.max(0) as u64,
            },
            head: CompletenessHead {
                root: customer_head.root.to_hex(),
                size,
            },
            map_proof,
            days,
        }))
    }
}

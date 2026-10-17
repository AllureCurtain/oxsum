//! End-user signed usage reports (issue #261).
//!
//! An organization bills its own downstream customers out of the same ledger
//! that mixes every customer's usage — so an `endUser` cannot verify a bill by
//! reading the ledger. A usage report is the bundle the org hands that
//! customer: the settlement entries attributed to them over `[from, to]`, each
//! with its own inclusion proof, plus the one marker the org committed into
//! its ledger for that span.
//!
//! The hard part a proof has to answer is *completeness*, which a Merkle
//! inclusion proof cannot give: it proves membership, never that the subset is
//! every row. The honest shape is a marker — a self-balancing 1-minor
//! `Equity:Adjustments` entry, the same shape `anchor_statement` uses — whose
//! `DocumentRef` binds the subset digest (`oxsum/usage-report/v1` folded over
//! the shown entries' content hashes in log order) and whose description
//! commits `{endUser, from, to, entryCount, chargedTotalMinor, subsetHash}`.
//! One marker per `(tenant, endUser, from, to)`, ever: the marker's
//! idempotency key is derived from the span, and `usage_reports` enforces the
//! same uniqueness, so a second request replays the standing report rather
//! than committing a second subset. The frozen `usage_report_rows` are what a
//! re-fetch serves — never a re-query — so a report cannot grow after mint to
//! absorb usage the org omitted.
//!
//! What this proves and what it cannot is recorded in `docs/decisions.md`: the
//! subset is the org's signed claim of "all the usage for `endUser` over the
//! span", and the customer detects an omission as a count or total mismatch
//! against their own request log — the marker makes that claim checkable, it
//! does not make it provable.

use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::WalletError;
use crate::heads::{UsageReportV1, canonical_usage_report, usage_report_subset_hash};
use crate::wallet::{SCALE, Wallet};
use doubleentry::{Balanced, Entry, InclusionProof};

/// One entry in a usage report's bundle — the entry itself, its recorded
/// content hash, and its inclusion proof into the bundle's head. The route
/// serializes this verbatim into the report's `marker`/`entries` wire shape.
#[derive(Debug)]
pub struct ReportEntryBundle {
    /// The entry, balanced — serialized as doubleentry's wire encoding.
    pub entry: Entry<Balanced, SCALE>,
    /// The content hash the Merkle log commits, lowercase hex — what the
    /// verifier re-derives with `adopt_verified` before trusting the entry.
    pub content_hash: String,
    /// The entry's inclusion proof into the report's head.
    pub proof: InclusionProof,
}

/// One entry a usage report's subset shows — enough for the bundle's per-entry
/// proof and the fold the subset digest commits.
#[derive(Debug, Clone)]
pub struct UsageReportRow {
    /// The settlement entry's id.
    pub entry_id: Uuid,
    /// Its position in the log — the order the digest pins.
    pub log_index: i64,
    /// Its content hash, lowercase hex as the Merkle log commits it.
    pub content_hash: String,
    /// Its inclusion proof, served on the bundle. Filled by the caller.
    pub proof: Option<serde_json::Value>,
}

/// A minted usage report as the API answers it — the descriptor the customer
/// reads beside the bundle.
#[derive(Debug, Clone)]
pub struct UsageReport {
    /// The marker entry's id — `GET /api/v1/entries/{id}/proof` resolves it.
    pub report_id: Uuid,
    /// The organization the report belongs to.
    pub organization_id: Uuid,
    /// The end user the subset is attributed to.
    pub end_user: String,
    /// First UTC day of the span, inclusive.
    pub from_date: time::Date,
    /// Last UTC day of the span, inclusive.
    pub to_date: time::Date,
    /// Settlements the subset commits.
    pub entry_count: i64,
    /// The subset's summed settled charge, minor units.
    pub charged_total_minor: i64,
    /// The subset digest the marker's `DocumentRef` binds.
    pub subset_hash: String,
    /// The marker's position in the log.
    pub anchor_log_index: i64,
    /// The shown subset, in log order.
    pub rows: Vec<UsageReportRow>,
}

impl crate::Db {
    /// The settlement entries attributed to `end_user` in `[from, to]`, in log
    /// order — the subset a report commits. `booking_date` periodizes, the same
    /// basis a statement's window reads, so a report and a statement count a
    /// turn on the same day.
    ///
    /// `status = 'settled'` is the only usage a report commits: it is the charge
    /// the customer was billed, and the marker's once-per-span rule wants the
    /// subset settled before the report commits.
    async fn usage_report_subset(
        &self,
        tenant_id: &str,
        end_user: &str,
        from: time::Date,
        to: time::Date,
    ) -> Result<Vec<UsageReportRow>, WalletError> {
        use sqlx::Row;
        let rows = sqlx::query(&format!(
            "SELECT u.entry_id, e.log_index, e.content_hash \
             FROM oxsum.usage_records u \
             JOIN ledger_{tenant_id}.entries e ON e.entry_id = u.entry_id \
             WHERE u.tenant_id = $1 AND u.end_user = $2 \
               AND e.booking_date >= $3 AND e.booking_date <= $4 \
               AND u.status = 'settled' \
             ORDER BY e.log_index"
        ))
        .bind(tenant_id)
        .bind(end_user)
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let hash_bytes: Vec<u8> = row.try_get("content_hash")?;
            let array: [u8; 32] = hash_bytes.as_slice().try_into().map_err(|_| {
                WalletError::InvalidInput("a shown content hash is not 32 bytes".into())
            })?;
            out.push(UsageReportRow {
                entry_id: row.try_get("entry_id")?,
                log_index: row.try_get("log_index")?,
                content_hash: crate::Hash::from_bytes(array).to_hex(),
                proof: None,
            });
        }
        Ok(out)
    }

    /// Mints a usage report for `end_user` over `[from, to]` — or replays the
    /// standing one: the marker's ledger idempotency and the table's UNIQUE on
    /// the span make one report per `(tenant, endUser, span)` ever.
    ///
    /// The span must be fully ended — `to` before today, the same discipline a
    /// statement applies to its month — because the marker attests a complete
    /// subset, and an open day can still book usage the report would omit.
    /// An empty subset is refused: a report commits something, and an org with
    /// no usage for the user has nothing to attest.
    ///
    /// `wallet` is the organization's open ledger. The marker is appended and
    /// the descriptor and rows written in the same transaction, so a minted
    /// report either lands whole or not at all — the bundle a later read
    /// serves is the bundle the ledger committed.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a malformed span, an open `to`, an
    /// empty subset, or an `end_user` the shape forbids; storage and ledger
    /// failures surface as [`WalletError`].
    pub async fn mint_usage_report(
        &self,
        wallet: &Wallet,
        organization_id: Uuid,
        tenant_id: &str,
        end_user: &str,
        from: time::Date,
        to: time::Date,
    ) -> Result<UsageReport, WalletError> {
        if end_user.is_empty() || end_user.len() > crate::usage::MAX_END_USER {
            return Err(WalletError::InvalidInput(
                "endUser is required and at most 128 characters".into(),
            ));
        }
        if from > to {
            return Err(WalletError::InvalidInput(
                "`from` must not be after `to`".into(),
            ));
        }
        let today = OffsetDateTime::now_utc().date();
        if to >= today {
            return Err(WalletError::InvalidInput(
                "`to` must be a fully ended day — a report attests a complete subset".into(),
            ));
        }

        let rows = self
            .usage_report_subset(tenant_id, end_user, from, to)
            .await?;
        if rows.is_empty() {
            return Err(WalletError::InvalidInput(
                "the organization has no settled usage for this endUser over the span".into(),
            ));
        }
        let entry_count = rows.len() as i64;
        let charged_total_minor = self
            .usage_report_charged_total(tenant_id, end_user, from, to)
            .await?;

        // The subset digest and the canonical commitment the marker's
        // description carries — the two bind each other, not just the entry.
        let hashes: Vec<crate::Hash> = rows
            .iter()
            .map(|row| {
                crate::Hash::parse_hex(&row.content_hash).map_err(|_| {
                    WalletError::InvalidInput("a shown content hash is not hex".into())
                })
            })
            .collect::<Result<_, WalletError>>()?;
        let subset_hash = usage_report_subset_hash(&hashes);
        let from_str = from.to_string();
        let to_str = to.to_string();
        let description = canonical_usage_report(&UsageReportV1 {
            v: 1,
            end_user,
            from: &from_str,
            to: &to_str,
            entry_count,
            charged_total_minor,
            subset_hash: &subset_hash.to_hex(),
        })?;

        // The marker — appended once per span. A re-request reads back the
        // standing entry; the UNIQUE on the table is the same rule.
        let (marker_id, log_index) = wallet
            .anchor_usage_report(end_user, &from_str, &to_str, &description, subset_hash)
            .await?;

        // Persist the descriptor and the frozen subset in the one transaction,
        // so the bundle a later read serves is the bundle the ledger committed.
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO oxsum.usage_reports \
             (report_id, organization_id, tenant_id, end_user, from_date, to_date, \
              entry_count, charged_total_minor, subset_hash, anchor_log_index) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (tenant_id, end_user, from_date, to_date) DO NOTHING",
        )
        .bind(marker_id.as_uuid())
        .bind(organization_id)
        .bind(tenant_id)
        .bind(end_user)
        .bind(from)
        .bind(to)
        .bind(entry_count)
        .bind(charged_total_minor)
        .bind(subset_hash.to_hex())
        .bind(log_index as i64)
        .execute(&mut *tx)
        .await?;
        for (seq, row) in rows.iter().enumerate() {
            sqlx::query(
                "INSERT INTO oxsum.usage_report_rows (report_id, seq, entry_id, content_hash, log_index) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (report_id, seq) DO NOTHING",
            )
            .bind(marker_id.as_uuid())
            .bind(seq as i32)
            .bind(row.entry_id)
            .bind(&row.content_hash)
            .bind(row.log_index)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        Ok(UsageReport {
            report_id: *marker_id.as_uuid(),
            organization_id,
            end_user: end_user.to_owned(),
            from_date: from,
            to_date: to,
            entry_count,
            charged_total_minor,
            subset_hash: subset_hash.to_hex(),
            anchor_log_index: log_index as i64,
            rows,
        })
    }

    /// The summed settled charge over the subset — what the marker's
    /// `chargedTotalMinor` commits. Read beside the rows, not from them: the
    /// figure is the customer's check that no shown row was dropped cheaply.
    async fn usage_report_charged_total(
        &self,
        tenant_id: &str,
        end_user: &str,
        from: time::Date,
        to: time::Date,
    ) -> Result<i64, WalletError> {
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT COALESCE(SUM(u.charged_minor), 0)::bigint \
             FROM oxsum.usage_records u \
             JOIN ledger_{tenant_id}.entries e ON e.entry_id = u.entry_id \
             WHERE u.tenant_id = $1 AND u.end_user = $2 \
               AND e.booking_date >= $3 AND e.booking_date <= $4 \
               AND u.status = 'settled'"
        ))
        .bind(tenant_id)
        .bind(end_user)
        .bind(from)
        .bind(to)
        .fetch_one(self.pool())
        .await?;
        Ok(total)
    }

    /// Re-fetches a minted report by its marker id — the descriptor and the
    /// frozen subset, never a re-query. `None` when the id is no report.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn usage_report(&self, report_id: Uuid) -> Result<Option<UsageReport>, WalletError> {
        use sqlx::Row;
        let Some(head) = sqlx::query(
            "SELECT organization_id, end_user, from_date, to_date, entry_count, \
                    charged_total_minor, subset_hash, anchor_log_index \
             FROM oxsum.usage_reports WHERE report_id = $1",
        )
        .bind(report_id)
        .fetch_optional(self.pool())
        .await?
        else {
            return Ok(None);
        };
        let rows = sqlx::query(
            "SELECT entry_id, log_index, content_hash FROM oxsum.usage_report_rows \
             WHERE report_id = $1 ORDER BY seq",
        )
        .bind(report_id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|row| {
            Ok(UsageReportRow {
                entry_id: row.try_get("entry_id")?,
                log_index: row.try_get("log_index")?,
                content_hash: row.try_get("content_hash")?,
                proof: None,
            })
        })
        .collect::<Result<Vec<_>, WalletError>>()?;
        Ok(Some(UsageReport {
            report_id,
            organization_id: head.try_get("organization_id")?,
            end_user: head.try_get("end_user")?,
            from_date: head.try_get("from_date")?,
            to_date: head.try_get("to_date")?,
            entry_count: head.try_get("entry_count")?,
            charged_total_minor: head.try_get("charged_total_minor")?,
            subset_hash: head.try_get("subset_hash")?,
            anchor_log_index: head.try_get("anchor_log_index")?,
            rows,
        }))
    }

    /// The usage reports an organization has minted, newest first — the list a
    /// member's dashboard browses. Light on rows: a mint is rare, so the list
    /// reads without the subset until the bundle is asked for.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn usage_reports_of(
        &self,
        organization_id: Uuid,
    ) -> Result<Vec<UsageReport>, WalletError> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT report_id, end_user, from_date, to_date, entry_count, \
                    charged_total_minor, subset_hash, anchor_log_index \
             FROM oxsum.usage_reports WHERE organization_id = $1 ORDER BY created_at DESC",
        )
        .bind(organization_id)
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            out.push(UsageReport {
                report_id: row.try_get("report_id")?,
                organization_id,
                end_user: row.try_get("end_user")?,
                from_date: row.try_get("from_date")?,
                to_date: row.try_get("to_date")?,
                entry_count: row.try_get("entry_count")?,
                charged_total_minor: row.try_get("charged_total_minor")?,
                subset_hash: row.try_get("subset_hash")?,
                anchor_log_index: row.try_get("anchor_log_index")?,
                rows: Vec::new(),
            });
        }
        Ok(out)
    }
}

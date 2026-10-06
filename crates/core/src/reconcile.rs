//! Reconciliation: the read that checks the books against the projections.
//!
//! oxsum keeps two kinds of truth. The tenant ledgers are the source of truth for
//! money — append-only, hash-chained, sealed monthly. Beside them sit mutable
//! projections written for speed and retention: `usage_records` for the normalized
//! turn, `deposits` for money arriving over a rail, `open_holds` for the sweeper's
//! watch. Every projection claims something about the ledger; this module asks
//! whether the ledger agrees, in both directions, and answers one [`DriftClass`]
//! per way the two can disagree.
//!
//! The report is deliberately read-only. A reconciler that patches the books it
//! finds divergent is a second writer nobody asked for, and a silent repair hides
//! the bug that caused the divergence; drift lands here for an operator to read.
//! Comparing against upstream's own bills is out of scope for the same reason the
//! margin view carries `untrackedTurns`: no upstream rail exposes a bill to check
//! — `upstream_cost_minor` stays the estimate.
//!
//! The eight classes, as [`DriftKind`] names them:
//!
//! - `UsageOrphans` — a usage row whose settlement entry is not on the books
//!   (including rows naming a tenant whose ledger does not exist)
//! - `SettlementsUnrecorded` — a settlement entry (`{"v":…,"request":…}`) that
//!   wrote no usage row: the requests page, the rollup and the margin view all
//!   miss a charge that happened
//! - `DepositsUnbooked` — a deposit marked `credited` whose ledger entry is
//!   absent: money claimed credited with no entry behind it
//! - `DepositsMismatched` — `received_minor` differs from `amount_minor`: the
//!   rail paid more or less than the deposit expected (reconciles by hand, per
//!   the deposits decision)
//! - `DepositsStuck` — `confirmed` past the grace window without reaching
//!   `credited` or `reversed`: the rail confirmed, the wallet never moved
//! - `WatchesOrphaned` — a watch row whose hold has no live pending reservation
//!   in the ledger (a stale row the sweeper will keep tripping over)
//! - `HoldsUnwatched` — a live pending `req-*:hold` with no watch row:
//!   invisible to the sweeper, so the freeze can never be collected
//! - `LogGaps` — a log whose `log_index` positions are not dense from zero, or
//!   entries still unsequenced past the grace window

use serde::Serialize;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::WalletError;
use crate::wallet::{entry_id_for, settlement_key_for};

/// How many identifiers one class carries as a sample; `count` still reports the
/// full drift when the sample truncates.
const SAMPLE_LIMIT: i64 = 20;

/// One way the projections and the books can disagree, as the contract names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    UsageOrphans,
    SettlementsUnrecorded,
    DepositsUnbooked,
    DepositsMismatched,
    DepositsStuck,
    WatchesOrphaned,
    HoldsUnwatched,
    LogGaps,
}

/// The classes, in the fixed order the report answers them.
const DRIFT_KINDS: [DriftKind; 8] = [
    DriftKind::UsageOrphans,
    DriftKind::SettlementsUnrecorded,
    DriftKind::DepositsUnbooked,
    DriftKind::DepositsMismatched,
    DriftKind::DepositsStuck,
    DriftKind::WatchesOrphaned,
    DriftKind::HoldsUnwatched,
    DriftKind::LogGaps,
];

/// One offending identifier an operator follows — the request id, the hold key,
/// the deposit's `rail:payment_ref`, or a gap summary.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftSample {
    /// The organization the drift belongs to, or the raw tenant id when the
    /// organization no longer exists.
    pub organization: String,
    /// The identifier to look up.
    pub detail: String,
}

/// One drift class: how much drift it holds and a bounded sample of it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftClass {
    pub class: DriftKind,
    /// The full count, whatever the sample truncated.
    pub count: i64,
    /// Up to [`SAMPLE_LIMIT`] offenders, newest first.
    pub samples: Vec<DriftSample>,
}

/// The whole report, as `GET /api/v1/admin/reconciliation` answers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reconciliation {
    /// When the scan ran.
    #[serde(with = "time::serde::rfc3339")]
    pub checked_at: OffsetDateTime,
    /// The organizations whose ledgers were scanned.
    pub organizations: i64,
    /// Every class counted zero.
    pub clean: bool,
    /// Every drift class, zero-count ones included, in [`DRIFT_KINDS`] order.
    pub classes: Vec<DriftClass>,
}

/// One organization's identity for the scan.
struct Tenant {
    /// `deposits` and `organizations` key on this; `open_holds` and
    /// `usage_records` key on the tenant id.
    organization_id: Uuid,
    /// The organization's display name — the sample's `organization`.
    name: String,
    /// Its ledger tenant id; `ledger_<tenant_id>` is the schema.
    tenant_id: String,
}

/// The report's accumulator: one slot per kind, filled by the queries below.
struct Classes(Vec<DriftClass>);

impl Classes {
    fn new() -> Self {
        Self(
            DRIFT_KINDS
                .iter()
                .map(|&kind| DriftClass {
                    class: kind,
                    count: 0,
                    samples: Vec::new(),
                })
                .collect(),
        )
    }

    fn class(&mut self, kind: DriftKind) -> &mut DriftClass {
        // DRIFT_KINDS is the Vec's order; the discriminant indexes the slot.
        &mut self.0[kind as usize]
    }

    /// Folds one sampled query into a class: `total` is the window-function
    /// count the query carried on every row, `details` the sampled identifiers.
    fn fold(&mut self, kind: DriftKind, organization: &str, rows: &[sqlx::postgres::PgRow]) {
        if let Some(first) = rows.first() {
            self.class(kind).count += first.get::<i64, _>("total");
        }
        let class = self.class(kind);
        for row in rows {
            if class.samples.len() >= SAMPLE_LIMIT as usize {
                break;
            }
            class.samples.push(DriftSample {
                organization: organization.to_owned(),
                detail: row.get::<String, _>("detail"),
            });
        }
    }
}

/// Builds the schema-qualified FROM clause with the tenant id escaped — the
/// same quoting the rollup applies.
fn entries(schema: &str) -> String {
    format!("\"{schema}\".entries")
}

impl Db {
    /// Runs the reconciliation scan over every organization's ledger and answers
    /// the drift report. Read-only by design — see the module docs.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn reconcile(&self) -> Result<Reconciliation, WalletError> {
        let tenants = self.reconcile_tenants().await?;
        let mut classes = Classes::new();

        // Usage rows naming a tenant no organization claims — a deleted
        // organization, a synthetic id — are orphans by definition: the bill
        // they describe has no books to live in. The raw tenant id stands in
        // for the organization name.
        let known: Vec<&str> = tenants.iter().map(|t| t.tenant_id.as_str()).collect();
        let orphans = sqlx::query(
            "SELECT tenant_id, request_id, count(*) OVER() AS total \
             FROM oxsum.usage_records \
             WHERE NOT (tenant_id = ANY($1)) \
             ORDER BY settled_at DESC LIMIT $2",
        )
        .bind(&known)
        .bind(SAMPLE_LIMIT)
        .fetch_all(self.pool())
        .await?;
        if let Some(first) = orphans.first() {
            classes.class(DriftKind::UsageOrphans).count += first.get::<i64, _>("total");
        }
        for row in &orphans {
            let class = classes.class(DriftKind::UsageOrphans);
            if class.samples.len() >= SAMPLE_LIMIT as usize {
                break;
            }
            class.samples.push(DriftSample {
                organization: row.get("tenant_id"),
                detail: row.get("request_id"),
            });
        }

        for tenant in &tenants {
            let schema = format!("ledger_{}", tenant.tenant_id.replace('"', "\"\""));
            let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
                .bind(format!("{schema}.entries"))
                .fetch_one(self.pool())
                .await?;
            match exists {
                Some(_) => self.reconcile_tenant(tenant, &schema, &mut classes).await?,
                None => self.tenant_without_ledger(tenant, &mut classes).await?,
            }
        }

        // The deposit classes that need no ledger read run once across all rows.
        self.deposits_unbooked(&tenants, &mut classes).await?;
        self.deposits_mismatched(&mut classes).await?;
        self.deposits_stuck(&mut classes).await?;

        let classes = classes.0;
        let clean = classes.iter().all(|class| class.count == 0);
        Ok(Reconciliation {
            checked_at: OffsetDateTime::now_utc(),
            organizations: tenants.len() as i64,
            clean,
            classes,
        })
    }

    /// `(organization_id, name, tenant_id)` of every organization — the scan's
    /// tenant list.
    async fn reconcile_tenants(&self) -> Result<Vec<Tenant>, WalletError> {
        let rows = sqlx::query(
            "SELECT organization_id, name, tenant_id \
             FROM oxsum.organizations ORDER BY created_at",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| {
                Ok(Tenant {
                    organization_id: row.try_get("organization_id")?,
                    name: row.try_get("name")?,
                    tenant_id: row.try_get("tenant_id")?,
                })
            })
            .collect()
    }

    /// The per-tenant checks that read the ledger.
    async fn reconcile_tenant(
        &self,
        tenant: &Tenant,
        schema: &str,
        classes: &mut Classes,
    ) -> Result<(), WalletError> {
        // A usage row whose settlement entry is not on the books.
        let sql = format!(
            "SELECT request_id AS detail, count(*) OVER() AS total \
             FROM oxsum.usage_records u \
             WHERE u.tenant_id = $1 \
               AND NOT EXISTS (SELECT 1 FROM {e} e WHERE e.entry_id = u.entry_id) \
             ORDER BY u.settled_at DESC LIMIT $2",
            e = entries(schema)
        );
        let rows = sqlx::query(&sql)
            .bind(&tenant.tenant_id)
            .bind(SAMPLE_LIMIT)
            .fetch_all(self.pool())
            .await?;
        classes.fold(DriftKind::UsageOrphans, &tenant.name, &rows);

        // A settlement entry that wrote no usage row. The request id is read
        // with a regex, not a `::jsonb` cast: the planner may evaluate the cast
        // before the LIKE guard, and a non-JSON description would fail the scan.
        let sql = format!(
            "SELECT substring(e.description from '\"request\":\"([^\"]+)\"') || \
                    ' (entry ' || e.entry_id || ')' AS detail, count(*) OVER() AS total \
             FROM {e} e \
             WHERE e.description LIKE '{{\"v\":%' \
               AND substring(e.description from '\"request\":\"([^\"]+)\"') IS NOT NULL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM oxsum.usage_records u \
                 WHERE u.request_id = substring(e.description from '\"request\":\"([^\"]+)\"')) \
             ORDER BY e.recorded_at DESC LIMIT $1",
            e = entries(schema)
        );
        let rows = sqlx::query(&sql)
            .bind(SAMPLE_LIMIT)
            .fetch_all(self.pool())
            .await?;
        classes.fold(DriftKind::SettlementsUnrecorded, &tenant.name, &rows);

        // Hold liveness. A hold entry's pending postings stay pending-layer
        // forever — the release legs live on the settlement entry — so "live"
        // is not a postings question but an existence one: the hold is open
        // while its settlement entry is absent. The settlement's key derives
        // from the hold key (`settlement_key_for`), which only Rust can
        // compute, so the candidates fetch in one pass and the settle-entry
        // check runs batched by entry id.
        let watch_rows: Vec<String> =
            sqlx::query_scalar("SELECT hold_key FROM oxsum.open_holds WHERE tenant_id = $1")
                .bind(&tenant.tenant_id)
                .fetch_all(self.pool())
                .await?;
        let hold_ids: Vec<Uuid> = watch_rows
            .iter()
            .map(|k| *entry_id_for(k).as_uuid())
            .collect();
        let settle_ids: Vec<Uuid> = watch_rows
            .iter()
            .map(|k| *entry_id_for(&settlement_key_for(k)).as_uuid())
            .collect();
        let sql = format!(
            "SELECT entry_id FROM {e} WHERE entry_id = ANY($1)",
            e = entries(schema)
        );
        let present: Vec<Uuid> = sqlx::query_scalar(&sql)
            .bind(&hold_ids)
            .fetch_all(self.pool())
            .await?;
        let settled: Vec<Uuid> = sqlx::query_scalar(&sql)
            .bind(&settle_ids)
            .fetch_all(self.pool())
            .await?;
        // A stale watch: no hold behind it, or the hold already released.
        let mut orphaned: Vec<String> = Vec::new();
        for (i, key) in watch_rows.iter().enumerate() {
            if !present.contains(&hold_ids[i]) || settled.contains(&settle_ids[i]) {
                orphaned.push(key.clone());
            }
        }
        let watches = classes.class(DriftKind::WatchesOrphaned);
        watches.count += orphaned.len() as i64;
        for key in orphaned
            .into_iter()
            .take(SAMPLE_LIMIT as usize - watches.samples.len())
        {
            watches.samples.push(DriftSample {
                organization: tenant.name.clone(),
                detail: key,
            });
        }

        // Gateway holds — `req-*:hold` keys — are what the watch table covers.
        // Manual API holds carry caller-chosen keys and stay intentionally
        // unwatched.
        let sql = format!(
            "SELECT convert_from(e.idempotency_key, 'UTF8') AS hold_key \
             FROM {e} e \
             WHERE convert_from(e.idempotency_key, 'UTF8') LIKE 'req-%:hold'",
            e = entries(schema)
        );
        let candidates: Vec<String> = sqlx::query_scalar(&sql).fetch_all(self.pool()).await?;
        let settle_ids: Vec<Uuid> = candidates
            .iter()
            .map(|k| *entry_id_for(&settlement_key_for(k)).as_uuid())
            .collect();
        let sql = format!(
            "SELECT entry_id FROM {e} WHERE entry_id = ANY($1)",
            e = entries(schema)
        );
        let settled: Vec<Uuid> = sqlx::query_scalar(&sql)
            .bind(&settle_ids)
            .fetch_all(self.pool())
            .await?;
        for (i, key) in candidates.iter().enumerate() {
            if settled.contains(&settle_ids[i]) || watch_rows.contains(key) {
                continue;
            }
            let class = classes.class(DriftKind::HoldsUnwatched);
            class.count += 1;
            if class.samples.len() < SAMPLE_LIMIT as usize {
                class.samples.push(DriftSample {
                    organization: tenant.name.clone(),
                    detail: key.clone(),
                });
            }
        }

        // The log's positions are dense from zero: a gap means the sequencer
        // stepped over an entry; a long-unsequenced one means it stopped.
        let sql = format!(
            "SELECT count(*) FILTER (WHERE log_index IS NOT NULL) AS sequenced, \
                    max(log_index) AS top, \
                    count(*) FILTER (WHERE log_index IS NULL \
                                     AND recorded_at < now() - interval '1 hour') AS stale \
             FROM {e}",
            e = entries(schema)
        );
        let row = sqlx::query(&sql).fetch_one(self.pool()).await?;
        let sequenced: i64 = row.try_get("sequenced")?;
        let top: Option<i64> = row.try_get("top")?;
        let stale: i64 = row.try_get("stale")?;
        let missing = top.map_or(0, |top| top + 1 - sequenced) + stale;
        if missing > 0 {
            let class = classes.class(DriftKind::LogGaps);
            class.count += missing;
            if class.samples.len() < SAMPLE_LIMIT as usize {
                class.samples.push(DriftSample {
                    organization: tenant.name.clone(),
                    detail: format!(
                        "{missing} missing ({sequenced} sequenced, {stale} stale unsequenced)"
                    ),
                });
            }
        }
        Ok(())
    }

    /// An organization whose ledger schema is absent: every claim its
    /// projections make is unbooked, and its watches orphan by definition.
    async fn tenant_without_ledger(
        &self,
        tenant: &Tenant,
        classes: &mut Classes,
    ) -> Result<(), WalletError> {
        let rows = sqlx::query(
            "SELECT request_id AS detail, count(*) OVER() AS total \
             FROM oxsum.usage_records WHERE tenant_id = $1 \
             ORDER BY settled_at DESC LIMIT $2",
        )
        .bind(&tenant.tenant_id)
        .bind(SAMPLE_LIMIT)
        .fetch_all(self.pool())
        .await?;
        classes.fold(DriftKind::UsageOrphans, &tenant.name, &rows);

        let rows = sqlx::query(
            "SELECT hold_key AS detail, count(*) OVER() AS total \
             FROM oxsum.open_holds WHERE tenant_id = $1 \
             ORDER BY opened_at DESC LIMIT $2",
        )
        .bind(&tenant.tenant_id)
        .bind(SAMPLE_LIMIT)
        .fetch_all(self.pool())
        .await?;
        classes.fold(DriftKind::WatchesOrphaned, &tenant.name, &rows);
        // Credited deposits on a missing ledger land in `deposits_unbooked`,
        // which runs once across all rows below — nothing to do here.
        Ok(())
    }

    /// Credited deposits whose entry is missing — or was never recorded. A
    /// deposit on an organization whose ledger is absent counts as unbooked
    /// too: `entry_id` names an entry that cannot exist.
    async fn deposits_unbooked(
        &self,
        tenants: &[Tenant],
        classes: &mut Classes,
    ) -> Result<(), WalletError> {
        // Per organization, because the entry lookup is schema-qualified.
        for tenant in tenants {
            let schema = format!("ledger_{}", tenant.tenant_id.replace('"', "\"\""));
            let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
                .bind(format!("{schema}.entries"))
                .fetch_one(self.pool())
                .await?;
            let missing = match exists {
                Some(_) => format!(
                    "d.entry_id IS NULL OR NOT EXISTS \
                     (SELECT 1 FROM {} e WHERE e.entry_id = d.entry_id)",
                    entries(&schema)
                ),
                None => "TRUE".to_owned(),
            };
            let sql = format!(
                "SELECT d.rail || ':' || d.payment_ref AS detail, count(*) OVER() AS total \
                 FROM oxsum.deposits d \
                 WHERE d.organization_id = $1 AND d.status = 'credited' AND ({missing}) \
                 ORDER BY d.created_at DESC LIMIT $2"
            );
            let rows = sqlx::query(&sql)
                .bind(tenant.organization_id)
                .bind(SAMPLE_LIMIT)
                .fetch_all(self.pool())
                .await?;
            classes.fold(DriftKind::DepositsUnbooked, &tenant.name, &rows);
        }
        Ok(())
    }

    /// Credited deposits where the rail reported a different amount than the
    /// deposit expected — the under/over-payment the deposits table records.
    async fn deposits_mismatched(&self, classes: &mut Classes) -> Result<(), WalletError> {
        let rows = sqlx::query(
            "SELECT o.name AS organization, \
                    d.rail || ':' || d.payment_ref || \
                    ' (expected ' || d.amount_minor || ', received ' || d.received_minor || ')' \
                    AS detail, count(*) OVER() AS total \
             FROM oxsum.deposits d JOIN oxsum.organizations o \
               ON o.organization_id = d.organization_id \
             WHERE d.status = 'credited' \
               AND d.received_minor IS NOT NULL AND d.received_minor <> d.amount_minor \
             ORDER BY d.created_at DESC LIMIT $1",
        )
        .bind(SAMPLE_LIMIT)
        .fetch_all(self.pool())
        .await?;
        if let Some(first) = rows.first() {
            classes.class(DriftKind::DepositsMismatched).count +=
                first.try_get::<i64, _>("total")?;
        }
        for row in &rows {
            let class = classes.class(DriftKind::DepositsMismatched);
            if class.samples.len() >= SAMPLE_LIMIT as usize {
                break;
            }
            class.samples.push(DriftSample {
                organization: row.try_get("organization")?,
                detail: row.try_get("detail")?,
            });
        }
        Ok(())
    }

    /// Deposits the rail confirmed that never reached `credited` or `reversed`,
    /// past a short grace so a deposit mid-transition is not flagged.
    async fn deposits_stuck(&self, classes: &mut Classes) -> Result<(), WalletError> {
        let rows = sqlx::query(
            "SELECT o.name AS organization, d.rail || ':' || d.payment_ref AS detail, \
                    count(*) OVER() AS total \
             FROM oxsum.deposits d JOIN oxsum.organizations o \
               ON o.organization_id = d.organization_id \
             WHERE d.status = 'confirmed' \
               AND d.updated_at < now() - interval '5 minutes' \
             ORDER BY d.updated_at DESC LIMIT $1",
        )
        .bind(SAMPLE_LIMIT)
        .fetch_all(self.pool())
        .await?;
        if let Some(first) = rows.first() {
            classes.class(DriftKind::DepositsStuck).count += first.try_get::<i64, _>("total")?;
        }
        for row in &rows {
            let class = classes.class(DriftKind::DepositsStuck);
            if class.samples.len() >= SAMPLE_LIMIT as usize {
                break;
            }
            class.samples.push(DriftSample {
                organization: row.try_get("organization")?,
                detail: row.try_get("detail")?,
            });
        }
        Ok(())
    }
}

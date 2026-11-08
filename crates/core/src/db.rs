//! The process's database handle: one pool, plus oxsum's own tables.
//!
//! Two kinds of tables live in this database. A ledger schema per organization
//! (`ledger_<tenant_id>`) is created and versioned by doubleentry's `migrate`, which owns
//! its tables and is never edited by hand. Everything oxsum itself needs — users,
//! organizations, memberships, API keys — lives in the single `oxsum` schema, created and
//! versioned here. The two never mix.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::{Acquire, ConnectOptions, PgConnection, PgPool};

use crate::error::WalletError;
use crate::pools::SharedAffinity;
use crate::routing::SharedRouteStats;

/// The schema holding oxsum's own tables.
pub const SCHEMA: &str = "oxsum";

/// The migration files, in order. Embedded, so a binary carries its own schema.
///
/// Adding a migration means adding one line here and one file under `migrations/`. A
/// version is applied once, in one transaction, and recorded in `oxsum._migrations` in the
/// same transaction: a half-applied migration cannot exist, and a restart continues where
/// it stopped.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_identity",
        include_str!("../migrations/0001_identity.sql"),
    ),
    (
        "0002_channels",
        include_str!("../migrations/0002_channels.sql"),
    ),
    (
        "0003_open_holds",
        include_str!("../migrations/0003_open_holds.sql"),
    ),
    (
        "0004_sessions",
        include_str!("../migrations/0004_sessions.sql"),
    ),
    (
        "0005_key_spend_limits",
        include_str!("../migrations/0005_key_spend_limits.sql"),
    ),
    (
        "0006_invitations",
        include_str!("../migrations/0006_invitations.sql"),
    ),
    (
        "0007_usage_records",
        include_str!("../migrations/0007_usage_records.sql"),
    ),
    (
        "0008_channel_protocol",
        include_str!("../migrations/0008_channel_protocol.sql"),
    ),
    (
        "0009_itemized_prices",
        include_str!("../migrations/0009_itemized_prices.sql"),
    ),
    (
        "0010_upstream_cost",
        include_str!("../migrations/0010_upstream_cost.sql"),
    ),
    (
        "0011_deposits",
        include_str!("../migrations/0011_deposits.sql"),
    ),
    (
        "0012_key_constraints",
        include_str!("../migrations/0012_key_constraints.sql"),
    ),
    (
        "0013_statements",
        include_str!("../migrations/0013_statements.sql"),
    ),
    (
        "0014_usage_daily",
        include_str!("../migrations/0014_usage_daily.sql"),
    ),
    (
        "0015_rate_limits",
        include_str!("../migrations/0015_rate_limits.sql"),
    ),
    (
        "0016_idempotency_records",
        include_str!("../migrations/0016_idempotency_records.sql"),
    ),
    (
        "0017_dead_holds",
        include_str!("../migrations/0017_dead_holds.sql"),
    ),
    (
        "0018_webhooks",
        include_str!("../migrations/0018_webhooks.sql"),
    ),
    (
        "0019_email_tokens",
        include_str!("../migrations/0019_email_tokens.sql"),
    ),
    ("0020_oauth", include_str!("../migrations/0020_oauth.sql")),
    (
        "0021_device_codes",
        include_str!("../migrations/0021_device_codes.sql"),
    ),
    (
        "0022_tiers_and_discounts",
        include_str!("../migrations/0022_tiers_and_discounts.sql"),
    ),
    (
        "0023_admin_audit",
        include_str!("../migrations/0023_admin_audit.sql"),
    ),
    (
        "0024_suspension_and_member_budgets",
        include_str!("../migrations/0024_suspension_and_member_budgets.sql"),
    ),
    ("0025_jobs", include_str!("../migrations/0025_jobs.sql")),
    (
        "0026_failover",
        include_str!("../migrations/0026_failover.sql"),
    ),
    (
        "0027_billing_modes",
        include_str!("../migrations/0027_billing_modes.sql"),
    ),
    (
        "0028_service_credentials",
        include_str!("../migrations/0028_service_credentials.sql"),
    ),
    (
        "0029_metering_attempts",
        include_str!("../migrations/0029_metering_attempts.sql"),
    ),
    (
        "0030_webhook_balance_low",
        include_str!("../migrations/0030_webhook_balance_low.sql"),
    ),
    (
        "0031_tpm_rate_limits",
        include_str!("../migrations/0031_tpm_rate_limits.sql"),
    ),
    (
        "0032_tpm_tag_scopes",
        include_str!("../migrations/0032_tpm_tag_scopes.sql"),
    ),
    (
        "0033_budget_exhaustion_forecast",
        include_str!("../migrations/0033_budget_exhaustion_forecast.sql"),
    ),
    (
        "0034_statement_anchor",
        include_str!("../migrations/0034_statement_anchor.sql"),
    ),
    (
        "0035_upstream_price_drift",
        include_str!("../migrations/0035_upstream_price_drift.sql"),
    ),
    (
        "0037_usage_outcome",
        include_str!("../migrations/0037_usage_outcome.sql"),
    ),
    (
        "0038_usage_count_provenance",
        include_str!("../migrations/0038_usage_count_provenance.sql"),
    ),
    (
        "0039_budget_thresholds",
        include_str!("../migrations/0039_budget_thresholds.sql"),
    ),
    (
        "0040_witness_checkpoints",
        include_str!("../migrations/0040_witness_checkpoints.sql"),
    ),
    (
        "0041_mandates",
        include_str!("../migrations/0041_mandates.sql"),
    ),
    (
        "0042_dunning",
        include_str!("../migrations/0042_dunning.sql"),
    ),
    (
        "0043_display_currency",
        include_str!("../migrations/0043_display_currency.sql"),
    ),
    (
        "0044_witness_served",
        include_str!("../migrations/0044_witness_served.sql"),
    ),
    (
        "0045_usage_reports",
        include_str!("../migrations/0045_usage_reports.sql"),
    ),
    (
        "0046_x402_rail",
        include_str!("../migrations/0046_x402_rail.sql"),
    ),
    (
        "0047_job_leader",
        include_str!("../migrations/0047_job_leader.sql"),
    ),
    (
        "0048_facilitator_tx_hash",
        include_str!("../migrations/0048_facilitator_tx_hash.sql"),
    ),
    (
        "0049_upstream_bills",
        include_str!("../migrations/0049_upstream_bills.sql"),
    ),
    (
        "0050_airwallex_rail",
        include_str!("../migrations/0050_airwallex_rail.sql"),
    ),
    (
        "0051_model_aliases",
        include_str!("../migrations/0051_model_aliases.sql"),
    ),
    (
        "0052_report_links",
        include_str!("../migrations/0052_report_links.sql"),
    ),
    (
        "0053_payment_events",
        include_str!("../migrations/0053_payment_events.sql"),
    ),
    (
        "0054_hyperswitch_rail",
        include_str!("../migrations/0054_hyperswitch_rail.sql"),
    ),
    (
        "0055_usage_day_log",
        include_str!("../migrations/0055_usage_day_log.sql"),
    ),
    (
        "0056_usage_anchors",
        include_str!("../migrations/0056_usage_anchors.sql"),
    ),
    (
        "0057_credit_notes",
        include_str!("../migrations/0057_credit_notes.sql"),
    ),
    (
        "0058_deposit_reversal",
        include_str!("../migrations/0058_deposit_reversal.sql"),
    ),
    ("0059_teams", include_str!("../migrations/0059_teams.sql")),
    (
        "0060_channel_credentials",
        include_str!("../migrations/0060_channel_credentials.sql"),
    ),
    (
        "0061_subscriptions",
        include_str!("../migrations/0061_subscriptions.sql"),
    ),
    ("0062_tasks", include_str!("../migrations/0062_tasks.sql")),
    (
        "0063_rate_windows",
        include_str!("../migrations/0063_rate_windows.sql"),
    ),
    (
        "0064_semantic_cache",
        include_str!("../migrations/0064_semantic_cache.sql"),
    ),
    (
        "0065_guardrails",
        include_str!("../migrations/0065_guardrails.sql"),
    ),
    (
        "0066_oidc_totp",
        include_str!("../migrations/0066_oidc_totp.sql"),
    ),
    (
        "0067_model_fallbacks",
        include_str!("../migrations/0067_model_fallbacks.sql"),
    ),
    (
        "0068_request_log",
        include_str!("../migrations/0068_request_log.sql"),
    ),
    (
        "0069_experiments",
        include_str!("../migrations/0069_experiments.sql"),
    ),
    (
        "0070_request_groups",
        include_str!("../migrations/0070_request_groups.sql"),
    ),
    (
        "0071_manual_receipts",
        include_str!("../migrations/0071_manual_receipts.sql"),
    ),
    (
        "0072_business_ownership",
        include_str!("../migrations/0072_business_ownership.sql"),
    ),
    (
        "0073_business_setup",
        include_str!("../migrations/0073_business_setup.sql"),
    ),
    (
        "0074_business_authority",
        include_str!("../migrations/0074_business_authority.sql"),
    ),
    (
        "0075_customer_lifecycle",
        include_str!("../migrations/0075_customer_lifecycle.sql"),
    ),
    (
        "0076_invitation_consent",
        include_str!("../migrations/0076_invitation_consent.sql"),
    ),
    (
        "0077_integration_credentials",
        include_str!("../migrations/0077_integration_credentials.sql"),
    ),
    (
        "0078_business_provider_mutations",
        include_str!("../migrations/0078_business_provider_mutations.sql"),
    ),
    (
        "0079_provider_destinations",
        include_str!("../migrations/0079_provider_destinations.sql"),
    ),
    (
        "0080_response_cache_v2",
        include_str!("../migrations/0080_response_cache_v2.sql"),
    ),
    (
        "0081_sale_books",
        include_str!("../migrations/0081_sale_books.sql"),
    ),
    (
        "0082_sale_snapshot_digests",
        include_str!("../migrations/0082_sale_snapshot_digests.sql"),
    ),
    (
        "0085_merchant_configuration",
        include_str!("../migrations/0085_merchant_configuration.sql"),
    ),
    (
        "0086_request_operations",
        include_str!("../migrations/0086_request_operations.sql"),
    ),
    (
        "0087_merchant_event_intake",
        include_str!("../migrations/0087_merchant_event_intake.sql"),
    ),
    (
        "0088_payment_orders",
        include_str!("../migrations/0088_payment_orders.sql"),
    ),
    (
        "0089_usage_cost_basis",
        include_str!("../migrations/0089_usage_cost_basis.sql"),
    ),
    (
        "0090_sold_plans",
        include_str!("../migrations/0090_sold_plans.sql"),
    ),
    (
        "0091_manual_payment_orders",
        include_str!("../migrations/0091_manual_payment_orders.sql"),
    ),
    (
        "0092_promo_lots",
        include_str!("../migrations/0092_promo_lots.sql"),
    ),
    (
        "0093_order_refunds",
        include_str!("../migrations/0093_order_refunds.sql"),
    ),
    (
        "0094_invoice_journals",
        include_str!("../migrations/0094_invoice_journals.sql"),
    ),
    (
        "0095_plan_statements",
        include_str!("../migrations/0095_plan_statements.sql"),
    ),
    (
        "0096_reconciliation",
        include_str!("../migrations/0096_reconciliation.sql"),
    ),
    (
        "0097_evidence_bundles",
        include_str!("../migrations/0097_evidence_bundles.sql"),
    ),
    (
        "0098_business_profile",
        include_str!("../migrations/0098_business_profile.sql"),
    ),
    (
        "0098_staff_invitations",
        include_str!("../migrations/0098_staff_invitations.sql"),
    ),
    (
        "0099_hyperswitch_recovery",
        include_str!("../migrations/0099_hyperswitch_recovery.sql"),
    ),
    (
        "0101_collection_terms",
        include_str!("../migrations/0101_collection_terms.sql"),
    ),
    (
        "0104_customer_invitation_delivery",
        include_str!("../migrations/0104_customer_invitation_delivery.sql"),
    ),
    (
        "0105_integration_provision",
        include_str!("../migrations/0105_integration_provision.sql"),
    ),
    (
        "0106_integration_billing_read",
        include_str!("../migrations/0106_integration_billing_read.sql"),
    ),
    (
        "0107_deposit_refund_recovery",
        include_str!("../migrations/0107_deposit_refund_recovery.sql"),
    ),
    (
        "0108_bundle_offers",
        include_str!("../migrations/0108_bundle_offers.sql"),
    ),
    (
        "0109_plan_payment_authority",
        include_str!("../migrations/0109_plan_payment_authority.sql"),
    ),
    (
        "0110_plan_refund_effects",
        include_str!("../migrations/0110_plan_refund_effects.sql"),
    ),
    (
        "0111_plan_usage_corrections",
        include_str!("../migrations/0111_plan_usage_corrections.sql"),
    ),
];

/// Advisory-lock key serialising the migration runner across processes.
///
/// Two server processes starting against one database would otherwise both pass the
/// "already applied?" check and race on `CREATE TABLE`. Database-wide, because the `oxsum`
/// schema is. Held on one session for the whole run, not per transaction: the point is that
/// only one process migrates at a time.
const MIGRATION_LOCK: i64 = i64::from_be_bytes(*b"oxsumidn");

/// One shared pool, shared by every tenant and every table.
#[derive(Clone)]
pub struct Db {
    pool: PgPool,
    /// The process-local conversation→credential memory (issue #444): a cache
    /// warmer for the pool's pick, `Arc`-shared across `Db`'s cheap clones.
    pub(crate) affinity: SharedAffinity,
    /// The process-local per-channel upstream-health window (issue #499): the
    /// routing hint `route_order` reads and the relay writes — `Arc`-shared
    /// across `Db`'s cheap clones so a request's pick and its attempt's sample
    /// meet in the one store.
    pub(crate) route_stats: SharedRouteStats,
}

/// How long a checkout may wait for a free connection before failing. See
/// `Db::connect`.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

impl Db {
    /// Connects the one pool this process uses for everything.
    ///
    /// `max_connections` is the process's whole connection budget, not a per-tenant or
    /// per-request allowance: each PostgreSQL connection is a backend process. See
    /// docs/decisions.md, "all tenants share one connection pool".
    ///
    /// The pool's slow-statement and slow-acquire WARN thresholds are raised from sqlx's
    /// defaults (1s / 2s), which a local Docker or a cold start routinely trips without
    /// meaning anything is wrong. 10 seconds is the point at which a statement or a
    /// pool wait is actually worth an operator's look — anything shorter is noise on a
    /// dev machine and still catches the real stall in production (issue #182).
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Self, WalletError> {
        let options: sqlx::postgres::PgConnectOptions = database_url.parse()?;
        let options = options
            .log_slow_statements(log::LevelFilter::Warn, Duration::from_secs(10))
            .application_name("oxsum");
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_slow_threshold(Duration::from_secs(10))
            // Bound how long a checkout may wait for a free connection: under
            // load the pool fills and requests must fail fast (surfacing as a
            // pool-timeout error the gateway maps) instead of queueing behind
            // each other with no bound. Deliberately shorter than sqlx's 30s
            // default — a checkout waiting this long means the pool, not the
            // query, is the bottleneck.
            .acquire_timeout(ACQUIRE_TIMEOUT)
            // A dropped connection gets detected on checkout, not on use: the
            // pool pings before handing it over, so a stale backend never serves
            // a query that then dies mid-flight.
            .test_before_acquire(true)
            .connect_with(options)
            .await?;
        Ok(Self {
            pool,
            affinity: Default::default(),
            route_stats: Default::default(),
        })
    }

    /// Wraps an already built pool. Tests use this; `connect` is the server's path.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            affinity: Default::default(),
            route_stats: Default::default(),
        }
    }

    /// The shared pool.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The latest schema version this binary carries — the last entry of
    /// `MIGRATIONS`, which the version endpoint reports as `schemaVersion`.
    #[must_use]
    pub fn latest_schema_version() -> &'static str {
        // MIGRATIONS is a compile-time constant and never empty.
        MIGRATIONS[MIGRATIONS.len() - 1].0
    }

    /// How far the database's migration ledger has run, for the readiness probe.
    ///
    /// `None` means the migration ledger itself is absent — a database nothing
    /// has ever migrated, which `/readyz` reports as not ready rather than as
    /// version zero: the schema was never created.
    ///
    /// # Errors
    ///
    /// Whatever the pool or the query fails with; the caller maps it to
    /// not-ready.
    pub async fn applied_schema_version(&self) -> Result<Option<String>, WalletError> {
        let ledger_exists: bool = sqlx::query_scalar(&format!(
            "SELECT to_regclass('{SCHEMA}._migrations') IS NOT NULL"
        ))
        .fetch_one(self.pool())
        .await?;
        if !ledger_exists {
            return Ok(None);
        }
        let latest: Option<String> =
            sqlx::query_scalar(&format!("SELECT max(version) FROM {SCHEMA}._migrations"))
                .fetch_one(self.pool())
                .await?;
        Ok(latest)
    }

    /// Whether any organization exists — the demo seed's guard: it only ever
    /// writes to a database nothing has used yet (issue #619).
    ///
    /// Answers `false` on an unmigrated database too, so the seed's ordering —
    /// check first, then migrate, then write — never has to special-case a
    /// schema that is not there yet.
    ///
    /// # Errors
    ///
    /// Whatever the pool or the query fails with.
    pub async fn any_organizations(&self) -> Result<bool, WalletError> {
        let table_exists: bool = sqlx::query_scalar(&format!(
            "SELECT to_regclass('{SCHEMA}.organizations') IS NOT NULL"
        ))
        .fetch_one(self.pool())
        .await?;
        if !table_exists {
            return Ok(false);
        }
        sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM {SCHEMA}.organizations)"
        ))
        .fetch_one(self.pool())
        .await
        .map_err(Into::into)
    }

    /// Creates the `oxsum` schema and applies every pending migration.
    ///
    /// Idempotent, and safe to run from several processes at once.
    pub async fn migrate(&self) -> Result<(), WalletError> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATION_LOCK)
            .execute(&mut *conn)
            .await?;
        let applied = apply_migrations(&mut conn).await;
        // Unlock before surfacing the migration's own result, so a failed migration does
        // not hand the connection back to the pool still locked.
        let unlocked = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK)
            .execute(&mut *conn)
            .await;
        applied?;
        unlocked?;
        Ok(())
    }
}

async fn apply_migrations(conn: &mut PgConnection) -> Result<(), WalletError> {
    // The schema and the record of applied versions come first: a migration cannot be
    // recorded in a table that does not exist yet. `SCHEMA` is a compile-time constant and
    // never caller input, so interpolating it here is not a parameterisation hole.
    let create_schema = format!("CREATE SCHEMA IF NOT EXISTS {SCHEMA}");
    sqlx::Executor::execute(&mut *conn, create_schema.as_str()).await?;
    let create_ledger = format!(
        "CREATE TABLE IF NOT EXISTS {SCHEMA}._migrations (\
             version    text        PRIMARY KEY, \
             name       text        NOT NULL, \
             applied_at timestamptz NOT NULL DEFAULT now())"
    );
    sqlx::Executor::execute(&mut *conn, create_ledger.as_str()).await?;

    let check = format!("SELECT EXISTS (SELECT 1 FROM {SCHEMA}._migrations WHERE version = $1)");
    let record = format!("INSERT INTO {SCHEMA}._migrations (version, name) VALUES ($1, $2)");

    for (version, sql) in MIGRATIONS {
        // One transaction per migration: it is applied and recorded together, or not at
        // all. The advisory lock is session-scoped and held by the caller, and the file
        // carries no transaction control of its own, so an enclosing transaction is safe.
        let mut tx = conn.begin().await?;
        // Migrations are written schema-qualified. Clearing `search_path` means an
        // unqualified name fails loudly instead of silently creating tables in `public`,
        // which is exactly the bug this line was added for. `pg_catalog` is always
        // searched, so built-in types and `now()` still resolve.
        sqlx::query("SET LOCAL search_path = ''")
            .execute(&mut *tx)
            .await?;
        let already: bool = sqlx::query_scalar(&check)
            .bind(version)
            .fetch_one(&mut *tx)
            .await?;
        if already {
            tx.rollback().await?;
            continue;
        }
        // A migration file is multi-statement SQL, so it runs through the simple query
        // protocol. The text is our own, embedded at compile time.
        sqlx::Executor::execute(&mut *tx, *sql).await?;
        sqlx::query(&record)
            .bind(version)
            .bind(version)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn migration_registry_matches_every_file_in_version_order() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut files: Vec<_> = std::fs::read_dir(&directory)
            .expect("migration directory exists")
            .map(|file| file.expect("migration file is readable").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
            .collect();
        files.sort();
        assert_eq!(files.len(), MIGRATIONS.len());
        for (file, (version, sql)) in files.iter().zip(MIGRATIONS) {
            assert_eq!(
                file.file_stem().and_then(|name| name.to_str()),
                Some(*version)
            );
            assert_eq!(
                std::fs::read_to_string(file).expect("migration SQL is readable"),
                *sql
            );
        }
        assert!(MIGRATIONS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(
            Db::latest_schema_version(),
            MIGRATIONS.last().expect("migrations exist").0
        );
    }
}

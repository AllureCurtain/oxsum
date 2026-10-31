//! The process's database handle: one pool, plus oxsum's own tables.
//!
//! Two kinds of tables live in this database. A ledger schema per organization
//! (`ledger_<tenant_id>`) is created and versioned by doubleentry's `migrate`, which owns
//! its tables and is never edited by hand. Everything oxsum itself needs — users,
//! organizations, memberships, API keys — lives in the single `oxsum` schema, created and
//! versioned here. The two never mix.

use sqlx::postgres::PgPoolOptions;
use sqlx::{Acquire, ConnectOptions, PgConnection, PgPool};

use crate::error::WalletError;

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
    pub(crate) affinity: crate::pools::SharedAffinity,
}

/// How long a checkout may wait for a free connection before failing. See
/// `Db::connect`.
const ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
            .log_slow_statements(log::LevelFilter::Warn, std::time::Duration::from_secs(10))
            .application_name("oxsum");
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_slow_threshold(std::time::Duration::from_secs(10))
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
        })
    }

    /// Wraps an already built pool. Tests use this; `connect` is the server's path.
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            affinity: Default::default(),
        }
    }

    /// The shared pool.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
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

use std::net::SocketAddr;
use std::time::Duration;

use oxsum_core::{Db, SWEEP_INTERVAL, Tenants, sweep_stale_holds};
use oxsum_server::Config;
use time::OffsetDateTime;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pick up `.env` when it is there, so the quick start in README.md works as written.
    // A missing file is not an error: deployments pass real environment variables instead,
    // and real variables always win over the file.
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let database_url = required_env("DATABASE_URL")?;
    let addr: SocketAddr = std::env::var("OXSUM_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".into())
        .parse()?;
    let config = Config::from_env()?;

    // One pool for the whole database, shared by every tenant. Each PostgreSQL connection
    // is a backend process, so this number is the process's whole connection budget rather
    // than a per-tenant allowance; see docs/decisions.md, "all tenants share one connection
    // pool". Tenants are told apart by `search_path`, pinned per transaction, not by pool.
    let max_connections = pool_size()?;
    let db = Db::connect(&database_url, max_connections).await?;
    tracing::info!(%max_connections, "database pool ready");

    // oxsum's own tables (users, organizations, memberships, API keys, channels and their prices)
    // before serving: a registration that arrives first must find them.
    db.migrate().await?;
    tracing::info!("oxsum schema ready");

    // Seeding the bootstrap channel and opening every stored channel credential happen before the
    // listener: a deployment that cannot open its channels must not accept a request at all.
    oxsum_server::prepare(&db, &config).await?;
    // A crashed gateway turn leaves its hold outstanding; the sweeper releases holds older than
    // the configured timeout, every minute. It also runs once right away, so a restart heals
    // what the crashed process left behind without waiting for the first interval.
    let _sweeper = spawn_sweeper(db.clone(), config.hold_timeout());
    let app = oxsum_server::app(db, config);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "oxsum listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// The background job that releases holds whose request never settled: every [`SWEEP_INTERVAL`]
/// it settles the watched holds older than `timeout` at 0 with kind `swept`.
///
/// The task ends with the process — a sweep is idempotent (a row is only cleared once its hold is
/// settled or gone), so an interrupted pass is simply retried on the next start.
fn spawn_sweeper(db: Db, timeout: Duration) -> tokio::task::JoinHandle<()> {
    let tenants = Tenants::new(db.pool().clone());
    tokio::spawn(async move {
        loop {
            let older_than = OffsetDateTime::now_utc() - timeout;
            let on = OffsetDateTime::now_utc().date();
            match sweep_stale_holds(&db, &tenants, older_than, on).await {
                Ok(resolved) => {
                    if resolved > 0 {
                        tracing::info!(resolved, "the hold sweeper resolved stale holds");
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "the hold sweeper could not read its watch list");
                }
            }
            tokio::time::sleep(SWEEP_INTERVAL).await;
        }
    })
}

/// The shared pool's size, from `OXSUM_DB_MAX_CONNECTIONS` (default 10).
fn pool_size() -> Result<u32, String> {
    match std::env::var("OXSUM_DB_MAX_CONNECTIONS") {
        Ok(v) => v
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                format!("OXSUM_DB_MAX_CONNECTIONS must be a positive integer, got {v:?}")
            }),
        Err(_) => Ok(10),
    }
}

/// Validates required environment variables at startup and exits rather than running with an empty config.
fn required_env(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(format!("{name} must be set (see .env.example)")),
    }
}

async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::error!(error = %e, "failed to listen for ctrl-c");
    }
}

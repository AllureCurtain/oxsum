use std::net::SocketAddr;

use oxsum_core::Db;
use oxsum_server::Config;
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
    let app = oxsum_server::app(db, config);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "oxsum listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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

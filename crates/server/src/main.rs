use std::net::SocketAddr;

use oxsum_core::Tenants;
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
    let api_token = required_env("OXSUM_API_TOKEN")?;
    let addr: SocketAddr = std::env::var("OXSUM_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".into())
        .parse()?;

    let app = oxsum_server::app(Tenants::new(database_url), api_token);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "oxsum listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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

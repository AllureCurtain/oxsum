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
    let config = Config::from_env()?;

    // One pool for the whole database, shared by every tenant. Each PostgreSQL connection
    // is a backend process, so this number is the process's whole connection budget rather
    // than a per-tenant allowance; see docs/decisions.md, "all tenants share one connection
    // pool". Tenants are told apart by `search_path`, pinned per transaction, not by pool.
    let max_connections = pool_size()?;
    let db = Db::connect(&database_url, max_connections).await?;
    tracing::info!(%max_connections, "database pool ready");

    // oxsum's own tables (users, organizations, memberships, API keys, channels and their prices)
    // before anything else: a registration or a seed that arrives first must find them.
    db.migrate().await?;
    tracing::info!("oxsum schema ready");

    // `oxsum seed` fills the database with the demo world and exits — it shares this boot path so
    // a seeded deployment looks exactly like one the server has been running on.
    match std::env::args().nth(1).as_deref() {
        Some("seed") => {
            // Seeding reads only the catalog — channels and prices, never the
            // credentials — so it does not need OXSUM_SECRET_KEY the way serving
            // does. With one configured, `prepare` still runs so the bootstrap
            // channel lands before the demo turns pick models from it.
            if config.secret().is_some() {
                oxsum_server::prepare(&db, &config).await?;
            }
            return seed(&db).await;
        }
        Some(other) => {
            return Err(
                format!("unknown argument {other:?}: the only subcommand is `seed`").into(),
            );
        }
        None => {}
    }

    let addr: SocketAddr = std::env::var("OXSUM_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3000".into())
        .parse()?;

    // Seeding the bootstrap channel and opening every stored channel credential happen before the
    // listener: a deployment that cannot open its channels must not accept a request at all.
    oxsum_server::prepare(&db, &config).await?;
    // The app's own metrics registry comes back out so the jobs worker can count its passes into
    // the same registry `/metrics` renders.
    let (app, _events, metrics) = oxsum_server::app_with_billing(db.clone(), config.clone());
    // One worker claims the periodic jobs off oxsum.jobs: the hold sweeper that
    // releases crashed turns' holds, the webhook delivery pass (only when the
    // sealing key can open endpoint secrets — `POST /api/v1/webhooks` refuses
    // without it either), the daily reconcile/statements/retention passes.
    let _jobs_worker = oxsum_server::jobs::spawn_worker(
        db,
        oxsum_server::jobs::JobContext {
            hold_timeout: config.hold_timeout(),
            metrics: metrics.clone(),
            webhook: config
                .secret()
                .cloned()
                .map(|secret| (reqwest::Client::new(), secret)),
            retention: config.retention(),
        },
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "oxsum listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// `oxsum seed` (roadmap P7-4): populates the demo world and prints the logins
/// and the first key's secret — the only place the secret is ever visible.
async fn seed(db: &Db) -> Result<(), Box<dyn std::error::Error>> {
    let report = db.seed_demo().await?;
    if !report.created {
        println!(
            "already seeded — log in as {} / {} (the API key secret was printed on the first run)",
            report.email,
            oxsum_core::DEMO_PASSWORD
        );
        return Ok(());
    }
    println!(
        "seeded {:?}: {} turns settled ({} of {} minor spent), statement {}",
        report.organization,
        report.turns,
        report.spent_minor,
        report.topped_up_minor,
        report.statement.as_deref().unwrap_or("none"),
    );
    println!(
        "  log in as  {} / {}",
        report.email,
        oxsum_core::DEMO_PASSWORD
    );
    println!(
        "  member     {} / {}",
        report.member_email,
        oxsum_core::DEMO_PASSWORD
    );
    if let Some(secret) = &report.api_secret {
        println!("  api key    {secret}  (shown once)");
    }
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

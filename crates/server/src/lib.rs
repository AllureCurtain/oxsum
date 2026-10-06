//! oxsum HTTP layer: routing, auth, error mapping. Business logic lives in oxsum-core.

mod admin;
mod auth;
mod billing;
mod bills;
mod config;
mod error;
mod gateway;
mod metrics;
mod routes;
mod web;
mod ws;

use std::sync::Arc;

use axum::Router;
use axum::extract::FromRef;
use leptos::config::LeptosOptions;
use oxsum_core::{Db, Tenants, WalletError};
use time::OffsetDateTime;
use tokio::sync::broadcast;

pub use billing::BillingEvent;
pub use config::{Config, Gateway, Signup};
pub use metrics::Metrics;

/// Everything a request handler may need.
///
/// One database handle and one pool for the whole process: the ledger facades and oxsum's own
/// tables share it (docs/decisions.md, "all tenants share one connection pool"). The HTTP client is
/// shared for the same reason: one connection pool to upstream rather than one per request.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Db,
    pub(crate) tenants: Tenants,
    pub(crate) config: Config,
    pub(crate) http: reqwest::Client,
    /// Where the gateway publishes [`BillingEvent`]s: the `/ws/billing` route forwards
    /// them to the dashboard. Process-wide and in-memory — a missed event is a missed
    /// live update, not lost state, because the watch table stays the record.
    pub(crate) billing: broadcast::Sender<BillingEvent>,
    /// The per-key request limiter the hold-creation paths consult: in-process today,
    /// the trait is the seam a shared backend slots behind (docs/decisions.md).
    pub(crate) rate_limiter: Arc<dyn oxsum_core::RateLimiter>,
    /// The process's own telemetry registry: per-app, not the `metrics` facade's global
    /// slot, so a test reads back exactly the counts it caused (src/metrics.rs).
    pub(crate) metrics: Metrics,
    pub(crate) leptos_options: LeptosOptions,
}

impl FromRef<AppState> for LeptosOptions {
    fn from_ref(state: &AppState) -> Self {
        state.leptos_options.clone()
    }
}

/// Builds the full application router. Shared by main and the integration tests.
///
/// The router alone: what makes a deployment *able* to serve is [`prepare`], which a binary calls
/// first. Keeping them apart is what lets a test build a router over a pool that never connects, for
/// requests that are answered without the database.
pub fn app(db: Db, config: Config) -> Router {
    app_with_billing(db, config).0
}

/// Builds the router plus the broadcast sender the gateway publishes [`BillingEvent`]s to
/// and the metrics registry `GET /metrics` renders.
///
/// The dashboard's `/ws/billing` forwards the events to the browser; tests subscribe to the
/// sender to assert what a turn publishes, without opening a socket. The metrics come back
/// out so `main` can hand the same registry to the sweeper task, whose pass count would
/// otherwise record into a registry nobody scrapes.
pub fn app_with_billing(
    db: Db,
    config: Config,
) -> (Router, broadcast::Sender<BillingEvent>, Metrics) {
    // The billing broadcast is best-effort: a slow dashboard misses an event, and the
    // watch table stays the record of in-flight holds.
    let (billing, _) = broadcast::channel(128);
    let metrics = Metrics::new();
    let state = AppState {
        tenants: Tenants::new(db.pool().clone()),
        db,
        config,
        http: gateway::client(),
        billing: billing.clone(),
        rate_limiter: Arc::new(oxsum_core::SlidingWindow::default()),
        metrics: metrics.clone(),
        leptos_options: web::options(),
    };
    (routes::router(state), billing, metrics)
}

/// Prepares a database for serving: seed the first channel, and make sure the configured key opens
/// every channel there is.
///
/// `main` calls this once at startup, and the integration tests call it too, so what they serve is
/// what a deployment serves. Two promises live here:
///
/// - The bootstrap channel the environment describes is written into `oxsum.channels` when the
///   database has no channels yet. A database that has them is left exactly as it is: configuration
///   outlives the environment it started in.
/// - Every stored channel credential is opened once. A deployment whose `OXSUM_SECRET_KEY` does not
///   match its channels — the wrong key, or none at all — refuses to start and says so, instead of
///   answering every relayed request with a 500.
///
/// # Errors
///
/// A message naming what is wrong, for `main` to print before it exits.
pub async fn prepare(db: &Db, config: &Config) -> Result<(), WalletError> {
    let Some(secret) = config.secret() else {
        // Without a key there can be no channels: either there are none and nothing is served, or
        // the deployment lost the key it wrote with and must not start.
        return if db.has_channels().await? {
            Err(WalletError::Misconfigured(
                "the database holds channels, but OXSUM_SECRET_KEY is not set, so their upstream \
                 credentials cannot be opened"
                    .into(),
            ))
        } else {
            Ok(())
        };
    };
    if !db.has_channels().await?
        && let Some(bootstrap) = config.bootstrap()
    {
        let seeded = db
            .seed_channel(
                bootstrap.book(),
                bootstrap.base_url(),
                bootstrap.api_key(),
                secret,
            )
            .await?;
        if seeded {
            tracing::info!(
                channel = bootstrap.book().channel(),
                "seeded the first channel from the environment"
            );
        }
    }
    // Every credential must open: the alternative is finding out one request at a time.
    db.check_sealed_keys(secret).await
}

/// The posting date of every ledger write: the server's current UTC date, which a client cannot
/// choose (crates/server/AGENTS.md).
pub(crate) fn today() -> time::Date {
    OffsetDateTime::now_utc().date()
}

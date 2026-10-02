//! oxsum HTTP layer: routing, auth, error mapping. Business logic lives in oxsum-core.

mod admin;
mod auth;
mod config;
mod error;
mod gateway;
mod routes;

use axum::Router;
use oxsum_core::{Db, Tenants, WalletError};
use time::OffsetDateTime;

pub use config::{Config, Gateway, Signup};

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
}

/// Builds the full application router. Shared by main and the integration tests.
///
/// The router alone: what makes a deployment *able* to serve is [`prepare`], which a binary calls
/// first. Keeping them apart is what lets a test build a router over a pool that never connects, for
/// requests that are answered without the database.
pub fn app(db: Db, config: Config) -> Router {
    let state = AppState {
        tenants: Tenants::new(db.pool().clone()),
        db,
        config,
        http: gateway::client(),
    };
    routes::router(state)
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

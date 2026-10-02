//! oxsum HTTP layer: routing, auth, error mapping. Business logic lives in oxsum-core.

mod auth;
mod config;
mod error;
mod gateway;
mod routes;

use axum::Router;
use oxsum_core::{Db, Tenants};
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
pub fn app(db: Db, config: Config) -> Router {
    let state = AppState {
        tenants: Tenants::new(db.pool().clone()),
        db,
        config,
        http: gateway::client(),
    };
    routes::router(state)
}

/// The posting date of every ledger write: the server's current UTC date, which a client cannot
/// choose (crates/server/AGENTS.md).
pub(crate) fn today() -> time::Date {
    OffsetDateTime::now_utc().date()
}

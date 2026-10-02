//! oxsum HTTP layer: routing, auth, error mapping. Business logic lives in oxsum-core.

mod auth;
mod config;
mod error;
mod routes;

use axum::Router;
use oxsum_core::{Db, Tenants};

pub use config::{Config, Signup};

/// Everything a request handler may need.
///
/// One database handle and one pool for the whole process: the ledger facades and oxsum's
/// own tables share it (docs/decisions.md, "all tenants share one connection pool").
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) db: Db,
    pub(crate) tenants: Tenants,
    pub(crate) config: Config,
}

/// Builds the full application router. Shared by main and the integration tests.
pub fn app(db: Db, config: Config) -> Router {
    let state = AppState {
        tenants: Tenants::new(db.pool().clone()),
        db,
        config,
    };
    routes::router(state)
}

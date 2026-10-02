//! oxsum HTTP layer: routing, auth, error mapping. Business logic lives in oxsum-core.

mod auth;
mod error;
mod routes;

use std::sync::Arc;

use axum::Router;
use oxsum_core::Tenants;

/// Builds the full application router. Shared by main and the integration tests.
pub fn app(tenants: Tenants, api_token: impl Into<Arc<str>>) -> Router {
    routes::router(tenants, auth::ApiToken::new(api_token))
}

//! oxsum web dashboard: Topcoat pages, server-side rendered, mounted into the
//! same binary as the API through a tower service.
//!
//! This is the `spike/topcoat-dashboard` rewrite of the Leptos dashboard. Every
//! page is server-side rendered; there is no wasm bundle and no client build
//! step. Pages call `oxsum-core` directly, the way the Leptos server functions
//! did.
//!
//! ## Trust-model changes vs the Leptos dashboard
//!
//! - The public `/verify` page ran `oxsum_verify::verify_bundle` **in the
//!   browser** (WASM): the user verified a bill without trusting the server.
//!   Topcoat has no browser target, so the check now runs **on the server**:
//!   the user must trust the server's answer. The proof math is unchanged.
//! - The bills page's tree-head archive check likewise ran in the browser
//!   against `localStorage`; the spike answers it server-side on demand.

mod bills;
mod heads;
mod pages_admin;
mod pages_auth;
mod pages_dashboard;
mod pages_misc;
mod requests;
mod session;
mod usage;
mod views;

// Kept public: the server's CSV/JSON download routes build on these, exactly as
// before (`crates/server/src/bills.rs`).
pub use bills::{BillView, csv, json};
// The requests page's row shape, shared with the page (as before).
pub use requests::{REQUESTS_PATH, RequestFilters, RequestView};
// The usage page's row shape (as before).
pub use usage::{USAGE_DAYS, UsageView};

use oxsum_core::{Db, Tenants};
use topcoat::context::Cx;
use topcoat::cookie::RouterBuilderCookieExt;
use topcoat::router::Router;

/// What the pages need beyond the database: the deployment's head-signing seed
/// (for the bills page's tree-head section), whether mail flows (for the
/// verification banner), and whether the session cookie is `Secure`.
#[derive(Debug, Clone)]
pub struct WebConfig {
    pub head_seed: Option<[u8; 32]>,
    pub mail_configured: bool,
    pub session_cookie_secure: bool,
    /// Whether the deployment offers GitHub OAuth: the login page only draws
    /// the button where the flow exists.
    pub oauth_github: bool,
}

/// Builds the dashboard router: every page URL the Leptos dashboard served.
///
/// The server mounts the result as an axum fallback service
/// (`crates/server/src/web.rs`), so the API routes keep precedence and every
/// other path lands here with its original URI intact.
pub fn router(db: Db, tenants: Tenants, config: WebConfig) -> Router {
    let builder = Router::builder()
        .app_context(db)
        .app_context(tenants)
        .app_context(config)
        .cookies();
    let builder = pages_auth::register(builder);
    let builder = pages_dashboard::register(builder);
    let builder = pages_admin::register(builder);
    let builder = pages_misc::register(builder);
    builder.build()
}

/// Reads the typed app-context value the server registered, or fails the page
/// when the server was miswired. A missing value is a server bug, not a user
/// error, so this is an internal error either way.
pub(crate) fn app_value<T: Clone + Send + Sync + 'static>(cx: &Cx) -> topcoat::Result<T> {
    topcoat::context::try_app_context::<T>(cx)
        .cloned()
        .ok_or_else(|| topcoat::Error::msg("the server is not ready"))
}

/// The database handle the server registered.
pub(crate) fn db(cx: &Cx) -> topcoat::Result<Db> {
    app_value(cx)
}

/// The tenants handle the server registered.
pub(crate) fn tenants(cx: &Cx) -> topcoat::Result<Tenants> {
    app_value(cx)
}

/// The dashboard config the server registered.
pub(crate) fn web_config(cx: &Cx) -> topcoat::Result<WebConfig> {
    app_value(cx)
}

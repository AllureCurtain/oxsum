//! oxsum web: the server-rendered sliver that survived the dashboard's move to
//! React (issue #256), plus the bills-export view types the server reuses.
//!
//! The dashboard itself — overview, bills, keys, members, admin, the auth and
//! device pages — is the React app under `web/`, served at `/app`
//! (`crates/server/src/spa.rs`); the server redirects the legacy page paths
//! there. What stays in this crate is deliberately not in the browser bundle:
//!
//! - `/verify` — the public bill/usage-report verification page: a plain
//!   server-rendered form plus the `oxsum_verify` wasm bundle the page's
//!   `verify.js` loads when the browser supports it (issue #284) — the check
//!   runs on the client's machine when it can, on the server as the floor.
//!   Keeping the skeleton server-rendered is a choice, not a leftover: the
//!   one page whose whole point is not trusting the served app serves the
//!   verifier itself, not a verdict.
//! - `/style/main.css` — the stylesheet the verify page links.
//! - [`BillView`] + [`csv`]/[`json`] — the bills-export row shape the server's
//!   download routes (`crates/server/src/bills.rs`) build on.

mod bills;
mod pages_misc;
mod views;

// Kept public: the server's CSV/JSON download routes build on these, exactly as
// before (`crates/server/src/bills.rs`).
pub use bills::{BillView, csv, json};

use oxsum_core::{Db, Tenants};
use topcoat::context::Cx;
use topcoat::cookie::RouterBuilderCookieExt;
use topcoat::router::Router;

/// What the surviving page needs beyond the database: the deployment's
/// head-signing seed, which `/verify` uses to publish the operator's key when it
/// checks a usage report against a signed head.
///
/// The dashboard-era fields (mail, cookie `Secure`, OAuth, the witness pinset)
/// went with the pages that read them.
#[derive(Debug, Clone)]
pub struct WebConfig {
    pub head_seed: Option<[u8; 32]>,
}

/// Builds the router for the server-rendered pages that remain.
///
/// The server mounts the result as an axum fallback service
/// (`crates/server/src/web.rs`), so the API routes and the `/app` SPA keep
/// precedence and only the legacy paths that did not redirect land here.
pub fn router(db: Db, tenants: Tenants, config: WebConfig) -> Router {
    let builder = Router::builder()
        .app_context(db)
        .app_context(tenants)
        .app_context(config)
        .cookies();
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

/// The dashboard config the server registered.
pub(crate) fn web_config(cx: &Cx) -> topcoat::Result<WebConfig> {
    app_value(cx)
}

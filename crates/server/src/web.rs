//! The Leptos pages, mounted into the same axum router through `leptos_axum`.
//!
//! One binary serves the API, the gateway and the pages (docs/decisions.md, "pages move
//! to Leptos"): server-side rendering calls `oxsum-core` directly, and browser
//! interactions call server code through Leptos server functions — no hand-written page
//! API (docs/architecture.md). The server functions reach the database through the
//! context provided here; the request's own parts (for the session cookie) are provided
//! by `leptos_axum` itself.

use axum::Router;
use leptos::prelude::*;
use leptos_axum::{LeptosRoutes, generate_route_list};

use crate::AppState;

/// Mounts the dashboard pages and their server functions into the router.
///
/// `leptos_axum` renders the [`oxsum_web::Shell`] as the whole HTML document; the route
/// list comes from [`oxsum_web::App`], the route tree inside that shell.
pub fn mount(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    let routes = generate_route_list(oxsum_web::App);
    let db = state.db.clone();
    let tenants = state.tenants.clone();
    let options = state.leptos_options.clone();
    router.leptos_routes_with_context(
        state,
        routes,
        move || {
            provide_context(db.clone());
            provide_context(tenants.clone());
        },
        move || view! { <oxsum_web::Shell options=options.clone()/> },
    )
}

/// The Leptos options, from `crates/web/Cargo.toml`'s `[package.metadata.leptos]`
/// with environment overrides (what `cargo leptos serve` sets).
///
/// Panics when the manifest lacks the section: that is a broken checkout, not a
/// runtime failure, and nothing can be served without it.
pub fn options() -> leptos::config::LeptosOptions {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../web/Cargo.toml");
    match leptos::config::get_configuration(Some(manifest)) {
        Ok(conf) => conf.leptos_options,
        Err(error) => panic!("crates/web/Cargo.toml carries [package.metadata.leptos]: {error}"),
    }
}

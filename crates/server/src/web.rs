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
/// list comes from [`oxsum_web::App`], the route tree inside that shell. The static
/// bundle `cargo leptos build` writes to the site dir (`target/site`, `/pkg/*` inside
/// it) is served too: without that route the shell references assets that answer 404
/// and the pages render but never hydrate — the dashboard's live side and the
/// in-browser `/verify` check stay dead, in the Docker image as in local development.
pub fn mount(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    let routes = generate_route_list(oxsum_web::App);
    let db = state.db.clone();
    let tenants = state.tenants.clone();
    let options = state.leptos_options.clone();
    let router = router.route_service(
        &leptos_axum::site_pkg_dir_service_route_path(&options),
        leptos_axum::site_pkg_dir_service(&options),
    );
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

/// The Leptos options, from the workspace `Cargo.toml`'s `[[workspace.metadata.leptos]]`
/// with environment overrides (what `cargo leptos serve` sets).
///
/// Panics when the manifest lacks the section: that is a broken checkout, not a
/// runtime failure, and nothing can be served without it.
pub fn options() -> leptos::config::LeptosOptions {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    match leptos::config::get_configuration(Some(manifest)) {
        Ok(conf) => conf.leptos_options,
        Err(error) => {
            panic!("the workspace Cargo.toml carries [[workspace.metadata.leptos]]: {error}")
        }
    }
}

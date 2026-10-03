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
/// `site-root` is a workspace-relative path — that is how `cargo leptos` reads it — but
/// the pkg service resolves it against the process's working directory, so the resolved
/// value is made absolute here rather than depending on where the server was started
/// (issue #44).
///
/// Panics when the manifest lacks the section: that is a broken checkout, not a
/// runtime failure, and nothing can be served without it.
pub fn options() -> leptos::config::LeptosOptions {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    match leptos::config::get_configuration(Some(manifest)) {
        Ok(conf) => {
            let mut options = conf.leptos_options;
            let site_root = std::path::Path::new(&*options.site_root);
            if site_root.is_relative() {
                let resolved = workspace_root().join(site_root);
                options.site_root = resolved.to_string_lossy().into_owned().into();
            }
            options
        }
        Err(error) => {
            panic!("the workspace Cargo.toml carries [[workspace.metadata.leptos]]: {error}")
        }
    }
}

/// The workspace root: this crate sits at `<root>/crates/server`, and the workspace
/// `Cargo.toml` was found the same way above.
fn workspace_root() -> &'static std::path::Path {
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    match crate_dir.parent().and_then(std::path::Path::parent) {
        Some(root) => root,
        None => panic!("crates/server sits under the workspace root"),
    }
}

#[cfg(test)]
mod tests {
    use super::{options, workspace_root};

    /// The pkg service serves `<site-root>/pkg` relative to the process's working
    /// directory, so a relative site root would answer 404 under `cargo run` from
    /// anywhere but the workspace root. The real options must name the workspace's built
    /// site whatever the working directory is (issue #44).
    #[test]
    fn the_real_options_name_the_workspace_site() {
        let options = options();
        assert_eq!(
            std::path::Path::new(&*options.site_root),
            workspace_root().join("target/site"),
            "the site root is the workspace's built site, not the working directory"
        );
        assert_eq!(&*options.site_pkg_dir, "pkg");
    }
}

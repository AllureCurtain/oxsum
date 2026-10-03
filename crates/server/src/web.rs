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
///
/// A 404 is not the only way those pages stay inert: the *name* the markup asks for has
/// to be the one the site holds. leptos's `HydrationScripts` appends `_bg` unless
/// `LEPTOS_OUTPUT_NAME` was set while it was compiled, and cargo-leptos writes
/// `pkg/oxsum.wasm`, so a server built by plain `cargo run` asked for a file no build
/// writes (issue #65). `.cargo/config.toml` sets that variable for every cargo
/// invocation in the workspace; `the_shell_asks_for_the_wasm_file_the_built_site_holds`
/// in `crates/server/tests/dashboard.rs` pins the name.
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
/// the pkg service resolves it against the process's working directory, so a relative
/// value is made absolute here rather than depending on where the server was started
/// (issue #44). An absolute value, which is what a deployment sets through
/// `LEPTOS_SITE_ROOT`, is used as it is.
///
/// The manifest is a build-time path: the release image ships the binary without the
/// checkout, so it is read only when it exists and the environment is authoritative
/// otherwise. Requiring it made the image's server panic at startup, which its own gate
/// caught (issue #49).
pub fn options() -> leptos::config::LeptosOptions {
    match leptos::config::get_configuration(source_manifest()) {
        Ok(conf) => {
            let mut options = conf.leptos_options;
            options.site_root = absolutise(&options.site_root, workspace_root()).into();
            options
        }
        Err(error) => {
            panic!("the workspace Cargo.toml carries [[workspace.metadata.leptos]]: {error}")
        }
    }
}

/// The workspace manifest, when this binary runs from a checkout. `None` in the release
/// image, where only the binary and the built site are present.
fn source_manifest() -> Option<&'static str> {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    match std::path::Path::new(manifest).exists() {
        true => Some(manifest),
        false => None,
    }
}

/// An absolute site root is kept; a relative one is resolved against `workspace`.
fn absolutise(site_root: &str, workspace: &std::path::Path) -> String {
    let path = std::path::Path::new(site_root);
    match path.is_absolute() {
        true => site_root.to_owned(),
        false => workspace.join(path).to_string_lossy().into_owned(),
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
    use super::{absolutise, options, source_manifest, workspace_root};

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

    /// A deployment names its site absolutely (`LEPTOS_SITE_ROOT=/app/site` in the release
    /// image); that value must survive untouched, because there is no checkout to resolve
    /// it against (issue #49). The spelled path is built from the workspace root so the
    /// test means the same thing on every platform.
    #[test]
    fn an_absolute_site_root_is_used_as_it_is() {
        let deployed = workspace_root().join("deployment-site");
        let spelled = deployed.to_string_lossy().into_owned();
        assert_eq!(absolutise(&spelled, workspace_root()), spelled);
    }

    /// A relative one, which is how the workspace manifest spells it, resolves against the
    /// workspace instead of whatever the working directory happens to be (issue #44).
    #[test]
    fn a_relative_site_root_resolves_against_the_workspace() {
        assert_eq!(
            absolutise("target/site", std::path::Path::new("/srv/oxsum")),
            std::path::Path::new("/srv/oxsum")
                .join("target/site")
                .to_string_lossy()
        );
    }

    /// This test binary runs from a checkout, so the manifest is there; the release image
    /// is the case where it is not.
    #[test]
    fn a_checkout_has_its_manifest() {
        assert!(
            source_manifest().is_some(),
            "cargo test runs from the checkout"
        );
    }
}

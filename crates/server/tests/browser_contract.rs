//! The browser contract: the built site, the assets the server serves out of it, and the
//! HTML the browser needs to become interactive.
//!
//! Every other server suite stops at what a router answers. The two defects that reached
//! `main` on 2026-10-03 were both invisible from there and visible from the built site and
//! the served HTML: the site root resolved against the working directory, so `/pkg/*`
//! answered 404 and the pages never hydrated (issue #44); and the wasm module never
//! exported `hydrate`, so the module script threw right after the wasm had loaded and the
//! login form fell back to a native GET that put the credentials in the URL (issue #46).
//!
//! The tests are `#[ignore]`d because they need a site: `cargo leptos build` takes minutes,
//! and a plain `cargo test --workspace` must stay green on a machine that never ran it. CI
//! runs them explicitly, with `OXSUM_SITE_DIR` pointing at the built site
//! (docs/development.md, "Browser contract"). No database is needed: `/pkg/*` and `/login`
//! are answered without one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use regex::Regex;
use std::path::PathBuf;
use tower::ServiceExt;

/// The wasm-bindgen glue, named after `output-name` in `[[workspace.metadata.leptos]]`.
const GLUE: &str = "pkg/oxsum.js";

/// The wasm module, named after `output-name` as well.
///
/// wasm-bindgen emits `oxsum_bg.wasm`, and `cargo-leptos` 0.3.11 renames it to
/// `oxsum.wasm` (its front-end compile step does this "for backward compatibility with
/// leptos' `HydrationScripts`"), which is the name `HydrationScripts` computes whenever
/// `LEPTOS_OUTPUT_NAME` is set — and `cargo leptos build` sets it for the server half. So
/// the module the served HTML references, and the one the site must hold, is
/// `pkg/oxsum.wasm`.
const WASM: &str = "pkg/oxsum.wasm";

/// The site directory `cargo leptos build` wrote, from `OXSUM_SITE_DIR`.
///
/// A relative value resolves against the workspace root — where `cargo leptos build`
/// writes the site, and what `site-root` in the workspace `Cargo.toml` is relative to —
/// not against the process's working directory, which for a test binary is the crate's own
/// directory.
///
/// Skipping when the variable is unset would hide exactly what this gate is for, so it
/// fails and says how to get a site.
fn site_dir() -> PathBuf {
    let dir = match std::env::var("OXSUM_SITE_DIR") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => panic!(
            "OXSUM_SITE_DIR is not set: run `cargo leptos build`, then point it at the built \
             site, for example `OXSUM_SITE_DIR=target/site cargo test -p oxsum-server --test \
             browser_contract -- --ignored --nocapture` (docs/development.md, \"Browser \
             contract\")"
        ),
    };
    if dir.is_relative() {
        return workspace_root().join(dir);
    }
    dir
}

/// The workspace root: this crate sits at `<root>/crates/server`.
fn workspace_root() -> &'static std::path::Path {
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    match crate_dir.parent().and_then(std::path::Path::parent) {
        Some(root) => root,
        None => panic!("crates/server sits under the workspace root"),
    }
}

/// The app with the project's real Leptos options, over a pool that never connects.
///
/// The options come from `web::options` — the site root the server really serves from,
/// which is the whole point of the `/pkg/*` assertions (issue #44). A lazy pool is enough
/// because neither the static bundle nor the login page reaches the database.
fn app() -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL");
    oxsum_server::app(Db::from_pool(pool), Config::new(Signup::Open, None))
}

/// One response, with its body kept as bytes: the wasm module is not text.
struct Res {
    status: StatusCode,
    body: Vec<u8>,
}

impl Res {
    /// The body as text; for the HTML and JS assertions.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// One GET against the app.
async fn get(app: &Router, path: &str) -> Res {
    let req = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("the request is valid");
    let res = app.clone().oneshot(req).await.expect("the app answers");
    let status = res.status();
    let body = res
        .into_body()
        .collect()
        .await
        .expect("the body is readable")
        .to_bytes()
        .to_vec();
    Res { status, body }
}

/// The site holds what the browser loads: the wasm-bindgen glue and the wasm module
/// itself. A missing, failed or half-finished `cargo leptos build` fails here, naming the
/// file and the site directory it looked in.
#[test]
#[ignore = "needs a built site: OXSUM_SITE_DIR, see docs/development.md"]
fn the_built_site_carries_the_glue_and_the_wasm_module() {
    let site = site_dir();
    for asset in [GLUE, WASM] {
        let path = site.join(asset);
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "{} is not readable ({error}): `cargo leptos build` writes it, and \
                 OXSUM_SITE_DIR is {}",
                path.display(),
                site.display()
            )
        });
        assert!(
            !bytes.is_empty(),
            "{} is empty: the leptos build did not finish",
            path.display()
        );
    }
}

/// The shell's assets are served from the site root the server really resolved, whatever
/// the working directory is. A relative site root answers 404 here — the pages render and
/// never hydrate (issue #44).
#[tokio::test]
#[ignore = "needs a built site: OXSUM_SITE_DIR, see docs/development.md"]
async fn the_pkg_bundle_is_served_from_the_real_site_root() {
    let app = app();
    for asset in [GLUE, WASM] {
        let res = get(&app, &format!("/{asset}")).await;
        assert_eq!(
            res.status,
            StatusCode::OK,
            "/{asset} answered {} with the project's own leptos options: the shell \
             references it, so a 404 leaves every page inert",
            res.status
        );
        assert!(
            !res.body.is_empty(),
            "/{asset} answered 200 with an empty body"
        );
    }
}

/// `/login` carries the two things the browser contract needs from the HTML: the leptos
/// hydration script — the module script that loads the pkg bundle and calls
/// `mod.hydrate()` — and a login form whose method is `post`.
///
/// The method is what a browser without hydration falls back to: a form without one
/// issues a native `GET`, which puts the email and the password in the query string
/// (issue #46).
#[tokio::test]
#[ignore = "needs a built site: OXSUM_SITE_DIR, see docs/development.md"]
async fn the_login_page_carries_the_hydration_script_and_a_post_form() {
    let app = app();
    let res = get(&app, "/login").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /login answered {}",
        res.status
    );
    let html = res.text();

    // The shell's hydration script: a module script that awaits the wasm and then
    // hydrates. `<script type="module">` carries the CSP nonce, hence the attribute list.
    let hydration = Regex::new(r#"(?s)<script[^>]*type="module"[^>]*>.*?mod\.hydrate\(\)"#)
        .expect("the hydration script pattern is valid");
    assert!(
        hydration.is_match(&html),
        "GET /login carries no leptos hydration script, so nothing hydrates the page"
    );
    // The module it imports, named the way the pkg service must answer it.
    let module = Regex::new(r#"<link rel="modulepreload" href="/pkg/oxsum\.js""#)
        .expect("the module preload pattern is valid");
    assert!(
        module.is_match(&html),
        "GET /login does not preload /pkg/oxsum.js, so the browser has nothing to import"
    );

    // The login form itself: a POST, with the password field inside it.
    let form = Regex::new(r#"(?s)<form[^>]*\bmethod="post"[^>]*>.*?name="password""#)
        .expect("the login form pattern is valid");
    assert!(
        form.is_match(&html),
        "the login form is not a POST form: a browser without hydration submits it natively \
         and the credentials end up in the URL (issue #46)"
    );
}

/// The glue exports the module entry point the generated HTML calls.
///
/// The hydration script the shell emits is
/// `import("/pkg/oxsum.js").then(mod => mod.default({module_or_path: "/pkg/oxsum.wasm"}).then(() => mod.hydrate()))`
/// — it imports the glue, awaits the default initialiser, then calls `mod.hydrate()`.
/// wasm-bindgen only exports `#[wasm_bindgen]` items, so without an `export function
/// hydrate` in the glue that call throws `mod.hydrate is not a function` once the wasm has
/// loaded, and every page stays exactly as the server rendered it (issue #46).
#[test]
#[ignore = "needs a built site: OXSUM_SITE_DIR, see docs/development.md"]
fn the_glue_exports_the_entry_point_the_hydration_script_calls() {
    let path = site_dir().join(GLUE);
    let glue = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is not readable ({error})", path.display()));
    let export = Regex::new(r"(?m)^export\s+function\s+hydrate\s*\(")
        .expect("the hydrate export pattern is valid");
    assert!(
        export.is_match(&glue),
        "{} does not export `hydrate`: the hydration script calls `mod.hydrate()`, so \
         nothing hydrates (issue #46)",
        path.display()
    );
}

//! The React dashboard (`web/`, issue #234): its Vite build output lands in
//! `web/dist` and is embedded into the binary, served under `/app` — the same
//! single-binary deployment the Topcoat pages get, on a path of its own so
//! the two frontends coexist until parity.
//!
//! `rust-embed`'s `debug-embed` feature reads `web/dist` from disk in debug
//! builds — a `npm run build` is visible without a cargo rebuild — while
//! release builds embed the directory at compile time. A committed placeholder
//! `web/dist/index.html` keeps the folder present in a fresh checkout.

use axum::Router;
use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use rust_embed::Embed;

/// The built React app, embedded — `../../web/dist` relative to this crate.
#[derive(Embed)]
#[folder = "../../web/dist"]
struct SpaAssets;

/// The media type of every asset Vite can emit. An unknown extension answers
/// `application/octet-stream` rather than guessing badly.
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json" | "map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// One embedded file, or the app shell for a client-side route.
fn serve(path: &str) -> Response {
    // An empty path and every route the SPA owns itself (no extension) both
    // answer the app shell; an extension names a concrete asset.
    let looks_like_asset = path
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'));
    let target = if path.is_empty() || !looks_like_asset {
        "index.html"
    } else {
        path
    };
    match SpaAssets::get(target) {
        Some(file) => {
            let cache = if target == "index.html" {
                // The shell must never stale — Vite's hashed assets may be
                // immutable, the entry point that names them may not.
                "no-cache"
            } else if target.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            (
                [
                    (header::CONTENT_TYPE, content_type(target)),
                    (header::CACHE_CONTROL, cache),
                ],
                file.data.into_owned(),
            )
                .into_response()
        }
        // An asset path that does not exist is a real 404 — the shell fallback
        // is only for client-side routes, not for missing files.
        None => (StatusCode::NOT_FOUND, Html("not found")).into_response(),
    }
}

async fn index() -> Response {
    serve("")
}

async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    // A nested service still sees the full URI; the "/app" prefix comes off
    // once — a dist file genuinely named `app/…` must not be eaten.
    serve(path.strip_prefix("app/").unwrap_or(path))
}

/// The `/app` subtree: `/app` and `/app/` answer the shell, every deeper path
/// the asset or the shell fallback.
pub fn router() -> Router<crate::AppState> {
    Router::new()
        .route("/", get(index))
        .route("/{*path}", get(asset))
}

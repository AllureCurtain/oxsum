//! The last server-rendered page (`/verify`), mounted into the axum router as a
//! fallback tower service — plus the 301 redirects that send the retired
//! dashboard's paths to the React app at `/app` (issue #256).
//!
//! One binary serves the API, the gateway, the SPA and this page. The Topcoat
//! router sees every request the API routes and the `/app` nest did not match,
//! with its original URI intact.

use axum::Router;
use axum::extract::{Path, Request};
use axum::http::header::{CACHE_CONTROL, REFERRER_POLICY};
use axum::http::{HeaderValue, StatusCode, Uri, header::LOCATION};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use topcoat::router::tower::TowerService;
use tower::ServiceBuilder;

use crate::AppState;

/// Mounts `/verify`, the public bill page and the stylesheet as the router's
/// fallback: the API routes, the `/app` SPA and the legacy-path redirects keep
/// precedence, and every other path lands on the server-rendered pages.
pub fn mount(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    let config = oxsum_web::WebConfig {
        head_seed: state.config.head_signing_seed(),
        // The pinned witness keys are public by design (the same set
        // `/api/v1/log/checkpoint` serves); the bill page embeds them so the
        // customer's browser can check cosignatures without a session.
        witness_keys: state
            .config
            .witness_keys()
            .iter()
            .map(|key| oxsum_web::WitnessPin {
                witness_url: key.witness_url.clone(),
                key_name: key.key_name.clone(),
                public_key: key.public_key.clone(),
                key_hash: key.key_hash.clone(),
            })
            .collect(),
    };
    let dashboard = oxsum_web::router(state.db.clone(), state.tenants.clone(), config);
    let pages = ServiceBuilder::new()
        .layer(middleware::from_fn(page_headers))
        .service(TowerService::new(dashboard));
    router.merge(redirects()).fallback_service(pages)
}

/// Headers every server-rendered page carries: the pages are not indexed, and a
/// bill link — a capability in the URL — must not leak through a referrer. The
/// bill itself is also never cached: a shared cache holding it would hand one
/// customer's bill to the next request for that URL.
async fn page_headers(request: Request, next: Next) -> Response {
    let bill = request.uri().path().starts_with("/bill/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    if bill {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    }
    response
}

/// Every page URL the retired Topcoat dashboard answered, mapped onto the React
/// route that serves it now (issue #256). These are permanent moves — the pages
/// are gone for good — so each answers `301 Moved Permanently` and a bookmark
/// keeps working.
///
/// Only paths the React app owns are listed. The bills export URLs under
/// `/dashboard/bills/export.*` stay real file routes on the server (the React
/// bills page links them), so they are absent here on purpose; `/verify` is
/// likewise absent — it is still served, not redirected.
fn redirects() -> Router<AppState> {
    Router::new()
        // The bare root used to land on the dashboard; now the app is `/app`.
        .route("/", get(|| async { moved("/app") }))
        // The auth pages each map to the same path under `/app`. `/logout` has
        // no React page — a logged-out browser lands on `/app/login`.
        .route("/login", get(|| async { moved("/app/login") }))
        .route("/logout", get(|| async { moved("/app/login") }))
        .route("/register", get(|| async { moved("/app/register") }))
        .route(
            "/forgot-password",
            get(|| async { moved("/app/forgot-password") }),
        )
        .route(
            "/reset-password",
            get(|| async { moved("/app/reset-password") }),
        )
        .route(
            "/verify-email",
            get(|| async { moved("/app/verify-email") }),
        )
        // The device grant's approval page.
        .route("/device", get(|| async { moved("/app/device") }))
        // `/dashboard` is the overview; the rest map path-for-path.
        .route("/dashboard", get(|| async { moved("/app") }))
        .route(
            "/dashboard/{*rest}",
            get(|Path(rest): Path<String>, uri: Uri| async move {
                // `/dashboard/bills/export.{csv,json}` are real download routes
                // on the server, not pages — they never reach this arm (the
                // export routes are registered first), and a stray one that
                // slips through is sent to the bills page rather than a file.
                moved(&page_target(&rest, &uri))
            }),
        )
        // The operator console: every `/admin/*` page lands on the single React
        // admin route, which has its own section nav.
        .route("/admin", get(|| async { moved("/app/admin") }))
        .route("/admin/{*rest}", get(|| async { moved("/app/admin") }))
}

/// The `/app` path a retired `/dashboard/<rest>` page maps to. Each page's
/// React route shares its leaf name, so the mapping is the identity for every
/// leaf the app serves; an unrecognized tail falls back to the overview rather
/// than a broken deep link. The query string is carried across so a filtered
/// list (`?model=…`) keeps its filter.
fn page_target(rest: &str, uri: &Uri) -> String {
    // The React route names for the leaves the dashboard had; a sub-path that
    // is not one of these (a deeper detail route, an export slip) reads best as
    // its first segment's page.
    const LEAVES: &[&str] = &[
        "chat", "bills", "keys", "members", "log", "requests", "usage",
    ];
    let first = rest.split('/').next().unwrap_or_default();
    let leaf = if LEAVES.contains(&first) { first } else { "" };
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    if leaf.is_empty() {
        format!("/app{query}")
    } else {
        format!("/app/{leaf}{query}")
    }
}

/// A `301 Moved Permanently`: the page is gone for good, so the redirect is the
/// permanent kind a browser may cache. (axum's `Redirect::permanent` is a 308,
/// which keeps the method — right for a moved endpoint, more than a moved page
/// asks for; the issue specifies 301.)
fn moved(target: &str) -> Response {
    (
        StatusCode::MOVED_PERMANENTLY,
        [(
            LOCATION,
            HeaderValue::from_str(target).unwrap_or(HeaderValue::from_static("/app")),
        )],
    )
        .into_response()
}

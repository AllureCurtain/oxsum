//! The browser contract: the assets the server serves and the redirects that
//! keep the retired dashboard's bookmarks working (issue #256).
//!
//! The dashboard, auth and admin pages moved to the React app under `/app`;
//! what this suite pins now is that the legacy paths answer a permanent
//! redirect to their `/app` twin, and that the two things still served — the
//! `/verify` page and its stylesheet — are answered. No database is needed:
//! redirects and the verify form do not reach one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use tower::ServiceExt;

/// The app over a pool that never connects: the pages and redirects these tests
/// hit do not reach the database.
fn app() -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL");
    oxsum_server::app(Db::from_pool(pool), Config::new(Signup::Open, None))
}

/// One response, with its body kept as bytes.
struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Res {
    /// The body as text; for the HTML assertions.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn content_type(&self) -> String {
        self.headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned()
    }

    fn location(&self) -> String {
        self.headers
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned()
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
    let headers = res.headers().clone();
    let body = res
        .into_body()
        .collect()
        .await
        .expect("the body is readable")
        .to_bytes()
        .to_vec();
    Res {
        status,
        headers,
        body,
    }
}

/// The `/verify` stylesheet is served: the one server-rendered page links it, so
/// a 404 leaves it unstyled.
#[tokio::test]
async fn the_stylesheet_is_served() {
    let app = app();
    let res = get(&app, "/style/main.css").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /style/main.css answered {}",
        res.status
    );
    assert!(
        res.content_type().contains("text/css"),
        "the stylesheet is CSS, got {}",
        res.content_type()
    );
    assert!(!res.body.is_empty(), "the stylesheet is empty");
}

/// `/verify` is public: anyone holding a bill can load it, no session needed.
/// It is the one page deliberately still server-rendered.
#[tokio::test]
async fn the_verify_page_is_public() {
    let app = app();
    let res = get(&app, "/verify").await;
    assert_eq!(
        res.status,
        StatusCode::OK,
        "GET /verify answered {}",
        res.status
    );
    assert!(
        res.text().contains("Proof bundle"),
        "the verify page names the bundle field"
    );
}

/// Every retired page path answers `301 Moved Permanently` at its `/app` twin,
/// so a bookmark keeps working. The redirect is the issue's contract: the page
/// is gone for good, and a browser may cache the new location.
#[tokio::test]
async fn the_retired_pages_redirect_permanently() {
    let app = app();
    // (legacy path, expected /app target)
    let cases = [
        ("/", "/app"),
        ("/login", "/app/login"),
        ("/register", "/app/register"),
        ("/forgot-password", "/app/forgot-password"),
        ("/reset-password", "/app/reset-password"),
        ("/verify-email", "/app/verify-email"),
        ("/device", "/app/device"),
        ("/dashboard", "/app"),
        ("/dashboard/chat", "/app/chat"),
        ("/dashboard/bills", "/app/bills"),
        ("/dashboard/keys", "/app/keys"),
        ("/dashboard/members", "/app/members"),
        ("/dashboard/log", "/app/log"),
        ("/dashboard/requests", "/app/requests"),
        ("/dashboard/usage", "/app/usage"),
        ("/admin", "/app/admin"),
        ("/admin/channels", "/app/admin"),
    ];
    for (path, target) in cases {
        let res = get(&app, path).await;
        assert_eq!(
            res.status,
            StatusCode::MOVED_PERMANENTLY,
            "GET {path} answered {}",
            res.status
        );
        assert_eq!(
            res.location(),
            target,
            "GET {path} redirects to the wrong place"
        );
    }
}

/// A dashboard deep link's query string rides the redirect: a filtered list
/// keeps its filter on the React page.
#[tokio::test]
async fn a_dashboard_redirect_keeps_the_query() {
    let app = app();
    let res = get(&app, "/dashboard/requests?model=m").await;
    assert_eq!(res.status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(res.location(), "/app/requests?model=m");
}

/// The bills export routes are still served — they are downloads the React
/// page links, not pages, so `/dashboard/bills/export.csv` answers a file (or a
/// 401 without a session), never a redirect to `/app`.
#[tokio::test]
async fn the_bills_export_is_not_redirected() {
    let app = app();
    let res = get(&app, "/dashboard/bills/export.csv").await;
    assert_eq!(
        res.status,
        StatusCode::UNAUTHORIZED,
        "the export wants a session, answered {}",
        res.status
    );
}

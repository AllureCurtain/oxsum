//! The embedded React dashboard (issue #234): `/app` serves the app shell,
//! deep client-side routes fall back to it, and a missing asset is a real
//! 404. Works without a database and without a frontend build — the committed
//! `web/dist/index.html` placeholder is what the assertions read.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use tower::ServiceExt;

fn app() -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused")
        .expect("parses the placeholder URL");
    oxsum_server::app(Db::from_pool(pool), Config::new(Signup::Open, None))
}

async fn get(path: &str) -> (StatusCode, String) {
    let res = app()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    (status, body)
}

#[tokio::test]
async fn app_root_serves_the_shell() {
    let (status, body) = get("/app").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("<html"), "{body}");
}

#[tokio::test]
async fn deep_client_route_falls_back_to_the_shell() {
    // A react-router path carries no extension, so the shell answers and the
    // client router takes it from there.
    let (status, body) = get("/app/bills").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("<html"), "{body}");
}

#[tokio::test]
async fn a_missing_asset_is_a_real_404() {
    // An extension names a concrete file — absent means absent, not the shell.
    let (status, _) = get("/app/assets/no-such-file-0000.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

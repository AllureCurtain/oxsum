//! Personal HTTP authority and immediate canonical receipt navigation.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/ready_receipt.rs"]
mod setup;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::SettlementKind;
use oxsum_server::{Config, Signup};
use serde_json::Value;
use tower::ServiceExt;

async fn read(
    app: &Router,
    path: &str,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("oxsum_session={cookie}"));
    }
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn only_personal_scope_reads_a_provable_receipt_without_a_period_report() {
    let Some(f) = setup::fixture("customer_readiness_http").await else {
        return;
    };
    let app = oxsum_server::app(f.db.clone(), Config::new(Signup::Invite, None));
    let path = "/api/v1/org/readiness/receipt";
    assert_eq!(
        read(&app, path, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    for cookie in [None, Some(f.cookie.as_str())] {
        assert_eq!(
            read(&app, path, cookie, Some(&f.registration.api_key.secret))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, empty) = read(&app, path, Some(&f.cookie), None).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["data"]["state"], "missing");
    let row = setup::turn(&f, SettlementKind::Usage, 0, None).await;
    let (status, result) = read(&app, path, Some(&f.cookie), None).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["data"]["state"], "ready");
    let receipt = &result["data"]["receipt"];
    assert_eq!(receipt["entryId"], row.entry_id.to_string());
    let (status, proof) = read(
        &app,
        &format!("/api/v1/entries/{}/proof", row.entry_id),
        Some(&f.cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{proof}");
    let hash = oxsum_core::Hash::parse_hex(receipt["contentHash"].as_str().unwrap()).unwrap();
    assert!(oxsum_verify::verify_bundle(&proof["data"].to_string(), &hash).unwrap());
    f.db.logout(&f.cookie).await.unwrap();
    assert_eq!(
        read(&app, path, Some(&f.cookie), None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

async fn page(
    app: &Router,
    entry: &str,
    cookie: Option<&str>,
    machine: bool,
) -> (StatusCode, String) {
    let mut request = Request::get(format!("/verify?entry={entry}"));
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("oxsum_session={cookie}"));
    }
    if machine {
        request = request.header("x-api-key", "explicit-machine-identity");
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.headers().get("cache-control").unwrap(),
        "private, no-store"
    );
    let status = response.status();
    (
        status,
        String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn verify_entry_prefills_only_current_customer_scope_and_keeps_public_paste_mode() {
    let Some(f) = setup::fixture("customer_readiness_http").await else {
        return;
    };
    let row = setup::turn(&f, SettlementKind::Usage, 0, None).await;
    let id = row.entry_id.to_string();
    let app = oxsum_server::app(f.db.clone(), Config::new(Signup::Invite, None));
    assert_eq!(
        page(&app, &id, None, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        page(&app, &id, Some(&f.cookie), true).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        page(&app, "not-a-uuid", Some(&f.cookie), false).await.0,
        StatusCode::BAD_REQUEST
    );
    let other = setup::fixture("customer_readiness_http").await.unwrap();
    assert_eq!(
        page(&app, &id, Some(&other.cookie), false).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, html) = page(&app, &id, Some(&f.cookie), false).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains(&row.request_id));
    assert!(html.contains("same server"));
    let expected =
        f.db.customer_receipt_readiness(&f.principal)
            .await
            .unwrap()
            .receipt
            .unwrap();
    assert!(html.contains(&expected.content_hash));
    // Demotion leaves a sibling's immutable actor outside the current scope.
    sqlx::query("UPDATE oxsum.api_keys SET created_by=$1 WHERE key_id=$2")
        .bind(other.registration.user.id)
        .bind(f.registration.api_key.key.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.memberships SET role='member' WHERE user_id=$1 AND organization_id=$2",
    )
    .bind(f.registration.user.id)
    .bind(f.registration.organization.id)
    .execute(f.db.pool())
    .await
    .unwrap();
    assert_eq!(
        page(&app, &id, Some(&f.cookie), false).await.0,
        StatusCode::NOT_FOUND
    );
    f.db.logout(&f.cookie).await.unwrap();
    assert_eq!(
        page(&app, &id, Some(&f.cookie), false).await.0,
        StatusCode::UNAUTHORIZED
    );
    let response = app
        .oneshot(Request::get("/verify").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn serving_read_requires_current_personal_session_and_never_bootstraps() {
    let Some(f) = setup::fixture("customer_readiness_http").await else {
        return;
    };
    let app = oxsum_server::app(f.db.clone(), Config::new(Signup::Invite, None));
    let path = "/api/v1/org/readiness/serving";
    assert_eq!(
        read(&app, path, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    for cookie in [None, Some(f.cookie.as_str())] {
        assert_eq!(
            read(&app, path, cookie, Some(&f.registration.api_key.secret))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, body) = read(&app, path, Some(&f.cookie), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["keyReady"], true);
    assert_eq!(body["data"]["fundingReady"], false);
    assert_eq!(body["data"]["sample"], Value::Null);
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)")
            .bind(format!("ledger_{}", f.registration.organization.tenant_id))
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert!(!exists);
    f.db.logout(&f.cookie).await.unwrap();
    assert_eq!(
        read(&app, path, Some(&f.cookie), None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

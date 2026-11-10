//! Setup reads use current personal business authority, never deployment keys.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser, SessionContext, SessionContextRequest};
use oxsum_server::{Config, Signup};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

async fn read(app: &Router, cookie: Option<&str>, bearer: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::get("/api/v1/business/readiness");
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
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn no_legacy_token_in_browser_and_current_context_is_mandatory() {
    let Some(url) = oxsum_testkit::suite_db_url("business_readiness_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "readiness customer password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let token = db
        .login(&customer.user.email, "readiness customer password")
        .await
        .unwrap()
        .into_session()
        .token;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_admin_token("fixture-operator-only"),
    );
    assert_eq!(read(&app, None, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        read(&app, Some(&token), None).await.0,
        StatusCode::FORBIDDEN
    );
    for bearer in [&customer.api_key.secret, "fixture-operator-only"] {
        assert_eq!(
            read(&app, None, Some(bearer)).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            read(&app, Some(&owner), Some(bearer)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, body) = read(&app, Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["ready"], false);
    assert_eq!(body["data"]["steps"].as_array().unwrap().len(), 5);
    assert_eq!(body["data"]["steps"][0]["state"], "ready");
    assert_eq!(body["data"]["steps"][4]["state"], "ready");
    assert!(body["data"]["checkedAt"].as_str().is_some());
    db.switch_session_context(
        &owner,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Customer,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        read(&app, Some(&owner), None).await.0,
        StatusCode::FORBIDDEN
    );
}

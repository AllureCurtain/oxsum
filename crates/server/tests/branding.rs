//! Public defaults and persisted identity through the production router.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../core/tests/support/integration.rs"]
mod setup;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    BusinessRole, BusinessStaffRequest, Db, NewUser, SessionContext, SessionContextRequest,
};
use oxsum_server::{Branding, Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn call(
    app: &Router,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
    bearer: bool,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .uri(path)
        .method(if body.is_some() { "POST" } else { "GET" })
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("cookie", format!("oxsum_session={token}"));
    }
    if bearer {
        request = request.header("authorization", "Bearer deployment-token");
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn public_and_staff_profiles_share_authority_defaults_and_immutable_replay() {
    let Some(url) = oxsum_testkit::suite_db_url("business_profile_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let defaults = Branding {
        name: "Deployment name".into(),
        logo_url: Some("https://cdn.example.com/logo.svg".into()),
        accent_color: Some("#2d5bd8".into()),
        ..Branding::default()
    };
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_branding(defaults.clone()),
    );
    let (status, initial) = call(&app, "/api/v1/branding", None, None, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial["data"], json!(defaults));
    let owner = setup::owner(&db).await;
    let (_, public) = call(&app, "/api/v1/branding", None, None, false).await;
    assert_eq!(public["data"]["name"], "Integration fixture");
    assert_eq!(public["data"]["logoUrl"], defaults.logo_url.unwrap());
    let (_, profile) = call(&app, "/api/v1/business/profile", None, Some(&owner), false).await;
    assert_eq!(profile["data"]["revision"], 0);
    assert_eq!(profile["data"]["source"], "defaults");
    assert_eq!(
        call(&app, "/api/v1/business/profile", None, None, false)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "/api/v1/business/profile", None, Some(&owner), true)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let person = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "profile test user password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let personal = db
        .login(&person.user.email, "profile test user password")
        .await
        .unwrap()
        .into_session();
    let payload = json!({"idempotencyKey":"save-profile", "reason":"set public contact", "expectedRevision":0, "branding":{"name":"Fictional wallet", "contactEmail":"help@example.com", "helpUrl":"https://help.example.com/guide", "logoUrl":null, "accentColor":null}});
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(payload.clone()),
            Some(&personal.token),
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    db.set_business_staff(
        &owner,
        &BusinessStaffRequest {
            idempotency_key: "appoint-admin".into(),
            user_id: person.user.id,
            role: BusinessRole::Admin,
            reason: "role test".into(),
        },
    )
    .await
    .unwrap();
    db.switch_session_context(
        &personal.token,
        &SessionContextRequest {
            idempotency_key: "admin-context".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            None,
            Some(&personal.token),
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(payload.clone()),
            Some(&personal.token),
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut missing = payload.clone();
    missing["branding"]
        .as_object_mut()
        .unwrap()
        .remove("helpUrl");
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(missing),
            Some(&owner),
            false
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut bad = payload.clone();
    bad["branding"]["helpUrl"] = json!("javascript:alert(1)");
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(bad),
            Some(&owner),
            false
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, saved) = call(
        &app,
        "/api/v1/business/profile",
        Some(payload.clone()),
        Some(&owner),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["data"]["profile"]["revision"], 1);
    assert_eq!(saved["data"]["profile"]["source"], "stored");
    assert_eq!(saved["data"]["profile"]["branding"], payload["branding"]);
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(payload.clone()),
            Some(&owner),
            false
        )
        .await
        .1,
        saved
    );
    let mut next = payload.clone();
    next["idempotencyKey"] = json!("save-next");
    next["expectedRevision"] = json!(1);
    next["branding"]["contactEmail"] = Value::Null;
    next["branding"]["helpUrl"] = Value::Null;
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(next.clone()),
            Some(&owner),
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(payload.clone()),
            Some(&owner),
            false
        )
        .await
        .1,
        saved
    );
    let changed = {
        let mut v = payload.clone();
        v["reason"] = json!("changed replay");
        v
    };
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(changed),
            Some(&owner),
            false
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut stale = payload;
    stale["idempotencyKey"] = json!("stale-editor");
    assert_eq!(
        call(
            &app,
            "/api/v1/business/profile",
            Some(stale),
            Some(&owner),
            false
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let reboot = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_branding(Branding {
            name: "Changed environment".into(),
            logo_url: Some("https://changed.example/logo.svg".into()),
            ..Branding::default()
        }),
    );
    assert_eq!(
        call(&reboot, "/api/v1/branding", None, None, false).await.1["data"],
        next["branding"]
    );
    let name: String = sqlx::query_scalar("SELECT name FROM oxsum.businesses WHERE singleton")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(name, "Fictional wallet");
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.admin_audit WHERE action='business.profile.update'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audits, 2);
    assert!(
        sqlx::query("UPDATE oxsum.business_profile_revisions SET revision=8")
            .execute(db.pool())
            .await
            .is_err()
    );
}

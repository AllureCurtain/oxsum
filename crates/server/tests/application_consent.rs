//! Personal application consent and machine scope HTTP boundaries.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    if let Some(bearer) = bearer {
        req = req.header("authorization", format!("Bearer {bearer}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn service_creation_personal_preview_accept_and_rotated_revoke() {
    let Some(url) = oxsum_testkit::suite_db_url("application_consent_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let namespace = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: "app".into(),
                key: "consent".into(),
                name: "External app".into(),
                customer_limit: 3,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let mut grant = setup::request(
        vec![],
        vec![
            oxsum_core::IntegrationScope::CustomersProvision,
            oxsum_core::IntegrationScope::CustomerLinkWrite,
        ],
    );
    grant.application_id = Some(namespace);
    let service = db
        .create_integration_credential(&owner, &grant)
        .await
        .unwrap()
        .secret
        .unwrap();
    let actor = db.authenticate_service(&service).await.unwrap().unwrap();
    let customer = db
        .provision_application_customer(
            &actor,
            &oxsum_core::ProvisionCustomerRequest {
                idempotency_key: "customer".into(),
                external_id: "recipient".into(),
                name: "Linked account".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    let person = db
        .register(oxsum_core::NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "consent test password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let personal = db
        .login(&person.user.email, "consent test password")
        .await
        .unwrap()
        .into_session()
        .token;
    let path = format!("/api/v1/integration/customers/{customer}/link-invitations");
    let req = json!({"idempotencyKey":"link","intendedEmail":person.user.email,"reason":"request portal access"});
    assert_eq!(
        call(&app, "POST", &path, req.clone(), Some(&personal), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, made) = call(&app, "POST", &path, req.clone(), None, Some(&service)).await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let id = made["data"]["invitationId"].as_str().unwrap();
    let token = made["data"]["token"].as_str().unwrap();
    let retry = call(&app, "POST", &path, req.clone(), None, Some(&service)).await;
    assert_eq!(retry.0, StatusCode::OK);
    assert!(retry.1["data"]["token"].is_null());
    let body = json!({"token":token});
    for suffix in ["preview", "accept"] {
        let path = format!("/api/v1/application-links/{suffix}");
        assert_eq!(
            call(&app, "POST", &path, body.clone(), None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "POST", &path, body.clone(), None, Some(&service))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "POST",
                &path,
                body.clone(),
                Some(&personal),
                Some(&person.api_key.secret)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", &path, body.clone(), Some(&personal), None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    // The real verification endpoint consumes a local token without sending mail.
    let email_token = db
        .mint_email_token(person.user.id, oxsum_core::EmailPurpose::Verify)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/verify",
            json!({"token":email_token.token}),
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, preview) = call(
        &app,
        "POST",
        "/api/v1/application-links/preview",
        body.clone(),
        Some(&personal),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["data"]["organizationId"], customer.to_string());
    assert!(preview["data"]["token"].is_null());
    let (status, accepted) = call(
        &app,
        "POST",
        "/api/v1/application-links/accept",
        body.clone(),
        Some(&personal),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert!(accepted["data"]["acceptedAt"].is_string());
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/application-links/accept",
            body,
            Some(&personal),
            None
        )
        .await
        .1,
        accepted
    );
    let revoke = format!("{path}/{id}/revoke");
    let revoke_body = json!({"idempotencyKey":"revoke","reason":"cancel"});
    assert_eq!(
        call(
            &app,
            "POST",
            &revoke,
            revoke_body.clone(),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &revoke,
            revoke_body.clone(),
            None,
            Some(&service)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let next = db
        .provision_application_customer(
            &actor,
            &oxsum_core::ProvisionCustomerRequest {
                idempotency_key: "other".into(),
                external_id: "other".into(),
                name: "Unclaimed".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    let path = format!("/api/v1/integration/customers/{next}/link-invitations");
    let (_, made) = call(
        &app,
        "POST",
        &path,
        json!({"idempotencyKey":"next-link","intendedEmail":person.user.email,"reason":"fixture"}),
        None,
        Some(&service),
    )
    .await;
    let id = made["data"]["invitationId"].as_str().unwrap();
    let revoke = format!("{path}/{id}/revoke");
    let (status, revoked) = call(&app, "POST", &revoke, revoke_body, None, Some(&service)).await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert!(revoked["data"]["revokedAt"].is_string());
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/application-links/accept",
            json!({"token":made["data"]["token"]}),
            Some(&personal),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

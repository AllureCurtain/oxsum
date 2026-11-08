//! Application key lifecycle HTTP identity, replay and scope boundaries.
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
async fn application_key_routes_enforce_machine_namespace_and_one_time_secrets() {
    let Some(url) = oxsum_testkit::suite_db_url("application_customer_keys_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let (_,application)=call(&app,"POST","/api/v1/business/applications",json!({"idempotencyKey":"keys-app","key":"keys-app","name":"Keys App","customerLimit":2,"reason":"application keys"}),Some(&owner),None).await;
    let app_id = application["data"]["applicationId"].clone();
    let (_,credential)=call(&app,"POST","/api/v1/business/integration-credentials",json!({"idempotencyKey":"keys-grant","applicationId":app_id,"name":"runtime","scopes":["customersProvision","customerKeysWrite"],"customerIds":[],"expiresAt":(time::OffsetDateTime::now_utc()+time::Duration::days(1)).format(&time::format_description::well_known::Rfc3339).unwrap(),"reason":"runtime"}),Some(&owner),None).await;
    let secret = credential["data"]["secret"].as_str().unwrap();
    let (_, customer) = call(
        &app,
        "POST",
        "/api/v1/integration/customers",
        json!({"idempotencyKey":"keys-customer","externalId":"one","name":"One"}),
        None,
        Some(secret),
    )
    .await;
    let id = customer["data"]["customer"]["organizationId"]
        .as_str()
        .unwrap();
    let path = format!("/api/v1/integration/customers/{id}/keys");
    let body = json!({"idempotencyKey":"runtime-key","name":"App key","reason":"inference","expiresAt":(time::OffsetDateTime::now_utc()+time::Duration::hours(1)).format(&time::format_description::well_known::Rfc3339).unwrap(),"spendLimitMinor":1000,"modelAllowlist":["allowed"],"requestsPerMinute":5,"maxConcurrentHolds":1});
    assert_eq!(
        call(&app, "POST", &path, body.clone(), Some(&owner), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, created) = call(&app, "POST", &path, body.clone(), None, Some(secret)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let key = created["data"]["key"]["id"].as_str().unwrap();
    let consumption = created["data"]["secret"].as_str().unwrap();
    assert!(created["data"]["key"]["createdBy"].is_null());
    assert_eq!(
        call(&app, "GET", &path, Value::Null, None, Some(consumption))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, replay) = call(&app, "POST", &path, body.clone(), None, Some(secret)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(replay["data"]["secret"].is_null());
    assert_eq!(replay["data"]["key"], created["data"]["key"]);
    let (status, list) = call(
        &app,
        "GET",
        &format!("{path}?limit=1"),
        Value::Null,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"]["keys"][0]["id"], key);
    assert!(list["data"]["keys"][0].get("secret").is_none());
    let mut changed = body.clone();
    changed["spendLimitMinor"] = json!(2000);
    assert_eq!(
        call(&app, "POST", &path, changed, None, Some(secret))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut unconstrained = body;
    unconstrained["idempotencyKey"] = json!("unbounded");
    unconstrained["spendLimitMinor"] = Value::Null;
    assert_eq!(
        call(&app, "POST", &path, unconstrained, None, Some(secret))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let foreign = format!("/api/v1/integration/customers/{}/keys", Uuid::new_v4());
    assert_eq!(
        call(&app, "GET", &foreign, Value::Null, None, Some(secret))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, revoked) = call(
        &app,
        "POST",
        &format!("{path}/{key}/revoke"),
        json!({"idempotencyKey":"revoke-key","reason":"replace"}),
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert!(revoked["data"]["revokedAt"].is_string());
    assert!(db.authenticate(consumption).await.unwrap().is_none());
    let credential_id =
        Uuid::parse_str(credential["data"]["credentialId"].as_str().unwrap()).unwrap();
    db.revoke_integration_credential(
        &owner,
        credential_id,
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: "revoke-management".into(),
            reason: "done".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(&app, "GET", &path, Value::Null, None, Some(secret))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

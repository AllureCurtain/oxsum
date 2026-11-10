//! Scoped publication HTTP contract and separate original-byte read authority.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::Value;
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
async fn scoped_http_publication_needs_write_and_download_needs_read() {
    let Some(url) = oxsum_testkit::suite_db_url("application_publication_http") else {
        return;
    };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let namespace = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: "app".into(),
                key: "publication".into(),
                name: "Publication fixture".into(),
                customer_limit: 5,
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
            oxsum_core::IntegrationScope::EvidenceWrite,
        ],
    );
    grant.application_id = Some(namespace);
    let secret = db
        .create_integration_credential(&owner, &grant)
        .await
        .unwrap()
        .secret
        .unwrap();
    let service = db.authenticate_service(&secret).await.unwrap().unwrap();
    let org = db
        .provision_application_customer(
            &service,
            &oxsum_core::ProvisionCustomerRequest {
                idempotency_key: "customer".into(),
                external_id: "one".into(),
                name: "Publication customer".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    let app = oxsum_server::app(
        Db::connect(&url, 1).await.unwrap(),
        Config::new(Signup::Invite, None),
    );
    let root = format!("/api/v1/integration/customers/{org}");
    let path = format!("{root}/evidence-publications");
    let day = (time::OffsetDateTime::now_utc().date() - time::Duration::days(1)).to_string();
    let request =
        serde_json::json!({"idempotencyKey":"publish","kind":"period","from":day,"to":day});
    assert_eq!(
        call(&app, "POST", &path, request.clone(), Some(&owner), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, first) = call(&app, "POST", &path, request.clone(), None, Some(&secret)).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["data"]["status"], "published");
    let id = first["data"]["bundle"]["bundleId"].as_str().unwrap();
    assert_eq!(
        call(&app, "POST", &path, request.clone(), None, Some(&secret))
            .await
            .1,
        first
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{root}/evidence-bundles/{id}"),
            Value::Null,
            None,
            Some(&secret)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut read = setup::request(vec![], vec![oxsum_core::IntegrationScope::EvidenceRead]);
    read.application_id = Some(namespace);
    let reader = db
        .create_integration_credential(&owner, &read)
        .await
        .unwrap()
        .secret
        .unwrap();
    assert_eq!(
        call(&app, "POST", &path, request.clone(), None, Some(&reader))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, bundle) = call(
        &app,
        "GET",
        &format!("{root}/evidence-bundles/{id}"),
        Value::Null,
        None,
        Some(&reader),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bundle["bundleId"], id);
    assert_eq!(bundle["scope"]["upstreamCostIncluded"], false);
    let mut changed = request.clone();
    changed["from"] = serde_json::json!(
        (time::OffsetDateTime::now_utc().date() - time::Duration::days(2)).to_string()
    );
    assert_eq!(
        call(&app, "POST", &path, changed, None, Some(&secret))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!(
                "/api/v1/integration/customers/{}/evidence-publications",
                Uuid::new_v4()
            ),
            request.clone(),
            None,
            Some(&secret)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    db.revoke_integration_credential(
        &owner,
        service.credential_id(),
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: "revoke".into(),
            reason: "fixture".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(&app, "POST", &path, request, None, Some(&secret))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

//! Application evidence list and original-byte download HTTP contract.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
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
async fn evidence_is_explicitly_scoped_and_downloads_the_original_file() {
    let Some(url) = oxsum_testkit::suite_db_url("application_evidence_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let namespace = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: "app".into(),
                key: "evidence".into(),
                name: "Evidence fixture".into(),
                customer_limit: 5,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let mut req = setup::request(
        vec![],
        vec![
            oxsum_core::IntegrationScope::CustomersProvision,
            oxsum_core::IntegrationScope::EvidenceRead,
        ],
    );
    req.application_id = Some(namespace);
    let secret = db
        .create_integration_credential(&owner, &req)
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
                name: "Evidence customer".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant)
        .await
        .unwrap();
    let day = time::OffsetDateTime::now_utc().date() - time::Duration::days(1);
    let id = db
        .mint_period_evidence(&wallet, org, &tenant, day, day, false, None)
        .await
        .unwrap()
        .bundle_id;
    let bytes = db.evidence_bundle_json(id).await.unwrap().unwrap();
    // Every route, including authentication plus the body read, works with one slot.
    let single = Db::connect(&url, 1).await.unwrap();
    let app = oxsum_server::app(single, Config::new(Signup::Invite, None));
    let path = format!("/api/v1/integration/customers/{org}/evidence-bundles");
    for target in [&path, &format!("{path}/{id}")] {
        assert_eq!(
            call(&app, "GET", target, Value::Null, None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", target, Value::Null, Some(&owner), None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &app,
                "GET",
                target,
                Value::Null,
                None,
                Some("oxs-int-invalid")
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let (status, page) = call(
        &app,
        "GET",
        &format!("{path}?limit=1"),
        Value::Null,
        None,
        Some(&secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["data"]["bundles"][0]["bundleId"], id.to_string());
    assert!(page["data"]["nextCursor"].is_null());
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{path}?limit=0"),
            Value::Null,
            None,
            Some(&secret)
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.clone().oneshot(
            Request::builder()
                .uri(format!("{path}/{id}"))
                .header("authorization", format!("Bearer {secret}"))
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains(&format!("evidence-{id}.json"))
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        bytes
    );
    let wrong = format!(
        "/api/v1/integration/customers/{}/evidence-bundles/{id}",
        Uuid::new_v4()
    );
    assert_eq!(
        call(&app, "GET", &wrong, Value::Null, None, Some(&secret))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    req.idempotency_key = "narrow".into();
    req.scopes = vec![oxsum_core::IntegrationScope::BillingRead];
    let narrow = db
        .create_integration_credential(&owner, &req)
        .await
        .unwrap()
        .secret
        .unwrap();
    assert_eq!(
        call(&app, "GET", &path, Value::Null, None, Some(&narrow))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE oxsum.evidence_bundles SET bundle_json=$2 WHERE bundle_id=$1")
        .bind(id)
        .bind(b"invalid".to_vec())
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{path}/{id}"),
            Value::Null,
            None,
            Some(&secret)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

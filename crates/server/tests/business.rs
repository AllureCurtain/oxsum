//! Deployment ownership reads cannot be reached by customer credentials.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser};
use oxsum_server::{Config, Signup};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn ownership_and_customer_reads_require_deployment_authority() {
    let Some(url) = oxsum_testkit::suite_db_url("business_http") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    let reg = db
        .register(NewUser {
            email: "business-http@example.com".into(),
            password: "business fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let operator = "business-fixture-operator";
    let app = oxsum_server::app(
        db,
        Config::new(Signup::Open, None).with_admin_token(operator),
    );
    for path in [
        "/api/v1/admin/business".into(),
        format!("/api/v1/admin/business/customers/{}", reg.organization.id),
    ] {
        for credential in [None, Some(reg.api_key.secret.as_str()), Some(operator)] {
            let mut req = Request::get(&path);
            if let Some(token) = credential {
                req = req.header("authorization", format!("Bearer {token}"));
            }
            let res = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                res.status(),
                if credential == Some(operator) {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
            if credential == Some(operator) {
                let value: Value =
                    serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                        .unwrap();
                if path.ends_with("business") {
                    assert!(value["data"]["business"]["initializedAt"].is_null());
                } else {
                    assert_eq!(
                        value["data"]["organizationId"],
                        reg.organization.id.to_string()
                    );
                    assert_eq!(value["data"]["tenantId"], reg.organization.tenant_id);
                }
            }
        }
    }
    let res = app
        .oneshot(
            Request::get(format!(
                "/api/v1/admin/business/customers/{}",
                Uuid::new_v4()
            ))
            .header("authorization", format!("Bearer {operator}"))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

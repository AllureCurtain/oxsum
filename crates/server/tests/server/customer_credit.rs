//! Monthly credit terms through the production router, including credential precedence.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    BusinessRole, BusinessStaffRequest, Db, NewUser, SessionContext, SessionContextRequest, Tenants,
};
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
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn request(key: &str) -> Value {
    json!({"idempotencyKey":key,"creditLimitMinor":1000,"paymentTermsDays":14,"dunningGraceDays":null,"reason":"HTTP monthly approval"})
}

#[tokio::test]
async fn monthly_credit_contract_authority_replay_and_atomic_refusal() {
    let Some(url) = oxsum_testkit::suite_db_url("customer_credit_http") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "credit HTTP customer password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let personal = db
        .login(&user.user.email, "credit HTTP customer password")
        .await
        .unwrap()
        .into_session()
        .token;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_admin_token("credit-http-operator"),
    );
    let path = format!(
        "/api/v1/business/customers/{}/credit-terms",
        user.organization.id
    );
    for method in ["GET", "PUT"] {
        for (cookie, bearer, status) in [
            (None, None, StatusCode::UNAUTHORIZED),
            (Some(personal.as_str()), None, StatusCode::FORBIDDEN),
            (Some(owner.as_str()), Some(""), StatusCode::FORBIDDEN),
            (
                Some(owner.as_str()),
                Some("oxs-int-unknown"),
                StatusCode::FORBIDDEN,
            ),
            (
                Some(owner.as_str()),
                Some("credit-http-operator"),
                StatusCode::FORBIDDEN,
            ),
            (
                None,
                Some(user.api_key.secret.as_str()),
                StatusCode::FORBIDDEN,
            ),
        ] {
            assert_eq!(
                call(&app, method, &path, request("denied"), cookie, bearer)
                    .await
                    .0,
                status
            );
        }
    }
    let exists: bool = sqlx::query_scalar("SELECT to_regnamespace($1) IS NOT NULL")
        .bind(format!("ledger_{}", user.organization.tenant_id))
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!exists);
    let (status, initial) = call(&app, "GET", &path, Value::Null, Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial["data"]["creditLimitMinor"], 0);
    for (field, value) in [
        ("creditLimitMinor", json!(-1)),
        ("creditLimitMinor", json!(9_007_199_254_740_992i64)),
        ("creditLimitMinor", json!(1.5)),
        ("paymentTermsDays", json!(3651)),
        ("dunningGraceDays", json!(-1)),
    ] {
        let mut body = request("invalid");
        body[field] = value;
        assert_eq!(
            call(&app, "PUT", &path, body, Some(&owner), None).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut missing = request("missing");
    missing.as_object_mut().unwrap().remove("dunningGraceDays");
    assert_eq!(
        call(&app, "PUT", &path, missing, Some(&owner), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (status, receipt) = call(&app, "PUT", &path, request("grant"), Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(
        call(&app, "PUT", &path, request("grant"), Some(&owner), None)
            .await
            .1,
        receipt
    );
    let mut conflict = request("grant");
    conflict["paymentTermsDays"] = json!(30);
    assert_eq!(
        call(&app, "PUT", &path, conflict, Some(&owner), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let wallet = Tenants::new(db.pool().clone())
        .get(&user.organization.tenant_id)
        .await
        .unwrap();
    wallet
        .hold(
            "hold",
            "HTTP credit reservation",
            800,
            time::OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let mut shrink = request("shrink");
    shrink["creditLimitMinor"] = json!(700);
    shrink["paymentTermsDays"] = json!(90);
    let (status, error) = call(&app, "PUT", &path, shrink, Some(&owner), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "VALIDATION_ERROR");
    let current = call(&app, "GET", &path, Value::Null, Some(&owner), None)
        .await
        .1;
    assert_eq!(current["data"]["creditLimitMinor"], 1000);
    assert_eq!(current["data"]["creditUsedMinor"], 800);
    assert_eq!(current["data"]["paymentTermsDays"], 14);
    for (role, read, write) in [
        (BusinessRole::Finance, StatusCode::OK, StatusCode::OK),
        (BusinessRole::Admin, StatusCode::OK, StatusCode::FORBIDDEN),
        (
            BusinessRole::Support,
            StatusCode::FORBIDDEN,
            StatusCode::FORBIDDEN,
        ),
    ] {
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                user_id: user.user.id,
                role,
                reason: "HTTP role boundary".into(),
            },
        )
        .await
        .unwrap();
        db.switch_session_context(
            &personal,
            &SessionContextRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                context: SessionContext::Business,
                organization_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            call(&app, "GET", &path, Value::Null, Some(&personal), None)
                .await
                .0,
            read
        );
        assert_eq!(
            call(
                &app,
                "PUT",
                &path,
                request(&Uuid::new_v4().to_string()),
                Some(&personal),
                None
            )
            .await
            .0,
            write
        );
    }
    let foreign = format!("/api/v1/business/customers/{}/credit-terms", Uuid::new_v4());
    for method in ["GET", "PUT"] {
        assert_eq!(
            call(
                &app,
                method,
                &foreign,
                request("foreign"),
                Some(&owner),
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
}

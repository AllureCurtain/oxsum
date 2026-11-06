//! Customer management and archived admission through the production router.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod support;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use oxsum_core::*;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
const PASSWORD: &str = "customer http fixture password";
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
    if let Some(token) = cookie {
        req = req.header("cookie", format!("oxsum_session={token}"));
    }
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
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
async fn user(db: &Db, name: &str) -> Registration {
    db.register(NewUser {
        email: format!("{name}@example.com"),
        password: PASSWORD.into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn customer_routes_scope_roles_and_keep_archive_independent_of_suspension() {
    let Some(url) = oxsum_testkit::suite_db_url("customer_lifecycle_http") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = user(&db, "owner").await;
    let owner_token = db
        .login(&owner.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token;
    let setup = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &BusinessSetupTokenRequest {
                idempotency_key: "setup".into(),
                owner_email: owner.user.email.clone(),
            },
        )
        .await
        .unwrap();
    db.accept_business_setup(
        &owner_token,
        &BusinessSetupRequest {
            token: setup.token.unwrap(),
            business_name: "Business".into(),
        },
    )
    .await
    .unwrap();
    db.switch_session_context(
        &owner_token,
        &SessionContextRequest {
            idempotency_key: "business".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    let customer = user(&db, "customer").await;
    let customer_token = db
        .login(&customer.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token;
    let secret = SecretKey::from_bytes([37; 32]);
    let model = support::flat_model(&db, &secret, 1).await;
    let wallet = Tenants::new(db.pool().clone())
        .get(&customer.organization.tenant_id)
        .await
        .unwrap();
    wallet
        .top_up(
            "fixture-funding",
            100,
            time::OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None)
            .with_secret(secret)
            .with_admin_token("test-deployment-token"),
    );
    let path = format!("/api/v1/business/customers/{}", customer.organization.id);
    for (method, url, body) in [
        ("GET", path.clone(), Value::Null),
        (
            "PATCH",
            path.clone(),
            json!({"idempotencyKey":"rename","name":"Renamed","contactEmail":null,"reason":"contact update"}),
        ),
        (
            "POST",
            format!("{path}/lifecycle"),
            json!({"idempotencyKey":"archive","state":"archived","reason":"closed"}),
        ),
        (
            "POST",
            format!("{path}/suspension"),
            json!({"idempotencyKey":"suspend","suspended":true,"reason":"review"}),
        ),
    ] {
        assert_eq!(
            call(&app, method, &url, body.clone(), None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(
                &app,
                method,
                &url,
                body.clone(),
                Some(&customer_token),
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        for key in [&customer.api_key.secret, "test-deployment-token"] {
            assert_eq!(
                call(
                    &app,
                    method,
                    &url,
                    body.clone(),
                    Some(&owner_token),
                    Some(key)
                )
                .await
                .0,
                if method == "GET" {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::FORBIDDEN
                }
            );
        }
    }
    let list = call(
        &app,
        "GET",
        "/api/v1/business/customers?limit=1",
        Value::Null,
        Some(&owner_token),
        None,
    )
    .await;
    assert_eq!(list.0, StatusCode::OK);
    assert_eq!(list.1["data"]["customers"].as_array().unwrap().len(), 1);
    assert!(list.1["data"]["nextCursor"].is_string());
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business/customers?limit=0",
            Value::Null,
            Some(&owner_token),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/v1/business/customers/{}", uuid::Uuid::new_v4()),
            Value::Null,
            Some(&owner_token),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let rename = json!({"idempotencyKey":"rename","name":"Renamed","contactEmail":"accounts@example.com","reason":"contact update"});
    let first = call(
        &app,
        "PATCH",
        &path,
        rename.clone(),
        Some(&owner_token),
        None,
    )
    .await;
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(
        call(&app, "PATCH", &path, rename, Some(&owner_token), None)
            .await
            .1,
        first.1
    );
    let before = call(&app, "GET", &path, Value::Null, Some(&owner_token), None)
        .await
        .1;
    assert_eq!(before["data"]["name"], "Renamed");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{path}/lifecycle"),
            json!({"idempotencyKey":"archive","state":"archived","reason":"closed"}),
            Some(&owner_token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{path}/suspension"),
            json!({"idempotencyKey":"clear-manual","suspended":false,"reason":"resolved"}),
            Some(&owner_token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let archived = call(&app, "GET", &path, Value::Null, Some(&owner_token), None)
        .await
        .1;
    assert_eq!(archived["data"]["state"], "archived");
    assert_eq!(archived["data"]["suspended"], true);
    assert_eq!(before["data"]["externalId"], archived["data"]["externalId"]);
    let available = wallet.available().await.unwrap();
    let denied = call(
        &app,
        "POST",
        "/v1/chat/completions",
        support::chat_body(&model),
        None,
        Some(&customer.api_key.secret),
    )
    .await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN, "{}", denied.1);
    assert_eq!(wallet.available().await.unwrap(), available);
    // Reads still work for the actual customer; archive cannot erase their bills.
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/org",
            Value::Null,
            Some(&customer_token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{path}/lifecycle"),
            json!({"idempotencyKey":"restore","state":"active","reason":"reopen"}),
            Some(&owner_token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let restored = call(&app, "GET", &path, Value::Null, Some(&owner_token), None)
        .await
        .1;
    assert_eq!(restored["data"]["suspended"], false);
    let served = call(
        &app,
        "POST",
        "/v1/chat/completions",
        support::chat_body(&model),
        None,
        Some(&customer.api_key.secret),
    )
    .await;
    assert_eq!(served.0, StatusCode::OK, "{}", served.1);
    assert_eq!(wallet.available().await.unwrap(), available - 1);
    let invalid = json!({"idempotencyKey":"identity-change","name":"X","contactEmail":null,"reason":"x","externalId":"other"});
    assert_eq!(
        call(&app, "PATCH", &path, invalid, Some(&owner_token), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

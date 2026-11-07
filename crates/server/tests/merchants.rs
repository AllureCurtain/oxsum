//! Production-router merchant configuration and key-loss startup refusal.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, MerchantCreateRequest, NewUser, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    token: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("cookie", format!("oxsum_session={token}"));
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
fn config() -> Value {
    json!({"apiUrl":"https://sandbox.hyperswitch.io","apiKey":"snd_http_merchant_secret","webhookSecret":"http-merchant-signing-secret","publishableKey":"pk_snd_http_merchant","sdkUrl":"https://sdk.example"})
}
fn create() -> Value {
    json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"HTTP merchant fixture","name":format!("http-{}",Uuid::new_v4().simple()),"rail":"hyperswitch","mode":"test","accountId":Uuid::new_v4().to_string(),"config":config()})
}
#[tokio::test]
async fn merchant_routes_require_personal_business_authority_and_never_return_credentials() {
    let Some(url) = oxsum_testkit::suite_db_url("merchants_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None)
            .with_secret(SecretKey::from_bytes([39; 32]))
            .with_admin_token("merchant-deployment-token"),
    );
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "merchant customer password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let customer = db
        .login(&customer.user.email, "merchant customer password")
        .await
        .unwrap()
        .into_session()
        .token;
    let root = "/api/v1/business/merchants";
    let request = create();
    for (token, bearer, status) in [
        (None, None, StatusCode::UNAUTHORIZED),
        (Some(customer.as_str()), None, StatusCode::FORBIDDEN),
        (
            Some(owner.as_str()),
            Some("merchant-deployment-token"),
            StatusCode::FORBIDDEN,
        ),
        (
            Some(owner.as_str()),
            Some("oxs-int-unknown"),
            StatusCode::FORBIDDEN,
        ),
        (Some(owner.as_str()), Some(""), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            call(&app, "POST", root, request.clone(), token, bearer)
                .await
                .0,
            status
        );
        assert_eq!(
            call(&app, "GET", root, Value::Null, token, bearer).await.0,
            status
        );
    }
    let (status, first) = call(&app, "POST", root, request.clone(), Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(
        call(&app, "POST", root, request.clone(), Some(&owner), None)
            .await
            .1,
        first
    );
    let merchant = first["data"]["merchantId"].as_str().unwrap();
    let versions = format!("{root}/{merchant}/versions");
    let status_path = format!("{root}/{merchant}/status");
    let rotation = json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"rotate HTTP fixture","config":config()});
    let (status, second) = call(
        &app,
        "POST",
        &versions,
        rotation.clone(),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["data"]["revision"], 2);
    assert_eq!(
        call(&app, "POST", &versions, rotation, Some(&owner), None)
            .await
            .1,
        second
    );
    let change = json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"enable checkout","enabled":true});
    let (status, enabled) = call(
        &app,
        "POST",
        &status_path,
        change.clone(),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(enabled["data"]["enabled"], true);
    assert_eq!(
        call(&app, "POST", &status_path, change, Some(&owner), None)
            .await
            .1,
        enabled
    );
    for path in [root, &versions] {
        let (status, result) = call(&app, "GET", path, Value::Null, Some(&owner), None).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        for sensitive in [
            "snd_http_merchant_secret",
            "http-merchant-signing-secret",
            "apiKey",
            "webhookSecret",
            "credentialsSealed",
        ] {
            assert!(!result.to_string().contains(sensitive));
        }
    }
    let mut changed = request.clone();
    changed["config"]["apiKey"] = json!("snd_changed_secret");
    assert_eq!(
        call(&app, "POST", root, changed, Some(&owner), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut malformed = create();
    malformed["config"]["unknownSecret"] = json!("unexpected");
    assert_eq!(
        call(&app, "POST", root, malformed, Some(&owner), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{root}/{}/versions", Uuid::new_v4()),
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for (method, path, body) in [
        ("GET", versions.clone(), Value::Null),
        (
            "POST",
            versions,
            json!({"idempotencyKey":"blocked-rotate","reason":"blocked","config":config()}),
        ),
        (
            "POST",
            status_path,
            json!({"idempotencyKey":"blocked-status","reason":"blocked","enabled":false}),
        ),
    ] {
        assert_eq!(
            call(&app, method, &path, body, Some(&customer), None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
}
#[tokio::test]
async fn startup_checks_all_historical_merchant_secrets_and_refuses_missing_key() {
    let Some(url) = oxsum_testkit::suite_db_url("merchant_prepare") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let key = SecretKey::from_bytes([41; 32]);
    let request: MerchantCreateRequest = serde_json::from_value(create()).unwrap();
    db.create_merchant(&owner, &request, &key).await.unwrap();
    assert!(
        oxsum_server::prepare(&db, &Config::new(Signup::Invite, None))
            .await
            .is_err()
    );
    assert!(
        oxsum_server::prepare(
            &db,
            &Config::new(Signup::Invite, None).with_secret(SecretKey::from_bytes([42; 32]))
        )
        .await
        .is_err()
    );
    oxsum_server::prepare(&db, &Config::new(Signup::Invite, None).with_secret(key))
        .await
        .unwrap();
    let app = oxsum_server::app(db, Config::new(Signup::Invite, None));
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/merchants",
            create(),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

//! Initial owner setup uses deployment authorization and personal consent.
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
const OPERATOR: &str = "setup-fixture-operator";
async fn call(
    app: &Router,
    path: &str,
    body: Value,
    bearer: Option<&str>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::post(path).header("content-type", "application/json");
    if let Some(token) = bearer {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    if let Some(token) = cookie {
        req = req.header("cookie", format!("oxsum_session={token}"));
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
#[tokio::test]
async fn only_deployment_authority_invites_and_only_the_named_session_accepts() {
    let Some(url) = oxsum_testkit::suite_db_url("business_setup_http") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    let owner = db
        .register(NewUser {
            email: "owner@example.com".into(),
            password: "business owner fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let oxsum_core::LoginOutcome::Session(session) = db
        .login(&owner.user.email, "business owner fixture password")
        .await
        .unwrap()
    else {
        panic!("session");
    };
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_admin_token(OPERATOR),
    );
    let mint = json!({"idempotencyKey":"setup","ownerEmail":owner.user.email});
    for (bearer, cookie) in [
        (None, None),
        (Some(owner.api_key.secret.as_str()), None),
        (None, Some(session.token.as_str())),
    ] {
        assert_eq!(
            call(
                &app,
                "/api/v1/admin/business/setup-tokens",
                mint.clone(),
                bearer,
                cookie
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let (status, created) = call(
        &app,
        "/api/v1/admin/business/setup-tokens",
        mint.clone(),
        Some(OPERATOR),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let token = created["data"]["token"].as_str().unwrap();
    let (_, replay) = call(
        &app,
        "/api/v1/admin/business/setup-tokens",
        mint,
        Some(OPERATOR),
        None,
    )
    .await;
    assert!(replay["data"]["token"].is_null());
    let consent = json!({"token":token,"businessName":"Owner's business"});
    assert_eq!(
        call(&app, "/api/v1/business/setup", consent.clone(), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/business/setup",
            consent.clone(),
            Some(owner.api_key.secret.as_str()),
            Some(&session.token)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, result) = call(
        &app,
        "/api/v1/business/setup",
        consent.clone(),
        None,
        Some(&session.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(result["data"]["registration"].is_null());
    assert_eq!(
        result["data"]["ownership"]["business"]["name"],
        "Owner's business"
    );
    assert!(!result.to_string().contains(token));
    assert_eq!(
        call(
            &app,
            "/api/v1/business/setup",
            consent,
            None,
            Some(&session.token)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

//! A01/A04 through the production router, including the legacy environment override.
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    BusinessSetupRegistrationRequest, BusinessSetupTokenRequest, Db, NewUser, SecretKey,
    SetupAuthority,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
const PASSWORD: &str = "business http fixture password";
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
async fn login(app: &Router, email: &str) -> String {
    let res = app
        .clone()
        .oneshot(
            Request::post("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"email":email,"password":PASSWORD}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("oxsum_session=")
        .unwrap()
        .to_owned()
}
#[tokio::test]
async fn explicit_contexts_staff_routes_and_saved_policy() {
    let Some(url) = oxsum_testkit::suite_db_url("business_staff_http") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Open, None)
            .with_secret(SecretKey::from_bytes([31; 32]))
            .with_admin_token("business-test-operator"),
    );
    let signup = json!({"email":"new@example.com","password":PASSWORD});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/register",
            signup.clone(),
            None,
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "legacy open config cannot override fresh business policy"
    );
    let setup = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &BusinessSetupTokenRequest {
                idempotency_key: "setup".into(),
                owner_email: "owner@example.com".into(),
            },
        )
        .await
        .unwrap();
    let initial = db
        .register_business_owner(&BusinessSetupRegistrationRequest {
            token: setup.token.unwrap(),
            business_name: "HTTP business".into(),
            email: "owner@example.com".into(),
            password: PASSWORD.into(),
        })
        .await
        .unwrap()
        .registration
        .unwrap();
    let owner = login(&app, "owner@example.com").await;
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/session/context",
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .1["data"]["context"],
        "customer"
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business",
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/session/context",
            json!({"idempotencyKey":"business","context":"business"}),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    for path in [
        "/api/v1/balance",
        "/api/v1/entries",
        "/api/v1/org/members",
        "/api/v1/session",
    ] {
        assert_eq!(
            call(&app, "GET", path, Value::Null, Some(&owner), None)
                .await
                .0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    assert_eq!(
        call(
            &app,
            "DELETE",
            "/api/v1/users/me",
            json!({"password":PASSWORD}),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let paths = [
        ("GET", "/api/v1/business", Value::Null),
        ("GET", "/api/v1/business/staff", Value::Null),
        (
            "POST",
            "/api/v1/business/registration",
            json!({"idempotencyKey":"deny","mode":"open","reason":"test"}),
        ),
    ];
    for (method, path, body) in paths {
        assert_eq!(
            call(&app, method, path, body.clone(), None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        for key in [&initial.api_key.secret, "business-test-operator", "invalid"] {
            assert_eq!(
                call(&app, method, path, body.clone(), Some(&owner), Some(key))
                    .await
                    .0,
                StatusCode::FORBIDDEN,
                "{path}"
            );
        }
    }
    let staff = db
        .register(NewUser {
            email: "staff@example.com".into(),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let invite = json!({"idempotencyKey":"staff","userId":staff.user.id,"role":"finance","reason":"billing operations"});
    let appointed = call(
        &app,
        "POST",
        "/api/v1/business/staff",
        invite.clone(),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(appointed.0, StatusCode::OK, "{}", appointed.1);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/staff",
            invite,
            Some(&owner),
            None
        )
        .await
        .1,
        appointed.1
    );
    // Staff authority survives the removal of all independent customer seats.
    sqlx::query("DELETE FROM oxsum.memberships WHERE user_id=$1")
        .bind(staff.user.id)
        .execute(db.pool())
        .await
        .unwrap();
    let staff_token = login(&app, &staff.user.email).await;
    assert!(
        call(
            &app,
            "GET",
            "/api/v1/session/context",
            Value::Null,
            Some(&staff_token),
            None
        )
        .await
        .1["data"]["organizationId"]
            .is_null()
    );

    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/session/context",
            json!({"idempotencyKey":"business","context":"business"}),
            Some(&staff_token),
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
            "/api/v1/auth/totp/enroll",
            Value::Null,
            Some(&staff_token),
            None
        )
        .await
        .0,
        StatusCode::OK,
        "staff without customer membership can manage personal security"
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/auth/verify/request",
            Value::Null,
            Some(&staff_token),
            None
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE,
        "personal identity resolves; only mail configuration is absent"
    );
    let view = call(
        &app,
        "GET",
        "/api/v1/business",
        Value::Null,
        Some(&staff_token),
        None,
    )
    .await;
    assert_eq!(view.0, StatusCode::OK);
    assert_eq!(view.1["data"]["role"], "finance");
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business/staff",
            Value::Null,
            Some(&staff_token),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/registration",
            json!({"idempotencyKey":"open","mode":"open","reason":"enable signup"}),
            Some(&staff_token),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/registration",
            json!({"idempotencyKey":"open","mode":"open","reason":"enable signup"}),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    // Even the legacy invite config does not override the owner-selected open policy.
    let legacy_invite = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    assert_eq!(
        call(
            &legacy_invite,
            "POST",
            "/api/v1/auth/register",
            signup,
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let list = call(
        &app,
        "GET",
        "/api/v1/business/staff",
        Value::Null,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(list.1["data"].as_array().unwrap().len(), 2);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/staff/revoke",
            json!({"idempotencyKey":"revoke","userId":staff.user.id,"reason":"end access"}),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business",
            Value::Null,
            Some(&staff_token),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    sqlx::query(
        "INSERT INTO oxsum.memberships (organization_id,user_id,role) VALUES ($1,$2,'owner')",
    )
    .bind(staff.organization.id)
    .bind(staff.user.id)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/session/context",
            json!({"idempotencyKey":"customer","context":"customer","organizationId":staff.organization.id}),
            Some(&staff_token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/org",
            Value::Null,
            Some(&staff_token),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(call(&app,"POST","/api/v1/business/staff",json!({"idempotencyKey":"reappoint","userId":staff.user.id,"role":"admin","reason":"successor"}),Some(&owner),None).await.0,StatusCode::OK);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/ownership",
            json!({"idempotencyKey":"transfer","userId":staff.user.id,"reason":"handover"}),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business/staff",
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    db.logout(&owner).await.unwrap();
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/session/context",
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

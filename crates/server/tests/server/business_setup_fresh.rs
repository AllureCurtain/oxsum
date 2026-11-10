//! Fresh invite-only installation has a complete initial-owner path.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{BusinessSetupTokenRequest, Db, SetupAuthority};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
#[tokio::test]
async fn invited_registration_initializes_without_public_signup_or_provider_configuration() {
    let Some(url) = oxsum_testkit::suite_db_url("business_setup_fresh_http") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    let invitation = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &BusinessSetupTokenRequest {
                idempotency_key: "fresh".into(),
                owner_email: "fresh@example.com".into(),
            },
        )
        .await
        .unwrap();
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let response=app.oneshot(Request::post("/api/v1/business/setup/register").header("content-type","application/json").body(Body::from(json!({"token":invitation.token,"businessName":"Fresh","email":"fresh@example.com","password":"fresh business fixture password"}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        result["data"]["ownership"]["business"]["registrationMode"],
        "invite"
    );
    assert_eq!(
        result["data"]["registration"]["user"]["email"],
        "fresh@example.com"
    );
    assert!(
        result["data"]["registration"]["apiKey"]["secret"]
            .as_str()
            .unwrap()
            .starts_with("oxs-")
    );
    assert!(
        db.login("fresh@example.com", "fresh business fixture password")
            .await
            .is_ok()
    );
}

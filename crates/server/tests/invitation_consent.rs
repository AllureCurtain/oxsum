//! Production-router consent flows, including retired direct-add denial.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::*;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
const PASSWORD: &str = "customer invitation http password";
async fn call(
    app: &Router,
    path: &str,
    body: Value,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
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
    (
        status,
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
            .unwrap_or(Value::Null),
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
async fn login(db: &Db, user: &User) -> String {
    db.login(&user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token
}
#[tokio::test]
async fn business_and_team_links_require_recipient_consent_through_real_routes() {
    let Some(url) = oxsum_testkit::suite_db_url("invitation_http") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = user(&db, "owner").await;
    let owner_token = login(&db, &owner.user).await;
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
            business_name: "Test merchant".into(),
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
    let recipient = user(&db, "recipient").await;
    let token = login(&db, &recipient.user).await;
    let stranger = user(&db, "stranger").await;
    let wrong = login(&db, &stranger.user).await;
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let terms = json!({"idempotencyKey":"new-customer","email":recipient.user.email,"name":"Offered account","externalId":"external-42","reason":"Customer enrollment"});
    let create = "/api/v1/business/customer-invitations";
    for (cookie, bearer, expected) in [
        (None, None, StatusCode::UNAUTHORIZED),
        (Some(token.as_str()), None, StatusCode::FORBIDDEN),
        (
            Some(owner_token.as_str()),
            Some(recipient.api_key.secret.as_str()),
            StatusCode::FORBIDDEN,
        ),
    ] {
        assert_eq!(
            call(&app, create, terms.clone(), cookie, bearer).await.0,
            expected
        );
    }
    let (status, invite) = call(&app, create, terms.clone(), Some(&owner_token), None).await;
    assert_eq!(status, StatusCode::OK, "{invite}");
    let data = &invite["data"];
    let secret = data["token"].as_str().unwrap();
    let id = data["id"].as_str().unwrap();
    let org = data["organizationId"].as_str().unwrap();
    let preview = "/api/v1/invitations/preview";
    let accept = "/api/v1/invitations/accept";
    let body = json!({"token":secret});
    let (status, previewed) = call(&app, preview, body.clone(), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(previewed["data"]["name"], "Offered account");
    assert_eq!(previewed["data"]["role"], "owner");
    assert!(previewed["data"].get("email").is_none());
    assert!(previewed["data"].get("token").is_none());
    assert_eq!(
        call(&app, accept, body.clone(), None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, accept, body.clone(), Some(&wrong), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            accept,
            body.clone(),
            Some(&token),
            Some(&recipient.api_key.secret)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            accept,
            json!({"token":secret,"role":"owner"}),
            Some(&token),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (status, accepted) = call(&app, accept, body.clone(), Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["data"]["organizationId"], org);
    assert_eq!(
        call(&app, accept, body, Some(&token), None).await.1,
        accepted
    );
    let replay = call(&app, create, terms, Some(&owner_token), None).await.1;
    assert_eq!(replay["data"]["id"], id);
    assert!(replay["data"]["token"].is_null());
    assert_eq!(
        call(
            &app,
            &format!("{create}/{id}/revoke"),
            json!({"idempotencyKey":"too-late","reason":"Cancel"}),
            Some(&owner_token),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // New-user registration creates the reserved customer despite invite-only policy.
    let terms = json!({"idempotencyKey":"fresh","email":"fresh@example.com","name":"Fresh customer","externalId":"fresh-external","reason":"Enroll"});
    let fresh = call(&app, create, terms, Some(&owner_token), None).await.1;
    let fresh_body =
        json!({"token":fresh["data"]["token"],"email":"fresh@example.com","password":PASSWORD});
    let (status, registered) = call(
        &app,
        "/api/v1/invitations/redeem",
        fresh_body.clone(),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{registered}");
    assert_eq!(
        registered["data"]["organization"]["id"],
        fresh["data"]["organizationId"]
    );
    assert!(
        registered["data"]["apiKey"]["secret"]
            .as_str()
            .unwrap()
            .starts_with("oxs-")
    );
    assert_eq!(
        call(&app, "/api/v1/invitations/redeem", fresh_body, None, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let terms = json!({"idempotencyKey":"cancel","email":"cancel@example.com","name":"Canceled customer","externalId":"cancel-external","reason":"Enroll"});
    let canceled = call(&app, create, terms, Some(&owner_token), None).await.1;
    let cancel_path = format!(
        "{create}/{}/revoke",
        canceled["data"]["id"].as_str().unwrap()
    );
    let cancel = json!({"idempotencyKey":"revoke-customer","reason":"Withdraw"});
    let (status, receipt) =
        call(&app, &cancel_path, cancel.clone(), Some(&owner_token), None).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(
        call(&app, &cancel_path, cancel, Some(&owner_token), None)
            .await
            .1,
        receipt
    );
    assert_eq!(
        call(
            &app,
            preview,
            json!({"token":canceled["data"]["token"]}),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Existing customer team: no membership until acceptance, no first-key changes.
    for body in [json!({"email":stranger.user.email}), Value::Null] {
        assert_eq!(
            call(&app, "/api/v1/org/members", body, Some(&token), None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let team = "/api/v1/org/invitations";
    let terms = json!({"idempotencyKey":"team","intendedEmail":stranger.user.email});
    let (status, invited) = call(&app, team, terms.clone(), Some(&token), None).await;
    assert_eq!(status, StatusCode::OK, "{invited}");
    assert_eq!(
        db.members(recipient.organization.id).await.unwrap().len(),
        1
    );
    let replay = call(&app, team, terms, Some(&token), None).await.1;
    assert_eq!(replay["data"]["id"], invited["data"]["id"]);
    assert!(replay["data"]["token"].is_null());
    let (status, receipt) = call(
        &app,
        accept,
        json!({"token":invited["data"]["token"]}),
        Some(&wrong),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["data"]["role"], "member");
    assert_eq!(
        db.members(recipient.organization.id).await.unwrap().len(),
        2
    );
    let canceled = call(
        &app,
        team,
        json!({"idempotencyKey":"team-cancel"}),
        Some(&token),
        None,
    )
    .await
    .1;
    let path = format!("{team}/{}/revoke", canceled["data"]["id"].as_str().unwrap());
    let (status, receipt) = call(
        &app,
        &path,
        json!({"idempotencyKey":"cancel","reason":"Withdraw"}),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(
        call(
            &app,
            accept,
            json!({"token":canceled["data"]["token"]}),
            Some(&wrong),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        db.authenticate(&recipient.api_key.secret)
            .await
            .unwrap()
            .unwrap()
            .0
            .id,
        recipient.organization.id
    );
    assert_eq!(
        db.members(org.parse::<Uuid>().unwrap())
            .await
            .unwrap()
            .len(),
        1
    );
}

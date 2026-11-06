//! A07: production business routes and loopback provider transport.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    routing::any,
};
use http_body_util::BodyExt;
use oxsum_core::*;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;

async fn call(app: &Router, path: &str, body: Value, token: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("cookie", format!("oxsum_session={token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}
fn request(config: Value) -> Value {
    json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"explicit provider fixture","config":config})
}

#[tokio::test]
async fn explicit_check_requires_owner_commits_once_and_does_not_infer_on_save() {
    let Some(url) = oxsum_testkit::suite_db_url("provider_destinations") else {
        return;
    };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let admin = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "provider admin fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.set_business_staff(
        &owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "provider fixture admin".into(),
            user_id: admin.user.id,
            role: BusinessRole::Admin,
        },
    )
    .await
    .unwrap();
    let admin_token = db
        .login(&admin.user.email, "provider admin fixture password")
        .await
        .unwrap()
        .into_session()
        .token;
    db.switch_session_context(
        &admin_token,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_secret(SecretKey::from_bytes([9; 32])),
    );
    let seen = Arc::new(Mutex::new(Vec::<(String, HeaderMap)>::new()));
    let capture = seen.clone();
    let upstream = Router::new().fallback(any(
        move |method: axum::http::Method, headers: HeaderMap| {
            let capture = capture.clone();
            async move {
                capture.lock().unwrap().push((method.to_string(), headers));
                StatusCode::OK
            }
        },
    ));
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", socket.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        axum::serve(socket, upstream).await.unwrap();
    });
    for (protocol, header) in [
        ("openai", "authorization"),
        ("anthropic", "x-api-key"),
        ("gemini", "x-goog-api-key"),
    ] {
        let name = format!("check-{protocol}");
        let before = seen.lock().unwrap().len();
        let mut config = json!({"name":name,"baseUrl":base,"protocol":protocol,"apiKeys":["connection-secret-key"]});
        assert_eq!(
            call(
                &app,
                "/api/v1/business/channels",
                request(config.clone()),
                Some(&owner)
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        config["destinationPolicy"] = json!({"network":"internal","allowedIps":["127.0.0.1"]});
        assert_eq!(
            call(
                &app,
                "/api/v1/business/channels",
                request(config),
                Some(&owner)
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(seen.lock().unwrap().len(), before, "save must not probe");
        let path = format!("/api/v1/business/channels/{name}/connection-check");
        let req = request(json!({}));
        assert_eq!(
            call(&app, &path, req.clone(), Some(&admin_token)).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, &path, req.clone(), None).await.0,
            StatusCode::UNAUTHORIZED
        );
        let (a, b) = tokio::join!(
            call(&app, &path, req.clone(), Some(&owner)),
            call(&app, &path, req.clone(), Some(&owner))
        );
        assert_eq!(a.0, StatusCode::OK);
        assert_eq!(b.0, StatusCode::OK);
        assert_eq!(a.1["data"]["id"], b.1["data"]["id"]);
        let final_result = call(&app, &path, req.clone(), Some(&owner)).await.1;
        assert_eq!(final_result["data"]["state"], "reachable");
        assert_eq!(final_result["data"]["httpStatus"], 200);
        assert!(!final_result.to_string().contains("secret"));
        {
            let records = seen.lock().unwrap();
            assert_eq!(
                records.len(),
                before + 1,
                "concurrent replay must send only once"
            );
            assert_eq!(records.last().unwrap().0, "HEAD");
            assert!(
                records.last().unwrap().1[header]
                    .to_str()
                    .unwrap()
                    .contains("connection-secret-key")
            );
        }
        let mut changed = req;
        changed["reason"] = json!("different reason");
        assert_eq!(
            call(&app, &path, changed, Some(&owner)).await.0,
            StatusCode::CONFLICT
        );
    }
    // Existing deployments receive restrictive public policy until explicitly approved.
    db.set_channel(
        "legacy-internal",
        &base,
        "legacy-provider-key",
        "openai",
        &SecretKey::from_bytes([9; 32]),
    )
    .await
    .unwrap();
    let denied = call(
        &app,
        "/api/v1/business/channels/legacy-internal/connection-check",
        request(json!({})),
        Some(&owner),
    )
    .await;
    assert_eq!(denied.1["data"]["state"], "blocked");
    assert_eq!(seen.lock().unwrap().len(), 3);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.provider_connection_checks")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 4);
    // A committed but interrupted intent is never automatically resent.
    let pending = request(json!({}));
    let started = db
        .begin_provider_check(
            &owner,
            "check-openai",
            pending["idempotencyKey"].as_str().unwrap(),
            pending["reason"].as_str().unwrap(),
        )
        .await
        .unwrap();
    assert!(started.endpoint.is_some());
    let replay = call(
        &app,
        "/api/v1/business/channels/check-openai/connection-check",
        pending,
        Some(&owner),
    )
    .await;
    assert_eq!(replay.1["data"]["state"], "pending");
    assert_eq!(seen.lock().unwrap().len(), 3);
    // Audit and pending result roll back before any provider request.
    use sqlx::Executor;
    db.pool().execute("CREATE FUNCTION oxsum.reject_check_audit() RETURNS trigger AS $$ BEGIN IF NEW.action='business.provider.connection_check' THEN RAISE EXCEPTION 'check audit fault'; END IF; RETURN NEW; END; $$ LANGUAGE plpgsql; CREATE TRIGGER check_audit_fault BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_check_audit()").await.unwrap();
    assert_eq!(
        call(
            &app,
            "/api/v1/business/channels/check-openai/connection-check",
            request(json!({})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(seen.lock().unwrap().len(), 3);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.provider_connection_checks")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 5);
    db.pool()
        .execute("DROP TRIGGER check_audit_fault ON oxsum.admin_audit")
        .await
        .unwrap();
    assert!(
        sqlx::query(
            "UPDATE oxsum.provider_connection_checks SET state='pending' WHERE state='reachable'"
        )
        .execute(db.pool())
        .await
        .is_err()
    );
    db.logout(&owner).await.unwrap();
    assert_eq!(
        call(
            &app,
            "/api/v1/business/channels/check-openai/connection-check",
            request(json!({})),
            Some(&owner)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    handle.abort();
}

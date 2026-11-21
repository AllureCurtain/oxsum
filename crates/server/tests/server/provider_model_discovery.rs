//! Model discovery through the production router and a local provider catalog.

#[path = "../../../core/tests/support/integration.rs"]
mod setup;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use http_body_util::BodyExt;
use oxsum_core::{
    BusinessRole, BusinessStaffRequest, Db, NewUser, SecretKey, SessionContext,
    SessionContextRequest,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "provider-discovery-fixture-key";

fn intent(config: Value) -> Value {
    json!({"idempotencyKey": Uuid::new_v4(), "reason": "discover fixture models", "config": config})
}

async fn post(app: &Router, path: &str, body: &Value, token: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::post(path).header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("cookie", format!("oxsum_session={token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn admin_session(db: &Db, owner: &str) -> String {
    let email = format!("{}@example.com", Uuid::new_v4());
    let password = "discovery admin fixture password";
    let admin = db
        .register(NewUser {
            email: email.clone(),
            password: password.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.set_business_staff(
        owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "discovery admin fixture".into(),
            user_id: admin.user.id,
            role: BusinessRole::Admin,
        },
    )
    .await
    .unwrap();
    let token = db
        .login(&email, password)
        .await
        .unwrap()
        .into_session()
        .token;
    db.switch_session_context(
        &token,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    token
}

fn catalog(uri: &Uri) -> Response {
    match uri.path() {
        "/openai/v1/models" => axum::Json(json!({"data": [
            {"id":"org/model-b", "unrelated":"private raw metadata"}, {"id":"model-a"},
            {"id":"model-a"}, {"id":"invalid\nname"}, {"id":KEY}
        ]}))
        .into_response(),
        "/anthropic/v1/models" => {
            axum::Json(json!({"data":[{"id":"claude-fixture"}],"has_more":true})).into_response()
        }
        "/gemini/v1beta/models" => axum::Json(
            json!({"models":[{"name":"models/gemini-fixture"}],"nextPageToken":"private-cursor"}),
        )
        .into_response(),
        "/empty/v1/models" => axum::Json(json!({"data":[]})).into_response(),
        "/invalid/v1/models" => axum::Json(json!({"error":"private raw metadata"})).into_response(),
        "/large/v1/models" => "x".repeat(1024 * 1024 + 1).into_response(),
        "/many/v1/models" => {
            let data: Vec<_> = (0..501)
                .rev()
                .map(|i| json!({"id":format!("fixture-{i:03}")}))
                .collect();
            axum::Json(json!({"data":data})).into_response()
        }
        "/missing/v1/models" => StatusCode::NOT_FOUND.into_response(),
        "/bad-key/v1/models" => (StatusCode::UNAUTHORIZED, "private raw metadata").into_response(),
        "/redirect/v1/models" => axum::response::Redirect::temporary("/unintended").into_response(),
        _ => panic!("unexpected request: {uri}"),
    }
}

#[tokio::test]
async fn discovery_lists_candidates_once_without_publishing_models() {
    let url = oxsum_testkit::suite_db!();
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let admin = admin_session(&db, &owner).await;
    let secret = SecretKey::from_bytes([9; 32]);
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_secret(secret.clone()),
    );

    let seen = Arc::new(Mutex::new(Vec::new()));
    let capture = seen.clone();
    let upstream_db = db.clone();
    let upstream = Router::new().fallback(any(move |method: Method, uri: Uri, headers: HeaderMap| {
        let capture = capture.clone();
        let db = upstream_db.clone();
        async move {
            let channel = uri.path().split('/').nth(1).unwrap();
            let audited: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oxsum.provider_model_discoveries d JOIN oxsum.channels c USING(channel_id) JOIN oxsum.business_mutations m USING(mutation_id) WHERE c.name=$1 AND d.state='pending' AND m.action='business.provider.model_discovery')")
                .bind(channel).fetch_one(db.pool()).await.unwrap();
            assert!(audited, "intent must be committed before the upstream call");
            capture.lock().unwrap().push((method, uri.clone(), headers));
            catalog(&uri)
        }
    }));
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", socket.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(socket, upstream).await.unwrap();
    });

    for (name, protocol, wire_version, upstream_status, state, models, partial) in [
        (
            "openai",
            "openai",
            "v1",
            Some(200),
            "listed",
            vec!["model-a", "org/model-b"],
            true,
        ),
        (
            "anthropic",
            "anthropic",
            "v1",
            Some(200),
            "listed",
            vec!["claude-fixture"],
            true,
        ),
        (
            "gemini",
            "gemini",
            "v1beta",
            Some(200),
            "listed",
            vec!["gemini-fixture"],
            true,
        ),
        ("empty", "openai", "v1", Some(200), "listed", vec![], false),
        (
            "invalid",
            "openai",
            "v1",
            Some(200),
            "invalidCatalog",
            vec![],
            false,
        ),
        (
            "large",
            "openai",
            "v1",
            Some(200),
            "invalidCatalog",
            vec![],
            false,
        ),
        (
            "missing",
            "openai",
            "v1",
            Some(404),
            "unsupported",
            vec![],
            false,
        ),
        (
            "bad-key",
            "openai",
            "v1",
            Some(401),
            "authenticationFailed",
            vec![],
            false,
        ),
        (
            "redirect",
            "openai",
            "v1",
            Some(307),
            "httpError",
            vec![],
            false,
        ),
        ("tasks", "tasks", "v1", None, "unsupported", vec![], false),
        ("many", "openai", "v1", Some(200), "listed", vec![], true),
    ] {
        let prior = seen.lock().unwrap().len();
        let (status, body) = post(&app, "/api/v1/business/channels", &intent(json!({
            "name":name, "baseUrl":format!("{base}/{name}/{wire_version}"), "protocol":protocol,
            "apiKeys":[KEY], "destinationPolicy":{"network":"internal", "allowedIps":["127.0.0.1"]}
        })), Some(&owner)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let channel_id = body["data"]["resourceId"].clone();
        assert_eq!(seen.lock().unwrap().len(), prior, "save must not discover");

        let path = format!("/api/v1/business/channels/{name}/model-discovery");
        let body = intent(json!({}));
        if name == "openai" {
            assert_eq!(
                post(&app, &path, &body, None).await.0,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                post(&app, &path, &body, Some(&admin)).await.0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(seen.lock().unwrap().len(), prior);
        }
        let (a, b) = tokio::join!(
            post(&app, &path, &body, Some(&owner)),
            post(&app, &path, &body, Some(&owner))
        );
        assert_eq!(a.0, StatusCode::OK, "{}", a.1);
        assert_eq!(b.0, StatusCode::OK, "{}", b.1);
        let result = post(&app, &path, &body, Some(&owner)).await.1;
        assert_eq!(a.1["data"]["id"], result["data"]["id"]);
        assert_eq!(b.1["data"]["id"], result["data"]["id"]);
        assert_eq!(result["data"]["channelId"], channel_id);
        assert_eq!(result["data"]["state"], state, "{name}: {result}");
        assert_eq!(result["data"]["httpStatus"], json!(upstream_status));
        assert_eq!(result["data"]["partial"], partial);
        if name == "many" {
            let expected: Vec<_> = (0..500).map(|i| format!("fixture-{i:03}")).collect();
            assert_eq!(result["data"]["models"], json!(expected));
        } else {
            assert_eq!(result["data"]["models"], json!(models));
        }
        assert!(!result.to_string().contains("private"));
        assert!(!result.to_string().contains(KEY));
        {
            let calls = seen.lock().unwrap();
            assert_eq!(
                calls.len(),
                prior + usize::from(upstream_status.is_some()),
                "{name}"
            );
            if upstream_status.is_some() {
                let (method, uri, headers) = calls.last().unwrap();
                assert_eq!(*method, Method::GET);
                assert_eq!(uri.to_string(), format!("/{name}/{wire_version}/models"));
                match protocol {
                    "anthropic" => {
                        assert_eq!(headers["x-api-key"], KEY);
                        assert_eq!(headers["anthropic-version"], "2023-06-01");
                    }
                    "gemini" => assert_eq!(headers["x-goog-api-key"], KEY),
                    _ => assert_eq!(headers["authorization"], format!("Bearer {KEY}")),
                }
            }
        }
        if name == "openai" {
            let mut changed = body.clone();
            changed["reason"] = json!("different intent");
            assert_eq!(
                post(&app, &path, &changed, Some(&owner)).await.0,
                StatusCode::CONFLICT
            );
            assert_eq!(seen.lock().unwrap().len(), prior + 1);
        }
    }
    let prior = seen.lock().unwrap().len();
    db.set_channel("blocked-catalog", &base, KEY, "openai", &secret)
        .await
        .unwrap();
    let blocked = post(
        &app,
        "/api/v1/business/channels/blocked-catalog/model-discovery",
        &intent(json!({})),
        Some(&owner),
    )
    .await;
    assert_eq!(blocked.1["data"]["state"], "blocked");
    let pending = intent(json!({}));
    let started = db
        .begin_provider_model_discovery(
            &owner,
            "openai",
            pending["idempotencyKey"].as_str().unwrap(),
            pending["reason"].as_str().unwrap(),
        )
        .await
        .unwrap();
    let replay = post(
        &app,
        "/api/v1/business/channels/openai/model-discovery",
        &pending,
        Some(&owner),
    )
    .await;
    assert!(started.endpoint.is_some());
    assert_eq!(replay.1["data"]["id"], started.result.id.to_string());
    assert_eq!(replay.1["data"]["state"], "pending");
    assert_eq!(seen.lock().unwrap().len(), prior);
    let prices: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.channel_prices")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(prices, 0);
    server.abort();
}

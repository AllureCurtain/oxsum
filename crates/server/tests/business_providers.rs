//! Production-router coverage of every business provider operation.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::*;
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
fn body(config: Value) -> Value {
    json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"provider HTTP fixture","config":config})
}
#[tokio::test]
async fn all_provider_routes_bind_business_authority_and_replay_receipts() {
    let Some(url) = oxsum_testkit::suite_db_url("business_provider_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None)
            .with_secret(SecretKey::from_bytes([9; 32]))
            .with_admin_token("provider-deployment-fixture"),
    );
    let name = format!("http-{}", Uuid::new_v4().simple());
    let root = format!("/api/v1/business/channels/{name}");
    let request = body(
        json!({"name":name,"baseUrl":"https://provider.example/v1","protocol":"openai","apiKeys":["provider-http-secret"]}),
    );
    for (token, bearer, status) in [
        (None, None, StatusCode::UNAUTHORIZED),
        (
            Some(owner.as_str()),
            Some("provider-deployment-fixture"),
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
            call(
                &app,
                "POST",
                "/api/v1/business/channels",
                request.clone(),
                token,
                bearer
            )
            .await
            .0,
            status
        );
    }
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/business/channels",
        request.clone(),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(
        created,
        call(
            &app,
            "POST",
            "/api/v1/business/channels",
            request,
            Some(&owner),
            None
        )
        .await
        .1
    );
    let (status, list) = call(
        &app,
        "GET",
        "/api/v1/business/channels?limit=1",
        Value::Null,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"]["channels"].as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains("provider-http-secret"));
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business/channels?limit=0",
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mutations = [
        (
            "POST",
            format!("{root}/billing-credential"),
            json!({"apiKey":"billing-http-secret"}),
        ),
        (
            "POST",
            format!("{root}/cache"),
            json!({"policy":{"enabled":true,"ttlSeconds":60}}),
        ),
        (
            "POST",
            format!("{root}/prices"),
            json!({"model":"http-model-a","price":{"inputPricePerMillion":10,"outputPricePerMillion":10,"maxOutputTokens":100},"weight":100}),
        ),
        (
            "POST",
            format!("{root}/prices"),
            json!({"model":"http-model-b","price":{"inputPricePerMillion":20,"outputPricePerMillion":20,"maxOutputTokens":100},"weight":100}),
        ),
        (
            "POST",
            format!("{root}/aliases"),
            json!({"alias":"http-alias","upstreamModel":"http-model-a"}),
        ),
        (
            "PUT",
            "/api/v1/business/model-fallbacks/http-alias".into(),
            json!({"fallbacks":["http-model-b"]}),
        ),
    ];
    for (method, path, config) in mutations {
        let request = body(config);
        let (status, result) = call(&app, method, &path, request.clone(), Some(&owner), None).await;
        assert_eq!(status, StatusCode::OK, "{path}: {result}");
        assert_eq!(
            result,
            call(&app, method, &path, request, Some(&owner), None)
                .await
                .1
        );
    }
    for path in [
        format!("{root}/prices"),
        format!("{root}/aliases"),
        "/api/v1/business/model-fallbacks/http-alias".into(),
    ] {
        assert_eq!(
            call(&app, "GET", &path, Value::Null, Some(&owner), None)
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            call(
                &app,
                "GET",
                &path,
                Value::Null,
                None,
                Some("provider-deployment-fixture")
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, added) = call(
        &app,
        "POST",
        &format!("{root}/credentials"),
        body(json!({"apiKey":"second-http-secret"})),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let credential = added["data"]["resourceId"].as_str().unwrap();
    for status in ["disabled", "active"] {
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("{root}/credentials/{credential}/status"),
                body(json!({"status":status})),
                Some(&owner),
                None
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    for path in [
        format!("{root}/credentials/{credential}/remove"),
        format!("{root}/aliases/http-alias/remove"),
    ] {
        let request = body(json!({}));
        let (status, result) = call(&app, "POST", &path, request.clone(), Some(&owner), None).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(
            result,
            call(&app, "POST", &path, request, Some(&owner), None)
                .await
                .1
        );
    }
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/billing-credential"),
            body(json!({"apiKey":null})),
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
            "POST",
            &format!("{root}/cache"),
            body(json!({"policy":null})),
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
            "PUT",
            "/api/v1/business/model-fallbacks/http-model-a",
            body(json!({"fallbacks":[]})),
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
            "/api/v1/business/channels/unknown/prices",
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let mut invalid = body(json!({"apiKey":"valid-http-secret"}));
    invalid["unexpectedSecret"] = json!("no");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/credentials"),
            invalid,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for (suffix, config) in [
        ("billing-credential", json!({})),
        ("cache", json!({})),
        (
            "cache",
            json!({"policy":{"enabled":true,"apiKey":"hidden-secret"}}),
        ),
        (
            "prices",
            json!({"model":"missing-fields","price":{"inputPricePerMillion":10},"weight":100}),
        ),
    ] {
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("{root}/{suffix}"),
                body(config),
                Some(&owner),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    db.logout(&owner).await.unwrap();
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business/channels",
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

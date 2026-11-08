//! Customer checkout uses explicit owned routes and retained versions over HTTP.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("cookie", format!("oxsum_session={token}"));
    }
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn owner_created_route_scopes_choices_and_checkout_replays_pinned_version() {
    let Some(url) = oxsum_testkit::suite_db_url("checkout_routes_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    db.bootstrap_collection_terms(&oxsum_core::CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "customer fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let session = db
        .login(&customer.user.email, "customer fixture password")
        .await
        .unwrap()
        .into_session()
        .token;
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let stub=Router::new().route("/payments",axum::routing::post(move |axum::Json(body):axum::Json<Value>|{let calls=Arc::clone(&counted);async move{
        calls.fetch_add(1,Ordering::SeqCst);
        axum::Json(json!({"payment_id":body["payment_id"],"client_secret":"fixture_client_secret","status":"requires_payment_method","merchant_id":"fixture-account","amount":body["amount"],"currency":body["currency"],"metadata":body["metadata"]}))
    }})).route("/payments/{id}", axum::routing::get(|| async { (StatusCode::NOT_FOUND, axum::Json(json!({"error":{"code":"HE_02"}}))) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None)
            .with_secret(SecretKey::from_bytes([49; 32]))
            .with_public_url("https://shop.example.com"),
    );
    let root = "/api/v1/business/checkout-routes";
    assert_eq!(
        call(&app, "GET", root, Value::Null, Some(&session), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/checkout-options",
            Value::Null,
            None,
            Some(&customer.api_key.secret)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/checkout-options",
            Value::Null,
            Some(&session),
            None
        )
        .await
        .1["data"],
        json!([])
    );
    let config = json!({"apiUrl":format!("http://{addr}"),"apiKey":"snd_fixture_api_key","webhookSecret":"fixture-webhook-secret","publishableKey":"pk_snd_original","sdkUrl":"https://sdk.example.com/old.js","destinationPolicy":{"network":"internal","allowedIps":["127.0.0.1"]}});
    let create = json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"create customer checkout","name":"http-primary","rail":"hyperswitch","mode":"test","accountId":"fixture-account","config":config});
    let (status, result) = call(
        &app,
        "POST",
        "/api/v1/business/merchants",
        create,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let merchant = result["data"]["merchantId"].as_str().unwrap();
    let version = result["data"]["versionId"].as_str().unwrap();
    let selection = json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"select merchant","rail":"hyperswitch","expectedRevision":0,"merchantId":merchant});
    assert_eq!(
        call(&app, "POST", root, selection.clone(), Some(&owner), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&app, "POST", root, selection.clone(), Some(&session), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let change = |enabled| json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"merchant state","enabled":enabled});
    let status_path = format!("/api/v1/business/merchants/{merchant}/status");
    assert_eq!(
        call(&app, "POST", &status_path, change(true), Some(&owner), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, selected) = call(&app, "POST", root, selection.clone(), Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(
        call(&app, "POST", root, selection, Some(&owner), None)
            .await
            .1,
        selected
    );
    let (status, options) = call(
        &app,
        "GET",
        "/api/v1/checkout-options",
        Value::Null,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["data"][0]["merchantId"], merchant);
    assert_eq!(options["data"][0]["mode"], "test");
    for hidden in [
        "apiUrl",
        "apiKey",
        "webhookSecret",
        "accountId",
        "publishableKey",
        "credentials",
    ] {
        assert!(!options.to_string().contains(hidden));
    }
    let order = json!({"idempotencyKey":Uuid::new_v4().to_string(),"rail":"hyperswitch","purpose":"walletTopup","displayAmountMinor":200,"displayCurrency":"USD","expectedCheckout":{"revision":1,"merchantId":merchant,"versionId":version}});
    let (status, first) = call(
        &app,
        "POST",
        "/api/v1/orders",
        order.clone(),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut rotated = config;
    rotated["publishableKey"] = json!("pk_snd_rotated");
    rotated["sdkUrl"] = json!("https://sdk.example.com/new.js");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/v1/business/merchants/{merchant}/versions"),
            json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"rotate","config":rotated}),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut stale = order.clone();
    stale["idempotencyKey"] = json!(Uuid::new_v4().to_string());
    assert_eq!(
        call(&app, "POST", "/api/v1/orders", stale, Some(&session), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &status_path,
            change(false),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, replay) = call(
        &app,
        "POST",
        "/api/v1/orders",
        order.clone(),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, first);
    assert_eq!(replay["data"]["sdkUrl"], "https://sdk.example.com/old.js");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut other = order;
    other["rail"] = json!("stripe");
    assert_eq!(
        call(&app, "POST", "/api/v1/orders", other, Some(&session), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let disabled = json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"stop new checkout","rail":"hyperswitch","expectedRevision":1,"merchantId":null});
    assert_eq!(
        call(&app, "POST", root, disabled, Some(&owner), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/checkout-options",
            Value::Null,
            Some(&session),
            None
        )
        .await
        .1["data"],
        json!([])
    );
    task.abort();
}

//! Application namespace HTTP contract and current authority.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::Db;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
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
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn owner_namespace_and_machine_customer_routes_remain_separate() {
    let Some(url) = oxsum_testkit::suite_db_url("application_accounts_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let req = json!({"idempotencyKey":"application","key":"example-app","name":"Example","customerLimit":2,"reason":"fixture"});
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/business/applications",
        req.clone(),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, made) = call(
        &app,
        "POST",
        "/api/v1/business/applications",
        req.clone(),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let id = made["data"]["applicationId"].clone();
    let (status, replay) = call(
        &app,
        "POST",
        "/api/v1/business/applications",
        req.clone(),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, made);
    let mut changed = req;
    changed["customerLimit"] = json!(3);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/applications",
            changed,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, page) = call(
        &app,
        "GET",
        "/api/v1/business/applications?limit=1",
        Value::Null,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["data"]["applications"][0]["applicationId"], id);
    let mint = json!({"idempotencyKey":"grant","applicationId":id,"name":"runtime","scopes":["customersProvision","customersRead","billingRead","customersLifecycleWrite"],"customerIds":[],"expiresAt":(time::OffsetDateTime::now_utc()+time::Duration::days(1)).format(&time::format_description::well_known::Rfc3339).unwrap(),"reason":"fixture grant"});
    let (status, credential) = call(
        &app,
        "POST",
        "/api/v1/business/integration-credentials",
        mint,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{credential}");
    let secret = credential["data"]["secret"].as_str().unwrap();
    let provision =
        json!({"idempotencyKey":"customer","externalId":"user-1","name":"Service user"});
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/integration/customers",
            provision.clone(),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, customer) = call(
        &app,
        "POST",
        "/api/v1/integration/customers",
        provision.clone(),
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{customer}");
    let org = customer["data"]["customer"]["organizationId"]
        .as_str()
        .unwrap();
    let (status, read) = call(
        &app,
        "GET",
        "/api/v1/integration/customers?externalId=user-1",
        Value::Null,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read, customer);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/business/customers/{org}/billing"),
        Value::Null,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let archived = call(
        &app,
        "POST",
        &format!("/api/v1/integration/customers/{org}/archive"),
        json!({"idempotencyKey":"archive","reason":"user closed account"}),
        None,
        Some(secret),
    )
    .await;
    assert_eq!(archived.0, StatusCode::OK, "{}", archived.1);
    assert_eq!(archived.1["data"]["customer"]["state"], "archived");
    // Provision retries retain their historical receipt instead of reopening.
    let replay = call(
        &app,
        "POST",
        "/api/v1/integration/customers",
        provision,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(replay.1, customer);
    let current = call(
        &app,
        "GET",
        "/api/v1/integration/customers?externalId=user-1",
        Value::Null,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(current.1["data"]["customer"]["state"], "archived");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/v1/integration/customers/{}/archive", Uuid::new_v4()),
            json!({"idempotencyKey":"foreign","reason":"denied"}),
            None,
            Some(secret)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

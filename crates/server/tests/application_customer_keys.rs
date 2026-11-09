//! Application key lifecycle HTTP identity, replay and scope boundaries.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;
mod support;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey, Tenants};
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
async fn application_key_routes_enforce_machine_namespace_and_one_time_secrets() {
    let Some(url) = oxsum_testkit::suite_db_url("application_customer_keys_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let encryption = SecretKey::from_bytes([71; 32]);
    let model = support::flat_model(&db, &encryption, 40).await;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_secret(encryption),
    );
    let (_,application)=call(&app,"POST","/api/v1/business/applications",json!({"idempotencyKey":"keys-app","key":"keys-app","name":"Keys App","customerLimit":2,"reason":"application keys"}),Some(&owner),None).await;
    let app_id = application["data"]["applicationId"].clone();
    let (_,credential)=call(&app,"POST","/api/v1/business/integration-credentials",json!({"idempotencyKey":"keys-grant","applicationId":app_id,"name":"runtime","scopes":["customersProvision","customerKeysWrite"],"customerIds":[],"expiresAt":(time::OffsetDateTime::now_utc()+time::Duration::days(1)).format(&time::format_description::well_known::Rfc3339).unwrap(),"reason":"runtime"}),Some(&owner),None).await;
    let secret = credential["data"]["secret"].as_str().unwrap();
    let (_, customer) = call(
        &app,
        "POST",
        "/api/v1/integration/customers",
        json!({"idempotencyKey":"keys-customer","externalId":"one","name":"One"}),
        None,
        Some(secret),
    )
    .await;
    let id = customer["data"]["customer"]["organizationId"]
        .as_str()
        .unwrap();
    let path = format!("/api/v1/integration/customers/{id}/keys");
    let body = json!({"idempotencyKey":"runtime-key","name":"App key","reason":"inference","expiresAt":(time::OffsetDateTime::now_utc()+time::Duration::hours(1)).format(&time::format_description::well_known::Rfc3339).unwrap(),"spendLimitMinor":40,"modelAllowlist":[model],"requestsPerMinute":5,"maxConcurrentHolds":1});
    assert_eq!(
        call(&app, "POST", &path, body.clone(), Some(&owner), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, created) = call(&app, "POST", &path, body.clone(), None, Some(secret)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let key = created["data"]["key"]["id"].as_str().unwrap();
    let consumption = created["data"]["secret"].as_str().unwrap();
    assert!(created["data"]["key"]["createdBy"].is_null());
    assert_eq!(
        call(&app, "GET", &path, Value::Null, None, Some(consumption))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, replay) = call(&app, "POST", &path, body.clone(), None, Some(secret)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(replay["data"]["secret"].is_null());
    assert_eq!(replay["data"]["key"], created["data"]["key"]);
    let (status, list) = call(
        &app,
        "GET",
        &format!("{path}?limit=1"),
        Value::Null,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"]["keys"][0]["id"], key);
    assert!(list["data"]["keys"][0].get("secret").is_none());
    let mut changed = body.clone();
    changed["spendLimitMinor"] = json!(2000);
    assert_eq!(
        call(&app, "POST", &path, changed, None, Some(secret))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut unconstrained = body;
    unconstrained["idempotencyKey"] = json!("unbounded");
    unconstrained["spendLimitMinor"] = Value::Null;
    assert_eq!(
        call(&app, "POST", &path, unconstrained, None, Some(secret))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let foreign = format!("/api/v1/integration/customers/{}/keys", Uuid::new_v4());
    assert_eq!(
        call(&app, "GET", &foreign, Value::Null, None, Some(secret))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, revoked) = call(
        &app,
        "POST",
        &format!("{path}/{key}/revoke"),
        json!({"idempotencyKey":"revoke-key","reason":"replace"}),
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    assert!(revoked["data"]["revokedAt"].is_string());
    assert!(db.authenticate(consumption).await.unwrap().is_none());
    // A key issued through machine authority drives the actual gateway and
    // produces its own receipt; the management secret itself cannot consume.
    let runtime = json!({"idempotencyKey":"runtime-proof","name":"Proof key","reason":"integration acceptance",
        "expiresAt":(time::OffsetDateTime::now_utc()+time::Duration::hours(1)).format(&time::format_description::well_known::Rfc3339).unwrap(),
        "spendLimitMinor":40,"modelAllowlist":[model],"requestsPerMinute":5,"maxConcurrentHolds":1});
    let (status, created) = call(&app, "POST", &path, runtime.clone(), None, Some(secret)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let consumer = created["data"]["secret"].as_str().unwrap();
    let (organization, _) = db.authenticate(consumer).await.unwrap().unwrap();
    let wallet = Tenants::new(db.pool().clone())
        .get(&organization.tenant_id)
        .await
        .unwrap();
    wallet
        .top_up(
            "isolated-application-key-fixture",
            100,
            time::OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let chat = support::chat_body(&model);
    assert_ne!(
        call(
            &app,
            "POST",
            "/v1/chat/completions",
            chat.clone(),
            None,
            Some(secret)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, answer) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        chat.clone(),
        None,
        Some(consumer),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if wallet.reserved().await.unwrap() == 0 && wallet.available().await.unwrap() == 60 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let entry: Uuid = sqlx::query_scalar(
        "SELECT entry_id FROM oxsum.usage_records WHERE tenant_id=$1 AND charged_minor=40",
    )
    .bind(&organization.tenant_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let proof = wallet
        .receipt_proof(oxsum_core::EntryId::from_uuid(entry), None)
        .await
        .unwrap()
        .unwrap();
    let recorded: Vec<u8> = sqlx::query_scalar(&format!(
        "SELECT content_hash FROM ledger_{}.entries WHERE entry_id=$1",
        organization.tenant_id
    ))
    .bind(entry)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let recorded = oxsum_core::Hash::from_bytes(recorded.try_into().unwrap());
    assert!(oxsum_core::verify_bundle(&serde_json::to_string(&proof).unwrap(), &recorded).unwrap());
    assert_ne!(
        call(
            &app,
            "POST",
            "/v1/chat/completions",
            chat,
            None,
            Some(consumer)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(wallet.available().await.unwrap(), 60);
    let credential_id =
        Uuid::parse_str(credential["data"]["credentialId"].as_str().unwrap()).unwrap();
    db.revoke_integration_credential(
        &owner,
        credential_id,
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: "revoke-management".into(),
            reason: "done".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(&app, "GET", &path, Value::Null, None, Some(secret))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

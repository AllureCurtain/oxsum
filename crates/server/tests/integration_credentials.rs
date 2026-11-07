//! T05/A01/A06 production-router integration boundaries and explicit credentials.
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
const PASSWORD: &str = "integration http fixture password";
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
async fn user(db: &Db) -> Registration {
    db.register(NewUser {
        email: format!("{}@example.com", Uuid::new_v4()),
        password: PASSWORD.into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn owner_issuance_scoped_customer_reads_and_personal_boundaries() {
    let Some(url) = oxsum_testkit::suite_db_url("integration_http") else {
        return;
    };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let a = user(&db).await;
    let b = user(&db).await;
    let personal = db
        .login(&a.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_admin_token("integration-deployment-fixture"),
    );
    let req = serde_json::to_value(setup::request(
        vec![a.organization.id],
        vec![IntegrationScope::CustomersRead],
    ))
    .unwrap();
    for (cookie, bearer) in [
        (Some(personal.as_str()), None),
        (Some(owner.as_str()), Some(a.api_key.secret.as_str())),
        (Some(owner.as_str()), Some("")),
        (None, Some("integration-deployment-fixture")),
    ] {
        let (status, _) = call(
            &app,
            "POST",
            "/api/v1/business/integration-credentials",
            req.clone(),
            cookie,
            bearer,
        )
        .await;
        assert!(status.is_client_error());
    }
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/business/integration-credentials",
        req.clone(),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let secret = created["data"]["secret"].as_str().unwrap();
    let id = created["data"]["credentialId"].as_str().unwrap();
    let (status, replay) = call(
        &app,
        "POST",
        "/api/v1/business/integration-credentials",
        req,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(replay["data"]["secret"].is_null());
    assert_eq!(replay["data"]["credentialId"], id);
    let (status, page) = call(
        &app,
        "GET",
        "/api/v1/business/integration-credentials",
        Value::Null,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!page.to_string().contains(secret));
    let (status, customers) = call(
        &app,
        "GET",
        "/api/v1/business/customers?limit=1",
        Value::Null,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{customers}");
    assert_eq!(customers["data"]["customers"].as_array().unwrap().len(), 1);
    assert_eq!(
        customers["data"]["customers"][0]["organizationId"],
        a.organization.id.to_string()
    );
    assert!(customers["data"]["nextCursor"].is_null());
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/business/customers/{}", a.organization.id),
        Value::Null,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/business/customers/{}", b.organization.id),
        Value::Null,
        Some(&owner),
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for path in [
        "/api/v1/business/staff",
        "/api/v1/business/integration-credentials",
        "/api/v1/balance",
        "/api/v1/org/keys",
        "/api/v1/admin/organizations",
        "/v1/models",
    ] {
        let (status, _) = call(&app, "GET", path, Value::Null, Some(&owner), Some(secret)).await;
        assert!(status.is_client_error(), "{path}: {status}");
    }
    let meter = json!({"idempotencyKey":"denied","organizationId":a.organization.id,"channel":"not-read","billableCode":"event","usage":{"inputTokens":1,"eventType":"event"}});
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/metering/holds",
        meter,
        None,
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/api/v1/business/customers/{}", a.organization.id),
        json!({"idempotencyKey":"denied","name":"changed","reason":"test"}),
        Some(&owner),
        Some(secret),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/business/integration-credentials/{id}/revoke"),
        json!({"idempotencyKey":"revoke","reason":"test"}),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business/customers",
            Value::Null,
            Some(&owner),
            Some(secret)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    // A malformed explicit header and a legacy key never inherit owner cookies.
    for secret in ["", "oxs-svc-legacy"] {
        assert_eq!(
            call(
                &app,
                "GET",
                "/api/v1/business/customers",
                Value::Null,
                Some(&owner),
                Some(secret)
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn documented_python_client_reads_meters_replays_and_refuses_foreign_customer() {
    let Some(url) = oxsum_testkit::suite_db_url("integration_client") else {
        return;
    };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    setup::owner(&db).await;
    let a = user(&db).await;
    let b = user(&db).await;
    let tenant = Tenants::new(db.pool().clone());
    let wallet = tenant.get(&a.organization.tenant_id).await.unwrap();
    wallet
        .top_up("fund", 1000, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    db.set_channel(
        "client-events",
        "http://127.0.0.1:9/unused",
        "no-provider",
        "openai",
        &SecretKey::from_bytes([8; 32]),
    )
    .await
    .unwrap();
    let price = Price {
        input_price_per_million: 1_000_000,
        output_price_per_million: 0,
        max_output_tokens: 0,
        mode: BillingMode::Event,
        cache_read_price_per_million: None,
        cache_write_5m_price_per_million: None,
        cache_write_1h_price_per_million: None,
        reasoning_price_per_million: None,
        cost_per_request: None,
        upstream: None,
        rules: vec![],
    };
    db.append_price("client-events", "example-event", price, 100)
        .await
        .unwrap();
    let minted = setup::mint(
        &db,
        vec![a.organization.id],
        vec![
            IntegrationScope::CustomersRead,
            IntegrationScope::MeteringWrite,
        ],
    )
    .await;
    let secret = minted.secret.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/scoped-integration/client.py"
    );
    let run = |args: &[&str]| {
        std::process::Command::new("python3")
            .arg(script)
            .args(["--base-url", &address])
            .args(args)
            .env("OXSUM_INTEGRATION_SECRET", &secret)
            .output()
            .unwrap()
    };
    let customer = a.organization.id.to_string();
    let foreign = b.organization.id.to_string();
    let read = run(&["customer", &customer]);
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    let args = [
        "meter",
        &customer,
        "--channel",
        "client-events",
        "--code",
        "example-event",
        "--key",
        "client-event-001",
        "--units",
        "25",
        "--event-id",
        "customer-label",
        "--confirm-metering",
    ];
    let first = run(&args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let replay = run(&args);
    assert!(replay.status.success());
    assert_eq!(first.stdout, replay.stdout);
    assert_eq!(wallet.settled().await.unwrap(), 975);
    let denied = run(&["customer", &foreign]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("404"));
    for output in [&read, &first, &replay, &denied] {
        assert!(!String::from_utf8_lossy(&output.stdout).contains(&secret));
        assert!(!String::from_utf8_lossy(&output.stderr).contains(&secret));
    }
    server.abort();
}

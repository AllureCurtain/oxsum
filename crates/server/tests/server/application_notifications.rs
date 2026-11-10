//! Owner/runtime HTTP boundaries and actual signed loopback delivery retries.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::{Json, Router};
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey};
use oxsum_integrations::provider_http::ProviderHttp;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
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
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(s) = cookie {
        r = r.header("cookie", format!("oxsum_session={s}"));
    }
    if let Some(s) = bearer {
        r = r.header("authorization", format!("Bearer {s}"));
    }
    let res = app
        .clone()
        .oneshot(r.body(Body::from(body.to_string())).unwrap())
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
async fn scoped_endpoints_deliver_exact_signed_bytes_and_retry_without_reexposing_secrets() {
    let Some(url) = oxsum_testkit::suite_db_url("application_notifications_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let namespace = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: "app".into(),
                key: "notifications".into(),
                name: "Notification app".into(),
                customer_limit: 5,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let mut grant = setup::request(
        vec![],
        vec![
            oxsum_core::IntegrationScope::NotificationsRead,
            oxsum_core::IntegrationScope::CustomersProvision,
        ],
    );
    grant.application_id = Some(namespace);
    let token = db
        .create_integration_credential(&owner, &grant)
        .await
        .unwrap()
        .secret
        .unwrap();
    let service = db.authenticate_service(&token).await.unwrap().unwrap();
    let seen = Arc::new(Mutex::new(Vec::<(HeaderMap, String)>::new()));
    let capture = Arc::clone(&seen);
    let stub = Router::new().route(
        "/events",
        axum::routing::post(move |headers: HeaderMap, body: String| {
            let capture = Arc::clone(&capture);
            async move {
                let mut seen = capture.lock().unwrap();
                seen.push((headers, body));
                if seen.len() == 1 {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error":"private receiver diagnostic"})),
                    )
                } else {
                    (StatusCode::NO_CONTENT, Json(Value::Null))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let key = SecretKey::from_bytes([93; 32]);
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_secret(key.clone()),
    );
    let root = format!("/api/v1/business/applications/{namespace}/notification-endpoints");
    let request = json!({"idempotencyKey":"endpoint","reason":"owned loopback fixture","url":format!("http://{addr}/events"),"destinationPolicy":{"network":"internal","allowedIps":["127.0.0.1"]},"events":["customer.changed"]});
    assert_eq!(
        call(&app, "POST", &root, request.clone(), None, Some(&token))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, created) = call(&app, "POST", &root, request.clone(), Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let endpoint = created["data"]["endpoint"]["endpointId"].as_str().unwrap();
    let signing = created["data"]["secret"].as_str().unwrap();
    assert!(
        call(&app, "POST", &root, request, Some(&owner), None)
            .await
            .1["data"]["secret"]
            .is_null()
    );
    let (status, list) = call(&app, "GET", &root, Value::Null, Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!list.to_string().contains(signing));
    let (status, list) = call(
        &app,
        "GET",
        "/api/v1/integration/notification-endpoints",
        Value::Null,
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"]["endpoints"].as_array().unwrap().len(), 1);
    let org = db
        .provision_application_customer(
            &service,
            &oxsum_core::ProvisionCustomerRequest {
                idempotency_key: "customer".into(),
                external_id: "customer".into(),
                name: "Fixture".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    oxsum_server::application_notifications::deliver_due(
        &db,
        &ProviderHttp::default(),
        &key,
        Some(namespace),
    )
    .await;
    let path = format!("/api/v1/integration/notification-endpoints/{endpoint}/deliveries");
    let (status, page) = call(&app, "GET", &path, Value::Null, None, Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["data"]["deliveries"][0]["status"], "pending");
    assert_eq!(
        page["data"]["deliveries"][0]["lastError"],
        "receiver rejected"
    );
    let id = page["data"]["deliveries"][0]["deliveryId"]
        .as_str()
        .unwrap();
    let id = Uuid::parse_str(id).unwrap();
    sqlx::query("UPDATE oxsum.application_notification_deliveries SET next_attempt_at=clock_timestamp() WHERE delivery_id=$1").bind(id).execute(db.pool()).await.unwrap();
    oxsum_server::application_notifications::deliver_due(
        &db,
        &ProviderHttp::default(),
        &key,
        Some(namespace),
    )
    .await;
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].1, seen[1].1);
        for (headers, body) in seen.iter() {
            let header = headers["x-oxsum-signature"].to_str().unwrap();
            let timestamp = header
                .split(',')
                .next()
                .unwrap()
                .strip_prefix("t=")
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(
                header,
                oxsum_core::signature(signing, timestamp, body.as_bytes())
            );
            assert_ne!(
                header,
                oxsum_core::signature(signing, timestamp, b"tampered")
            );
            let value: Value = serde_json::from_str(body).unwrap();
            assert_eq!(value["organizationId"], org.to_string());
            assert_eq!(value["id"], id.to_string());
            assert_eq!(value["action"], "readCurrentState");
            assert_eq!(headers["x-oxsum-delivery"], id.to_string());
        }
    }
    // The independent standard-library receiver checks these actual Rust bytes.
    let verification = {
        let captured = seen.lock().unwrap();
        json!({"application":namespace,"secret":signing,"header":captured[0].0["x-oxsum-signature"].to_str().unwrap(),"delivery":id,"event":"customer.changed","body":captured[0].1})
    };
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/application-notifications");
    let mut child = std::process::Command::new("python3")
        .arg("-c").arg("import sys,json,tempfile;sys.path.insert(0,sys.argv[1]);from receiver import Receiver;v=json.load(sys.stdin);f=tempfile.TemporaryDirectory();r=Receiver(f.name+'/inbox.db',v['application'],v['secret'],'http://127.0.0.1:3000','oxs-int-fixture');b=v['body'].encode();r.accept(v['header'],v['delivery'],v['event'],b);r.accept(v['header'],v['delivery'],v['event'],b);assert r.db.execute('SELECT count(*) FROM inbox').fetchone()[0]==1;r.db.close();f.cleanup()")
        .arg(script).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    std::io::Write::write_all(
        child.stdin.as_mut().unwrap(),
        verification.to_string().as_bytes(),
    )
    .unwrap();
    drop(child.stdin.take());
    let checked = child.wait_with_output().unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let owner_path = format!("{root}/{endpoint}/deliveries");
    assert_eq!(
        call(&app, "GET", &owner_path, Value::Null, Some(&owner), None)
            .await
            .1["data"]["deliveries"][0]["status"],
        "delivered"
    );
    let retry_path = format!("{root}/{endpoint}/deliveries/{id}/retry");
    let retry = json!({"idempotencyKey":"retry","reason":"repair receiver"});
    assert_eq!(
        call(&app, "POST", &retry_path, retry.clone(), Some(&owner), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    sqlx::query(
        "UPDATE oxsum.application_notification_deliveries SET status='failed' WHERE delivery_id=$1",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(
        call(&app, "POST", &retry_path, retry.clone(), Some(&owner), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "POST", &retry_path, retry, Some(&owner), None)
            .await
            .0,
        StatusCode::OK
    );
    let disable = json!({"idempotencyKey":"disable","reason":"stop receiver"});
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/{endpoint}/disable"),
            disable,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    oxsum_server::application_notifications::deliver_due(
        &db,
        &ProviderHttp::default(),
        &key,
        Some(namespace),
    )
    .await;
    assert_eq!(seen.lock().unwrap().len(), 2);
    db.revoke_integration_credential(
        &owner,
        service.credential_id(),
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: "revoke".into(),
            reason: "fixture".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(&app, "GET", &path, Value::Null, None, Some(&token))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}

struct PrivateReceiverDns;
impl oxsum_integrations::provider_http::ProviderResolver for PrivateReceiverDns {
    fn resolve(&self, _host: &str, _port: u16) -> oxsum_integrations::provider_http::Resolution {
        Box::pin(async { Ok(vec![std::net::Ipv4Addr::LOCALHOST.into()]) })
    }
}

#[tokio::test]
async fn application_worker_refuses_public_receiver_rebinding_before_sending() {
    let Some(url) = oxsum_testkit::suite_db_url("application_notification_destination") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: "app".into(),
                key: "blocked-receiver".into(),
                name: "Blocked receiver".into(),
                customer_limit: 5,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let key = SecretKey::from_bytes([94; 32]);
    let endpoint = db
        .create_application_notification_endpoint(
            &owner,
            app,
            &oxsum_core::ApplicationNotificationEndpointRequest {
                idempotency_key: "endpoint".into(),
                reason: "public receiver fixture".into(),
                url: "https://receiver.example/events".into(),
                destination_policy: Default::default(),
                events: vec!["customer.changed".into()],
            },
            &key,
        )
        .await
        .unwrap()
        .endpoint;
    let mut grant = setup::request(
        vec![],
        vec![oxsum_core::IntegrationScope::CustomersProvision],
    );
    grant.application_id = Some(app);
    let credential = db
        .create_integration_credential(&owner, &grant)
        .await
        .unwrap();
    let actor = db
        .authenticate_service(credential.secret.as_deref().unwrap())
        .await
        .unwrap()
        .unwrap();
    db.provision_application_customer(
        &actor,
        &oxsum_core::ProvisionCustomerRequest {
            idempotency_key: "customer".into(),
            external_id: "customer".into(),
            name: "Customer".into(),
            contact_email: None,
        },
    )
    .await
    .unwrap();
    oxsum_server::application_notifications::deliver_due(
        &db,
        &ProviderHttp::with_resolver(Arc::new(PrivateReceiverDns)),
        &key,
        Some(app),
    )
    .await;
    let rows = db
        .business_application_notification_deliveries(&owner, app, endpoint.endpoint_id, None, 50)
        .await
        .unwrap();
    assert_eq!(rows.deliveries.len(), 1);
    let row = &rows.deliveries[0];
    assert_eq!(row.status, "pending");
    assert_eq!(row.attempts, 1);
    assert_eq!(row.last_error.as_deref(), Some("destination blocked"));
    assert!(row.response_status.is_none());
}

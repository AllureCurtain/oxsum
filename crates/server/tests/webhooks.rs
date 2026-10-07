//! Webhook surface tests (issue #144): the REST CRUD under `require_principal`,
//! and the worker's signed delivery against a stub receiver it really posts to.
//! The tests need DATABASE_URL and skip without it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use http_body_util::BodyExt;
use oxsum_core::{Db, SecretKey, SettlementKind, UsageRecord, UsageRow};
use oxsum_server::{Config, Metrics, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const ADMIN_TOKEN: &str = "operator-token-1234";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

fn sealing_key() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

/// An app wired for webhooks: a real database, the sealing key signing needs,
/// an operator token so `/metrics` can be scraped, and the app's own registry
/// so the worker's counts land where the scrape reads.
async fn world() -> Option<(Router, Db, Metrics)> {
    let u = url()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None)
        .with_secret(sealing_key())
        .with_admin_token(ADMIN_TOKEN);
    let (app, _events, metrics) = oxsum_server::app_with_billing(db.clone(), config);
    Some((app, db, metrics))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// The metrics scrape, answered raw (it is not JSON).
async fn scrape(app: &Router) -> String {
    let req = Request::builder()
        .method("GET")
        .uri("/metrics")
        .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Registers an organization and answers its API key secret.
async fn register(app: &Router) -> String {
    let email = format!(
        "wh_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    body["data"]["apiKey"]["secret"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A stub receiver on localhost: captures each POST's headers and body, answers
/// `answer`'s status — shared, so a test can turn it hostile mid-flight.
#[derive(Clone, Default)]
struct Receiver {
    calls: Arc<Mutex<Vec<(HeaderMap, String)>>>,
    answer: Arc<AtomicU16>,
}

async fn stub() -> (String, Receiver) {
    let receiver = Receiver {
        answer: Arc::new(AtomicU16::new(200)),
        ..Receiver::default()
    };
    let app = Router::new()
        .route(
            "/hook",
            post(
                |State(receiver): State<Receiver>, headers: HeaderMap, body: String| async move {
                    receiver.calls.lock().unwrap().push((headers, body));
                    StatusCode::from_u16(receiver.answer.load(Ordering::Relaxed)).unwrap()
                },
            ),
        )
        .with_state(receiver.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/hook"), receiver)
}

/// The key's organization id — also the tenant id `usage_records` carries in
/// its 32-hex form.
async fn org_of(db: &Db, key_secret: &str) -> Uuid {
    db.authenticate(key_secret)
        .await
        .unwrap()
        .expect("the key authenticates")
        .0
        .id
}

/// A minimal settled-turn row, as the gateway writes it.
fn usage_row(request_id: &str, tenant: &str) -> UsageRow {
    UsageRow {
        request_id: request_id.to_owned(),
        tenant_id: tenant.to_owned(),
        key_id: None,
        model: "m".to_owned(),
        channel: "c".to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: Uuid::new_v4(),
        usage: UsageRecord::tokens(10, 2).unwrap(),
        charged_minor: 12,
        freeze_minor: 100,
        upstream_cost_minor: None,
    }
}

/// The create/list/delete surface, scoped to the caller's organization.
#[tokio::test]
async fn the_endpoint_crud_is_scoped() {
    let Some((app, _db, _metrics)) = world().await else {
        return;
    };
    let mine = register(&app).await;
    let theirs = register(&app).await;

    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/webhooks",
        Some(json!({"url": "https://receiver.example/hook", "events": ["request.settled"]})),
        Some(&mine),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let endpoint_id = body["data"]["id"].as_str().unwrap();
    assert!(
        body["data"]["secret"]
            .as_str()
            .unwrap()
            .starts_with("whsec-")
    );
    assert!(body["data"]["secretLast4"].is_string());

    // Listings never carry the secret; another organization sees nothing.
    let (status, body) = call(&app, "GET", "/api/v1/webhooks", None, Some(&mine)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert!(body["data"][0].get("secret").is_none());
    let (status, body) = call(&app, "GET", "/api/v1/webhooks", None, Some(&theirs)).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 0);
    assert_eq!(status, StatusCode::OK);

    // Another organization's endpoint id is not found, not forbidden.
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/v1/webhooks/{endpoint_id}"),
        None,
        Some(&theirs),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/webhooks/{endpoint_id}/deliveries"),
        None,
        Some(&theirs),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/api/v1/webhooks/{endpoint_id}"),
        None,
        Some(&mine),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Malformed requests are refused in the API envelope.
#[tokio::test]
async fn malformed_registrations_are_refused() {
    let Some((app, _db, _metrics)) = world().await else {
        return;
    };
    let key = register(&app).await;
    for body in [
        json!({"url": "not a url", "events": ["request.settled"]}),
        json!({"url": "http://receiver.example/hook", "events": ["request.settled"]}),
        json!({"url": "https://receiver.example/hook", "events": []}),
        json!({"url": "https://receiver.example/hook", "events": ["turn.started"]}),
    ] {
        let (status, body) = call(&app, "POST", "/api/v1/webhooks", Some(body), Some(&key)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    }
}

/// The worker signs and posts a queued delivery; the receiver can verify the
/// signature against the secret it was given at registration.
#[tokio::test]
async fn a_due_delivery_arrives_signed() {
    let Some((app, db, metrics)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let (url, receiver) = stub().await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/webhooks",
        Some(json!({"url": url, "events": ["request.settled"]})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let secret = body["data"]["secret"].as_str().unwrap();
    let endpoint_id = body["data"]["id"].as_str().unwrap().to_owned();

    // A settled turn enqueues the delivery in the same transaction as its usage row.
    let org = org_of(&db, &key).await;
    db.record_usage(&usage_row(
        &Uuid::new_v4().to_string(),
        &org.simple().to_string(),
    ))
    .await
    .unwrap();

    oxsum_server::webhooks::deliver_due(
        &db,
        &reqwest::Client::new(),
        &sealing_key(),
        &metrics,
        Some(org),
    )
    .await;

    {
        let calls = receiver.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "one delivery was posted");
        let (headers, body) = &calls[0];
        assert_eq!(
            headers.get("x-oxsum-event").and_then(|v| v.to_str().ok()),
            Some("request.settled")
        );
        let signature_header = headers
            .get("x-oxsum-signature")
            .and_then(|v| v.to_str().ok())
            .expect("the signature header is present");
        // `t=<unix>,v1=<hmac>` — recompute the receiver's check and compare.
        let (t, _v1) = signature_header.split_once(",v1=").unwrap();
        let timestamp: i64 = t.trim_start_matches("t=").parse().unwrap();
        assert_eq!(
            oxsum_core::signature(secret, timestamp, body.as_bytes()),
            signature_header
        );
        let payload: Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload["type"], "request.settled");
        assert_eq!(payload["data"]["chargedMinor"], 12);
    }

    // The row is delivered, and the listing shows it — polled, since a sibling
    // test's worker may be mid-flight on the claim.
    let mut delivery = Value::Null;
    for _ in 0..40 {
        let (status, body) = call(
            &app,
            "GET",
            &format!("/api/v1/webhooks/{endpoint_id}/deliveries"),
            None,
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        delivery = body["data"][0].clone();
        if delivery["status"] != "sending" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(delivery["status"], "delivered", "{delivery}");
    assert_eq!(delivery["attempts"], 1);
    assert_eq!(delivery["responseStatus"], 200);
    let rendered = scrape(&app).await;
    assert!(
        rendered.contains("oxsum_webhook_deliveries_total"),
        "{rendered}"
    );
}

/// A receiver that refuses reschedules instead of disappearing: the delivery
/// stays pending with its attempt counted, and the metric names the retry.
#[tokio::test]
async fn a_refusing_receiver_reschedules() {
    let Some((app, db, metrics)) = world().await else {
        return;
    };
    let key = register(&app).await;
    let (url, receiver) = stub().await;
    receiver.answer.store(500, Ordering::Relaxed);
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/webhooks",
        Some(json!({"url": url, "events": ["request.settled"]})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let endpoint_id = body["data"]["id"].as_str().unwrap().to_owned();

    let org = org_of(&db, &key).await;
    db.record_usage(&usage_row(
        &Uuid::new_v4().to_string(),
        &org.simple().to_string(),
    ))
    .await
    .unwrap();

    // The app's own registry, not a stray one: the count lands where the scrape reads.
    oxsum_server::webhooks::deliver_due(
        &db,
        &reqwest::Client::new(),
        &sealing_key(),
        &metrics,
        Some(org),
    )
    .await;

    // The claim is scoped to this organization, so the listing answers the
    // recorded attempt without waiting on the lease.
    let mut delivery = Value::Null;
    for _ in 0..40 {
        let (status, body) = call(
            &app,
            "GET",
            &format!("/api/v1/webhooks/{endpoint_id}/deliveries"),
            None,
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        delivery = body["data"][0].clone();
        if delivery["status"] != "sending" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(delivery["status"], "pending", "{delivery}");
    assert_eq!(delivery["attempts"], 1);
    assert_eq!(delivery["responseStatus"], 500);
}

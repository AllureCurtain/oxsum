//! A27 (issue #617): the developer-examples surface holds the documented
//! contract — the refusal catalog answers exactly what docs/unsupported.md
//! names, the versioned webhook intake dedups a concurrent retry storm to one
//! durable row, and a scoped credential cannot reach past its grant. Needs
//! DATABASE_URL; skips without it.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;
#[path = "support/mod.rs"]
mod support;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use hmac::{KeyInit, Mac, SimpleHmac};
use http_body_util::BodyExt;
use oxsum_core::*;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sha2::Sha512;
use tower::ServiceExt;
use uuid::Uuid;

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("examples_surface")
}

async fn post(app: &Router, uri: &str, body: Value, token: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::post(uri).header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
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

async fn get(app: &Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::get(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The documented refusal catalog (`docs/unsupported.md`) answers exactly the
/// codes it names — a caller can test each boundary rather than assume it.
#[tokio::test]
async fn documented_refusals_answer_their_named_codes() {
    let Some(url) = url() else { return };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let owner = setup::owner(&db).await;
    let _ = owner; // business is initialized; customer authority is separate
    let customer_password = "customer password 000";
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: customer_password.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let key = db
        .create_key(
            customer.organization.id,
            None,
            None,
            Some(customer.user.id),
            Default::default(),
        )
        .await
        .unwrap()
        .secret;

    // Client-supplied `amountMinor`/`rate` on order create is a schema
    // rejection — the schema carries only the display terms. Order create is
    // a person's action, so the caller needs a session, not a key.
    let session = support::login(&app, &customer.user.email, customer_password).await;
    let (status, _) = {
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/v1/orders")
                    .header("content-type", "application/json")
                    .header("cookie", format!("oxsum_session={session}"))
                    .body(Body::from(
                        json!({
                            "idempotencyKey": "order-1",
                            "rail": "hyperswitch",
                            "purpose": "walletTopup",
                            "displayAmountMinor": 2500,
                            "displayCurrency": "USD",
                            "amountMinor": 999_999_999,
                            "rate": "0.001"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
        )
    };
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "amountMinor must be a schema rejection"
    );

    // The retired customer manual top-up always refuses.
    let (status, _) = post(
        &app,
        "/api/v1/topups",
        json!({"idempotencyKey": "x", "amountMinor": 100}),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Raw customer holds/settlements are refused: customers freeze through
    // the gateway, never directly.
    let (status, _) = post(
        &app,
        "/api/v1/holds",
        json!({"idempotencyKey":"h","amountMinor":1,"description":"x"}),
        Some(&key),
    )
    .await;
    assert!(
        [
            StatusCode::FORBIDDEN,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::NOT_FOUND
        ]
        .contains(&status),
        "customer hold must refuse, got {status}"
    );

    // A consumption key cannot read the business surface or another
    // organization's records.
    let (status, _) = get(&app, "/api/v1/business/customers", &key).await;
    assert!(
        [StatusCode::FORBIDDEN, StatusCode::UNAUTHORIZED].contains(&status),
        "a consumption key must not reach the business surface, got {status}"
    );

    // An unknown share token is an indistinguishable 410.
    let res = app
        .clone()
        .oneshot(
            Request::get("/api/v1/evidence/not-a-real-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::GONE);
}

/// A scoped integration credential reaches exactly its grant — and nothing
/// past it: the customer surface refuses it, a foreign customer is 404, and
/// minting authority stays with the owner's session.
#[tokio::test]
async fn a_scoped_credential_cannot_reach_past_its_grant() {
    let Some(url) = url() else { return };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let selected = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "selected customer password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let foreign = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "foreign customer password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let minted = setup::mint(
        &db,
        vec![selected.organization.id],
        vec![IntegrationScope::CustomersRead],
    )
    .await;
    let secret = minted.secret.unwrap();

    // The grant reads the selected customer.
    let (status, body) = get(
        &app,
        &format!("/api/v1/business/customers/{}", selected.organization.id),
        &secret,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["organizationId"].as_str().unwrap(),
        selected.organization.id.to_string()
    );

    // A foreign customer is not found — the allowlist cannot be probed.
    let (status, _) = get(
        &app,
        &format!("/api/v1/business/customers/{}", foreign.organization.id),
        &secret,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // CustomersRead does not bill: a metering call without meteringWrite is
    // refused before the wallet is touched.
    let (status, _) = post(
        &app,
        "/api/v1/metering/settlements",
        json!({
            "idempotencyKey": "unscoped-1",
            "organizationId": selected.organization.id,
            "channel": "any",
            "billableCode": "any",
            "usage": {"inputTokens": 1, "eventType": "x"}
        }),
        Some(&secret),
    )
    .await;
    assert!(
        [StatusCode::FORBIDDEN, StatusCode::UNAUTHORIZED].contains(&status),
        "customersRead must not bill, got {status}"
    );

    // The credential cannot read the customer's own records — that view is
    // the customer's, not the integration's.
    for uri in ["/api/v1/operations", "/api/v1/bills", "/api/v1/balance"] {
        let (status, _) = get(&app, uri, &secret).await;
        assert!(
            [StatusCode::FORBIDDEN, StatusCode::UNAUTHORIZED].contains(&status),
            "{uri} must refuse an integration credential, got {status}"
        );
    }

    // Only the owner's session mints credentials — a scoped credential trying
    // to widen itself is refused.
    let (status, _) = post(
        &app,
        "/api/v1/business/integration-credentials",
        json!({
            "idempotencyKey": "self-widen",
            "scopes": ["meteringWrite"],
            "customerIds": [selected.organization.id],
            "expiresAt": (time::OffsetDateTime::now_utc() + time::Duration::days(1))
                .format(&time::format_description::well_known::Rfc3339).unwrap(),
            "reason": "credential tries to widen itself"
        }),
        Some(&secret),
    )
    .await;
    assert!(
        [StatusCode::FORBIDDEN, StatusCode::UNAUTHORIZED].contains(&status),
        "a credential cannot mint credentials, got {status}"
    );
    let _ = owner;
}

/// The versioned merchant intake under a concurrent retry storm: N identical
/// signed deliveries all answer 200 and the inbox holds exactly one row —
/// the dedup contract the webhook-tools example asserts against a live
/// deployment, proven here against the real router.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_webhook_retry_storm_single_credits() {
    let Some(url) = url() else { return };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_secret(SecretKey::from_bytes([77; 32])),
    );
    let webhook_secret = "whsec-storm-test";
    let merchant = db
        .create_merchant(
            &owner,
            &MerchantCreateRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "storm fixture".into(),
                name: format!("storm-{}", Uuid::new_v4().simple()),
                rail: MerchantRail::Hyperswitch,
                mode: MerchantMode::Test,
                account_id: "acct_storm".into(),
                config: serde_json::from_value(json!({
                    "apiUrl":"https://sandbox.hyperswitch.io",
                    "apiKey":"snd_storm",
                    "webhookSecret":webhook_secret,
                    "publishableKey":"pk_snd_storm",
                    "sdkUrl":"https://sdk.example"
                }))
                .unwrap(),
            },
            &SecretKey::from_bytes([77; 32]),
        )
        .await
        .unwrap();
    db.set_merchant_enabled(
        &owner,
        merchant.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "enable".into(),
            enabled: true,
        },
    )
    .await
    .unwrap();

    let event_id = format!("evt_{}", Uuid::new_v4().simple());
    let body = json!({
        "merchant_id": "acct_storm",
        "event_id": event_id,
        "event_type": "payment_succeeded",
        "livemode": false,
        "content": {"type":"payment_details","object":{
            "payment_id": "pay_storm",
            "amount": 5000,
            "currency": "USD",
            "metadata": {"payment_id": Uuid::new_v4(), "organization_id": Uuid::new_v4()}
        }}
    })
    .to_string();
    let mut mac =
        <SimpleHmac<Sha512> as KeyInit>::new_from_slice(webhook_secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let tag: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let app = app.clone();
        let body = body.clone();
        let tag = tag.clone();
        let (merchant_id, version_id) = (merchant.merchant_id, merchant.version_id);
        set.spawn(async move {
            let req = Request::post(format!(
                "/api/v1/webhooks/merchants/{merchant_id}/versions/{version_id}"
            ))
            .header("x-webhook-signature-512", &tag)
            .header("content-type", "application/json");
            let res = app
                .oneshot(req.body(Body::from(body)).unwrap())
                .await
                .unwrap();
            res.status()
        });
    }
    let mut oks = 0;
    while let Some(status) = set.join_next().await {
        if status.unwrap() == StatusCode::OK {
            oks += 1;
        }
    }
    assert_eq!(oks, 16, "every signed delivery must answer 200");

    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.merchant_event_inbox WHERE merchant_id=$1 AND event_id=$2",
    )
    .bind(merchant.merchant_id)
    .bind(&event_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1, "a retry storm must single-record");
}

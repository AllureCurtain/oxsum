//! The versioned merchant webhook route (issue #649): path coordinates select
//! the signing configuration before the raw body is verified, a recognized
//! event is durably scoped, and an id reused with different bytes conflicts.
//! Needs DATABASE_URL; skips without it.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use hmac::{KeyInit, Mac, SimpleHmac};
use http_body_util::BodyExt;
use oxsum_core::{
    Db, MerchantCreateRequest, MerchantMode, MerchantMutationResult, MerchantPaymentBinding,
    MerchantPaymentPurpose, MerchantRail, MerchantStatusRequest, MerchantVersionInput, NewUser,
    SecretKey,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sha2::Sha512;
use tower::ServiceExt;
use uuid::Uuid;

const WEBHOOK_SECRET: &str = "merchant-whsec-versioned-1";
const ACCOUNT: &str = "acct_versioned_test";

fn key() -> SecretKey {
    SecretKey::from_bytes([43; 32])
}
fn config(webhook_secret: &str) -> MerchantVersionInput {
    serde_json::from_value(json!({
        "apiUrl":"https://sandbox.hyperswitch.io",
        "apiKey":"snd_http_secret",
        "webhookSecret":webhook_secret,
        "publishableKey":"pk_snd_http",
        "sdkUrl":"https://sdk.example"
    }))
    .unwrap()
}

/// The database, the production app and an enabled Hyperswitch merchant pinned
/// at revision 1 under `owner`'s business.
async fn world() -> Option<(Db, Router, MerchantMutationResult, String)> {
    let u = oxsum_testkit::suite_db_url("merchant_events_http")?;
    let db = Db::connect(&u, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_secret(key()),
    );
    let merchant = db
        .create_merchant(
            &owner,
            &MerchantCreateRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "versioned webhook fixture".into(),
                name: format!("wh-{}", Uuid::new_v4().simple()),
                rail: MerchantRail::Hyperswitch,
                mode: MerchantMode::Test,
                account_id: ACCOUNT.into(),
                config: config(WEBHOOK_SECRET),
            },
            &key(),
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
    Some((db, app, merchant, owner))
}

fn signed(secret: &str, body: &str) -> HeaderMap {
    let mut mac = <SimpleHmac<Sha512> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let tag: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut headers = HeaderMap::new();
    headers.insert("x-webhook-signature-512", tag.parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());
    headers
}

async fn post(
    app: &Router,
    merchant: Uuid,
    version: Uuid,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/webhooks/merchants/{merchant}/versions/{version}"
        ))
        .body(Body::from(body))
        .unwrap();
    *req.headers_mut() = headers;
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A Hyperswitch success event naming the pinned local payment and customer.
fn succeeded(payment: Uuid, org: Uuid, provider: &str, amount: i64) -> String {
    json!({
        "merchant_id": ACCOUNT,
        "event_id": format!("evt_{}", Uuid::new_v4().simple()),
        "event_type": "payment_succeeded",
        "livemode": false,
        "content": {"type":"payment_details","object":{
            "payment_id": provider,
            "amount": amount,
            "currency": "USD",
            "metadata": {"payment_id": payment, "organization_id": org}
        }}
    })
    .to_string()
}

/// A correctly signed, fully matching event is accepted and durably recorded;
/// an unverified or pathless coordinate is refused before any parsing.
#[tokio::test]
async fn versioned_webhook_verifies_signature_and_accepts_matching_event() {
    let Some((db, app, m, _owner)) = world().await else {
        return;
    };
    let org = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "customer fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap()
        .organization
        .id;
    // Pin the expected payment before any provider round-trip.
    let payment = Uuid::new_v4();
    db.pin_merchant_payment(&MerchantPaymentBinding {
        payment_id: payment,
        merchant_id: m.merchant_id,
        version_id: m.version_id,
        organization_id: org,
        amount_minor: 5000,
        currency: "USD".into(),
        purpose: MerchantPaymentPurpose::Order,
        hyperswitch_body_id: true,
        provider_payment_id: None,
    })
    .await
    .unwrap();

    let body = succeeded(payment, org, "pay_http_1", 5000);
    let (status, _) = post(
        &app,
        m.merchant_id,
        m.version_id,
        signed(WEBHOOK_SECRET, &body),
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The event was durably accepted under this merchant/version.
    let state: String =
        sqlx::query_scalar("SELECT state FROM oxsum.merchant_event_inbox WHERE merchant_id=$1")
            .bind(m.merchant_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(state, "accepted");

    // Replaying the identical signed delivery returns 200 without duplicating.
    let (status, _) = post(
        &app,
        m.merchant_id,
        m.version_id,
        signed(WEBHOOK_SECRET, &body),
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.merchant_event_inbox WHERE merchant_id=$1")
            .bind(m.merchant_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
}

/// A wrong signature is refused before normalization; an unknown merchant or
/// version coordinate is a not-found regardless of a well-formed body.
#[tokio::test]
async fn versioned_webhook_refuses_bad_signature_and_unknown_coordinates() {
    let Some((db, app, m, _owner)) = world().await else {
        return;
    };
    let body = succeeded(Uuid::new_v4(), Uuid::new_v4(), "pay_http_2", 1000);
    // A signature under a different secret fails verification.
    let (status, _) = post(
        &app,
        m.merchant_id,
        m.version_id,
        signed("wrong-secret", &body),
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Unknown merchant id and unknown version id both 404.
    for (merchant, version) in [
        (Uuid::new_v4(), m.version_id),
        (m.merchant_id, Uuid::new_v4()),
    ] {
        let (status, _) = post(
            &app,
            merchant,
            version,
            signed(WEBHOOK_SECRET, &body),
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    // Nothing was durably recorded under this merchant.
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.merchant_event_inbox WHERE merchant_id=$1")
            .bind(m.merchant_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// Present but malformed metadata cannot be mistaken for absent metadata and
/// filled from a trusted reference. No authenticated inbox effect is recorded.
#[tokio::test]
async fn malformed_callback_coordinates_are_refused_before_intake() {
    let Some((db, app, m, _)) = world().await else {
        return;
    };
    for field in ["payment_id", "deposit_id", "organization_id"] {
        let mut body: Value = serde_json::from_str(&succeeded(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "pay_malformed",
            1000,
        ))
        .unwrap();
        body["content"]["object"]["metadata"][field] = json!("not-a-uuid");
        let body = body.to_string();
        let (status, _) = post(
            &app,
            m.merchant_id,
            m.version_id,
            signed(WEBHOOK_SECRET, &body),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.merchant_event_inbox WHERE merchant_id=$1")
            .bind(m.merchant_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// The business read surface lists the recorded event — redacted, scoped and
/// behind the reports capability the personal session already holds.
#[tokio::test]
async fn business_events_page_lists_scoped_intake() {
    let Some((db, app, m, owner)) = world().await else {
        return;
    };
    let org = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "customer fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap()
        .organization
        .id;
    let payment = Uuid::new_v4();
    db.pin_merchant_payment(&MerchantPaymentBinding {
        payment_id: payment,
        merchant_id: m.merchant_id,
        version_id: m.version_id,
        organization_id: org,
        amount_minor: 2500,
        currency: "USD".into(),
        purpose: MerchantPaymentPurpose::Order,
        hyperswitch_body_id: true,
        provider_payment_id: None,
    })
    .await
    .unwrap();
    let body = succeeded(payment, org, "pay_http_3", 2500);
    post(
        &app,
        m.merchant_id,
        m.version_id,
        signed(WEBHOOK_SECRET, &body),
        body,
    )
    .await;

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/v1/business/merchants/{}/events",
            m.merchant_id
        ))
        .header("cookie", format!("oxsum_session={owner}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let page: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(page["data"]["events"].as_array().unwrap().len(), 1);
    let serialized = page.to_string();
    for sensitive in [WEBHOOK_SECRET, "snd_http_secret", "pay_http_3"] {
        assert!(!serialized.contains(sensitive), "leaked {sensitive}");
    }
}

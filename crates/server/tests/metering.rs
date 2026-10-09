//! The metering API end to end (issue #172): the service credential's
//! authentication, the `holds`/`settle`/`release`/`settlements` calls, and the
//! admin surface that mints the credentials.
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! Every test names its channel, code and credential with a random suffix:
//! the database is shared between tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{BillingMode, Db, Price, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The operator token these tests configure, like admin.rs's.
const TOKEN: &str = "operator-token-0123456789";

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    std::env::var("DATABASE_URL").ok()
}

macro_rules! world_or_skip {
    ($top_up:expr) => {
        match url() {
            Some(u) => world(&u, $top_up).await,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// A channel/code name of this test's own, so tests sharing the database never collide.
fn fresh(name: &str) -> String {
    format!("{name}-{}", &Uuid::new_v4().simple().to_string()[..8])
}

/// One JSON call against the app, with whichever credential the test chooses.
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(secret) = bearer {
        request = request.header("authorization", format!("Bearer {secret}"));
    }
    let response = app
        .clone()
        .oneshot(
            request
                .header("content-type", "application/json")
                .body(
                    body.map_or_else(Body::empty, |b| Body::from(serde_json::to_vec(&b).unwrap())),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// An `event`-mode price: input metered, no output side, nothing forwarded.
fn event_price(input_per_million: i64) -> Price {
    Price {
        input_price_per_million: input_per_million,
        output_price_per_million: 0,
        max_output_tokens: 0,
        cache_read_price_per_million: None,
        cache_write_5m_price_per_million: None,
        cache_write_1h_price_per_million: None,
        reasoning_price_per_million: None,
        cost_per_request: None,
        upstream: None,
        mode: BillingMode::Event,
        rules: vec![],
    }
}

/// The declared usage of a `mail.send` event: `units` in the code's
/// denomination — input tokens here.
fn usage(units: i64) -> Value {
    json!({"inputTokens": units, "eventType": "mail.send"})
}

/// The world a metering test needs: an app, a funded organization, a channel
/// priced `mode: event` for a fresh billable code, and a minted service
/// credential's secret.
struct World {
    app: Router,
    /// The organization's API key — for asserting it cannot meter.
    org_key: String,
    organization_id: String,
    /// The minted service credential's plaintext secret.
    service_secret: String,
    credential_id: String,
    channel: String,
    code: String,
}

async fn world(url: &str, top_up: i64) -> World {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let config = Config::new(Signup::Open, None)
        .with_secret(SecretKey::from_bytes([7; 32]))
        .with_admin_token(TOKEN);
    oxsum_server::prepare(&db, &config)
        .await
        .expect("the deployment is prepared");
    let app = oxsum_server::app(db.clone(), config);

    // The billable event's price book entry: an `event`-mode price on a fresh
    // channel — metering never calls the upstream, so the address only has to
    // be a URL.
    let channel = fresh("metered");
    let code = fresh("mail.send");
    db.set_channel(
        &channel,
        "http://127.0.0.1:9/unused",
        "upstream-secret",
        "openai",
        &SecretKey::from_bytes([7; 32]),
    )
    .await
    .expect("the channel registers");
    db.append_price(&channel, &code, event_price(1_000_000), 100)
        .await
        .expect("the event price appends");

    // An organization with a funded wallet, through the API like a user's.
    let email = format!(
        "metering_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    let org_key = body["data"]["apiKey"]["secret"]
        .as_str()
        .expect("registration returns a key")
        .to_owned();
    let organization_id = body["data"]["organization"]["id"]
        .as_str()
        .expect("registration names the organization")
        .to_owned();
    if top_up > 0 {
        let (status, body) = call(
            &app,
            "POST",
            "/api/v1/topups",
            Some(json!({"idempotencyKey": "top-1", "amountMinor": top_up})),
            Some(&org_key),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "top-up failed: {body}");
    }

    // The service credential the deployer's service holds, through the admin
    // surface like an operator's.
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/admin/service-credentials",
        Some(json!({"name": fresh("mailer")})),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the credential mints: {body}");
    let service_secret = body["data"]["secret"]
        .as_str()
        .expect("the mint answers the secret")
        .to_owned();
    let credential_id = body["data"]["credentialId"]
        .as_str()
        .expect("the mint names the credential")
        .to_owned();
    World {
        app,
        org_key,
        organization_id,
        service_secret,
        credential_id,
        channel,
        code,
    }
}

impl World {
    /// A `holds` or one-shot `settlements` body for this world's code.
    fn event(&self, idempotency_key: &str, units: i64) -> Value {
        json!({
            "idempotencyKey": idempotency_key,
            "organizationId": self.organization_id,
            "channel": self.channel,
            "billableCode": self.code,
            "usage": usage(units),
        })
    }
}

// ── the credential's perimeter ───────────────────────────────────────────────

#[tokio::test]
async fn the_admin_mints_lists_and_revokes_service_credentials() {
    let w = world_or_skip!(0);

    // The mint answered the secret once, in the documented shape.
    assert!(w.service_secret.starts_with("oxs-svc-"));
    assert_eq!(w.service_secret.len(), 8 + 64);

    // The list carries metadata — id, name, prefix, timestamps — and never
    // the secret or its hash.
    let (status, body) = call(
        &w.app,
        "GET",
        "/api/v1/admin/service-credentials",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["credentialId"] == w.credential_id)
        .expect("the minted credential lists")
        .clone();
    assert!(listed["prefix"].as_str().unwrap().starts_with("oxs-svc-"));
    assert!(
        w.service_secret
            .starts_with(listed["prefix"].as_str().unwrap())
    );
    assert!(listed.get("secret").is_none());
    assert!(listed["revokedAt"].is_null());

    // The mint and the revoke land in the audit log.
    let (status, body) = call(
        &w.app,
        "DELETE",
        &format!("/api/v1/admin/service-credentials/{}", w.credential_id),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["revokedAt"].is_string());
    let (status, body) = call(
        &w.app,
        "GET",
        "/api/v1/admin/audit?action=service_credential.revoke",
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["target"].as_str() == Some(w.credential_id.as_str())),
        "the revoke is in the audit log: {body}"
    );

    // The revoked credential authenticates nothing; a second revoke is 404.
    let (status, _) = call(
        &w.app,
        "POST",
        "/api/v1/metering/settlements",
        Some(w.event("revoked", 1)),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &w.app,
        "DELETE",
        &format!("/api/v1/admin/service-credentials/{}", w.credential_id),
        None,
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_a_service_credential_meters() {
    let w = world_or_skip!(1_000_000);
    let event = w.event("auth", 10);

    // No credential, an organization's API key, a made-up secret: all refused
    // the same way — an organization can never report its own usage.
    for bearer in [None, Some(w.org_key.as_str()), Some("oxs-svc-0000")] {
        let (status, body) = call(
            &w.app,
            "POST",
            "/api/v1/metering/holds",
            Some(event.clone()),
            bearer,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{bearer:?}: {body}");
    }

    // The service credential meters — and does nothing else: it is not an
    // organization key and not a gateway bearer.
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(event),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for path in ["/api/v1/balance", "/v1/models"] {
        let (status, _) = call(&w.app, "GET", path, None, Some(&w.service_secret)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
}

// ── the calls ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_hold_then_settle_charges_the_actual_usage() {
    let w = world_or_skip!(1_000_000);

    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(w.event("h1", 100)),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let hold = &body["data"];
    assert_eq!(hold["holdKey"], "metering:h1:hold");
    assert_eq!(hold["freezeMinor"], 100);
    assert_eq!(hold["priceVersion"], 1);

    // A replay of the same key answers the same hold.
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(w.event("h1", 100)),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["freezeMinor"], 100);

    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/settle",
        Some(json!({
            "organizationId": w.organization_id,
            "holdKey": hold["holdKey"],
            "usage": usage(40),
        })),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["chargedMinor"], 40);
    assert_eq!(body["data"]["kind"], "usage");
    assert!(body["data"]["settlementId"].is_string());
}

#[tokio::test]
async fn a_settle_above_the_freeze_is_capped() {
    let w = world_or_skip!(1_000_000);
    call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(w.event("cap", 50)),
        Some(&w.service_secret),
    )
    .await;
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/settle",
        Some(json!({
            "organizationId": w.organization_id,
            "holdKey": "metering:cap:hold",
            "usage": usage(90),
        })),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["kind"], "capped");
    assert_eq!(body["data"]["chargedMinor"], 50);
}

#[tokio::test]
async fn a_release_lets_a_hold_go_and_a_late_settle_conflicts() {
    let w = world_or_skip!(1_000_000);
    call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(w.event("rel", 50)),
        Some(&w.service_secret),
    )
    .await;
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/release",
        Some(json!({
            "organizationId": w.organization_id,
            "holdKey": "metering:rel:hold",
        })),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["kind"], "released");
    assert_eq!(body["data"]["chargedMinor"], 0);

    // The release replays; settling the released hold is a conflict.
    let (status, _) = call(
        &w.app,
        "POST",
        "/api/v1/metering/release",
        Some(json!({
            "organizationId": w.organization_id,
            "holdKey": "metering:rel:hold",
        })),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/settle",
        Some(json!({
            "organizationId": w.organization_id,
            "holdKey": "metering:rel:hold",
            "usage": usage(10),
        })),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn a_one_shot_settlement_bills_a_completed_event() {
    let w = world_or_skip!(1_000_000);
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/settlements",
        Some(w.event("once", 70)),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["kind"], "usage");
    assert_eq!(body["data"]["chargedMinor"], 70);
    assert_eq!(body["data"]["priceVersion"], 1);

    // The replay answers the same settlement — no second charge.
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/settlements",
        Some(w.event("once", 70)),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["chargedMinor"], 70);
}

// ── the refusals ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_report_the_price_cannot_bill_is_refused() {
    let w = world_or_skip!(1_000_000);
    // No eventType names nothing — the usage names what it is billing.
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(json!({
            "idempotencyKey": "no-et",
            "organizationId": w.organization_id,
            "channel": w.channel,
            "billableCode": w.code,
            "usage": {"inputTokens": 10},
        })),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A tool call is a dimension no event price carries — refused, not billed
    // at zero.
    let mut event = w.event("unpriced", 10);
    event["usage"]["toolCalls"] = json!(1);
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(event),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // An unknown code or channel resolves no `event` price.
    for (channel, code) in [
        (w.channel.clone(), fresh("nope")),
        (fresh("nochan"), w.code.clone()),
    ] {
        let (status, body) = call(
            &w.app,
            "POST",
            "/api/v1/metering/holds",
            Some(json!({
                "idempotencyKey": "unk",
                "organizationId": w.organization_id,
                "channel": channel,
                "billableCode": code,
                "usage": usage(10),
            })),
            Some(&w.service_secret),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn an_insolvent_or_unknown_organization_is_refused() {
    let w = world_or_skip!(1_000_000);
    // A hold the wallet cannot cover answers 402, on either entry point.
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(w.event("poor", 2_000_000)),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/settlements",
        Some(w.event("poor-once", 2_000_000)),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");

    // An organization that is not there is 404.
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/holds",
        Some(json!({
            "idempotencyKey": "ghost",
            "organizationId": Uuid::new_v4(),
            "channel": w.channel,
            "billableCode": w.code,
            "usage": usage(10),
        })),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // And a settle or release naming a hold that is not outstanding is 404.
    for path in ["settle", "release"] {
        let mut body = json!({
            "organizationId": w.organization_id,
            "holdKey": "metering:never:hold",
        });
        if path == "settle" {
            body["usage"] = usage(1);
        }
        let (status, answer) = call(
            &w.app,
            "POST",
            &format!("/api/v1/metering/{path}"),
            Some(body),
            Some(&w.service_secret),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {answer}");
    }
}

/// The event's settlement lands on the organization's billing records like a
/// gateway turn's does — the same ledger, the same proof surface.
#[tokio::test]
async fn a_metered_event_lands_on_the_billing_records() {
    let w = world_or_skip!(1_000_000);
    let (status, body) = call(
        &w.app,
        "POST",
        "/api/v1/metering/settlements",
        Some(w.event("books", 33)),
        Some(&w.service_secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = call(
        &w.app,
        "GET",
        "/api/v1/billing-records",
        None,
        Some(&w.org_key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let record = body["data"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["model"] == w.code)
        .expect("the event's record is there");
    assert_eq!(record["chargedMinor"], 33);
    assert_eq!(record["kind"], "usage");
    // Nothing was relayed: the metered record names no upstream attempt.
    assert_eq!(record["upstreamAttempts"], 0);
}

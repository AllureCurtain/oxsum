//! A01/A02: production routers refuse forged funding and only deployment
//! authority can record a uniquely referenced manual receipt. No fixture route.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const OPERATOR: &str = "financial-authority-operator-test";
const PASSWORD: &str = "test financial authority password";

async fn world() -> Option<(Router, Db, Tenants, oxsum_core::Registration)> {
    let url = oxsum_testkit::suite_db_url("financial_authority")?;
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let reg = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let tenants = Tenants::new(db.pool().clone());
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Open, None).with_admin_token(OPERATOR),
    );
    Some((app, db, tenants, reg))
}

async fn call(
    app: &Router,
    path: &str,
    body: Value,
    bearer: Option<&str>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::post(path).header("content-type", "application/json");
    if let Some(bearer) = bearer {
        req = req.header("authorization", format!("Bearer {bearer}"));
    }
    if let Some(cookie) = cookie {
        req = req.header("cookie", format!("oxsum_session={cookie}"));
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap(),
    )
}

fn terms() -> Value {
    json!({"idempotencyKey": "receipt-1", "amountMinor": 5_000_000, "reference": Uuid::new_v4().to_string(), "reason": "Bank receipt verified against settlement record"})
}

#[tokio::test]
async fn all_customer_roles_and_keys_cannot_create_money_or_use_operator_finance() {
    let Some((app, db, tenants, reg)) = world().await else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let wallet = tenants.get(&reg.organization.tenant_id).await.unwrap();
    let head = wallet.log_size().await.unwrap();
    let oxsum_core::totp::LoginOutcome::Session(session) =
        db.login(&reg.user.email, PASSWORD).await.unwrap()
    else {
        panic!("session");
    };
    let manual = format!(
        "/api/v1/admin/organizations/{}/manual-receipts",
        reg.organization.id
    );
    for role in ["owner", "admin", "member"] {
        sqlx::query("UPDATE oxsum.memberships SET role=$3 WHERE organization_id=$1 AND user_id=$2")
            .bind(reg.organization.id)
            .bind(reg.user.id)
            .bind(role)
            .execute(db.pool())
            .await
            .unwrap();
        for (bearer, cookie) in [
            (Some(reg.api_key.secret.as_str()), None),
            (None, Some(session.token.as_str())),
        ] {
            let (status, body) = call(&app, "/api/v1/topups", terms(), bearer, cookie).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {body}");
            assert_eq!(body["error"]["code"], "FORBIDDEN");
            for path in [
                manual.clone(),
                format!(
                    "/api/v1/admin/organizations/{}/adjustments",
                    reg.organization.id
                ),
                format!(
                    "/api/v1/admin/organizations/{}/credit-notes",
                    reg.organization.id
                ),
                format!("/api/v1/admin/deposits/{}/reverse", Uuid::new_v4()),
                format!("/api/v1/admin/statements/{}/payments", Uuid::new_v4()),
                "/api/v1/metering/settlements".into(),
                "/api/v1/metering/holds".into(),
                "/api/v1/metering/settle".into(),
                "/api/v1/metering/release".into(),
            ] {
                let (status, body) = call(&app, &path, terms(), bearer, cookie).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{role}: {path}: {body}");
            }
            let mut request = Request::patch(format!(
                "/api/v1/admin/organizations/{}",
                reg.organization.id
            ))
            .header("content-type", "application/json");
            if let Some(token) = bearer {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            if let Some(token) = cookie {
                request = request.header("cookie", format!("oxsum_session={token}"));
            }
            let denied = app.clone().oneshot(request.body(Body::from(json!({"idempotencyKey":"forged-credit", "creditLimitMinor":100_000_000}).to_string())).unwrap()).await.unwrap();
            assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        }
    }
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(wallet.log_size().await.unwrap(), head);
    let receipts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.manual_receipts WHERE organization_id=$1")
            .bind(reg.organization.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(receipts, 0);
}

#[tokio::test]
async fn manual_receipt_replays_once_and_rejects_changed_terms_or_duplicate_reference() {
    let Some((app, db, tenants, reg)) = world().await else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let path = format!(
        "/api/v1/admin/organizations/{}/manual-receipts",
        reg.organization.id
    );
    let terms = terms();
    let (left, right) = tokio::join!(
        call(&app, &path, terms.clone(), Some(OPERATOR), None),
        call(&app, &path, terms.clone(), Some(OPERATOR), None)
    );
    assert_eq!(left.0, StatusCode::OK, "{}", left.1);
    assert_eq!(right.0, StatusCode::OK, "{}", right.1);
    assert_eq!(left.1["data"]["entryId"], right.1["data"]["entryId"]);
    assert_ne!(left.1["data"]["isNew"], right.1["data"]["isNew"]);
    for (field, value) in [
        ("amountMinor", json!(6_000_000)),
        ("reason", json!("changed")),
        ("reference", json!("another-reference")),
        ("idempotencyKey", json!("new-key-same-reference")),
    ] {
        let mut changed = terms.clone();
        changed[field] = value;
        assert_eq!(
            call(&app, &path, changed, Some(OPERATOR), None).await.0,
            StatusCode::CONFLICT,
            "{field}"
        );
    }
    let other = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let other_path = format!(
        "/api/v1/admin/organizations/{}/manual-receipts",
        other.organization.id
    );
    assert_eq!(
        call(&app, &other_path, terms, Some(OPERATOR), None).await.0,
        StatusCode::CONFLICT
    );
    let wallet = tenants.get(&reg.organization.tenant_id).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 5_000_000);
    assert_eq!(wallet.log_size().await.unwrap(), 1);
    let audit: (String,Value)=sqlx::query_as("SELECT actor, detail FROM oxsum.admin_audit WHERE target=$1 AND action='manual_receipt.authorized'").bind(reg.organization.id.to_string()).fetch_one(db.pool()).await.unwrap();
    for mutation in [
        "UPDATE oxsum.manual_receipts SET amount_minor = amount_minor + 1 WHERE organization_id = $1",
        "DELETE FROM oxsum.manual_receipts WHERE organization_id = $1",
    ] {
        assert!(
            sqlx::query(mutation)
                .bind(reg.organization.id)
                .execute(db.pool())
                .await
                .is_err()
        );
    }
    assert_eq!(wallet.available().await.unwrap(), 5_000_000);
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/v1/admin/audit?action=manual_receipt.authorized")
                .header("authorization", format!("Bearer {OPERATOR}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        page["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["target"] == reg.organization.id.to_string())
    );
    assert_eq!(audit.0, "operator");
    assert_eq!(audit.1["amountMinor"], 5_000_000);
    assert_eq!(
        audit.1["reason"],
        "Bank receipt verified against settlement record"
    );
}

#[tokio::test]
async fn interrupted_authorization_recovers_and_missing_evidence_never_credits() {
    let Some((app, db, tenants, reg)) = world().await else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let path = format!(
        "/api/v1/admin/organizations/{}/manual-receipts",
        reg.organization.id
    );
    let terms = terms();
    for field in ["reference", "reason", "idempotencyKey"] {
        let mut bad = terms.clone();
        bad[field] = json!("   ");
        assert_eq!(
            call(&app, &path, bad, Some(OPERATOR), None).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.manual_receipts (receipt_id,organization_id,idempotency_key,reference,reason,amount_minor) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(id).bind(reg.organization.id).bind(terms["idempotencyKey"].as_str().unwrap()).bind(terms["reference"].as_str().unwrap()).bind(terms["reason"].as_str().unwrap()).bind(5_000_000_i64).execute(db.pool()).await.unwrap();
    let wallet = tenants.get(&reg.organization.tenant_id).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
    let (status, first) = call(&app, &path, terms.clone(), Some(OPERATOR), None).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    // Simulate a lost response after commit. Retry must recover the same receipt.
    let (status, replay) = call(&app, &path, terms, Some(OPERATOR), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["data"]["isNew"], false);
    assert_eq!(first["data"]["entryId"], replay["data"]["entryId"]);
    assert_eq!(wallet.available().await.unwrap(), 5_000_000);
}

#[tokio::test]
async fn customer_roles_cannot_create_or_settle_a_trusted_hold() {
    let Some((app, db, tenants, reg)) = world().await else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let wallet = tenants.get(&reg.organization.tenant_id).await.unwrap();
    let day = time::OffsetDateTime::now_utc().date();
    wallet
        .top_up("fixture-funds", 5_000_000, day)
        .await
        .unwrap();
    let hold_key = format!("req-{}:hold", Uuid::new_v4());
    wallet
        .hold(&hold_key, "trusted admission", 1_000_000, day)
        .await
        .unwrap();
    db.note_open_hold(&oxsum_core::OpenHold {
        hold_key: hold_key.clone(),
        tenant_id: reg.organization.tenant_id.clone(),
        request_id: "trusted-request".into(),
        model: "fixture".into(),
        channel: "fixture".into(),
        price_version: 1,
        input_price: 1,
        output_price: 1,
        freeze_minor: 1_000_000,
        key_id: Some(reg.api_key.key.id),
        mandate_id: None,
        end_user: None,
        service_tier: None,
        tags: Default::default(),
        sweep_attempts: 0,
        last_error: None,
        dead_at: None,
    })
    .await
    .unwrap();
    let head = wallet.log_size().await.unwrap();
    let before: Value =
        sqlx::query_scalar("SELECT to_jsonb(h) FROM oxsum.open_holds h WHERE hold_key=$1")
            .bind(&hold_key)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let oxsum_core::totp::LoginOutcome::Session(session) =
        db.login(&reg.user.email, PASSWORD).await.unwrap()
    else {
        panic!("session");
    };
    for role in ["owner", "admin", "member"] {
        sqlx::query("UPDATE oxsum.memberships SET role=$3 WHERE organization_id=$1 AND user_id=$2")
            .bind(reg.organization.id)
            .bind(reg.user.id)
            .bind(role)
            .execute(db.pool())
            .await
            .unwrap();
        for (bearer, cookie) in [
            (Some(reg.api_key.secret.as_str()), None),
            (None, Some(session.token.as_str())),
        ] {
            for (path, body) in [
                (
                    "/api/v1/holds",
                    json!({"idempotencyKey":"forged", "amountMinor":1}),
                ),
                (
                    "/api/v1/settlements",
                    json!({"holdKey":hold_key,"actualMinor":0}),
                ),
                (
                    "/api/v1/settlements",
                    json!({"holdKey":hold_key,"actualMinor":1_000_000}),
                ),
                ("/api/v1/holds", Value::Null),
                ("/api/v1/settlements", Value::Null),
            ] {
                let (status, response) = call(&app, path, body, bearer, cookie).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {path}: {response}");
                assert_eq!(response["error"]["code"], "FORBIDDEN");
            }
        }
    }
    assert_eq!(wallet.available().await.unwrap(), 4_000_000);
    assert_eq!(wallet.reserved().await.unwrap(), 1_000_000);
    assert_eq!(wallet.log_size().await.unwrap(), head);
    let after: Value =
        sqlx::query_scalar("SELECT to_jsonb(h) FROM oxsum.open_holds h WHERE hold_key=$1")
            .bind(&hold_key)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(before, after, "denial cannot remove or edit recovery state");
    let response = app
        .oneshot(
            Request::get("/api/v1/holds")
                .header("authorization", format!("Bearer {}", reg.api_key.secret))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(
        body["data"]["holds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["requestId"] == "trusted-request")
    );
    // Recovery retains authority after all customer attempts are refused.
    wallet
        .settle(&hold_key, "trusted recovery", 300_000, day)
        .await
        .unwrap();
    db.clear_open_hold(&hold_key).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 4_700_000);
    assert_eq!(wallet.reserved().await.unwrap(), 0);
}

//! Operator-only diagnostics return fixed flags/counts, never stored identities.
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser, SecretKey};
use oxsum_integrations::mail::Mailer;
use oxsum_server::{Config, Signup};
use serde_json::Value;
use sqlx::Executor;
use tower::ServiceExt;

const TOKEN: &str = "operator-private-diagnostic-token";

fn config() -> Config {
    Config::new(Signup::Invite, None).with_admin_token(TOKEN)
}

async fn database(slug: &str) -> Option<Db> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url(slug)?;
    Some(Db::connect(&url, 5).await.unwrap())
}

async fn response(
    app: &axum::Router,
    bearer: Option<&str>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::get("/api/v1/admin/diagnostics");
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn operator_guard_rejects_customer_credentials_and_session_cookies() {
    let Some(db) = database("operator_diagnostics_guard").await else {
        return;
    };
    db.migrate().await.unwrap();
    let registration = db
        .register(NewUser {
            email: "private-diagnostics@example.test".into(),
            password: "private-customer-password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let app = oxsum_server::app(db.clone(), config());
    for bearer in [
        None,
        Some("invalid-token"),
        Some(registration.api_key.secret.as_str()),
    ] {
        assert_eq!(
            response(&app, bearer, None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    // A token in the cookie cannot become operator authority, even if its bytes
    // happen to equal the configured operator credential.
    assert_eq!(
        response(&app, None, Some(&format!("oxsum_session={TOKEN}")))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(response(&app, Some(TOKEN), None).await.0, StatusCode::OK);
    let disabled = oxsum_server::app(db, Config::new(Signup::Invite, None));
    assert_eq!(
        response(&disabled, Some(TOKEN), None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn flags_report_presence_without_exposing_addresses_or_keys() {
    let Some(db) = database("operator_diagnostics_flags").await else {
        return;
    };
    db.migrate().await.unwrap();
    let configured = config()
        .with_secret(SecretKey::from_bytes([123; 32]))
        .with_head_signing_seed([124; 32])
        .with_public_url("https://private-url.test")
        .with_mailer(
            Mailer::try_new(
                "smtp://private-user:private-password@127.0.0.1:1",
                "private-mail@example.test",
                "https://private-url.test",
            )
            .unwrap(),
        );
    let app = oxsum_server::app(db, configured);
    let (status, body) = response(&app, Some(TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data.as_object().unwrap().len(), 7);
    assert_eq!(data["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(data["schemaVersion"], Db::latest_schema_version());
    assert_eq!(data["appliedSchemaVersion"], Db::latest_schema_version());
    assert_eq!(data["databaseState"], "ready");
    assert_eq!(
        data["configuration"],
        serde_json::json!({"smtpConfigured":true,"publicUrlConfigured":true,"credentialSealConfigured":true,
        "headSigningConfigured":true,"stripeConfigured":false,"airwallexConfigured":false,"hyperswitchConfigured":false,"x402Configured":false})
    );
    assert_eq!(data["counts"].as_object().unwrap().len(), 23);
    assert!(
        data["counts"]
            .as_object()
            .unwrap()
            .values()
            .all(|value| value == 0)
    );
    assert!(!body.to_string().contains("private"));
}

#[tokio::test]
async fn missing_and_incompatible_schema_return_no_invented_zero_counts() {
    let Some(db) = database("operator_diagnostics_schema").await else {
        return;
    };
    let app = oxsum_server::app(db.clone(), config());
    let (_, fresh) = response(&app, Some(TOKEN), None).await;
    assert_eq!(fresh["data"]["databaseState"], "unmigrated");
    assert!(fresh["data"]["counts"].is_null());
    db.pool()
        .execute("CREATE SCHEMA oxsum; CREATE TABLE oxsum._migrations(version text)")
        .await
        .unwrap();
    for version in [
        "0001_old_schema",
        "PRIVATE_DATABASE_PASSWORD=https://private.test",
    ] {
        sqlx::query("DELETE FROM oxsum._migrations")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO oxsum._migrations VALUES ($1)")
            .bind(version)
            .execute(db.pool())
            .await
            .unwrap();
        let (status, body) = response(&app, Some(TOKEN), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["databaseState"], "schemaMismatch");
        assert!(body["data"]["counts"].is_null());
        if version == "0001_old_schema" {
            assert_eq!(body["data"]["appliedSchemaVersion"], version);
        } else {
            assert!(body["data"]["appliedSchemaVersion"].is_null());
            assert!(!body.to_string().contains("PRIVATE"));
        }
    }
}

#[tokio::test]
async fn unavailable_database_returns_no_storage_error_text() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(25))
        .connect_lazy("postgres://private-user:private-password@127.0.0.1:1/private-database")
        .unwrap();
    let app = oxsum_server::app(Db::from_pool(pool), config());
    let (status, body) = response(&app, Some(TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["databaseState"], "unavailable");
    assert!(body["data"]["counts"].is_null());
    assert!(body["data"]["appliedSchemaVersion"].is_null());
    assert!(!body.to_string().contains("private"));
}

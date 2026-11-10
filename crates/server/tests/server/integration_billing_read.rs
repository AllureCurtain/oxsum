//! #788 N21.b over HTTP: billingRead serves one granted customer's balance and
//! bills; wrong scope, wrong namespace and missing credential all fail closed.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, IntegrationScope, NewUser, Registration, Tenants};
use oxsum_server::{Config, Signup};
use serde_json::Value;
use time::OffsetDateTime;
use tower::ServiceExt;
use uuid::Uuid;
const PASSWORD: &str = "billing read http fixture password";
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
async fn mint(db: &Db, customers: Vec<Uuid>, scopes: Vec<IntegrationScope>) -> String {
    setup::mint(db, customers, scopes).await.secret.unwrap()
}
#[tokio::test]
async fn billing_read_serves_the_granted_customers_balance_and_bills() {
    let Some(url) = oxsum_testkit::suite_db_url("integration_billing_read_http") else {
        return;
    };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    setup::owner(&db).await;
    let a = user(&db).await;
    let b = user(&db).await;
    // Fund `a` so the balance read returns a real settled figure.
    let tenants = Tenants::new(db.pool().clone());
    let wa = tenants.get(&a.organization.tenant_id).await.unwrap();
    wa.top_up("seed", 7_500, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_admin_token("billing-read-fixture"),
    );
    let secret = mint(
        &db,
        vec![a.organization.id],
        vec![IntegrationScope::BillingRead],
    )
    .await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/v1/business/customers/{}/billing", a.organization.id),
        Value::Null,
        None,
        Some(&secret),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let balance = &body["data"]["balance"];
    assert_eq!(balance["availableMinor"], 7_500, "{balance}");
    assert!(balance["creditLimitMinor"].is_number());
    assert!(balance["reservedMinor"].is_number());
    // The seed top-up appears as a topUp row in the bill window.
    let rows = body["data"]["bills"]["rows"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|r| r["kind"] == "topUp" && r["amountMinor"] == 7_500),
        "{rows:?}"
    );
    // A customer outside the grant is 404; a session cookie is not a credential.
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/business/customers/{}/billing", b.organization.id),
        Value::Null,
        None,
        Some(&secret),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let personal = db
        .login(&a.user.email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token;
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/business/customers/{}/billing", a.organization.id),
        Value::Null,
        Some(&personal),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn wrong_scope_cannot_read_billing() {
    let Some(url) = oxsum_testkit::suite_db_url("integration_billing_read_scope") else {
        return;
    };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    setup::owner(&db).await;
    let a = user(&db).await;
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_admin_token("billing-read-fixture"),
    );
    for scope in [
        IntegrationScope::CustomersRead,
        IntegrationScope::MeteringWrite,
        IntegrationScope::CustomersProvision,
    ] {
        let secret = mint(&db, vec![a.organization.id], vec![scope]).await;
        let (status, _) = call(
            &app,
            "GET",
            &format!("/api/v1/business/customers/{}/billing", a.organization.id),
            Value::Null,
            None,
            Some(&secret),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "scope {scope:?}");
    }
    // A credential without any billing authority cannot mint itself one.
    let (status, _) = call(
        &app,
        "GET",
        &format!("/api/v1/business/customers/{}/billing", a.organization.id),
        Value::Null,
        None,
        Some("oxs-int-forged"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

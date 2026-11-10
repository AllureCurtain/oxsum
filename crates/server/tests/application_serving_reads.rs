//! Application entitlement/model reads use only the current explicit machine grant.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    Db, IntegrationApplicationRequest, IntegrationScope, KeyConstraints, ProvisionCustomerRequest,
    SaleChange, SaleChangeRequest, SecretKey,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn get(
    app: &Router,
    path: &str,
    cookie: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().uri(path);
    if let Some(cookie) = cookie {
        request = request.header("cookie", format!("oxsum_session={cookie}"));
    }
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn grant(db: &Db, owner: &str, app: Uuid, scopes: Vec<IntegrationScope>) -> String {
    let mut request = setup::request(vec![], scopes);
    request.application_id = Some(app);
    db.create_integration_credential(owner, &request)
        .await
        .unwrap()
        .secret
        .unwrap()
}

#[tokio::test]
async fn entitlement_and_model_routes_refuse_other_principals_and_return_actual_sale() {
    let Some(url) = oxsum_testkit::suite_db_url("application_serving_reads_http") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let application = db
        .create_integration_application(
            &owner,
            &IntegrationApplicationRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                key: "serving-read-http".into(),
                name: "Read HTTP".into(),
                customer_limit: 2,
                reason: "http fixture".into(),
            },
        )
        .await
        .unwrap();
    let provision = grant(
        &db,
        &owner,
        application.application_id,
        vec![IntegrationScope::CustomersProvision],
    )
    .await;
    let actor = db.authenticate_service(&provision).await.unwrap().unwrap();
    let customer = db
        .provision_application_customer(
            &actor,
            &ProvisionCustomerRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                external_id: "one".into(),
                name: "Read user".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    let secret = grant(
        &db,
        &owner,
        application.application_id,
        vec![IntegrationScope::EntitlementsRead],
    )
    .await;
    let consumption = db
        .create_key(customer, None, None, None, KeyConstraints::default())
        .await
        .unwrap();
    let seal = SecretKey::from_bytes([61; 32]);
    db.set_channel(
        "read-http",
        "https://private-provider.example/v1",
        "hidden-provider-secret",
        "openai",
        &seal,
    )
    .await
    .unwrap();
    db.append_price(
        "read-http",
        "public-read-model",
        serde_json::from_value(
            json!({"inputPricePerMillion":900,"outputPricePerMillion":900,"maxOutputTokens":20}),
        )
        .unwrap(),
        100,
    )
    .await
    .unwrap();
    db.change_sale_terms(&owner,&SaleChangeRequest {
        idempotency_key:Uuid::new_v4().to_string(),reason:"customer sale".into(),
        change:SaleChange::OverrideModel {organization_id:customer,model:"public-read-model".into(),terms:Box::new(serde_json::from_value(json!({"price":{"inputPricePerMillion":0,"outputPricePerMillion":0,"maxOutputTokens":20},"discountPercent":0})).unwrap()),effective_from:None,effective_until:None,replaces:None},
    }).await.unwrap();
    let server = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None).with_secret(seal),
    );
    let entitlement = format!("/api/v1/integration/customers/{customer}/entitlement");
    let models = format!("/api/v1/integration/customers/{customer}/models");
    for path in [&entitlement, &models] {
        assert_eq!(
            get(&server, path, None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&server, path, Some(&owner), None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&server, path, None, Some(&consumption.secret)).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(&server, path, None, Some(&provision)).await.0,
            StatusCode::FORBIDDEN
        );
        let unknown = path.replace(&customer.to_string(), &Uuid::new_v4().to_string());
        assert_eq!(
            get(&server, &unknown, None, Some(&secret)).await.0,
            StatusCode::NOT_FOUND
        );
    }
    let (status, standing) = get(&server, &entitlement, None, Some(&secret)).await;
    assert_eq!(status, StatusCode::OK, "{standing}");
    assert_eq!(standing["data"]["customerState"], "active");
    assert!(standing["data"]["plan"].is_null() && standing["data"]["quota"].is_null());
    let (status, page) = get(&server, &format!("{models}?limit=1"), None, Some(&secret)).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["data"]["models"][0]["model"], "public-read-model");
    assert_eq!(
        page["data"]["models"][0]["saleTerms"]["terms"]["price"]["inputPricePerMillion"],
        0
    );
    assert_eq!(page["data"]["models"][0]["saleTerms"]["source"], "override");
    assert!(page["data"]["nextCursor"].is_null());
    for invalid in [
        "limit=0",
        "limit=51",
        "limit=wrong",
        "keyId=wrong",
        "unknown=1",
        "after=%20padded%20",
    ] {
        assert_eq!(
            get(&server, &format!("{models}?{invalid}"), None, Some(&secret))
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{invalid}"
        );
    }
    assert_eq!(
        get(
            &server,
            &format!("{models}?keyId={}", consumption.key.id),
            None,
            Some(&secret)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Wholesale values must not appear anywhere in the response. The "900"
    // sweep is scoped to the `terms` subtree because timestamps (`asOf`,
    // `effectiveFrom`) can legitimately contain that digit run.
    let serialized = page.to_string();
    for value in [
        "hidden-provider-secret",
        "private-provider.example",
        "upstream",
    ] {
        assert!(!serialized.contains(value), "{value}");
    }
    let terms = page["data"]["models"][0]["saleTerms"]["terms"].to_string();
    assert!(!terms.contains("900"));
    let digests: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.sale_snapshot_digests WHERE organization_id=$1",
    )
    .bind(customer)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(digests, 0);
}

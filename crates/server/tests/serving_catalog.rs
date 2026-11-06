//! A06/A08: HTTP catalogs and estimates expose only admitted customer routes.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod support;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use oxsum_core::*;
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use uuid::Uuid;

async fn call(
    app: &Router,
    path: &str,
    body: Option<Value>,
    token: &str,
    cookie: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(path);
    req = if cookie {
        req.header("cookie", format!("oxsum_session={token}"))
    } else {
        req.header("authorization", format!("Bearer {token}"))
    };
    let response = app
        .clone()
        .oneshot(
            req.header("content-type", "application/json")
                .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}
use tower::ServiceExt;

#[tokio::test]
async fn catalog_estimate_and_alias_follow_customer_key_and_group_grants() {
    let Some(url) = oxsum_testkit::suite_db_url("serving_catalog") else {
        return;
    };
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "catalog fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let session = db
        .login(&customer.user.email, "catalog fixture password")
        .await
        .unwrap()
        .into_session();
    let secret = SecretKey::from_bytes([1; 32]);
    let book=PriceBook::from_json("a",r#"{"shared":{"inputPricePerMillion":1000000,"outputPricePerMillion":2000000,"maxOutputTokens":20},"hidden":{"inputPricePerMillion":1000000,"outputPricePerMillion":2000000,"maxOutputTokens":20}}"#).unwrap();
    for channel in ["a", "b"] {
        db.set_channel(
            channel,
            "https://provider.example/v1",
            "provider-fixture-secret",
            "openai",
            &secret,
        )
        .await
        .unwrap();
        for (model, price) in book.models() {
            db.append_price(channel, model, price.clone(), 100)
                .await
                .unwrap();
        }
    }
    db.create_alias("a", "public", "shared").await.unwrap();
    let group = db
        .put_group(
            customer.organization.id,
            "chosen",
            &[db.channel_id_named("a").await.unwrap().unwrap()],
        )
        .await
        .unwrap();
    let key = db
        .create_key(
            customer.organization.id,
            None,
            None,
            None,
            KeyConstraints {
                model_allowlist: Some(vec!["shared".into(), "public".into()]),
                group_id: Some(Some(group.id)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    db.set_tier("basic", None, Some(vec!["shared".into(), "public".into()]))
        .await
        .unwrap();
    db.set_organization_tier(customer.organization.id, Some("basic"))
        .await
        .unwrap();
    let app = support::app(
        db.clone(),
        Config::new(Signup::Invite, None)
            .with_secret(secret)
            .with_head_signing_seed([3; 32]),
    );
    let (status, list) = call(&app, "/v1/models", None, &key.secret, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["public", "shared"]
    );
    assert!(
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["owned_by"] == "a")
    );
    let (status, prices) = call(&app, "/api/v1/pricing", None, &key.secret, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(prices["data"]["models"].as_array().unwrap().len(), 2);
    assert!(!prices.to_string().contains("hidden"));
    let (status, estimate) = call(
        &app,
        "/api/v1/estimate-price",
        Some(json!({"model":"public","inputTokens":5,"outputTokens":2})),
        &key.secret,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{estimate}");
    assert_eq!(estimate["data"]["estimateMinor"], 9);
    assert_eq!(
        call(
            &app,
            "/api/v1/estimate-price",
            Some(json!({"model":"hidden","inputTokens":5})),
            &key.secret,
            false
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/log/price-head?channel=a&model=shared",
            None,
            &key.secret,
            false
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/log/price-head?channel=b&model=shared",
            None,
            &key.secret,
            false
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/log/price-head?channel=a&model=hidden",
            None,
            &key.secret,
            false
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let session_prices = call(&app, "/api/v1/pricing", None, &session.token, true)
        .await
        .1;
    assert_eq!(
        session_prices["data"]["models"].as_array().unwrap().len(),
        3,
        "session sees both tier-permitted routes"
    );
    db.update_key_constraints(
        customer.organization.id,
        key.key.id,
        KeyScope::Organization,
        KeyConstraints {
            model_allowlist: Some(vec!["public".into()]),
            group_id: Some(Some(group.id)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        call(&app, "/v1/models", None, &key.secret, false).await.1["data"]
            .as_array()
            .unwrap()
            .is_empty(),
        "alias cannot widen target access"
    );
    assert_eq!(
        call(
            &app,
            "/api/v1/estimate-price",
            Some(json!({"model":"public","inputTokens":5})),
            &key.secret,
            false
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let other = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "other catalog fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.set_tier("other-customer", None, Some(vec!["hidden".into()]))
        .await
        .unwrap();
    db.set_organization_tier(other.organization.id, Some("other-customer"))
        .await
        .unwrap();
    let (_, other_list) = call(&app, "/v1/models", None, &other.api_key.secret, false).await;
    assert_eq!(other_list["data"].as_array().unwrap().len(), 1);
    assert_eq!(other_list["data"][0]["id"], "hidden");
    db.revoke_key(customer.organization.id, key.key.id, KeyScope::Organization)
        .await
        .unwrap();
    assert_eq!(
        call(&app, "/v1/models", None, &key.secret, false).await.0,
        StatusCode::UNAUTHORIZED
    );
}

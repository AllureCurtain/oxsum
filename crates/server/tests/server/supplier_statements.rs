//! Business supplier statement routes: real format, current authority and exact export.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    session: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(s) = session {
        req = req.header("cookie", format!("oxsum_session={s}"));
    }
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let res = app
        .clone()
        .oneshot(
            req.body(
                body.map(|b| Body::from(b.to_string()))
                    .unwrap_or_else(Body::empty),
            )
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
#[tokio::test]
async fn import_preview_history_comparison_and_export_require_current_staff() {
    let url = oxsum_testkit::suite_url!().expect("supplier HTTP requires PostgreSQL");
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "supplier HTTP fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    sqlx::query("INSERT INTO oxsum.business_staff(business_id,user_id,role) SELECT business_id,$1,'owner' FROM oxsum.businesses WHERE singleton").bind(user.user.id).execute(db.pool()).await.unwrap();
    let session = db
        .login(&user.user.email, "supplier HTTP fixture")
        .await
        .unwrap()
        .into_session()
        .token;
    db.switch_session_context(
        &session,
        &oxsum_core::SessionContextRequest {
            idempotency_key: "switch".into(),
            context: oxsum_core::SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE oxsum.business_staff SET role='finance' WHERE user_id=$1")
        .bind(user.user.id)
        .execute(db.pool())
        .await
        .unwrap();
    let secret = SecretKey::from_bytes([22; 32]);
    db.set_channel(
        "official-openai",
        "https://api.openai.com/v1",
        "not-exportable",
        "openai",
        &secret,
    )
    .await
    .unwrap();
    let channel: Uuid =
        sqlx::query_scalar("SELECT channel_id FROM oxsum.channels WHERE name='official-openai'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Open, None).with_secret(secret),
    );
    let base = "/api/v1/business/supplier-statements";
    let key = db
        .create_key(user.organization.id, None, None, None, Default::default())
        .await
        .unwrap();
    for path in [base, &format!("{base}/choices")] {
        let response = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header("authorization", format!("Bearer {}", key.secret))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let customer = db
        .login(&user.user.email, "supplier HTTP fixture")
        .await
        .unwrap()
        .into_session()
        .token;
    assert_eq!(
        call(&app, "GET", base, Some(&customer), None).await.0,
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        call(&app, "GET", base, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, choices) = call(
        &app,
        "GET",
        &format!("{base}/choices"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(choices["data"][0]["channelId"], channel.to_string());
    assert!(!choices.to_string().contains("not-exportable"));
    let input = json!({"idempotencyKey":"import-http","reason":"Synthetic official-format import","providerOrganizationId":"org-fixture","billDate":"2026-11-01","fetchedAt":"2026-11-02T12:00:00Z","scopeKind":"organization","projectIds":[],"channelIds":[channel],"usdPerCredit":"1","mappingEvidence":"Synthetic scope and conversion assertion","query":{"startTime":1793491200_i64,"endTime":1793577600_i64,"bucketWidth":"1d","groupBy":[],"projectIds":[],"limit":7},"pages":[{"requestPage":null,"body":r#"{"object":"page","data":[{"object":"bucket","start_time":1793491200,"end_time":1793577600,"results":[{"object":"organization.costs.result","amount":{"value":0.06,"currency":"usd"}}]}],"has_more":false,"next_page":null}"#}]});
    let preview = call(
        &app,
        "POST",
        &format!("{base}/preview"),
        Some(&session),
        Some(input.clone()),
    )
    .await;
    assert_eq!(preview.0, StatusCode::OK, "{}", preview.1);
    assert_eq!(preview.1["data"]["amountUsd"], "0.06");
    let (status, result) = call(&app, "POST", base, Some(&session), Some(input.clone())).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let id = result["data"]["sourceId"].as_str().unwrap();
    assert_eq!(
        call(&app, "POST", base, Some(&session), Some(input.clone()))
            .await
            .1,
        result
    );
    assert_eq!(
        call(&app, "GET", &format!("{base}/{id}"), Some(&session), None)
            .await
            .1,
        result
    );
    let comparison = call(
        &app,
        "POST",
        &format!("{base}/{id}/compare"),
        Some(&session),
        Some(json!({"idempotencyKey":"second","reason":"Updated observation capture"})),
    )
    .await;
    assert_eq!(comparison.0, StatusCode::OK, "{}", comparison.1);
    let cid = comparison.1["data"]["comparisonId"].as_str().unwrap();
    let selected = call(
        &app,
        "GET",
        &format!("{base}/{id}?comparisonId={cid}"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(selected.1["data"]["comparison"], comparison.1["data"]);
    let history = call(
        &app,
        "GET",
        &format!("{base}/{id}/comparisons"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(history.1["data"].as_array().unwrap().len(), 2);
    assert_eq!(
        call(&app, "GET", base, Some(&session), None).await.1["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    sqlx::query("UPDATE oxsum.business_staff SET role='viewer' WHERE user_id=$1")
        .bind(user.user.id)
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(
        call(&app, "POST", base, Some(&session), Some(input))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", &format!("{base}/{id}"), Some(&session), None)
            .await
            .0,
        StatusCode::OK
    );
}

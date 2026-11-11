//! Production-router coverage of every sale book operation and strict preview input.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use tower::ServiceExt;
use uuid::Uuid;
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    token: Option<&str>,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("cookie", format!("oxsum_session={token}"));
    }
    if let Some(token) = bearer {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn request(change: Value) -> Value {
    json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"sale HTTP fixture","change":change})
}
async fn mutate(app: &Router, owner: &str, change: Value) -> Value {
    let body = request(change);
    let (status, result) = call(
        app,
        "POST",
        "/api/v1/business/sale-books/changes",
        body.clone(),
        Some(owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        call(
            app,
            "POST",
            "/api/v1/business/sale-books/changes",
            body,
            Some(owner),
            None
        )
        .await
        .1,
        result
    );
    result["data"].clone()
}
#[tokio::test]
async fn sale_endpoints_enforce_current_personal_authority_and_contract() {
    let Some(url) = oxsum_testkit::suite_db_url("sale_prices_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "customer sale fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let customer = db
        .login(&user.user.email, "customer sale fixture")
        .await
        .unwrap()
        .into_session()
        .token;
    let org = user.organization.id;
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));
    let create = request(json!({"operation":"createBook","name":"HTTP sale book"}));
    for (token, bearer, status) in [
        (None, None, StatusCode::UNAUTHORIZED),
        (Some(owner.as_str()), Some(""), StatusCode::FORBIDDEN),
        (
            Some(owner.as_str()),
            Some("oxs-int-unknown"),
            StatusCode::FORBIDDEN,
        ),
        (Some(customer.as_str()), None, StatusCode::FORBIDDEN),
        (
            None,
            Some(user.api_key.secret.as_str()),
            StatusCode::FORBIDDEN,
        ),
    ] {
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/business/sale-books/changes",
                create.clone(),
                token,
                bearer
            )
            .await
            .0,
            status
        );
    }
    let (_, created) = call(
        &app,
        "POST",
        "/api/v1/business/sale-books/changes",
        create.clone(),
        Some(&owner),
        None,
    )
    .await;
    let book = created["data"]["resourceId"].as_str().unwrap();
    let sale = json!({"price":{"inputPricePerMillion":1_000_000,"outputPricePerMillion":1_000_000,"maxOutputTokens":100},"discountPercent":50});
    let draft = mutate(
        &app,
        &owner,
        json!({"operation":"saveDraft","bookId":book,"expectedRevision":0,"models":{"model":sale}}),
    )
    .await;
    assert_eq!(draft["revision"], 1);
    mutate(
        &app,
        &owner,
        json!({"operation":"publishDraft","bookId":book,"draftRevision":1}),
    )
    .await;
    let start = (OffsetDateTime::now_utc() + Duration::hours(1))
        .format(&Rfc3339)
        .unwrap();
    let seam = (OffsetDateTime::now_utc() + Duration::hours(2))
        .format(&Rfc3339)
        .unwrap();
    let default = mutate(
        &app,
        &owner,
        json!({"operation":"scheduleDefault","bookId":book,"version":1,"effectiveFrom":start}),
    )
    .await;
    mutate(&app,&owner,json!({"operation":"assignBook","organizationId":org,"bookId":book,"version":1,"effectiveFrom":start})).await;
    let special=mutate(&app,&owner,json!({"operation":"overrideModel","organizationId":org,"model":"model","terms":{"price":{"inputPricePerMillion":3_000_000,"maxOutputTokens":100}},"effectiveFrom":start})).await;
    mutate(
        &app,
        &owner,
        json!({"operation":"endSchedule","scheduleId":special["resourceId"],"effectiveUntil":seam}),
    )
    .await;
    let (status, list) = call(
        &app,
        "GET",
        "/api/v1/business/sale-books?limit=1",
        Value::Null,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"]["books"][0]["id"], book);
    let (status, history) = call(
        &app,
        "GET",
        &format!("/api/v1/business/sale-books/{book}"),
        Value::Null,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history["data"]["versions"][0]["version"], 1);
    assert!(history["data"]["book"]["createdAt"].is_string());
    assert!(
        history["data"]["schedules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == default["resourceId"])
    );
    let (status, terms) = call(
        &app,
        "GET",
        &format!("/api/v1/business/customers/{org}/sale-terms"),
        Value::Null,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(terms["data"].as_array().unwrap().len(), 2);
    let preview =
        json!({"organizationId":org,"model":"model","at":start,"usage":{"inputTokens":2}});
    let (status, result) = call(
        &app,
        "POST",
        "/api/v1/business/sale-price-preview",
        preview.clone(),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["data"]["chargeMinor"], 6);
    assert_eq!(result["data"]["snapshot"]["source"], "override");
    let (_, result) = call(
        &app,
        "POST",
        "/api/v1/business/sale-price-preview",
        json!({"at":seam,"organizationId":org,"model":"model","usage":{"inputTokens":2}}),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(result["data"]["chargeMinor"], 1);
    assert_eq!(result["data"]["snapshot"]["source"], "assigned");
    let (_,result)=call(&app,"POST","/api/v1/business/sale-price-preview",json!({"organizationId":org,"model":"model","source":{"kind":"draft","bookId":book,"revision":1},"usage":{"inputTokens":2}}),Some(&owner),None).await;
    assert_eq!(result["data"]["chargeMinor"], 1);
    assert_eq!(result["data"]["snapshot"]["source"], "draft");
    for bad in [
        json!({"inputTokens":1,"unknownTokens":1}),
        json!({"toolCalls":1}),
        json!({"inputTokens":-1}),
        json!({"inputTokens":1,"cachedTokens":2}),
        json!({"usageDetails":{"unknown":1}}),
    ] {
        let mut body = preview.clone();
        body["usage"] = bad;
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/business/sale-price-preview",
                body,
                Some(&owner),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut invalid = create.clone();
    invalid["change"]["extra"] = json!(true);
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/sale-books/changes",
            invalid,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let bad = request(
        json!({"operation":"saveDraft","bookId":book,"expectedRevision":1,"models":{"model":{"price":{"inputPricePerMillion":0,"maxOutputTokens":10,"upstream":null}}}}),
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/sale-books/changes",
            bad,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for path in [
        "/api/v1/business/sale-books".to_string(),
        format!("/api/v1/business/sale-books/{book}"),
        format!("/api/v1/business/customers/{org}/sale-terms"),
    ] {
        assert_eq!(
            call(&app, "GET", &path, Value::Null, Some(&customer), None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "GET",
                &path,
                Value::Null,
                Some(&owner),
                Some(&user.api_key.secret)
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(
            &app,
            "POST",
            "/api/v1/business/sale-price-preview",
            preview,
            Some(&customer),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/business/sale-books?limit=0",
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/api/v1/business/sale-books/{}", Uuid::new_v4()),
            Value::Null,
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

/// `effectiveFrom` a few seconds behind the server's clock is the caller's own
/// "now" — the write's intent is immediate activation, and clock skew would
/// make a strict comparison refuse it (issue #1053). A genuinely past start is
/// still a backdated window and refused.
#[tokio::test]
async fn a_sale_window_accepts_an_effective_from_at_the_current_instant() {
    let Some(url) = oxsum_testkit::suite_db_url("sale_effective_now") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "effective-from fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = user.organization.id;
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Invite, None));

    let book = mutate(
        &app,
        &owner,
        json!({"operation":"createBook","name":"effective-now book"}),
    )
    .await["resourceId"]
        .as_str()
        .unwrap()
        .to_owned();
    let sale = json!({"price":{"inputPricePerMillion":1_000_000,"maxOutputTokens":10}});
    mutate(
        &app,
        &owner,
        json!({"operation":"saveDraft","bookId":book,"expectedRevision":0,"models":{"model":sale}}),
    )
    .await;
    mutate(
        &app,
        &owner,
        json!({"operation":"publishDraft","bookId":book,"draftRevision":1}),
    )
    .await;

    // "Now" — a timestamp milliseconds behind the server's own clock — must
    // land as immediate activation.
    let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
    mutate(
        &app,
        &owner,
        json!({"operation":"assignBook","organizationId":org,"bookId":book,"version":1,"effectiveFrom":now}),
    )
    .await;

    // A start genuinely in the past is still refused.
    let past = (OffsetDateTime::now_utc() - Duration::days(1))
        .format(&Rfc3339)
        .unwrap();
    let (status, result) = call(
        &app,
        "POST",
        "/api/v1/business/sale-books/changes",
        request(
            json!({"operation":"overrideModel","organizationId":org,"model":"model","terms":{"price":{"inputPricePerMillion":1_000_000,"maxOutputTokens":10}},"effectiveFrom":past}),
        ),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");

    // A second book with no effectiveFrom at all activates immediately — the
    // documented spelling for "now".
    let book2 = mutate(
        &app,
        &owner,
        json!({"operation":"createBook","name":"effective-now book two"}),
    )
    .await["resourceId"]
        .as_str()
        .unwrap()
        .to_owned();
    mutate(
        &app,
        &owner,
        json!({"operation":"saveDraft","bookId":book2,"expectedRevision":0,"models":{"model":sale}}),
    )
    .await;
    mutate(
        &app,
        &owner,
        json!({"operation":"publishDraft","bookId":book2,"draftRevision":1}),
    )
    .await;
    mutate(
        &app,
        &owner,
        json!({"operation":"scheduleDefault","bookId":book2,"version":1}),
    )
    .await;
}

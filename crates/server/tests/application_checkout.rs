//! Actual loopback checkout and scoped machine HTTP recovery boundaries.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "../../core/tests/support/integration.rs"]
mod setup;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::{Json, Router};
use http_body_util::BodyExt;
use oxsum_core::{Db, IntegrationScope, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
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
async fn credential(db: &Db, app: Uuid, scopes: Vec<IntegrationScope>) -> String {
    let mut request = setup::request(vec![], scopes);
    request.application_id = Some(app);
    db.create_integration_credential(&setup::owner(db).await, &request)
        .await
        .unwrap()
        .secret
        .unwrap()
}
#[tokio::test]
async fn machine_checkout_recovers_unknown_provider_outcomes_without_owner_runtime() {
    let Some(url) = oxsum_testkit::suite_db_url("application_checkout_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let appid = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: "app".into(),
                key: "checkout".into(),
                name: "External app".into(),
                customer_limit: 5,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let token = credential(
        &db,
        appid,
        vec![
            IntegrationScope::CustomersProvision,
            IntegrationScope::CheckoutWrite,
            IntegrationScope::CustomersLifecycleWrite,
            IntegrationScope::CustomerLinkWrite,
        ],
    )
    .await;
    let service = db.authenticate_service(&token).await.unwrap().unwrap();
    let org = db
        .provision_application_customer(
            &service,
            &oxsum_core::ProvisionCustomerRequest {
                idempotency_key: "customer".into(),
                external_id: "alice".into(),
                name: "Alice".into(),
                contact_email: None,
            },
        )
        .await
        .unwrap()
        .customer
        .organization_id;
    db.bootstrap_collection_terms(&oxsum_core::CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let attempts = Arc::new(Mutex::new(Vec::<Value>::new()));
    let state = Arc::clone(&attempts);
    let stub=Router::new().route("/payments",axum::routing::post(move |Json(body):Json<Value>| {let state=Arc::clone(&state);async move {
        let mut attempts=state.lock().unwrap();
        let first = !attempts.iter().any(|prior| prior["payment_id"] == body["payment_id"]);
        attempts.push(body.clone());
        if first {return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":"unknown fixture result"}))).into_response();}
        Json(json!({"payment_id":body["payment_id"],"client_secret":"fixture-secret","status":"requires_payment_method","merchant_id":"fixture-account","amount":body["amount"],"currency":body["currency"],"metadata":body["metadata"]})).into_response()
    }})).route("/payments/{id}",axum::routing::get(||async{(StatusCode::NOT_FOUND,Json(json!({"error":{"code":"HE_02"}})))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let secret = SecretKey::from_bytes([69; 32]);
    let merchant=db.create_merchant(&owner,&oxsum_core::MerchantCreateRequest{idempotency_key:"merchant".into(),reason:"fixture".into(),name:"local-merchant".into(),rail:oxsum_core::MerchantRail::Hyperswitch,mode:oxsum_core::MerchantMode::Test,account_id:"fixture-account".into(),config:serde_json::from_value(json!({"apiUrl":format!("http://{addr}"),"apiKey":"fixture-api","webhookSecret":"fixture-webhook","publishableKey":"pk_fixture","sdkUrl":"https://sdk.example/old.js","destinationPolicy":{"network":"internal","allowedIps":["127.0.0.1"]}})).unwrap()},&secret).await.unwrap();
    db.set_merchant_enabled(
        &owner,
        merchant.merchant_id,
        &oxsum_core::MerchantStatusRequest {
            idempotency_key: "enabled".into(),
            reason: "fixture".into(),
            enabled: true,
        },
    )
    .await
    .unwrap();
    db.set_checkout_route(
        &owner,
        &oxsum_core::CheckoutRouteRequest {
            idempotency_key: "route".into(),
            reason: "fixture".into(),
            rail: oxsum_core::MerchantRail::Hyperswitch,
            expected_revision: 0,
            merchant_id: Some(merchant.merchant_id),
        },
    )
    .await
    .unwrap();
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None)
            .with_secret(SecretKey::from_bytes([69; 32]))
            .with_public_url("https://billing.example"),
    );
    let root = format!("/api/v1/integration/customers/{org}");
    let selection = json!({"purpose":"walletTopup","rail":"hyperswitch","displayAmountMinor":200});
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/checkout-quote"),
            selection.clone(),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let narrow = credential(&db, appid, vec![IntegrationScope::BillingRead]).await;
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/checkout-quote"),
            selection.clone(),
            Some(&narrow)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, quote) = call(
        &app,
        "POST",
        &format!("{root}/checkout-quote"),
        selection.clone(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    let request = json!({"idempotencyKey":"customer-payment","quote":quote["data"],"returnUrl":"https://external.example/checkout/return","cancelUrl":"http://127.0.0.1:8080/cancel"});
    let (status, result) = call(
        &app,
        "POST",
        &format!("{root}/orders"),
        request.clone(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{result}");
    let (status, list) = call(
        &app,
        "GET",
        &format!("{root}/orders"),
        Value::Null,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["data"]["orders"].as_array().unwrap().len(), 1);
    let order = list["data"]["orders"][0]["orderId"].as_str().unwrap();
    assert_eq!(list["data"]["orders"][0]["status"], "created");
    let (status, resumed) = call(
        &app,
        "POST",
        &format!("{root}/orders/{order}/resume"),
        Value::Null,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    assert_eq!(resumed["data"]["order"]["orderId"], order);
    assert_eq!(resumed["data"]["clientSecret"], "fixture-secret");
    assert_eq!(resumed["data"]["order"]["status"], "provider_pending");
    let (status, replay) = call(
        &app,
        "POST",
        &format!("{root}/orders"),
        request.clone(),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["data"]["order"]["orderId"], order);
    {
        let calls = attempts.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["payment_id"], calls[1]["payment_id"]);
        assert_eq!(calls[1]["return_url"], request["returnUrl"]);
    }
    let mut changed = request.clone();
    changed["cancelUrl"] = json!("https://external.example/other");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/orders"),
            changed,
            Some(&token)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{root}/orders/{}", Uuid::new_v4()),
            Value::Null,
            Some(&token)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // A verified human can recover the same application order in the portal.
    // That alternate driver must retain the application's original return URL.
    let mut second = request.clone();
    second["idempotencyKey"] = json!("portal-recovery");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/orders"),
            second,
            Some(&token)
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (_, list) = call(
        &app,
        "GET",
        &format!("{root}/orders"),
        Value::Null,
        Some(&token),
    )
    .await;
    let second_order = list["data"]["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["orderId"] != order)
        .unwrap()["orderId"]
        .as_str()
        .unwrap();
    let person = db
        .register(oxsum_core::NewUser {
            email: "linked-checkout@example.com".into(),
            password: "linked checkout fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.mark_email_verified(person.user.id).await.unwrap();
    let session = db
        .login(&person.user.email, "linked checkout fixture password")
        .await
        .unwrap()
        .into_session()
        .token;
    let link = db
        .create_application_link(
            &service,
            org,
            &oxsum_core::ApplicationLinkRequest {
                idempotency_key: "checkout-link".into(),
                intended_email: person.user.email,
                reason: "Customer requested portal recovery".into(),
            },
        )
        .await
        .unwrap();
    db.accept_application_link(&session, link.token.as_deref().unwrap())
        .await
        .unwrap();
    db.switch_session_context(
        &session,
        &oxsum_core::SessionContextRequest {
            idempotency_key: "checkout-context".into(),
            context: oxsum_core::SessionContext::Customer,
            organization_id: Some(org),
        },
    )
    .await
    .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/orders/{second_order}/checkout-session"))
                .header("cookie", format!("oxsum_session={session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/orders/{second_order}/resume"),
            Value::Null,
            Some(&token)
        )
        .await
        .0,
        StatusCode::OK
    );
    {
        let calls = attempts.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[2]["payment_id"], calls[3]["payment_id"]);
        assert_eq!(calls[3]["return_url"], request["returnUrl"]);
    }
    let rotated = credential(&db, appid, vec![IntegrationScope::CheckoutWrite]).await;
    db.revoke_integration_credential(
        &owner,
        service.credential_id(),
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: "revoke".into(),
            reason: "rotate".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{root}/orders/{order}"),
            Value::Null,
            Some(&token)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{root}/orders/{order}"),
            Value::Null,
            Some(&rotated)
        )
        .await
        .0,
        StatusCode::OK
    );
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: "HTTP assigned plan".into(),
                price_minor: 1_000_001,
                currency: "USD".into(),
                quota_minor: Some(7_000_000),
                period_kind: oxsum_core::PeriodKind::CalendarMonth,
                overage_enabled: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    let plan = db
        .bind_customer_plan(
            org,
            sold.plan_version_id,
            oxsum_core::PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    let (status, quote) = call(
        &app,
        "POST",
        &format!("{root}/checkout-quote"),
        json!({"purpose":"planPeriod","rail":"hyperswitch","customerPlanId":plan.customer_plan_id}),
        Some(&rotated),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!(quote["data"]["purchasedMinor"], 0);
    assert_eq!(quote["data"]["planPayment"]["feeMinor"], 1_000_001);
    assert_eq!(quote["data"]["displayAmountMinor"], 101);
    let plan_request = json!({"idempotencyKey":"plan-period","quote":quote["data"],
        "returnUrl":"https://external.example/plan","cancelUrl":"https://external.example/cancel"});
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/orders"),
            plan_request.clone(),
            Some(&rotated)
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let (status, recovered) = call(
        &app,
        "POST",
        &format!("{root}/orders"),
        plan_request.clone(),
        Some(&rotated),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recovered}");
    assert_eq!(recovered["data"]["order"]["amountMinor"], 1_000_001);
    assert_eq!(recovered["data"]["order"]["purpose"], "planPeriod");
    let plan_order = &recovered["data"]["order"]["orderId"];
    assert!(
        db.plan_periods(plan.customer_plan_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/orders"),
            plan_request,
            Some(&rotated)
        )
        .await
        .1["data"]["order"]["orderId"],
        *plan_order
    );
    {
        let calls = attempts.lock().unwrap();
        assert_eq!(calls.len(), 6);
        assert_eq!(calls[4]["payment_id"], calls[5]["payment_id"]);
        assert_eq!(calls[5]["amount"], 101);
        assert_eq!(calls[5]["return_url"], "https://external.example/plan");
    }
    let lifecycle = credential(&db, appid, vec![IntegrationScope::CustomersLifecycleWrite]).await;
    let lifecycle = db.authenticate_service(&lifecycle).await.unwrap().unwrap();
    db.archive_application_customer(
        &lifecycle,
        org,
        &oxsum_core::ApplicationCustomerArchiveRequest {
            idempotency_key: "archive".into(),
            reason: "archive".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{root}/orders/{order}/resume"),
            Value::Null,
            Some(&rotated)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{root}/orders/{order}"),
            Value::Null,
            Some(&rotated)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(attempts.lock().unwrap().len(), 6);
    task.abort();
}

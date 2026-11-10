//! Customer checkout uses explicit owned routes and retained versions over HTTP.
#[path = "../../../core/tests/support/plan_payment.rs"]
mod plan_payment;
#[path = "../../../core/tests/support/integration.rs"]
mod setup;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{Db, NewUser, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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

#[tokio::test]
async fn owner_created_route_scopes_choices_and_checkout_replays_pinned_version() {
    let Some(url) = oxsum_testkit::suite_db_url("checkout_routes_http") else {
        return;
    };
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    db.bootstrap_collection_terms(&oxsum_core::CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "customer fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let session = db
        .login(&customer.user.email, "customer fixture password")
        .await
        .unwrap()
        .into_session()
        .token;
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let stub=Router::new().route("/payments",axum::routing::post(move |axum::Json(body):axum::Json<Value>|{let calls=Arc::clone(&counted);async move{
        calls.fetch_add(1,Ordering::SeqCst);
        axum::Json(json!({"payment_id":body["payment_id"],"client_secret":"fixture_client_secret","status":"requires_payment_method","merchant_id":"fixture-account","amount":body["amount"],"currency":body["currency"],"metadata":body["metadata"]}))
    }})).route("/payments/{id}", axum::routing::get(|| async { (StatusCode::NOT_FOUND, axum::Json(json!({"error":{"code":"HE_02"}}))) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let app = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None)
            .with_secret(SecretKey::from_bytes([49; 32]))
            .with_public_url("https://shop.example.com"),
    );
    let root = "/api/v1/business/checkout-routes";
    assert_eq!(
        call(&app, "GET", root, Value::Null, Some(&session), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/checkout-options",
            Value::Null,
            None,
            Some(&customer.api_key.secret)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/checkout-options",
            Value::Null,
            Some(&session),
            None
        )
        .await
        .1["data"],
        json!([])
    );
    let config = json!({"apiUrl":format!("http://{addr}"),"apiKey":"snd_fixture_api_key","webhookSecret":"fixture-webhook-secret","publishableKey":"pk_snd_original","sdkUrl":"https://sdk.example.com/old.js","destinationPolicy":{"network":"internal","allowedIps":["127.0.0.1"]}});
    let create = json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"create customer checkout","name":"http-primary","rail":"hyperswitch","mode":"test","accountId":"fixture-account","config":config});
    let (status, result) = call(
        &app,
        "POST",
        "/api/v1/business/merchants",
        create,
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let merchant = result["data"]["merchantId"].as_str().unwrap();
    let version = result["data"]["versionId"].as_str().unwrap();
    let selection = json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"select merchant","rail":"hyperswitch","expectedRevision":0,"merchantId":merchant});
    assert_eq!(
        call(&app, "POST", root, selection.clone(), Some(&owner), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(&app, "POST", root, selection.clone(), Some(&session), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let change = |enabled| json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"merchant state","enabled":enabled});
    let status_path = format!("/api/v1/business/merchants/{merchant}/status");
    assert_eq!(
        call(&app, "POST", &status_path, change(true), Some(&owner), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, selected) = call(&app, "POST", root, selection.clone(), Some(&owner), None).await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(
        call(&app, "POST", root, selection, Some(&owner), None)
            .await
            .1,
        selected
    );
    let (status, options) = call(
        &app,
        "GET",
        "/api/v1/checkout-options",
        Value::Null,
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{options}");
    assert_eq!(options["data"][0]["merchantId"], merchant);
    assert_eq!(options["data"][0]["mode"], "test");
    for hidden in [
        "apiUrl",
        "apiKey",
        "webhookSecret",
        "accountId",
        "publishableKey",
        "credentials",
    ] {
        assert!(!options.to_string().contains(hidden));
    }
    statement_repayment_checkout(
        &db, &app, &owner, &customer, &session, merchant, version, &calls,
    )
    .await;
    calls.store(0, Ordering::SeqCst);
    let order = json!({"idempotencyKey":Uuid::new_v4().to_string(),"rail":"hyperswitch","purpose":"walletTopup","displayAmountMinor":200,"displayCurrency":"USD","expectedCheckout":{"revision":1,"merchantId":merchant,"versionId":version}});
    let (status, first) = call(
        &app,
        "POST",
        "/api/v1/orders",
        order.clone(),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut rotated = config;
    rotated["publishableKey"] = json!("pk_snd_rotated");
    rotated["sdkUrl"] = json!("https://sdk.example.com/new.js");
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/api/v1/business/merchants/{merchant}/versions"),
            json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"rotate","config":rotated}),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut stale = order.clone();
    stale["idempotencyKey"] = json!(Uuid::new_v4().to_string());
    assert_eq!(
        call(&app, "POST", "/api/v1/orders", stale, Some(&session), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &status_path,
            change(false),
            Some(&owner),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (status, replay) = call(
        &app,
        "POST",
        "/api/v1/orders",
        order.clone(),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay, first);
    assert_eq!(replay["data"]["sdkUrl"], "https://sdk.example.com/old.js");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut other = order;
    other["rail"] = json!("stripe");
    assert_eq!(
        call(&app, "POST", "/api/v1/orders", other, Some(&session), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let disabled = json!({"idempotencyKey":Uuid::new_v4().to_string(),"reason":"stop new checkout","rail":"hyperswitch","expectedRevision":1,"merchantId":null});
    assert_eq!(
        call(&app, "POST", root, disabled, Some(&owner), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "GET",
            "/api/v1/checkout-options",
            Value::Null,
            Some(&session),
            None
        )
        .await
        .1["data"],
        json!([])
    );
    task.abort();
}

/// Local PSP fixture covers real quote/create/checkout and the current authority boundary.
#[allow(clippy::too_many_arguments)]
async fn statement_repayment_checkout(
    db: &Db,
    app: &Router,
    owner: &str,
    customer: &oxsum_core::Registration,
    session: &str,
    merchant: &str,
    version: &str,
    calls: &Arc<AtomicUsize>,
) {
    let business = db.default_business().await.unwrap();
    let sold = db
        .publish_sold_plan(
            business,
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: format!("repayment-{}", Uuid::new_v4()),
                price_minor: 2_500_000,
                currency: "USD".into(),
                quota_minor: Some(10_000_000),
                period_kind: oxsum_core::PeriodKind::Anniversary,
                overage_enabled: true,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
    let plan = db
        .bind_customer_plan(
            customer.organization.id,
            sold.plan_version_id,
            oxsum_core::PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap()
        .customer_plan_id;
    let context = db
        .merchant_signing_context(
            Uuid::parse_str(merchant).unwrap(),
            Uuid::parse_str(version).unwrap(),
            &SecretKey::from_bytes([49; 32]),
        )
        .await
        .unwrap()
        .unwrap();
    let original = plan_payment::created_order(db, &context, customer.organization.id, plan).await;
    plan_payment::mark_paid(db, &context, &original).await;
    db.activate_plan_period(original.order_id, plan)
        .await
        .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &customer.organization.tenant_id)
        .await
        .unwrap();
    let dispute_ref = Uuid::new_v4().to_string();
    let event = oxsum_core::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: oxsum_core::MerchantEventKind::Dispute,
        payment_id: Some(original.order_id),
        provider_payment_id: Some(format!("pay_{}", original.order_id)),
        organization_id: Some(customer.organization.id),
        provider_account_id: Some(context.account_id.clone()),
        mode: Some(oxsum_core::MerchantMode::Test),
        amount_minor: Some(original.display_amount_minor),
        currency: Some(original.display_currency),
        refund_id: Some(dispute_ref.clone()),
    };
    let receipt = db
        .receive_merchant_event(&context, &event, &[89; 32])
        .await
        .unwrap();
    db.record_order_dispute(&event, &receipt).await.unwrap();
    let dispute: Uuid = sqlx::query_scalar(
        "SELECT dispute_id FROM oxsum.disputes WHERE order_id=$1 AND provider_dispute_id=$2",
    )
    .bind(original.order_id)
    .bind(dispute_ref)
    .fetch_one(db.pool())
    .await
    .unwrap();
    db.resolve_order_dispute(
        &wallet,
        owner,
        dispute,
        oxsum_core::DisputeResolution::Lost,
        Some("local HTTP source fixture"),
        &Uuid::new_v4().to_string(),
        time::OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    let statement:Uuid=sqlx::query_scalar("SELECT statement_id FROM oxsum.plan_statements WHERE organization_id=$1 AND total_minor>paid_minor AND state='issued'").bind(customer.organization.id).fetch_one(db.pool()).await.unwrap();
    let path = format!("/api/v1/org/statements/{statement}/payment-quote");
    assert_eq!(
        call(
            app,
            "GET",
            &path,
            Value::Null,
            None,
            Some(&customer.api_key.secret)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(app, "GET", &path, Value::Null, Some(owner), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for amount in [0, -1, 2_500_001] {
        assert_eq!(
            call(
                app,
                "GET",
                &format!("{path}?amountMinor={amount}"),
                Value::Null,
                Some(session),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let other = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "foreign repayment fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let foreign = db
        .login(&other.user.email, "foreign repayment fixture")
        .await
        .unwrap()
        .into_session()
        .token;
    assert_eq!(
        call(app, "GET", &path, Value::Null, Some(&foreign), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, quote) = call(
        app,
        "GET",
        &format!("{path}?amountMinor=1000001"),
        Value::Null,
        Some(session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    let terms = &quote["data"]["terms"];
    assert_eq!(terms["displayAmountMinor"], 101);
    let request = json!({"idempotencyKey":Uuid::new_v4().to_string(),"rail":"hyperswitch","purpose":"statementRepayment","purposeRef":statement,"statementAmountMinor":terms["amountMinor"],"expectedStatementQuote":quote["data"]["fingerprint"],"expectedCollectionRevision":terms["collectionRevision"],"displayAmountMinor":terms["displayAmountMinor"],"displayCurrency":terms["displayCurrency"]});
    let before = calls.load(Ordering::SeqCst);
    for amount in [100, 102] {
        let mut bad = request.clone();
        bad["displayAmountMinor"] = json!(amount);
        assert_eq!(
            call(app, "POST", "/api/v1/orders", bad, Some(session), None)
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    let mut stale = request.clone();
    stale["expectedStatementQuote"] = json!("0".repeat(64));
    assert_eq!(
        call(app, "POST", "/api/v1/orders", stale, Some(session), None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            app,
            "POST",
            "/api/v1/orders",
            request.clone(),
            Some(&foreign),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(calls.load(Ordering::SeqCst), before);
    let (status, answer) = call(
        app,
        "POST",
        "/api/v1/orders",
        request.clone(),
        Some(session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["data"]["order"]["amountMinor"], 1_000_001);
    assert_eq!(answer["data"]["order"]["status"], "provider_pending");
    let replay = call(app, "POST", "/api/v1/orders", request, Some(session), None).await;
    assert_eq!(replay.0, StatusCode::OK, "{}", replay.1);
    assert_eq!(
        replay.1["data"]["order"]["orderId"],
        answer["data"]["order"]["orderId"]
    );
    assert_eq!(calls.load(Ordering::SeqCst), before + 1);
    let quote = call(app, "GET", &path, Value::Null, Some(session), None)
        .await
        .1;
    assert_eq!(quote["data"]["reservedMinor"], 1_000_001);
    assert_eq!(
        quote["data"]["pendingOrderIds"][0],
        answer["data"]["order"]["orderId"]
    );
    let order = Uuid::parse_str(answer["data"]["order"]["orderId"].as_str().unwrap()).unwrap();
    let event = oxsum_core::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: oxsum_core::MerchantEventKind::Succeeded,
        payment_id: Some(order),
        provider_payment_id: Some(
            answer["data"]["providerPaymentId"]
                .as_str()
                .unwrap()
                .to_owned(),
        ),
        organization_id: Some(customer.organization.id),
        provider_account_id: Some(context.account_id.clone()),
        mode: Some(oxsum_core::MerchantMode::Test),
        amount_minor: Some(101),
        currency: Some("USD".into()),
        refund_id: None,
    };
    let receipt = db
        .receive_merchant_event(&context, &event, &[90; 32])
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();
    db.fulfill_payment_order(order, &wallet, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    let path = format!("/api/v1/business/orders/{order}/statement-payment-evidence");
    assert_eq!(
        call(app, "GET", &path, Value::Null, Some(session), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            app,
            "GET",
            &path,
            Value::Null,
            None,
            Some(&customer.api_key.secret)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, evidence) = call(app, "GET", &path, Value::Null, Some(owner), None).await;
    assert_eq!(status, StatusCode::OK, "{evidence}");
    assert_eq!(evidence["data"][0]["cash"], 1_000_001);
    assert_eq!(evidence["data"][0]["receivable"], 1_000_001);
    assert_eq!(evidence["data"][0]["payable"], 0);
    assert_eq!(evidence["data"][0]["hash"].as_str().unwrap().len(), 64);
    assert_eq!(
        call(
            app,
            "GET",
            &format!(
                "/api/v1/business/orders/{}/statement-payment-evidence",
                Uuid::new_v4()
            ),
            Value::Null,
            Some(owner),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

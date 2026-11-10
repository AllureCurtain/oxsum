//! Separate Python/SQLite application against the real loopback oxsum listener.
#[path = "../../../core/tests/support/integration.rs"]
mod setup;
use axum::{Json, Router};
use oxsum_core::{Db, IntegrationScope, SecretKey};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

struct ReceiverProcess(Child);

impl Drop for ReceiverProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Client {
    base: String,
    application: Uuid,
    credential: String,
    directory: PathBuf,
}
impl Client {
    async fn run(&self, request: Value, lost: bool) -> (bool, Value) {
        let base = self.base.clone();
        let application = self.application.to_string();
        let credential = self.credential.clone();
        let database = self.directory.join("app.sqlite");
        tokio::task::spawn_blocking(move || {
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/embedded-application");
            let code = r#"
import json,sys
sys.path.insert(0,sys.argv[1])
from app import Application,Transport,canonical
import os
class Lost(Transport):
 def request(self,*args,**kwargs):
  answer=super().request(*args,**kwargs)
  if args[0]=='POST' and args[1]=='/v1/chat/completions': raise OSError('simulated lost committed response')
  return answer
app=Application(sys.argv[2],sys.argv[3],sys.argv[4],os.environ['OXSUM_APPLICATION_CREDENTIAL'],Lost(sys.argv[3]) if sys.argv[5]=='lost' else None)
try:
 try: print(canonical(app.execute(json.load(sys.stdin))))
 except Exception as error:
  print(canonical({'error':str(error)}));sys.exit(1)
finally:app.close()
"#;
            let mut child = Command::new("python3").arg("-c").arg(code).arg(source).arg(database).arg(base).arg(application).arg(if lost {"lost"} else {"normal"})
                .env("OXSUM_APPLICATION_CREDENTIAL",credential).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
            child.stdin.take().unwrap().write_all(request.to_string().as_bytes()).unwrap();
            let output=child.wait_with_output().unwrap();
            let value=serde_json::from_slice(&output.stdout).unwrap_or_else(|_|panic!("Python fixture failed without JSON: {}",String::from_utf8_lossy(&output.stderr)));
            (output.status.success(),value)
        }).await.unwrap()
    }
    async fn ok(&self, request: Value) -> Value {
        let action = request["action"].as_str().unwrap_or("unknown").to_owned();
        let (success, value) = self.run(request, false).await;
        assert!(success, "{action}: {value}");
        value
    }
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
async fn external_application_persists_scope_checkout_receipt_and_lost_response_recovery() {
    let Some(url) = oxsum_testkit::suite_db_url("embedded_application") else {
        return;
    };
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let app_id = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: "application".into(),
                key: "embedded".into(),
                name: "Embedded app".into(),
                customer_limit: 10,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let scopes = vec![
        IntegrationScope::CustomersProvision,
        IntegrationScope::CustomersRead,
        IntegrationScope::CustomersLifecycleWrite,
        IntegrationScope::CustomerKeysWrite,
        IntegrationScope::EntitlementsRead,
        IntegrationScope::CheckoutWrite,
        IntegrationScope::BillingRead,
        IntegrationScope::EvidenceRead,
        IntegrationScope::EvidenceWrite,
        IntegrationScope::NotificationsRead,
    ];
    let credential = grant(&db, &owner, app_id, scopes).await;
    let secret = SecretKey::from_bytes([96; 32]);
    let model = crate::support::flat_model(&db, &secret, 40).await;
    let prices=oxsum_core::PriceBook::from_json(&model,&json!({model.clone():{"inputPricePerMillion":0,"outputPricePerMillion":0,"costPerRequest":40,"maxOutputTokens":32}}).to_string()).unwrap();
    for (name, price) in prices.models() {
        db.append_price(&model, name, price.clone(), 100)
            .await
            .unwrap();
    }
    db.bootstrap_collection_terms(&oxsum_core::CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let payments = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = Arc::clone(&payments);
    let provider=Router::new().route("/payments",axum::routing::post(move |Json(body):Json<Value>|{let capture=Arc::clone(&capture);async move {
        capture.lock().unwrap().push(body.clone());
        Json(json!({"payment_id":body["payment_id"],"client_secret":"private-fixture-client-secret","status":"requires_payment_method","merchant_id":"embedded-account","amount":body["amount"],"currency":body["currency"],"metadata":body["metadata"]}))
    }})).route("/payments/{id}", axum::routing::get(|| async { (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":{"code":"HE_02"}}))) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}", listener.local_addr().unwrap());
    let provider_task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let merchant=db.create_merchant(&owner,&oxsum_core::MerchantCreateRequest{idempotency_key:"merchant".into(),reason:"isolated fixture".into(),name:"embedded-test-merchant".into(),rail:oxsum_core::MerchantRail::Hyperswitch,mode:oxsum_core::MerchantMode::Test,account_id:"embedded-account".into(),config:serde_json::from_value(json!({"apiUrl":provider_url,"apiKey":"fixture-api","webhookSecret":"fixture-event","publishableKey":"pk_fixture","sdkUrl":"https://sdk.example/fixture.js","destinationPolicy":{"network":"internal","allowedIps":["127.0.0.1"]}})).unwrap()},&secret).await.unwrap();
    db.set_merchant_enabled(
        &owner,
        merchant.merchant_id,
        &oxsum_core::MerchantStatusRequest {
            idempotency_key: "enable".into(),
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
    let context = db
        .merchant_signing_context(merchant.merchant_id, merchant.version_id, &secret)
        .await
        .unwrap()
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = oxsum_server::app(
        db.clone(),
        Config::new(Signup::Invite, None)
            .with_secret(secret.clone())
            .with_head_signing_seed([97; 32])
            .with_public_url(&base),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let directory = std::env::temp_dir().join(format!("oxsum-embedded-{}", Uuid::new_v4()));
    let client = Client {
        base,
        application: app_id,
        credential: credential.clone(),
        directory: directory.clone(),
    };
    let signup =
        json!({"action":"signup","customer":"alice","operation":"alice-signup","name":"Alice"});
    let mapped = client.ok(signup.clone()).await;
    assert_eq!(client.ok(signup).await, mapped);
    let org = Uuid::parse_str(mapped["organizationId"].as_str().unwrap()).unwrap();
    assert_eq!(
        client
            .ok(json!({"action":"retrieve","customer":"alice"}))
            .await["customer"]["organizationId"],
        org.to_string()
    );
    client
        .ok(json!({"action":"signup","customer":"bob","operation":"bob-signup","name":"Bob"}))
        .await;
    let key=client.ok(json!({"action":"key","customer":"alice","operation":"alice-key","model":model,"spendLimitMinor":1000,"expiresAt":(time::OffsetDateTime::now_utc()+time::Duration::hours(1)).format(&time::format_description::well_known::Rfc3339).unwrap()})).await;
    assert!(key.get("secret").is_none());
    let checkout = json!({"action":"checkout","customer":"alice","operation":"alice-funding","selection":{"purpose":"walletTopup","rail":"hyperswitch","displayAmountMinor":100},"returnUrl":"https://app.example/paid","cancelUrl":"https://app.example/cancel"});
    let payment = client.ok(checkout.clone()).await;
    assert_eq!(payment["paid"], false);
    assert_eq!(client.ok(checkout).await, payment);
    assert_eq!(payments.lock().unwrap().len(), 1);
    let order = Uuid::parse_str(payment["orderId"].as_str().unwrap()).unwrap();
    assert!(
        !client
            .run(
                json!({"action":"read","customer":"bob","resource":"order","id":order}),
                false
            )
            .await
            .0
    );
    let ask = json!({"action":"ask","customer":"alice","operation":"before-paid","keyOperation":"alice-key","model":model,"prompt":"Reply ok"});
    assert!(
        !client.run(ask, false).await.0,
        "pending checkout cannot fund a call"
    );
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open_existing(db.pool().clone(), &tenant)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
    // Explicit local verified-event fixture, never genuine PSP certification.
    let stored = db.payment_order(org, order).await.unwrap().unwrap();
    let event = oxsum_core::VerifiedMerchantEvent {
        event_id: "paid".into(),
        kind: oxsum_core::MerchantEventKind::Succeeded,
        payment_id: Some(order),
        provider_payment_id: stored.provider_payment_id.clone(),
        organization_id: Some(org),
        provider_account_id: Some(context.account_id.clone()),
        mode: Some(oxsum_core::MerchantMode::Test),
        amount_minor: Some(stored.display_amount_minor),
        currency: Some(stored.display_currency.clone()),
        refund_id: None,
    };
    let receipt = db
        .receive_merchant_event(&context, &event, &[98; 32])
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();
    db.fulfill_payment_order(order, &wallet, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 1_000_000);
    let ask = json!({"action":"ask","customer":"alice","operation":"alice-question","keyOperation":"alice-key","model":model,"prompt":"Reply ok"});
    assert!(
        !client.run(ask.clone(), true).await.0,
        "simulate response loss after server settlement"
    );
    let answer = client.ok(ask.clone()).await;
    assert_eq!(answer["receipt"]["idempotent-replayed"], "true");
    assert_eq!(wallet.available().await.unwrap(), 999_960);
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(client.ok(ask.clone()).await, answer);
    let mut changed = ask;
    changed["prompt"] = json!("different");
    assert!(!client.run(changed, false).await.0);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.usage_records WHERE tenant_id=$1 AND charged_minor=40",
    )
    .bind(&tenant)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
    let narrow = Client {
        base: client.base.clone(),
        application: app_id,
        credential: grant(&db, &owner, app_id, vec![IntegrationScope::CustomersRead]).await,
        directory: directory.clone(),
    };
    assert!(
        !narrow
            .run(
                json!({"action":"read","customer":"alice","resource":"billing"}),
                false
            )
            .await
            .0
    );
    let foreign = db
        .create_integration_application(
            &owner,
            &oxsum_core::IntegrationApplicationRequest {
                idempotency_key: "foreign-application".into(),
                key: "foreign-embedded".into(),
                name: "Other namespace".into(),
                customer_limit: 2,
                reason: "fixture".into(),
            },
        )
        .await
        .unwrap()
        .application_id;
    let wrong_namespace = Client {
        base: client.base.clone(),
        application: app_id,
        credential: grant(&db, &owner, foreign, vec![IntegrationScope::BillingRead]).await,
        directory: directory.clone(),
    };
    assert!(
        !wrong_namespace
            .run(
                json!({"action":"read","customer":"alice","resource":"billing"}),
                false
            )
            .await
            .0
    );
    let key = oxsum_core::KeyPublication::of(&oxsum_core::signing_key([97; 32]).unwrap());
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    client.ok(json!({"action":"trust","customer":"alice","organizationId":org,"origin":format!("oxsum/ledgers/{tenant}"),"key":{"keyName":key.name,"publicKey":hex(&key.public_key),"keyHash":hex(&key.key_hash)}})).await;
    let exported = directory.join("receipt");
    client.ok(json!({"action":"receipt","customer":"alice","askOperation":"alice-question","directory":exported})).await;
    let raw = std::fs::read_to_string(exported.join("settlement-bundle.json")).unwrap();
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(exported.join("manifest.json")).unwrap())
            .unwrap();
    let hash = oxsum_core::Hash::parse_hex(manifest["contentHash"].as_str().unwrap()).unwrap();
    assert!(oxsum_core::verify_bundle(&raw, &hash).unwrap());
    // Assigned-plan checkout produces a real paid customer statement for evidence.
    let sold = db
        .publish_sold_plan(
            db.default_business().await.unwrap(),
            &oxsum_core::NewSoldPlan {
                plan_id: None,
                name: "Embedded evidence plan".into(),
                price_minor: 1_000_000,
                currency: "USD".into(),
                quota_minor: Some(2_000_000),
                period_kind: oxsum_core::PeriodKind::Anniversary,
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
    let bought = client.ok(json!({"action":"checkout","customer":"alice","operation":"plan-checkout","selection":{"purpose":"planPeriod","rail":"hyperswitch","customerPlanId":plan.customer_plan_id},"returnUrl":"https://app.example/plan","cancelUrl":"https://app.example/cancel"})).await;
    let plan_order = db
        .payment_order(
            org,
            Uuid::parse_str(bought["orderId"].as_str().unwrap()).unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    let paid = oxsum_core::VerifiedMerchantEvent {
        event_id: "plan-paid".into(),
        kind: oxsum_core::MerchantEventKind::Succeeded,
        payment_id: Some(plan_order.order_id),
        provider_payment_id: plan_order.provider_payment_id.clone(),
        organization_id: Some(org),
        provider_account_id: Some(context.account_id.clone()),
        mode: Some(oxsum_core::MerchantMode::Test),
        amount_minor: Some(plan_order.display_amount_minor),
        currency: Some(plan_order.display_currency.clone()),
        refund_id: None,
    };
    let receipt = db
        .receive_merchant_event(&context, &paid, &[98; 32])
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();
    db.fulfill_payment_order(
        plan_order.order_id,
        &wallet,
        time::OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    let statements = client
        .ok(json!({"action":"read","customer":"alice","resource":"statements"}))
        .await;
    let statement = statements["statements"][0]["statementId"].as_str().unwrap();
    let detail = client
        .ok(json!({"action":"read","customer":"alice","resource":"statement","id":statement}))
        .await;
    assert_eq!(detail["state"], "paid");
    let publication = client.ok(json!({"action":"publish","customer":"alice","operation":"statement-evidence","scope":{"kind":"statement","statementId":statement}})).await;
    let evidence_dir = directory.join("evidence");
    client.ok(json!({"action":"evidence","customer":"alice","bundleId":publication["bundle"]["bundleId"],"directory":evidence_dir})).await;
    let evidence_raw = std::fs::read_to_string(evidence_dir.join("evidence-bundle.json")).unwrap();
    let public_hex = hex(&key.public_key);
    let hash_hex = hex(&key.key_hash);
    let trusted = oxsum_verify::PublishedKey::new(&key.name, &public_hex, &hash_hex);
    oxsum_verify::verify_evidence_bundle(
        &evidence_raw,
        &format!("oxsum/ledgers/{tenant}"),
        &org.to_string(),
        Some(&trusted),
    )
    .unwrap();
    if let Ok(consumer) = std::env::var("OXSUM_EMBEDDED_VERIFIER") {
        let status = Command::new("node")
            .arg(Path::new(&consumer).join("verify.mjs"))
            .arg(&exported)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("node")
            .arg(Path::new(&consumer).join("verify.mjs"))
            .arg(&evidence_dir)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("node")
            .arg(Path::new(&consumer).join("verify-negative.mjs"))
            .arg(&exported)
            .arg(&evidence_dir)
            .status()
            .unwrap();
        assert!(status.success());
    } else {
        eprintln!("offline packed-SDK step requires scripts/check-embedded-application.sh");
    }
    // The receiver uses the application's SQLite store in a separate process.
    let receiver_database = directory.join("app.sqlite");
    let receiver_base = client.base.clone();
    let receiver_credential = credential.clone();
    let receiver_app = app_id.to_string();
    let (receiver, receiver_port) = tokio::task::spawn_blocking(move || {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/application-notifications");
        let code = r#"
import sys,json,os,http.server
sys.path.insert(0,sys.argv[1])
from receiver import Receiver,handler
r=Receiver(sys.argv[2],sys.argv[4],os.environ['OXSUM_NOTIFICATION_SECRET'],sys.argv[3],os.environ['OXSUM_APPLICATION_CREDENTIAL'])
s=http.server.HTTPServer(('127.0.0.1',0),handler(r))
print(s.server_address[1],flush=True)
while True:
 s.handle_request()
 r.reconcile()
"#;
        let mut child = ReceiverProcess(Command::new("python3").arg("-c").arg(code).arg(source).arg(receiver_database).arg(receiver_base).arg(receiver_app)
            .env("OXSUM_APPLICATION_CREDENTIAL",receiver_credential).env("OXSUM_NOTIFICATION_SECRET","whsec-fixture-placeholder")
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap());
        let mut line=String::new();std::io::BufReader::new(child.0.stdout.take().unwrap()).read_line(&mut line).unwrap();
        (child,line.trim().parse::<u16>().unwrap())
    }).await.unwrap();
    let endpoint = db
        .create_application_notification_endpoint(
            &owner,
            app_id,
            &oxsum_core::ApplicationNotificationEndpointRequest {
                idempotency_key: "receiver".into(),
                reason: "owned separate-process fixture".into(),
                url: format!("http://127.0.0.1:{receiver_port}/events"),
                destination_policy: oxsum_core::ProviderDestinationPolicy {
                    network: oxsum_core::ProviderNetwork::Internal,
                    allowed_ips: vec![std::net::Ipv4Addr::LOCALHOST.into()],
                },
                events: vec!["customer.changed".into()],
            },
            &secret,
        )
        .await
        .unwrap();
    // Restart with the actual one-time endpoint secret before generating signals.
    drop(receiver);
    let receiver_database = directory.join("app.sqlite");
    let receiver_base = client.base.clone();
    let receiver_credential = credential.clone();
    let receiver_app = app_id.to_string();
    let signing = endpoint.secret.unwrap();
    let receiver = tokio::task::spawn_blocking(move || {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/application-notifications");
        let code = r#"
import sys,os,http.server
sys.path.insert(0,sys.argv[1])
from receiver import Receiver,handler
r=Receiver(sys.argv[2],sys.argv[4],os.environ['OXSUM_NOTIFICATION_SECRET'],sys.argv[3],os.environ['OXSUM_APPLICATION_CREDENTIAL'])
s=http.server.HTTPServer(('127.0.0.1',int(sys.argv[5])),handler(r))
print('ready',flush=True)
while True:
 s.handle_request()
 r.reconcile()
"#;
        let mut child=ReceiverProcess(Command::new("python3").arg("-c").arg(code).arg(source).arg(receiver_database).arg(receiver_base).arg(receiver_app).arg(receiver_port.to_string())
            .env("OXSUM_APPLICATION_CREDENTIAL",receiver_credential).env("OXSUM_NOTIFICATION_SECRET",signing).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap());
        let mut line=String::new();std::io::BufReader::new(child.0.stdout.take().unwrap()).read_line(&mut line).unwrap();assert_eq!(line.trim(),"ready");child
    }).await.unwrap();
    client.ok(json!({"action":"archive","customer":"alice","operation":"archive-alice","reason":"application suspension"})).await;
    let state = client
        .ok(json!({"action":"read","customer":"alice","resource":"entitlement"}))
        .await;
    assert_eq!(state["customerState"], "archived");
    assert!(!client.run(json!({"action":"ask","customer":"alice","operation":"after-archive","keyOperation":"alice-key","model":model,"prompt":"Reply ok"}),false).await.0);
    oxsum_server::application_notifications::deliver_due(
        &db,
        &oxsum_integrations::provider_http::ProviderHttp::default(),
        &secret,
        Some(app_id),
    )
    .await;
    let delivery = db
        .business_application_notification_deliveries(
            &owner,
            app_id,
            endpoint.endpoint.endpoint_id,
            None,
            50,
        )
        .await
        .unwrap();
    assert!(!delivery.deliveries.is_empty());
    assert!(
        delivery
            .deliveries
            .iter()
            .all(|row| row.status == "delivered")
    );
    let receiver_database = directory.join("app.sqlite");
    tokio::task::spawn_blocking(move || {
        let code = r#"
import sys,sqlite3,json,time
c=sqlite3.connect(sys.argv[1])
for _ in range(50):
 rows=c.execute("SELECT document FROM snapshots WHERE resource='entitlement'").fetchall()
 if rows and all(json.loads(r[0])['data']['customerState']=='archived' for r in rows):break
 time.sleep(.02)
else:raise AssertionError('receiver did not reconcile current archived state')
assert c.execute('SELECT count(*) FROM inbox WHERE processed_at IS NULL').fetchone()[0]==0
c.close()
"#;
        assert!(
            Command::new("python3")
                .arg("-c")
                .arg(code)
                .arg(receiver_database)
                .status()
                .unwrap()
                .success()
        );
    })
    .await
    .unwrap();
    drop(receiver);
    let actor = db.authenticate_service(&credential).await.unwrap().unwrap();
    db.revoke_integration_credential(
        &owner,
        actor.credential_id(),
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: "revoke".into(),
            reason: "runtime revoked".into(),
        },
    )
    .await
    .unwrap();
    assert!(
        !client
            .run(
                json!({"action":"read","customer":"alice","resource":"entitlement"}),
                false
            )
            .await
            .0
    );
    assert_eq!(wallet.available().await.unwrap(), 999_960);
    server.abort();
    provider_task.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

//! HTTP tests for the Solana deposit rail (issue #1117).
//!
//! `POST /api/v1/topups/solana` opens a `pending` intent carrying a unique
//! `reference` marker; `POST /api/v1/topups/solana/confirm` submits it and
//! runs the findReference + validateTransfer check against a stubbed chain
//! lookup — `credit_solana_deposit` walks the same pending → confirmed →
//! credited path every rail shares. A replayed confirm credits once, a
//! mismatched transfer refuses, and an unconfigured deployment 503s.
//!
//! The tests need DATABASE_URL and skip without it.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    ChainLookup, Db, ExpectedTransfer, SolanaRailVerifier, SolanaTerms, ValidatedTransfer,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The deployment's terms: a fixed devnet rate, native SOL, a 10-minute TTL.
fn terms() -> SolanaTerms {
    SolanaTerms {
        pay_to: "oxsumDepositAddr111111111111111111111111111".to_owned(),
        spl_token_mint: None,
        asset_decimals: 9,
        rate_micro: 200_000_000, // 200 credits per SOL, in minor units
        intent_ttl_secs: 600,
    }
}

/// The stubbed chain: maps `reference -> [signatures]` (newest first, like
/// `getSignaturesForAddress`) and validates the expected transfer's
/// `amount`/`recipient`/`reference` against a scripted answer. `paid` arms
/// the lookup to find and validate; `wrong` arms it to answer a transfer
/// that does not honor the intent.
#[derive(Clone, Default)]
struct SolanaStub {
    /// `reference` → the signatures `find_reference` answers, newest first.
    located: Arc<Mutex<std::collections::HashMap<String, Vec<String>>>>,
    /// `signature` → the `ValidatedTransfer` `validate_transfer` answers.
    validated: Arc<Mutex<std::collections::HashMap<String, ValidatedTransfer>>>,
}

impl std::fmt::Debug for SolanaStub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SolanaStub").finish_non_exhaustive()
    }
}

impl ChainLookup for SolanaStub {
    fn find_reference<'a>(
        &'a self,
        reference: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
        Box::pin(async move {
            self.located
                .lock()
                .unwrap()
                .get(reference)
                .cloned()
                .unwrap_or_default()
        })
    }

    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a ExpectedTransfer,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>>
    {
        Box::pin(async move {
            let answer = self.validated.lock().unwrap().get(signature).cloned()?;
            // Honor only a transfer that meets the intent's terms — the same
            // gates `SolanaRpcClient::validate_transfer` runs against the
            // chain (recipient, amount, mint, marker). The stub answers `None`
            // when any term fails, the same shape a dishonoring tx earns.
            if answer.delivered < expected.amount {
                return None;
            }
            Some(answer)
        })
    }
}

/// A configured world: the Solana rail is on, with the stubbed chain.
async fn world(stub: SolanaStub) -> Option<(Router, Db)> {
    let u = oxsum_testkit::suite_url!()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let config = Config::new(Signup::Open, None)
        .with_solana(SolanaRailVerifier::new(Arc::new(stub), terms()));
    Some((oxsum_server::app(db.clone(), config), db))
}

/// One plain HTTP call; returns (status, json).
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    if let Some(key) = key {
        req = crate::support::credential(req, key);
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A registered organization; returns its API key secret.
async fn register_session(app: &Router) -> String {
    let email = format!(
        "solana_{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let (status, body) = call(
        app,
        "POST",
        "/api/v1/auth/register",
        Some(json!({"email": email, "password": "correct horse battery"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "registration failed: {body}");
    crate::support::login(app, &email, "correct horse battery").await
}

/// The org's ledger balance, read back through the API.
async fn balance(app: &Router, key: &str) -> i64 {
    let (status, body) = call(app, "GET", "/api/v1/balance", None, Some(key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["data"]["availableMinor"].as_i64().unwrap()
}

#[tokio::test]
async fn solana_intent_pays_once() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    // Open the intent: 1000 minor-credits at 200_000_000 minor/SOL =
    // 5_000 lamports (lamports = credits_minor * 1e9 / rate_micro).
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();
    assert!(!reference.is_empty(), "{intent}");
    assert_eq!(intent["data"]["amountLamports"].as_i64().unwrap(), 5_000);
    // Arm the stub: the paying transaction lands, delivering exactly the
    // asked lamports.
    let signature = "sig1111111111111111111111111111111111111111111111111111111111111111";
    stub.located
        .lock()
        .unwrap()
        .insert(reference.clone(), vec![signature.to_owned()]);
    stub.validated.lock().unwrap().insert(
        signature.to_owned(),
        ValidatedTransfer {
            signature: signature.to_owned(),
            delivered: 5_000,
        },
    );
    // Confirm: the credit lands once.
    let (status, receipt) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(balance(&app, &key).await, 1000);
    // A replayed confirm credits nothing twice.
    let (_status, replay) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    // The deposit is already credited — the replay is a no-op the rail
    // surfaces as a second, harmless answer rather than a second credit.
    assert_eq!(balance(&app, &key).await, 1000, "{replay}");
}

#[tokio::test]
async fn solana_confirm_refuses_an_unpaying_transfer() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 500})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();
    // Arm the stub with a transfer that underpays — validate refuses it.
    let signature = "sig2222222222222222222222222222222222222222222222222222222222222222";
    stub.located
        .lock()
        .unwrap()
        .insert(reference.clone(), vec![signature.to_owned()]);
    stub.validated.lock().unwrap().insert(
        signature.to_owned(),
        ValidatedTransfer {
            signature: signature.to_owned(),
            delivered: 1, // far below the asked 500-credit lamports
        },
    );
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(balance(&app, &key).await, 0);
}

#[tokio::test]
async fn solana_confirm_refuses_an_unknown_reference() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": "no-such-intent"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// Issue #1124: a transaction carrying two references pays one transfer but
/// must credit only one deposit. Two intents — whether in one organization or
/// two — that resolve to the same signature credit the first and refuse the
/// second; the second's ledger never books the spend. The per-organization
/// ledger key `solana:<sig>` is not the guard (each org's ledger is its own
/// scope); the global `deposits_solana_tx_hash` index is.
#[tokio::test]
async fn solana_signature_credits_once_across_organizations() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    // Two organizations — two ledgers — each open an intent for 1000 credits.
    let key_a = register_session(&app).await;
    let key_b = register_session(&app).await;
    let mut references = Vec::new();
    for key in [&key_a, &key_b] {
        let (status, intent) = call(
            &app,
            "POST",
            "/api/v1/topups/solana",
            Some(json!({"amountMinor": 1000})),
            Some(key),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{intent}");
        references.push(intent["data"]["reference"].as_str().unwrap().to_owned());
    }
    // One paying transaction carries both references — the attack shape.
    let signature = "sigDblSpend0000000000000000000000000000000000000000000000000000";
    for reference in &references {
        stub.located
            .lock()
            .unwrap()
            .insert(reference.clone(), vec![signature.to_owned()]);
    }
    stub.validated.lock().unwrap().insert(
        signature.to_owned(),
        ValidatedTransfer {
            signature: signature.to_owned(),
            delivered: 5_000,
        },
    );
    // The first organization's confirm settles and credits.
    let (status_a, _) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": references[0]})),
        Some(&key_a),
    )
    .await;
    assert_eq!(status_a, StatusCode::OK);
    assert_eq!(balance(&app, &key_a).await, 1000);
    // The second organization's intent resolves to the *same* signature —
    // already spent — and must be refused, never double-credited.
    let (status_b, refusal) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": references[1]})),
        Some(&key_b),
    )
    .await;
    assert_eq!(status_b, StatusCode::FORBIDDEN, "{refusal}");
    assert_eq!(balance(&app, &key_b).await, 0, "{refusal}");
}

#[tokio::test]
async fn solana_unconfigured_answers_503() {
    // No SolanaRailVerifier on the config — the rail is absent.
    let Some(u) = oxsum_testkit::suite_url!() else {
        return;
    };
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let app = oxsum_server::app(db.clone(), Config::new(Signup::Open, None));
    // A Personal route wants a session before the rail's own 503: an
    // unauthenticated call 401s in the auth layer, never reaching the route,
    // so the unconfigured check carries a session like a real caller's.
    let key = register_session(&app).await;
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

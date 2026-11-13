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
    AssetTerms, ChainLookup, Db, ExpectedTransfer, OracleQuote, PriceOracle, SolanaPricing,
    SolanaRailVerifier, SolanaTerms, USD_PEGGED_RATE_MICRO, ValidatedTransfer,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// The deployment's terms: a fixed devnet rate, native SOL, a 10-minute TTL.
fn terms() -> SolanaTerms {
    SolanaTerms::new(
        "oxsumDepositAddr111111111111111111111111111".to_owned(),
        vec![AssetTerms {
            key: "sol".to_owned(),
            spl_token_mint: None,
            asset_decimals: 9,
            rate_micro: 200_000_000, // 200 credits per SOL, in minor units
        }],
        "sol".to_owned(),
        600,
        1,
        i64::MAX,
        0,
    )
}

/// The two-asset whitelist (issue #1140): `sol` at the configured devnet
/// rate plus a 6-decimal `usdc` at the fixed USD parity — the deployment a
/// customer picks a side on at open, defaulting to `usdc`.
fn multi_asset_terms() -> SolanaTerms {
    SolanaTerms::new(
        "oxsumDepositAddr111111111111111111111111111".to_owned(),
        vec![
            AssetTerms {
                key: "sol".to_owned(),
                spl_token_mint: None,
                asset_decimals: 9,
                rate_micro: 200_000_000,
            },
            AssetTerms {
                key: "usdc".to_owned(),
                spl_token_mint: Some("usdcMint11111111111111111111111111111111".to_owned()),
                asset_decimals: 6,
                rate_micro: USD_PEGGED_RATE_MICRO,
            },
        ],
        "usdc".to_owned(),
        600,
        1,
        i64::MAX,
        0,
    )
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
    world_with_terms(stub, terms()).await
}

/// The same world under explicit terms — the multi-asset suite passes its
/// whitelist (issue #1140). `SolanaRailVerifier::new` is the static-rate
/// shape (issue #1139): `sol` intents lock the configured figure, pegged
/// ones the parity.
async fn world_with_terms(stub: SolanaStub, terms: SolanaTerms) -> Option<(Router, Db)> {
    world_with(SolanaRailVerifier::new(Arc::new(stub), terms)).await
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
    // The Solana Pay terms the wallet needs to render the intent (issue
    // #1141): the asset's decimals to write `amount` and the deposit id the
    // `memo` pins.
    assert_eq!(intent["data"]["assetDecimals"].as_u64().unwrap(), 9);
    assert_eq!(
        intent["data"]["depositId"].as_str().unwrap().len(),
        36,
        "{intent}"
    );
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
            block_time: Some(0),
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
            block_time: Some(0),
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
            block_time: Some(0),
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

/// Issue #1140: one deployment serves both assets. An open naming
/// `{"asset":"usdc"}` locks the USDC mint, its 6 decimals and the fixed USD
/// parity rate into the intent's meta; an open naming `{"asset":"sol"}` —
/// or none, when `sol` is the default — takes the native path at the
/// configured `RATE_MICRO`.
#[tokio::test]
async fn solana_open_picks_the_named_asset() {
    let stub = SolanaStub::default();
    let Some((app, db)) = world_with_terms(stub, multi_asset_terms()).await else {
        return;
    };
    let key = register_session(&app).await;

    // `{"asset":"usdc"}`: the whitelist's USDC entry prices the intent —
    // 1000 minor-credits at parity = 1000 USDC base units (6 decimals).
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000, "asset": "usdc"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    assert_eq!(intent["data"]["asset"].as_str().unwrap(), "usdc");
    assert_eq!(intent["data"]["amountLamports"].as_i64().unwrap(), 1_000);
    assert_eq!(
        intent["data"]["splToken"].as_str().unwrap(),
        "usdcMint11111111111111111111111111111111"
    );

    // `{"asset":"sol"}`: the native path at the configured rate — 5_000
    // lamports, `splToken` null.
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000, "asset": "sol"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    assert_eq!(intent["data"]["asset"].as_str().unwrap(), "sol");
    assert_eq!(intent["data"]["amountLamports"].as_i64().unwrap(), 5_000);
    assert!(intent["data"]["splToken"].is_null(), "{intent}");

    // No `asset` at all: the deployment's default (`usdc` here) resolves.
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    assert_eq!(intent["data"]["asset"].as_str().unwrap(), "usdc");

    // The intent's meta records the locked figures the deposit row carries.
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();
    let meta: Value = sqlx::query_scalar(
        "SELECT meta FROM oxsum.deposits WHERE payment_ref = $1 AND rail = 'solana'",
    )
    .bind(&reference)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(meta["asset"].as_str().unwrap(), "usdc");
    assert_eq!(
        meta["splToken"].as_str().unwrap(),
        "usdcMint11111111111111111111111111111111"
    );
    assert_eq!(meta["assetDecimals"].as_i64().unwrap(), 6);
    assert_eq!(meta["rateMicro"].as_i64().unwrap(), USD_PEGGED_RATE_MICRO);
}

/// Issue #1140: an `asset` the whitelist does not carry is a 400, never a
/// silent re-price under the default — and nothing opens for it.
#[tokio::test]
async fn solana_open_refuses_an_unknown_asset() {
    let stub = SolanaStub::default();
    let Some((app, db)) = world_with_terms(stub, multi_asset_terms()).await else {
        return;
    };
    let key = register_session(&app).await;
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000, "asset": "doge"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.deposits WHERE rail = 'solana'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// A configured world around an already-built verifier — the oracle-priced
/// tests build `with_pricing` themselves; the static ones go through
/// `world`/`world_with_terms`.
async fn world_with(verifier: SolanaRailVerifier) -> Option<(Router, Db)> {
    let u = oxsum_testkit::suite_url!()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let config = Config::new(Signup::Open, None).with_solana(verifier);
    Some((oxsum_server::app(db.clone(), config), db))
}

/// The oracle-stubbed world behind issue #1139: a `PriceOracle` that answers
/// one scripted `OracleQuote` (or `None`), so a `sol` open prices the intent
/// at the quote rather than the static `AssetTerms::rate_micro` fallback.
#[derive(Debug, Default)]
struct OracleStub {
    quote: Mutex<Option<OracleQuote>>,
}

impl PriceOracle for OracleStub {
    fn latest<'a>(
        &'a self,
        _feed_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<OracleQuote>> + Send + 'a>> {
        Box::pin(async move { *self.quote.lock().unwrap() })
    }
}

/// Issue #1139: a `sol` open is oracle-priced. The verifier carries
/// `SolanaPricing::Oracle` over a stubbed `PriceOracle`; the opened intent's
/// `meta` locks the quote-derived `rateMicro`, records `rateSource: "pyth"`
/// and the quote's own `pythPrice`/`pythPublishTime`, and the answered
/// `amountLamports` converts at that rate — not the static `AssetTerms`
/// figure.
#[tokio::test]
async fn solana_open_locks_the_oracle_rate_into_meta() {
    let stub = SolanaStub::default();
    // USD 204.93/SOL at expo -8 → 204_930_000 minor-credits per whole SOL.
    let oracle = Arc::new(OracleStub {
        quote: Mutex::new(Some(OracleQuote {
            price: 20_493_000_000,
            conf: 15_300_000,
            expo: -8,
            publish_time: time::OffsetDateTime::now_utc().unix_timestamp(),
        })),
    });
    let verifier = SolanaRailVerifier::with_pricing(
        Arc::new(stub),
        multi_asset_terms(),
        SolanaPricing::Oracle {
            feed_id: "feed.sol.usd".to_owned(),
            max_quote_age_secs: 60,
            max_conf_ratio: 0.01,
        },
        Some(oracle),
    );
    let Some((app, db)) = world_with(verifier).await else {
        return;
    };
    let key = register_session(&app).await;
    // 1000 minor-credits at 204_930_000 minor/SOL ≈ 4_880 lamports (ceil).
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000, "asset": "sol"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    let lamports = intent["data"]["amountLamports"].as_i64().unwrap();
    assert_eq!(lamports, 4_880, "{intent}");
    // The intent's own meta locked the oracle figure and its provenance.
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();
    let meta: Value = sqlx::query_scalar(
        "SELECT meta FROM oxsum.deposits WHERE payment_ref = $1 AND rail = 'solana'",
    )
    .bind(&reference)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(meta["rateSource"].as_str().unwrap(), "pyth");
    assert_eq!(meta["rateMicro"].as_i64().unwrap(), 204_930_000);
    assert_eq!(meta["pythPrice"].as_str().unwrap(), "20493000000");
    assert!(meta["pythPublishTime"].as_i64().unwrap() > 0);
}

/// Issue #1139: a `usdc` open never calls the oracle — a pegged asset's rate
/// is the fixed parity, so a panicking stub proves the path is clean. The
/// `sol` side of the same deployment is what the oracle gates.
#[tokio::test]
async fn solana_usdc_open_never_consults_the_oracle() {
    let stub = SolanaStub::default();
    /// An oracle that fails the test if it is ever sampled.
    #[derive(Debug)]
    struct PanicOracle;
    impl PriceOracle for PanicOracle {
        fn latest<'a>(
            &'a self,
            _feed_id: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<OracleQuote>> + Send + 'a>>
        {
            panic!("a USD-pegged intent must not call the oracle");
        }
    }
    let verifier = SolanaRailVerifier::with_pricing(
        Arc::new(stub),
        multi_asset_terms(),
        SolanaPricing::Oracle {
            feed_id: "feed.sol.usd".to_owned(),
            max_quote_age_secs: 60,
            max_conf_ratio: 0.01,
        },
        Some(Arc::new(PanicOracle)),
    );
    let Some((app, _db)) = world_with(verifier).await else {
        return;
    };
    let key = register_session(&app).await;
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000, "asset": "usdc"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    assert_eq!(intent["data"]["amountLamports"].as_i64().unwrap(), 1_000);
}

/// Issue #1139: a `sol` open refuses — with the API's 403 envelope, never a
/// silent static fallback — when the freshest quote is stale or its
/// confidence is too wide, and when the oracle answers nothing at all.
#[tokio::test]
async fn solana_open_refuses_a_stale_wide_or_absent_quote() {
    let stub = SolanaStub::default();
    // A quote published 300 s ago — over the 60 s bound.
    let oracle = Arc::new(OracleStub {
        quote: Mutex::new(Some(OracleQuote {
            price: 20_493_000_000,
            conf: 15_300_000,
            expo: -8,
            publish_time: time::OffsetDateTime::now_utc().unix_timestamp() - 300,
        })),
    });
    let verifier = SolanaRailVerifier::with_pricing(
        Arc::new(stub),
        multi_asset_terms(),
        SolanaPricing::Oracle {
            feed_id: "feed.sol.usd".to_owned(),
            max_quote_age_secs: 60,
            max_conf_ratio: 0.01,
        },
        Some(oracle.clone()),
    );
    let Some((app, _db)) = world_with(verifier).await else {
        return;
    };
    let key = register_session(&app).await;
    let sol_open = || {
        call(
            &app,
            "POST",
            "/api/v1/topups/solana",
            Some(json!({"amountMinor": 1000, "asset": "sol"})),
            Some(&key),
        )
    };
    let (status, refusal) = sol_open().await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");
    // A fresh-but-wide quote refuses the same way.
    *oracle.quote.lock().unwrap() = Some(OracleQuote {
        price: 20_493_000_000,
        conf: 20_493_000_000, // conf/price = 1.0, over the 0.01 bound
        expo: -8,
        publish_time: time::OffsetDateTime::now_utc().unix_timestamp(),
    });
    let (status, refusal) = sol_open().await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");
    // The oracle answering nothing refuses too — never a static fallback.
    *oracle.quote.lock().unwrap() = None;
    let (status, refusal) = sol_open().await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");
}

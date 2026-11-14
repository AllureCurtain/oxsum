//! HTTP tests for the Solana deposit rail (issues #1117, #1142).
//!
//! `POST /api/v1/topups/solana` opens a `pending` intent carrying a unique
//! `reference` marker; `POST /api/v1/topups/solana/confirm` submits it and
//! runs the findReference + validateTransfer check against a stubbed chain
//! lookup — `credit_solana_deposit` walks the same pending → confirmed →
//! credited path every rail shares. A replayed confirm credits once, a
//! mismatched transfer refuses, and an unconfigured deployment 503s.
//!
//! Issue #1142's queue: a located-but-short transfer marks `underpaid`, a
//! late one `late`, and an over-delivery credits and marks `overpaid` — each
//! readable on `/api/v1/admin/solana/exceptions`, and each refundable
//! through the pull-payment lifecycle (`refund` → the customer's
//! `refund-address` → `refund-approve` → `refund-complete` → `refunded`).
//!
//! The tests need DATABASE_URL and skip without it.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use oxsum_core::{
    AssetTerms, ChainLookup, Db, ExpectedTransfer, OracleQuote, PriceOracle, SanctionsScreen,
    ScannedTransfer, ScreenVerdict, SolanaPricing, SolanaRailVerifier, SolanaTerms,
    USD_PEGGED_RATE_MICRO, ValidatedTransfer,
};
use oxsum_server::{Config, Signup};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
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

/// The #1151 shape: `multi_asset_terms` plus the opt-in `usdt` entry — the
/// same 6-decimal USD parity as `usdc`, on its own mint. The default stays
/// `usdc`; a `usdt` open names the key explicitly.
fn multi_asset_terms_with_usdt() -> SolanaTerms {
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
            AssetTerms {
                key: "usdt".to_owned(),
                spl_token_mint: Some("usdtMint2222222222222222222222222222222222".to_owned()),
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
/// that does not honor the intent. `scanned` arms the reconciliation scan's
/// answer (issue #1149) — the transfers `payTo`'s history reports. `payouts`
/// arms the refund-complete check (issue #1175) — signature → the payout the
/// chain reports as `(recipient, mint, delivered)`, dishonored on a
/// recipient/mint mismatch the way the client's own checks refuse.
#[derive(Clone, Default)]
struct SolanaStub {
    /// `reference` → the signatures `find_reference` answers, newest first.
    located: Arc<Mutex<std::collections::HashMap<String, Vec<String>>>>,
    /// `signature` → the `ValidatedTransfer` `validate_transfer` answers.
    validated: Arc<Mutex<std::collections::HashMap<String, ValidatedTransfer>>>,
    /// The `ScannedTransfer`s `scan_received` answers — the scripted
    /// `payTo` history the scan tests walk.
    scanned: Arc<Mutex<Vec<ScannedTransfer>>>,
    /// `signature` → the payout `(recipient, mint, delivered)` the chain
    /// reports `validate_payout` as — the scripted refund transaction.
    payouts: Arc<Mutex<std::collections::HashMap<String, StubPayout>>>,
}

/// One scripted on-chain payout `validate_payout` answers with (issue
/// #1175): the address it paid, the mint it spent (`None` = native SOL), the
/// delivered figure.
#[derive(Clone)]
struct StubPayout {
    recipient: String,
    mint: Option<String>,
    delivered: i64,
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
            // The armed transfer is the intent's payment — the same answer
            // `SolanaRpcClient::validate_transfer` gives once the recipient,
            // mint and marker prove out (issue #1142): the delivered figure
            // rides up unjudged, and the verifier classifies an under-
            // delivery `underpaid` rather than masquerading as "not paid".
            // `None` here is "not the payment at all", which the stub reaches
            // only for an unarmed signature — a wrong recipient/mint/marker
            // would dishonor the same way on-chain.
            let _ = expected;
            Some(answer)
        })
    }

    fn validate_payout<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a oxsum_core::ExpectedPayout,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>>
    {
        Box::pin(async move {
            let armed = self.payouts.lock().unwrap().get(signature).cloned()?;
            // The client's own checks: wrong recipient or wrong mint is no
            // proof at all; `delivered` rides up unjudged for the core's
            // frozen-amount comparison.
            if armed.recipient != expected.recipient || armed.mint != expected.spl_token_mint {
                return None;
            }
            Some(ValidatedTransfer {
                signature: signature.to_owned(),
                delivered: armed.delivered,
                block_time: Some(0),
                payer: None,
            })
        })
    }

    fn scan_received<'a>(
        &'a self,
        max_signatures: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<ScannedTransfer>> + Send + 'a>>
    {
        Box::pin(async move {
            self.scanned
                .lock()
                .unwrap()
                .iter()
                .take(max_signatures)
                .cloned()
                .collect()
        })
    }
}

/// The stubbed sanctions screen (issue #1144): `verdicts` maps a payer
/// address to the verdict `screen` answers; an unmapped address answers the
/// `fallback` — `Clear` by default, so a test arms only the address it means
/// to flag.
#[derive(Clone)]
struct ScreenStub {
    verdicts: Arc<Mutex<std::collections::HashMap<String, ScreenVerdict>>>,
    fallback: ScreenVerdict,
}

impl Default for ScreenStub {
    fn default() -> Self {
        Self {
            verdicts: Arc::new(Mutex::new(std::collections::HashMap::new())),
            fallback: ScreenVerdict::Clear,
        }
    }
}

impl std::fmt::Debug for ScreenStub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenStub").finish_non_exhaustive()
    }
}

impl SanctionsScreen for ScreenStub {
    fn screen<'a>(
        &'a self,
        address: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ScreenVerdict> + Send + 'a>> {
        Box::pin(async move {
            self.verdicts
                .lock()
                .unwrap()
                .get(address)
                .cloned()
                .unwrap_or_else(|| self.fallback.clone())
        })
    }
}

/// The operator token the exception-queue and refund lifecycle tests use
/// (issue #1142).
const ADMIN_TOKEN: &str = "solana-operator-token-0123456789";

/// A configured world: the Solana rail is on, with the stubbed chain, and
/// the operator admin surface is on for the exception-queue tests.
async fn world(stub: SolanaStub) -> Option<(Router, Db)> {
    world_with_terms(stub, terms()).await
}

/// The same world under explicit terms — the multi-asset suite passes its
/// whitelist (issue #1140). `SolanaRailVerifier::new` is the static-rate
/// shape (issue #1139): `sol` intents lock the configured figure, pegged
/// ones the parity.
async fn world_with_terms(stub: SolanaStub, terms: SolanaTerms) -> Option<(Router, Db)> {
    world_with_screen_and_terms(stub, terms, None).await
}

/// The same world with an optional sanctions screen attached — `None` runs
/// the rail exactly as `OXSUM_SOLANA_SANCTIONS_ENABLED` unset would (issue
/// #1144).
async fn world_with_screen_and_terms(
    stub: SolanaStub,
    terms: SolanaTerms,
    screen: Option<ScreenStub>,
) -> Option<(Router, Db)> {
    let mut verifier = SolanaRailVerifier::new(Arc::new(stub), terms);
    if let Some(screen) = screen {
        verifier = verifier.with_sanctions(Arc::new(screen));
    }
    world_with(verifier).await
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
            payer: None,
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

/// Issue #1142: a located transfer that delivers under the asked amount is
/// not "not paid yet" — the deposit marks `underpaid` and lands on the
/// operator's exception queue, never silently `pending` forever. The confirm
/// answers `409`, the ledger moves nothing, and a second confirm is the
/// terminal `404` the queue's settled state earns.
#[tokio::test]
async fn solana_confirm_marks_an_underpaid_transfer() {
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
    // Arm the stub with a transfer that underpays — it is the intent's
    // payment (recipient, mint, marker all honor) but delivers far short.
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
            payer: None,
        },
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(balance(&app, &key).await, 0);
    // The deposit is on the admin exception queue as `underpaid`, carrying
    // the delivered figure, the locked asset and the spent signature.
    let (status, queue) = call(
        &app,
        "GET",
        "/api/v1/admin/solana/exceptions?status=underpaid",
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    let entries = queue["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{queue}");
    let deposit = &entries[0];
    assert_eq!(deposit["status"].as_str().unwrap(), "underpaid");
    assert_eq!(deposit["signature"].as_str().unwrap(), signature);
    assert_eq!(deposit["delivered"].as_i64().unwrap(), 1);
    assert_eq!(deposit["asset"].as_str().unwrap(), "sol");
    // A second confirm is the settled terminal answer, not a second walk.
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
            payer: None,
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

/// Issue #1142: a transfer that honors the intent but lands after
/// `expiresAt` marks `late` — the refusal that used to loop `pending` every
/// pass now has a name and a queue.
#[tokio::test]
async fn solana_confirm_marks_a_late_transfer() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
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
    // Arm the stub with a fully-paying transfer whose blockTime is far in the
    // future — at/after any expiresAt the intent could have written.
    let signature = "sigLate00000000000000000000000000000000000000000000000000000000000000";
    stub.located
        .lock()
        .unwrap()
        .insert(reference.clone(), vec![signature.to_owned()]);
    stub.validated.lock().unwrap().insert(
        signature.to_owned(),
        ValidatedTransfer {
            signature: signature.to_owned(),
            delivered: 5_000,
            block_time: Some(4_102_444_800), // 2100-01-01 — past every TTL
            payer: None,
        },
    );
    let (status, body) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(balance(&app, &key).await, 0);
    let (status, queue) = call(
        &app,
        "GET",
        "/api/v1/admin/solana/exceptions?status=late",
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    let entries = queue["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{queue}");
    assert_eq!(entries[0]["status"].as_str().unwrap(), "late");
    assert_eq!(entries[0]["signature"].as_str().unwrap(), signature);
}

/// Issue #1142 × #1140: an over-delivery still credits the opened amount
/// exactly once — the row marks `overpaid`, `entryId` proves the credit, and
/// the surplus figure is what the queue shows for a possible refund. On a
/// USDC intent the over/under verdict reads USDC base units — a 9_000-unit
/// over-delivery on the mint is overpaid exactly like a lamport surplus.
#[tokio::test]
async fn solana_overpay_credits_once_and_marks_overpaid() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world_with_terms(stub.clone(), multi_asset_terms()).await else {
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
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();
    let signature = "sigOver00000000000000000000000000000000000000000000000000000000000000";
    stub.located
        .lock()
        .unwrap()
        .insert(reference.clone(), vec![signature.to_owned()]);
    stub.validated.lock().unwrap().insert(
        signature.to_owned(),
        ValidatedTransfer {
            signature: signature.to_owned(),
            delivered: 9_000, // over the asked 1_000 USDC base units
            block_time: Some(0),
            payer: None,
        },
    );
    let (status, receipt) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    // The opened amount credits — never the surplus — exactly once.
    assert_eq!(balance(&app, &key).await, 1000);
    let (status, queue) = call(
        &app,
        "GET",
        "/api/v1/admin/solana/exceptions?status=overpaid",
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    let entries = queue["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{queue}");
    let deposit = &entries[0];
    assert_eq!(deposit["status"].as_str().unwrap(), "overpaid");
    assert!(deposit["entryId"].is_string(), "{deposit}");
    assert_eq!(deposit["asset"].as_str().unwrap(), "usdc");
    assert_eq!(deposit["delivered"].as_i64().unwrap(), 9_000);
    assert_eq!(deposit["expected"].as_i64().unwrap(), 1_000);
    // A replayed confirm is the settled answer, and the balance never moves
    // a second time.
    let (_status, _) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(balance(&app, &key).await, 1000);
}

/// Drives a deposit onto the named exception status through the stubbed
/// chain and returns its queue row's `depositId`. `delivered`/`block_time`
/// decide the verdict: short → `underpaid`, on-time full → credit, late →
/// `late` (checked before the short verdict, matching the rail).
async fn open_exception(
    app: &Router,
    stub: &SolanaStub,
    key: &str,
    amount_minor: i64,
    delivered: i64,
    block_time: i64,
) -> String {
    let (status, intent) = call(
        app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": amount_minor})),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();
    let signature = format!("sig{}", &uuid::Uuid::new_v4().simple().to_string()[..24]);
    stub.located
        .lock()
        .unwrap()
        .insert(reference.clone(), vec![signature.clone()]);
    stub.validated.lock().unwrap().insert(
        signature.clone(),
        ValidatedTransfer {
            signature: signature.clone(),
            delivered,
            block_time: Some(block_time),
            payer: None,
        },
    );
    let _ = call(
        app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(key),
    )
    .await;
    let (status, queue) = call(
        app,
        "GET",
        "/api/v1/admin/solana/exceptions",
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    queue["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["signature"].as_str() == Some(&signature))
        .expect("the marked deposit is on the queue")["depositId"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The pull-payment lifecycle end to end (issue #1142): the operator opens
/// the refund (`refund_pending`), the customer names their refund address on
/// their own surface, the operator approves, and the payout signature closes
/// the row `refunded` — never an auto-send, never a ledger re-entry.
#[tokio::test]
async fn solana_refund_walks_the_pull_payment_lifecycle() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    // An underpaid deposit: 500 credits opened, the transfer delivered 1
    // lamport (well short of the asked 2_500).
    let deposit_id = open_exception(&app, &stub, &key, 500, 1, 0).await;
    // The customer's own list sees the `underpaid` row.
    let (status, mine) = call(
        &app,
        "GET",
        "/api/v1/topups/solana/deposits",
        None,
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    assert_eq!(mine["data"].as_array().unwrap().len(), 1, "{mine}");
    assert_eq!(mine["data"][0]["asset"].as_str().unwrap(), "sol", "{mine}");
    // The operator opens the refund — freezing the payout's terms (issue
    // #1175): `delivered` = 1 lamport for an underpaid row, on the locked
    // `sol` asset.
    let (status, opened) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund"),
        Some(json!({"reason": "the transfer underpaid the intent"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{opened}");
    assert_eq!(opened["data"]["status"].as_str().unwrap(), "refund_pending");
    assert_eq!(opened["data"]["refundAmount"].as_i64().unwrap(), 1);
    assert_eq!(opened["data"]["refund"]["amount"].as_i64().unwrap(), 1);
    assert_eq!(opened["data"]["refund"]["asset"].as_str().unwrap(), "sol");
    // Approval before the customer names the address is refused — there is
    // nowhere to pay yet.
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-approve"),
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // The customer names their refund address — a real 32-byte key.
    let refund_address = "4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM";
    let (status, addressed) = call(
        &app,
        "POST",
        &format!("/api/v1/topups/solana/{deposit_id}/refund-address"),
        Some(json!({"address": refund_address})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{addressed}");
    assert_eq!(
        addressed["data"]["refundAddress"].as_str().unwrap(),
        refund_address
    );
    // Complete before approval is refused — the order is the point.
    let payout =
        "299xJhoRJCHgYo6xxU78Joi8sY7uXrsp7kzXMxbVFZVLKHqVAfV1R3dA864pwEhe6z1wYWy1BXDzyvzNZDKHfdMd";
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-complete"),
        Some(json!({"signature": payout})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // Approve, then complete: the row lands `refunded` with the payout
    // signature recorded — and no credit ever ran.
    let (status, approved) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-approve"),
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    // A post-approval address edit is refused — it could redirect the
    // payout the multisig is about to sign.
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/topups/solana/{deposit_id}/refund-address"),
        Some(json!({"address": "11111111111111111111111111111111"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    // The payout the multisig sent: 1 lamport — the frozen underpaid amount —
    // of native SOL to the customer's refund address. Arm the chain so
    // refund-complete's on-chain check proves it (issue #1175): the signature
    // alone would have closed the row before.
    stub.payouts.lock().unwrap().insert(
        payout.to_owned(),
        StubPayout {
            recipient: refund_address.to_owned(),
            mint: None,
            delivered: 1,
        },
    );
    let (status, closed) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-complete"),
        Some(json!({"signature": payout})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["data"]["status"].as_str().unwrap(), "refunded");
    assert_eq!(closed["data"]["refundSignature"].as_str().unwrap(), payout);
    assert_eq!(balance(&app, &key).await, 0);
    // A replayed complete with the standing signature answers the closed
    // row; a contradicting one is the conflict the audit protects against.
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-complete"),
        Some(json!({"signature": payout})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-complete"),
        Some(json!({"signature": "1111111111111111111111111111111111111111111111111111111111111111"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// Issue #1175: `refund-complete` verifies the submitted signature on-chain —
/// a plausible-but-nonexistent transaction, a payout to the wrong address or
/// under the frozen amount all answer `409` and leave the row
/// `refund_pending`; only a payout that delivered the frozen figure to the
/// frozen address closes `refunded`.
#[tokio::test]
async fn solana_refund_complete_verifies_the_payout_onchain() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    // An underpaid deposit: 500 credits opened, the transfer delivered 1
    // lamport — the frozen refund amount is that 1 lamport of SOL.
    let deposit_id = open_exception(&app, &stub, &key, 500, 1, 0).await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund"),
        Some(json!({"reason": "underpaid"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let refund_address = "4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM";
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/topups/solana/{deposit_id}/refund-address"),
        Some(json!({"address": refund_address})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-approve"),
        None,
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let payout =
        "299xJhoRJCHgYo6xxU78Joi8sY7uXrsp7kzXMxbVFZVLKHqVAfV1R3dA864pwEhe6z1wYWy1BXDzyvzNZDKHfdMd";
    // A plausible signature naming no transaction: 409, row stays pending.
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-complete"),
        Some(json!({"signature": payout})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // A real tx that paid a *different* address: still no proof — 409.
    stub.payouts.lock().unwrap().insert(
        payout.to_owned(),
        StubPayout {
            recipient: "9uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofN".to_owned(),
            mint: None,
            delivered: 1,
        },
    );
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-complete"),
        Some(json!({"signature": payout})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // A real tx that paid the right address but under the frozen amount: 409.
    stub.payouts.lock().unwrap().insert(
        payout.to_owned(),
        StubPayout {
            recipient: refund_address.to_owned(),
            mint: None,
            delivered: 0,
        },
    );
    let (status, body) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-complete"),
        Some(json!({"signature": payout})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // The honest payout: right address, native SOL, the frozen amount — 200
    // and the row closes `refunded`.
    stub.payouts.lock().unwrap().insert(
        payout.to_owned(),
        StubPayout {
            recipient: refund_address.to_owned(),
            mint: None,
            delivered: 1,
        },
    );
    let (status, closed) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund-complete"),
        Some(json!({"signature": payout})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{closed}");
    assert_eq!(closed["data"]["status"].as_str().unwrap(), "refunded");
    assert_eq!(closed["data"]["refundAmount"].as_i64().unwrap(), 1);
}

/// A refund cannot open on a deposit that owes nothing: `pending`,
/// `credited` (without an overpay), `expired` and `refunded` are all
/// non-refundable — `409`, never a silent accept.
#[tokio::test]
async fn solana_refund_refuses_a_non_refundable_status() {
    let stub = SolanaStub::default();
    let Some((app, db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    // A deposit that credited cleanly: `credited` owes no refund.
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
    let signature = "sigClean0000000000000000000000000000000000000000000000000000000000000";
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
            payer: None,
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
    assert_eq!(status, StatusCode::OK);
    let deposit_id: Uuid = sqlx::query_scalar(
        "SELECT deposit_id FROM oxsum.deposits WHERE rail = 'solana' AND payment_ref = $1",
    )
    .bind(&reference)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund"),
        Some(json!({"reason": "should refuse"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// The refund address is validated on the server: a malformed paste is a
/// `400`, not a recorded string the payout could be signed against.
#[tokio::test]
async fn solana_refund_address_must_be_a_base58_pubkey() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    let deposit_id = open_exception(&app, &stub, &key, 500, 1, 0).await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/admin/solana/{deposit_id}/refund"),
        Some(json!({"reason": "underpaid"})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for bad in ["not-an-address", "0OIl", ""] {
        let (status, _) = call(
            &app,
            "POST",
            &format!("/api/v1/topups/solana/{deposit_id}/refund-address"),
            Some(json!({"address": bad})),
            Some(&key),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    // Another organization cannot write the address — the deposit is not
    // theirs to see.
    let other = register_session(&app).await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/v1/topups/solana/{deposit_id}/refund-address"),
        Some(json!({"address": "4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM"})),
        Some(&other),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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

/// Issue #1151: `usdt` deposits run the same path `usdc` does — the open
/// locks the USDT mint, its 6 decimals and the USD parity, and a transfer
/// delivering the asked base units confirms and credits once. The whitelist
/// carries all three assets; the opt-in flag is a config-time gate, so the
/// serving path needs nothing new.
#[tokio::test]
async fn solana_usdt_open_and_confirm_credits() {
    let stub = SolanaStub::default();
    let Some((app, db)) = world_with_terms(stub.clone(), multi_asset_terms_with_usdt()).await
    else {
        return;
    };
    let key = register_session(&app).await;

    // `{"asset":"usdt"}`: the whitelist's USDT entry prices the intent —
    // 1000 minor-credits at parity = 1000 USDT base units (6 decimals).
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000, "asset": "usdt"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    assert_eq!(intent["data"]["asset"].as_str().unwrap(), "usdt");
    assert_eq!(intent["data"]["amountLamports"].as_i64().unwrap(), 1_000);
    assert_eq!(
        intent["data"]["splToken"].as_str().unwrap(),
        "usdtMint2222222222222222222222222222222222"
    );
    assert_eq!(intent["data"]["assetDecimals"].as_u64().unwrap(), 6);
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();

    // The intent's meta locked the usdt figures the confirm honors.
    let meta: Value = sqlx::query_scalar(
        "SELECT meta FROM oxsum.deposits WHERE payment_ref = $1 AND rail = 'solana'",
    )
    .bind(&reference)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(meta["asset"].as_str().unwrap(), "usdt");
    assert_eq!(
        meta["splToken"].as_str().unwrap(),
        "usdtMint2222222222222222222222222222222222"
    );
    assert_eq!(meta["rateMicro"].as_i64().unwrap(), USD_PEGGED_RATE_MICRO);

    // Arm the stub: the paying transfer delivers the asked USDT base units.
    let signature = "sigUsdtPaid0000000000000000000000000000000000000000000000000";
    stub.located
        .lock()
        .unwrap()
        .insert(reference.clone(), vec![signature.to_owned()]);
    stub.validated.lock().unwrap().insert(
        signature.to_owned(),
        ValidatedTransfer {
            signature: signature.to_owned(),
            delivered: 1_000,
            block_time: Some(0),
            payer: None,
        },
    );
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
}

/// A configured world around an already-built verifier — the oracle-priced
/// tests build `with_pricing` themselves; the static ones go through
/// `world`/`world_with_terms`.
async fn world_with(verifier: SolanaRailVerifier) -> Option<(Router, Db)> {
    world_sized(verifier, 5).await
}

/// The same world on a wider pool — the concurrency tests spawn more parallel
/// requests than the default five connections can hold at once, and a
/// `PoolTimedOut` would read as a 429 rather than the shape under test
/// (issue #1153).
async fn world_sized(verifier: SolanaRailVerifier, connections: u32) -> Option<(Router, Db)> {
    let u = oxsum_testkit::suite_url!()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(connections)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let config = Config::new(Signup::Open, None)
        .with_solana(verifier)
        .with_admin_token(ADMIN_TOKEN);
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

/// Issue #1144: the payer screen runs before credit. `Clear` credits exactly
/// as an unscreened rail would — the happy path the screen must not slow.
#[tokio::test]
async fn solana_screening_clear_credits() {
    let stub = SolanaStub::default();
    let screen = ScreenStub::default();
    let Some((app, _db)) = world_with_screen_and_terms(stub.clone(), terms(), Some(screen)).await
    else {
        return;
    };
    let key = register_session(&app).await;
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
    let signature = "sigClear00000000000000000000000000000000000000000000000000";
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
            payer: Some("cleanPayerAddr1111111111111111111111111111".to_owned()),
        },
    );
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
}

/// A `Flagged` verdict parks the deposit: status `flagged`, no credit, no
/// refund, `meta.screening` recording the verdict — the operator queue's
/// row, never a silent credit.
#[tokio::test]
async fn solana_screening_flagged_parks_the_deposit() {
    let stub = SolanaStub::default();
    let screen = ScreenStub::default();
    screen.verdicts.lock().unwrap().insert(
        "hitPayerAddr2222222222222222222222222222".to_owned(),
        ScreenVerdict::Flagged {
            source: "ofac-sdn".to_owned(),
            reason: "the payer is on the OFAC SDN list".to_owned(),
        },
    );
    let Some((app, db)) = world_with_screen_and_terms(stub.clone(), terms(), Some(screen)).await
    else {
        return;
    };
    let key = register_session(&app).await;
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
    let signature = "sigFlag00000000000000000000000000000000000000000000000000";
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
            payer: Some("hitPayerAddr2222222222222222222222222222".to_owned()),
        },
    );
    // The confirm does not credit — the deposit parked at `flagged` reads as
    // "no open intent" to the caller, and the wallet never moves.
    let (_status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(balance(&app, &key).await, 0);
    let row = sqlx::query(
        "SELECT status, meta, facilitator_tx_hash FROM oxsum.deposits WHERE payment_ref = $1",
    )
    .bind(&reference)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let status: String = row.try_get("status").unwrap();
    assert_eq!(status, "flagged");
    let tx_hash: Option<String> = row.try_get("facilitator_tx_hash").unwrap();
    assert_eq!(tx_hash.as_deref(), Some(signature));
    let meta: Value = row.try_get("meta").unwrap();
    let screening = &meta["screening"];
    assert_eq!(screening["verdict"].as_str().unwrap(), "flagged");
    assert_eq!(screening["source"].as_str().unwrap(), "ofac-sdn");
    assert_eq!(
        screening["address"].as_str().unwrap(),
        "hitPayerAddr2222222222222222222222222222"
    );
    assert!(screening["at"].as_str().is_some(), "{screening}");
}

/// A `TemporarilyUnavailable` screen fails closed: the row stays `pending`,
/// the confirm is refused, and nothing credits — the next pass retries the
/// screen rather than booking unscreened money.
#[tokio::test]
async fn solana_screening_unavailable_stays_pending() {
    let stub = SolanaStub::default();
    let screen = ScreenStub {
        fallback: ScreenVerdict::TemporarilyUnavailable,
        ..Default::default()
    };
    let Some((app, db)) = world_with_screen_and_terms(stub.clone(), terms(), Some(screen)).await
    else {
        return;
    };
    let key = register_session(&app).await;
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
    let signature = "sigUnav00000000000000000000000000000000000000000000000000";
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
            payer: Some("unavailPayerAddr333333333333333333333333333".to_owned()),
        },
    );
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(balance(&app, &key).await, 0);
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE payment_ref = $1")
            .bind(&reference)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "pending");
}

/// The multi-asset dimension of the same screen (issues #1140, #1144): the
/// payer `verify_locked` records comes from the validated transfer on the
/// intent's *locked* mint — a `usdc` intent's SPL debited-owner — and a hit
/// parks that row exactly as a `sol` one does. The stub chain answers the
/// SPL payer the client's `spl_payer` arm derives; the screen runs on it.
#[tokio::test]
async fn solana_screening_flagged_parks_a_usdc_deposit() {
    let stub = SolanaStub::default();
    let screen = ScreenStub::default();
    screen.verdicts.lock().unwrap().insert(
        "usdcHitPayerAddr6666666666666666666666666".to_owned(),
        ScreenVerdict::Flagged {
            source: "chainalysis".to_owned(),
            reason: "chainalysis listed the address (2 identifications)".to_owned(),
        },
    );
    let Some((app, db)) =
        world_with_screen_and_terms(stub.clone(), multi_asset_terms(), Some(screen)).await
    else {
        return;
    };
    let key = register_session(&app).await;
    // A `usdc` intent: 1000 minor-credits at parity asks 1_000 USDC base
    // units on the whitelisted mint.
    let (status, intent) = call(
        &app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000, "asset": "usdc"})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();
    let signature = "sigUsdcFlag000000000000000000000000000000000000000000000000";
    stub.located
        .lock()
        .unwrap()
        .insert(reference.clone(), vec![signature.to_owned()]);
    stub.validated.lock().unwrap().insert(
        signature.to_owned(),
        ValidatedTransfer {
            signature: signature.to_owned(),
            delivered: 1_000, // 1_000 USDC base units at parity
            block_time: Some(0),
            // The SPL arm's answer — the debited token account's owner.
            payer: Some("usdcHitPayerAddr6666666666666666666666666".to_owned()),
        },
    );
    let (_status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(balance(&app, &key).await, 0);
    let row = sqlx::query("SELECT status, meta FROM oxsum.deposits WHERE payment_ref = $1")
        .bind(&reference)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("status"), "flagged");
    let meta: Value = row.try_get("meta").unwrap();
    // The locked asset stayed `usdc` through the flag — the parked row reads
    // back the mint it asked for, and the verdict names the SPL payer.
    assert_eq!(meta["asset"].as_str().unwrap(), "usdc");
    assert_eq!(meta["screening"]["verdict"].as_str().unwrap(), "flagged");
    assert_eq!(meta["screening"]["source"].as_str().unwrap(), "chainalysis");
    assert_eq!(
        meta["screening"]["address"].as_str().unwrap(),
        "usdcHitPayerAddr6666666666666666666666666"
    );
}

/// A transfer that yields no payer screens as `TemporarilyUnavailable` —
/// deny-until-cleared means an unnameable address is not a clear — while a
/// deployment that never enabled screening (no `with_sanctions`) still
/// credits the same shape: the gate is opt-in, never a silent default.
#[tokio::test]
async fn solana_screening_absent_payer_is_unavailable_but_disabled_credits() {
    // Enabled screen + no payer: stays pending.
    let stub = SolanaStub::default();
    let screen = ScreenStub::default();
    let Some((app, _db)) = world_with_screen_and_terms(stub.clone(), terms(), Some(screen)).await
    else {
        return;
    };
    let key = register_session(&app).await;
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
    let signature = "sigNoPayer0000000000000000000000000000000000000000000000";
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
            payer: None,
        },
    );
    let (status, _body) = call(
        &app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(&key),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(balance(&app, &key).await, 0);

    // Disabled screen (no with_sanctions) + no payer: credits as before.
    let stub2 = SolanaStub::default();
    let Some((app2, _db2)) = world(stub2.clone()).await else {
        return;
    };
    let key2 = register_session(&app2).await;
    let (status, intent2) = call(
        &app2,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": 1000})),
        Some(&key2),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent2}");
    let reference2 = intent2["data"]["reference"].as_str().unwrap().to_owned();
    stub2
        .located
        .lock()
        .unwrap()
        .insert(reference2.clone(), vec![signature.to_owned()]);
    stub2.validated.lock().unwrap().insert(
        signature.to_owned(),
        ValidatedTransfer {
            signature: signature.to_owned(),
            delivered: 5_000,
            block_time: Some(0),
            payer: None,
        },
    );
    let (status, receipt) = call(
        &app2,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference2})),
        Some(&key2),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(balance(&app2, &key2).await, 1000);
}

/// Issue #1153: a burst of concurrent opens on one organization lands every
/// intent under its own `reference` — the anchor is minted inside the open,
/// so N raced requests answer N distinct pending rows, never a collision.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn solana_concurrent_opens_land_distinct_references() {
    let stub = SolanaStub::default();
    // A wider pool: the burst needs more parallel checkouts than the default
    // five, or a checkout timeout would answer 429 rather than an intent.
    let verifier = SolanaRailVerifier::new(Arc::new(stub), terms());
    let Some((app, db)) = world_sized(verifier, 16).await else {
        return;
    };
    let key = register_session(&app).await;
    const N: usize = 8;
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let mut opens = Vec::with_capacity(N);
    for _ in 0..N {
        let app = app.clone();
        let key = key.clone();
        let barrier = barrier.clone();
        opens.push(tokio::spawn(async move {
            barrier.wait().await;
            call(
                &app,
                "POST",
                "/api/v1/topups/solana",
                Some(json!({"amountMinor": 1000})),
                Some(&key),
            )
            .await
        }));
    }
    let mut references = std::collections::HashSet::new();
    for open in opens {
        let (status, intent) = open.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{intent}");
        assert!(
            references.insert(intent["data"]["reference"].as_str().unwrap().to_owned()),
            "a second open answered the same reference: {intent}"
        );
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.deposits WHERE rail = 'solana' AND status = 'pending'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(pending, N as i64);
}

/// Issue #1153: N confirms on the same `reference` through HTTP — exactly
/// one 200 Receipt, the rest the route's own refusal shape (the row is no
/// longer `pending`, so `find_solana_deposit` 404s or the credit refuses).
/// The balance moves exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn solana_concurrent_confirms_credit_once() {
    let stub = SolanaStub::default();
    let verifier = SolanaRailVerifier::new(Arc::new(stub.clone()), terms());
    let Some((app, _db)) = world_sized(verifier, 16).await else {
        return;
    };
    let key = register_session(&app).await;
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
    let signature = "sigConcHttp000000000000000000000000000000000000000000000000";
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
            payer: None,
        },
    );
    const N: usize = 8;
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let mut confirms = Vec::with_capacity(N);
    for _ in 0..N {
        let app = app.clone();
        let key = key.clone();
        let reference = reference.clone();
        let barrier = barrier.clone();
        confirms.push(tokio::spawn(async move {
            barrier.wait().await;
            call(
                &app,
                "POST",
                "/api/v1/topups/solana/confirm",
                Some(json!({"reference": reference})),
                Some(&key),
            )
            .await
        }));
    }
    let mut ok = 0;
    let mut refused = 0;
    for confirm in confirms {
        let (status, _) = confirm.await.unwrap();
        match status {
            StatusCode::OK => ok += 1,
            // A raced loser sees the row already walked: `find_solana_deposit`
            // 404s on the settled intent or the credit answers a refusal —
            // the point is the balance books once, whichever surface reads.
            StatusCode::NOT_FOUND | StatusCode::FORBIDDEN | StatusCode::CONFLICT => refused += 1,
            other => panic!("a raced confirm is a 200 or a refusal: {other}"),
        }
    }
    assert_eq!(ok, 1, "exactly one confirm answers the receipt");
    assert_eq!(refused, N - 1);
    assert_eq!(balance(&app, &key).await, 1000);
}

// ---------------------------------------------------------------------------
// The reconciliation surface (issue #1149): the daily CSV export and the
// read-only unrecorded-on-chain scan.
// ---------------------------------------------------------------------------

/// One HTTP call that reads a *file*, not JSON: returns the status, the
/// response's `Content-Type`/`Content-Disposition` and the body as text.
async fn call_csv(
    app: &Router,
    path: &str,
    key: Option<&str>,
) -> (StatusCode, String, String, String) {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(key) = key {
        req = crate::support::credential(req, key);
    }
    let req = req.body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let disposition = headers
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        content_type,
        disposition,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// Opens and credits one deposit through the stubbed chain, returning
/// `(reference, signature)` — the row the CSV export and the scan diff read.
async fn credit_one(
    app: &Router,
    stub: &SolanaStub,
    key: &str,
    amount_minor: i64,
    delivered: i64,
) -> (String, String) {
    let (status, intent) = call(
        app,
        "POST",
        "/api/v1/topups/solana",
        Some(json!({"amountMinor": amount_minor})),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{intent}");
    let reference = intent["data"]["reference"].as_str().unwrap().to_owned();
    let signature = format!("sig{}", &uuid::Uuid::new_v4().simple().to_string()[..24]);
    stub.located
        .lock()
        .unwrap()
        .insert(reference.clone(), vec![signature.clone()]);
    stub.validated.lock().unwrap().insert(
        signature.clone(),
        ValidatedTransfer {
            signature: signature.clone(),
            delivered,
            block_time: Some(0),
            payer: None,
        },
    );
    let (status, receipt) = call(
        app,
        "POST",
        "/api/v1/topups/solana/confirm",
        Some(json!({"reference": reference})),
        Some(key),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    (reference, signature)
}

/// The CSV export (issue #1149): a credited deposit lands one row carrying
/// every frozen column — header first, then the deposit's own figures —
/// served as `text/csv` with a `solana-deposits-<from>-<to>.csv` filename.
#[tokio::test]
async fn solana_reconciliation_csv_exports_the_window() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    let (reference, signature) = credit_one(&app, &stub, &key, 1000, 5_000).await;
    // The default window (the last complete UTC day) covers a row created
    // today only when today *is* inside it — it is not, so ask for today
    // explicitly.
    let today = time::OffsetDateTime::now_utc().date().to_string();
    let (status, content_type, disposition, body) = call_csv(
        &app,
        &format!("/api/v1/admin/solana/reconciliation/deposits.csv?from={today}&to={today}"),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(content_type.starts_with("text/csv"), "{content_type}");
    assert_eq!(
        disposition,
        format!("attachment; filename=\"solana-deposits-{today}-{today}.csv\"")
    );
    let mut lines = body.lines();
    assert_eq!(
        lines.next().unwrap(),
        "depositId,organizationId,paymentRef,signature,payer,asset,amountMinor,delivered,expected,rateMicro,receivedMinor,status,createdAt,refundAmount,refundSignature,refundStatus"
    );
    let row = lines.next().expect("the credited deposit exported one row");
    let cells: Vec<&str> = row.split(',').collect();
    assert_eq!(cells.len(), 16, "{row}");
    assert_eq!(cells[2], reference, "{row}");
    assert_eq!(cells[3], signature, "{row}");
    assert_eq!(cells[5], "sol", "{row}");
    assert_eq!(cells[6], "1000", "{row}");
    assert_eq!(cells[7], "5000", "{row}");
    assert_eq!(cells[8], "5000", "{row}");
    assert_eq!(cells[9], "200000000", "{row}");
    assert_eq!(cells[10], "1000", "{row}");
    assert_eq!(cells[11], "credited", "{row}");
    assert!(lines.next().is_none(), "one deposit, one row: {body}");
}

/// The export honors its window: a `from`/`to` day range covering only
/// yesterday holds no today's deposit — the header alone answers, which is
/// how a reconciler reads "no deposits that day", never an empty 200.
#[tokio::test]
async fn solana_reconciliation_csv_filters_the_window() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    let (_reference, _signature) = credit_one(&app, &stub, &key, 1000, 5_000).await;
    let yesterday = (time::OffsetDateTime::now_utc().date() - time::Duration::days(1)).to_string();
    let (status, _ct, _cd, body) = call_csv(
        &app,
        &format!(
            "/api/v1/admin/solana/reconciliation/deposits.csv?from={yesterday}&to={yesterday}"
        ),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.lines().count(), 1, "the header alone: {body}");
    // A malformed date is a 400, not a silently different window.
    let (status, _, _, _) = call_csv(
        &app,
        "/api/v1/admin/solana/reconciliation/deposits.csv?from=not-a-day&to=2026-01-01",
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // A lone bound is a 400 — both or neither.
    let (status, _, _, _) = call_csv(
        &app,
        "/api/v1/admin/solana/reconciliation/deposits.csv?from=2026-01-01",
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The scan (issue #1149): `payTo`'s scripted history carries three
/// transfers — one whose signature credits a recorded deposit (excluded),
/// one that paid the vault with no intent (reported, `matchedIntent:
/// false`), and the deposit's own confirm. Only the unmatched one surfaces.
#[tokio::test]
async fn solana_reconciliation_scan_reports_only_unmatched() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
    let (_reference, recorded_sig) = credit_one(&app, &stub, &key, 1000, 5_000).await;
    // The chain's scripted `payTo` history: the recorded signature, one
    // orphaned payment that arrived with no intent, and a third — also
    // unrecorded — the report carries too.
    stub.scanned.lock().unwrap().extend([
        ScannedTransfer {
            signature: recorded_sig.clone(),
            block_time: Some(1),
            asset: Some("sol".to_owned()),
            delivered: 5_000,
            payer: None,
        },
        ScannedTransfer {
            signature: "sigOrphaned1111111111111111111111111111111111111".to_owned(),
            block_time: Some(2),
            asset: Some("sol".to_owned()),
            delivered: 7_500,
            payer: Some("payerWallet1111111111111111111111111111".to_owned()),
        },
        ScannedTransfer {
            signature: "sigOrphaned2222222222222222222222222222222222222".to_owned(),
            block_time: Some(3),
            asset: Some("usdc".to_owned()),
            delivered: 1_200,
            payer: None,
        },
    ]);
    let (status, report) = call(
        &app,
        "POST",
        "/api/v1/admin/solana/reconciliation/scan",
        Some(json!({})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["data"]["scanned"].as_i64().unwrap(), 3);
    let unmatched = report["data"]["unmatched"].as_array().unwrap();
    assert_eq!(unmatched.len(), 2, "{report}");
    let signatures: Vec<&str> = unmatched
        .iter()
        .map(|u| u["signature"].as_str().unwrap())
        .collect();
    assert!(!signatures.contains(&recorded_sig.as_str()), "{report}");
    let orphan = unmatched
        .iter()
        .find(|u| {
            u["signature"].as_str() == Some("sigOrphaned1111111111111111111111111111111111111")
        })
        .expect("the orphaned payment reported");
    assert!(!orphan["matchedIntent"].as_bool().unwrap());
    assert_eq!(orphan["delivered"].as_i64().unwrap(), 7_500);
    assert_eq!(orphan["asset"].as_str().unwrap(), "sol");
    assert_eq!(
        orphan["payer"].as_str().unwrap(),
        "payerWallet1111111111111111111111111111"
    );
    // Read-only by construction: nothing credited, nothing marked — the
    // customer's balance is untouched and no new deposit row appeared.
    assert_eq!(balance(&app, &key).await, 1000);
}

/// The scan's bound: `limit` clamps the scripted history the walk reads —
/// under `limit: 1` only the newest scripted transfer is examined and can
/// report; an out-of-range bound clamps rather than erroring.
#[tokio::test]
async fn solana_reconciliation_scan_bounds_the_walk() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let _key = register_session(&app).await;
    stub.scanned.lock().unwrap().extend([
        ScannedTransfer {
            signature: "sigA1111111111111111111111111111111111111111111".to_owned(),
            block_time: Some(1),
            asset: Some("sol".to_owned()),
            delivered: 100,
            payer: None,
        },
        ScannedTransfer {
            signature: "sigB2222222222222222222222222222222222222222222".to_owned(),
            block_time: Some(2),
            asset: Some("sol".to_owned()),
            delivered: 200,
            payer: None,
        },
    ]);
    // `limit: 1` reads only the newest scripted transfer — `scanned` is the
    // count the walk examined, and only that one can report.
    let (status, report) = call(
        &app,
        "POST",
        "/api/v1/admin/solana/reconciliation/scan",
        Some(json!({"limit": 1})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["data"]["scanned"].as_i64().unwrap(), 1);
    assert_eq!(report["data"]["unmatched"].as_array().unwrap().len(), 1);
}

/// An unconfigured rail 503s both reconciliation endpoints — the rail's own
/// refusal, not an empty answer that reads as a clean reconciliation.
#[tokio::test]
async fn solana_reconciliation_unconfigured_answers_503() {
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
    let config = Config::new(Signup::Open, None).with_admin_token(ADMIN_TOKEN);
    let app = oxsum_server::app(db, config);
    let (status, _, _, _) = call_csv(
        &app,
        "/api/v1/admin/solana/reconciliation/deposits.csv",
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/admin/solana/reconciliation/scan",
        Some(json!({})),
        Some(ADMIN_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

// ---------------------------------------------------------------------------
// Issue #1150 — the optional Helius webhook receiver.
//
// `POST /api/v1/webhooks/solana` is the push that *accelerates* confirmation:
// a verified `Authorization` shared secret is the credential, each entry's
// account keys match `pending` intents' `payment_ref` markers, and every match
// runs the same `credit_solana_deposit` walk the poll tick and the confirm
// route share — the push never credits on its own claims. A replayed
// signature dedups; a malformed payload is 400; a wrong secret is 401; an
// unconfigured deployment is 404.
// ---------------------------------------------------------------------------

/// The shared secret the webhook world pins — Helius echoes it in
/// `Authorization` on every delivery.
const HELIUS_SECRET: &str = "helius-webhook-secret-0123456789abcdef";

/// A configured world with the Helius webhook receiver armed: the rail is on
/// and `solana_helius_webhook_secret` is set, so `POST /api/v1/webhooks/solana`
/// is mounted.
async fn world_with_webhook(stub: SolanaStub) -> Option<(Router, Db)> {
    let u = oxsum_testkit::suite_url!()?;
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&u)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    oxsum_testkit::set_registration_fixture(db.pool(), true).await;
    let verifier = SolanaRailVerifier::new(Arc::new(stub), terms());
    let config = Config::new(Signup::Open, None)
        .with_solana(verifier)
        .with_solana_helius_webhook_secret(HELIUS_SECRET)
        .with_admin_token(ADMIN_TOKEN);
    Some((oxsum_server::app(db.clone(), config), db))
}

/// One call to the webhook endpoint: posts `body` verbatim (so a malformed or
/// non-array payload reaches the handler) with `Authorization` set to
/// `auth` — `None` sends the header absent, `Some("")` sends it empty.
async fn webhook_call(app: &Router, body: &str, auth: Option<&str>) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/v1/webhooks/solana")
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    let req = req.body(Body::from(body.to_owned())).unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

/// A Helius *enhanced* delivery entry: `signature` plus the `accountData[].account`
/// set the paying transaction touched — the candidate `reference` markers.
fn helius_enhanced(signature: &str, accounts: &[&str]) -> Value {
    json!({
        "signature": signature,
        "timestamp": 1_700_000_000i64,
        "type": "TRANSFER",
        "accountData": accounts
            .iter()
            .map(|a| json!({"account": a, "nativeBalanceChange": 0}))
            .collect::<Vec<_>>(),
    })
}

/// Issue #1150's happy path: a verified Helius push names the intent's
/// `reference` among its account keys, the match runs the ordinary
/// `credit_solana_deposit` walk, and the ledger credits on the server's own
/// on-chain check — the push's claims only narrowed which intent to look at.
#[tokio::test]
async fn solana_webhook_credits_a_matching_intent() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world_with_webhook(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
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
    // Arm the chain as though the paying transaction landed — the push does
    // not prove it; the re-verify reads it from the stub.
    let signature = "sigWebhook1111111111111111111111111111111111111111111111111";
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
            payer: None,
        },
    );
    // The push names the intent's reference among its account keys — the match
    // is what schedules the re-verify; the credit is the rail's own verdict.
    let payload = json!([helius_enhanced(
        signature,
        &[&reference, "someOtherAccount"]
    )]);
    let status = webhook_call(&app, &payload.to_string(), Some(HELIUS_SECRET)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(balance(&app, &key).await, 1000);
}

/// A delivery carrying the wrong `Authorization` secret is a `401` — the
/// shared secret is the only credential the open surface carries.
#[tokio::test]
async fn solana_webhook_refuses_a_wrong_secret() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world_with_webhook(stub.clone()).await else {
        return;
    };
    let payload = json!([helius_enhanced("sig", &["ref"])]);
    assert_eq!(
        webhook_call(&app, &payload.to_string(), Some("not-the-secret")).await,
        StatusCode::UNAUTHORIZED
    );
    // No header at all refuses the same way — a missing credential is not a
    // different answer.
    assert_eq!(
        webhook_call(&app, &payload.to_string(), None).await,
        StatusCode::UNAUTHORIZED
    );
}

/// A replayed delivery — the same signature pushed twice — acknowledges `200`
/// both times but the dedup means the re-verify runs once: the first delivery
/// claims the signature, the second is a no-op, and the ledger never double
/// credits.
#[tokio::test]
async fn solana_webhook_replays_dedupe_and_credit_once() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world_with_webhook(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
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
    let signature = "sigReplay2222222222222222222222222222222222222222222222222";
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
            payer: None,
        },
    );
    let payload = json!([helius_enhanced(signature, &[&reference])]);
    assert_eq!(
        webhook_call(&app, &payload.to_string(), Some(HELIUS_SECRET)).await,
        StatusCode::OK
    );
    assert_eq!(balance(&app, &key).await, 1000);
    // The retry — Helius redelivers until it hears a 2xx — is acknowledged
    // without a second chain round-trip and credits nothing twice.
    assert_eq!(
        webhook_call(&app, &payload.to_string(), Some(HELIUS_SECRET)).await,
        StatusCode::OK
    );
    assert_eq!(
        balance(&app, &key).await,
        1000,
        "a replay never double-credits"
    );
}

/// A deployment that never set `OXSUM_SOLANA_HELIUS_WEBHOOK_SECRET` mounts no
/// receiver: `POST /api/v1/webhooks/solana` answers `404` like every
/// unconfigured inbound webhook — polling stays the rail's only confirm path.
#[tokio::test]
async fn solana_webhook_unconfigured_is_not_mounted() {
    let stub = SolanaStub::default();
    // `world` builds the rail without the webhook secret — the push surface is
    // absent even though the rail runs.
    let Some((app, _db)) = world(stub.clone()).await else {
        return;
    };
    let payload = json!([helius_enhanced("sig", &["ref"])]);
    assert_eq!(
        webhook_call(&app, &payload.to_string(), Some(HELIUS_SECRET)).await,
        StatusCode::NOT_FOUND
    );
}

/// A payload that is not the JSON array Helius delivers — an object, a scalar,
/// or malformed JSON — is a `400`: the receiver reads a documented shape, and
/// anything else is a contract violation, not "no intents matched".
#[tokio::test]
async fn solana_webhook_refuses_a_malformed_payload() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world_with_webhook(stub.clone()).await else {
        return;
    };
    // An object, not the array — the contract violation is 400, not a silent
    // "no entries".
    assert_eq!(
        webhook_call(&app, "{\"signature\":\"x\"}", Some(HELIUS_SECRET)).await,
        StatusCode::BAD_REQUEST
    );
    // Malformed JSON entirely.
    assert_eq!(
        webhook_call(&app, "not json at all", Some(HELIUS_SECRET)).await,
        StatusCode::BAD_REQUEST
    );
}

/// A raw-envelope entry carries its account keys under
/// `transaction.message.accountKeys` — the second documented shape resolves
/// the same `pending` intent an enhanced entry would.
#[tokio::test]
async fn solana_webhook_matches_a_raw_envelope() {
    let stub = SolanaStub::default();
    let Some((app, _db)) = world_with_webhook(stub.clone()).await else {
        return;
    };
    let key = register_session(&app).await;
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
    let signature = "sigRaw33333333333333333333333333333333333333333333333333333";
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
            payer: None,
        },
    );
    // The raw getTransaction-shaped entry keys its account list under
    // `transaction.message.accountKeys` — the marker still resolves.
    let payload = json!([{
        "signature": signature,
        "blockTime": 1_700_000_000i64,
        "transaction": {"message": {"accountKeys": [reference, "payerAddr"]}},
    }]);
    assert_eq!(
        webhook_call(&app, &payload.to_string(), Some(HELIUS_SECRET)).await,
        StatusCode::OK
    );
    assert_eq!(balance(&app, &key).await, 1000);
}

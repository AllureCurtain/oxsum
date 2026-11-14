//! The Solana rail's intent bookkeeping against the scratch database
//! (issue #1117): `open_solana_deposit` inserts `pending` with the declared
//! terms in `meta`, `expire_solana_deposit` walks `pending` — and only
//! `pending`, a `confirmed` row holds a verified payment awaiting its credit
//! (issue #1174) — to `expired`, and neither misbehaves on a replayed or
//! mis-priced open. Skips without DATABASE_URL.
use oxsum_core::{
    AssetTerms, ChainLookup, Db, ExpectedTransfer, LockedRate, NewUser, SanctionsScreen,
    ScreenVerdict, SolanaCredit, SolanaRailVerifier, SolanaTerms, USD_PEGGED_RATE_MICRO,
    ValidatedTransfer, WalletError, solana_status,
};
use sqlx::Row;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

async fn database(slug: &str) -> Option<Db> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url(slug)?;
    let db = Db::connect(&url, 5).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}

/// The deployment's terms every test here opens under.
fn terms() -> SolanaTerms {
    SolanaTerms::new(
        "oxsumDepositAddr111111111111111111111111111".to_owned(),
        vec![AssetTerms {
            key: "sol".to_owned(),
            spl_token_mint: None,
            asset_decimals: 9,
            rate_micro: 200_000_000,
        }],
        "sol".to_owned(),
        600,
        1,
        i64::MAX,
        0,
    )
}

/// The two-asset whitelist (issue #1140): SOL plus a 6-decimal USDC mint at
/// the fixed USD parity rate, with `usdc` the deployment default.
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

/// The static `LockedRate` `resolve_rate` would answer for the named asset
/// under `SolanaPricing::Static` — the figure `open_solana_deposit` now
/// carries. Tests that build the verifier themselves keep this matching
/// `AssetTerms::rate_micro` for the asset they open.
fn rate(terms: &SolanaTerms, asset: Option<&str>) -> LockedRate {
    LockedRate::Static {
        rate_micro: terms
            .terms_for_asset(asset)
            .expect("the test asset is on the whitelist")
            .rate_micro,
    }
}

/// A fresh organization's id; `deposits.organization_id` references it.
async fn organization(db: &Db, slug: &str) -> Uuid {
    let registration = db
        .register(NewUser {
            email: format!(
                "solana-{slug}-{}@example.test",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "solana-private-password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    registration.organization.id
}

#[tokio::test]
async fn open_solana_deposit_inserts_pending_with_locked_terms() {
    let Some(db) = database("solana_open").await else {
        return;
    };
    let org = organization(&db, "open").await;
    let deposit_id = Uuid::new_v4();
    let (reference, lamports, expires_at) = db
        .open_solana_deposit(org, deposit_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("a fresh deposit inserts");
    // 1000 minor-credits at 200_000_000 minor-credits/SOL = 0.000005 SOL
    // = 5_000 lamports (lamports = credits_minor * 1e9 / rate_micro).
    assert_eq!(lamports, 5_000);
    // The returned deadline is the stored RFC3339 expiresAt, not a ttl the
    // caller would re-add to its own clock (issue #1131).
    let parsed_expiry =
        time::OffsetDateTime::parse(&expires_at, &time::format_description::well_known::Rfc3339)
            .expect("expiresAt parses as RFC3339");
    assert!(parsed_expiry > time::OffsetDateTime::now_utc());
    assert!(!reference.is_empty());
    let row = sqlx::query(
        "SELECT rail, status, amount_minor, payment_ref, meta FROM oxsum.deposits \
         WHERE deposit_id = $1",
    )
    .bind(deposit_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("rail"), "solana");
    assert_eq!(row.get::<String, _>("status"), "pending");
    assert_eq!(row.get::<i64, _>("amount_minor"), 1000);
    assert_eq!(row.get::<String, _>("payment_ref"), reference);
    let meta = row.get::<serde_json::Value, _>("meta");
    assert_eq!(meta["solLamports"].as_i64().unwrap(), 5_000);
    assert_eq!(meta["rateMicro"].as_i64().unwrap(), 200_000_000);
    assert_eq!(meta["rateSource"].as_str().unwrap(), "config");
    // The answered deadline is exactly the one persisted on the intent.
    assert_eq!(meta["expiresAt"].as_str().unwrap(), expires_at);
}

#[tokio::test]
async fn open_solana_deposit_refuses_a_nonpositive_amount() {
    let Some(db) = database("solana_open_refuse").await else {
        return;
    };
    let org = organization(&db, "open-refuse").await;
    for amount in [0_i64, -5] {
        let result = db
            .open_solana_deposit(
                org,
                Uuid::new_v4(),
                amount,
                &terms(),
                None,
                &rate(&terms(), None),
            )
            .await;
        assert!(
            matches!(result, Err(WalletError::InvalidInput(_))),
            "{result:?}"
        );
    }
    // Nothing was inserted.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.deposits WHERE organization_id = $1 AND rail = 'solana'",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn open_solana_deposit_replays_the_unique_anchor() {
    let Some(db) = database("solana_open_replay").await else {
        return;
    };
    let org = organization(&db, "open-replay").await;
    let deposit_id = Uuid::new_v4();
    let first = db
        .open_solana_deposit(org, deposit_id, 100, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("the first open inserts");
    // A second open mints a different reference — the (rail, org, payment_ref)
    // anchor is the reference, so a replay only collides when the marker is
    // reused, which a fresh mint cannot do. Prove the anchor by forcing a
    // duplicate payment_ref the way a raced retry of the same intent would.
    let inserted = sqlx::query(
        "INSERT INTO oxsum.deposits \
             (deposit_id, organization_id, rail, payment_ref, amount_minor, status, meta) \
         VALUES ($1, $2, 'solana', $3, 100, 'pending', '{}'::jsonb) \
         ON CONFLICT (rail, organization_id, payment_ref) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(org)
    .bind(&first.0)
    .execute(db.pool())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(inserted, 0, "the UNIQUE anchor refused the replay");
}

#[tokio::test]
async fn expire_solana_deposit_walks_pending_only() {
    let Some(db) = database("solana_expire").await else {
        return;
    };
    let org = organization(&db, "expire").await;
    let pending = Uuid::new_v4();
    db.open_solana_deposit(org, pending, 100, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    db.expire_solana_deposit(pending).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(pending)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "expired");
    // A second expire is a no-op: the row stays expired, never walks back.
    db.expire_solana_deposit(pending).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(pending)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "expired");
    // Only `pending` expires: `confirmed` holds a verified payment awaiting
    // its credit — never expirable (issue #1174) — and `credited` is done.
    let confirmed = Uuid::new_v4();
    let credited = Uuid::new_v4();
    for (id, st) in [(confirmed, "confirmed"), (credited, "credited")] {
        sqlx::query(
            "INSERT INTO oxsum.deposits \
                 (deposit_id, organization_id, rail, payment_ref, amount_minor, status, meta) \
             VALUES ($1, $2, 'solana', $3, 100, $4, '{}'::jsonb)",
        )
        .bind(id)
        .bind(org)
        .bind(format!("ref-{id}"))
        .bind(st)
        .execute(db.pool())
        .await
        .unwrap();
        db.expire_solana_deposit(id).await.unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
                .bind(id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(status, st, "a {st} row is untouched by expire");
    }
}

/// Issue #1127: an intent verifies under the terms it locked in `meta`, not
/// the deployment's live config. The stub answers whatever signature a
/// `validate_transfer` probes — the test opens an intent at one rate, builds
/// the verifier under a *different* live rate, and proves the credit settles
/// at the locked amount the intent promised, not the repriced figure the
/// live terms would demand.
#[derive(Debug)]
struct OneSig {
    reference: Mutex<String>,
    signature: String,
    delivered: i64,
    /// The on-chain `blockTime` the stub reports — far in the past so the
    /// intent's `expiresAt` (now + ttl) is always in the transfer's future.
    block_time: i64,
}

impl ChainLookup for OneSig {
    fn find_reference<'a>(
        &'a self,
        reference: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
        Box::pin(async move {
            *self.reference.lock().unwrap() = reference.to_owned();
            vec![self.signature.clone()]
        })
    }
    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a ExpectedTransfer,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>>
    {
        Box::pin(async move {
            if signature != self.signature {
                return None;
            }
            // Prove the transfer is the intent's payment — recipient and
            // marker honor — and report what it delivered; the delivered
            // figure rides up unjudged for the verifier to classify
            // (issue #1142): `delivered < expected` is underpaid, not
            // "not the payment".
            if expected.recipient != self.pay_to() {
                return None;
            }
            Some(ValidatedTransfer {
                signature: signature.to_owned(),
                delivered: self.delivered,
                block_time: Some(self.block_time),
                payers: Vec::new(),
            })
        })
    }
}

impl OneSig {
    fn pay_to(&self) -> String {
        "oxsumDepositAddr111111111111111111111111111".to_owned()
    }
}

/// Issue #1128: a flood of dishonoring transactions on the same marker does
/// not mask the real payment. The verifier walks the whole located list —
/// `find_reference` returns every signature newest-first — so the honoring
/// transfer is found even when many others land on the marker after it.
#[derive(Debug)]
struct ManySigs {
    paying: String,
    noise: Vec<String>,
    delivered: i64,
}

impl ChainLookup for ManySigs {
    fn find_reference<'a>(
        &'a self,
        _reference: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
        Box::pin(async move {
            // Newest first: the noise landed after the paying transaction.
            self.noise
                .iter()
                .cloned()
                .chain(std::iter::once(self.paying.clone()))
                .collect()
        })
    }
    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a ExpectedTransfer,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>>
    {
        Box::pin(async move {
            let _ = expected;
            if signature != self.paying {
                return None; // every noise signature dishonors
            }
            Some(ValidatedTransfer {
                signature: signature.to_owned(),
                delivered: self.delivered,
                block_time: Some(0),
                payers: Vec::new(),
            })
        })
    }
}

#[tokio::test]
async fn verify_walks_past_dishonoring_transactions_to_find_the_payment() {
    let Some(db) = database("solana_many").await else {
        return;
    };
    let org = organization(&db, "many").await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    // 25 dishonoring transactions land on the marker after the real payment —
    // well past the old limit-10 window, which would have masked it entirely.
    let noise: Vec<String> = (0..25)
        .map(|i| format!("noiseSig{i:010}000000000000000000000000000000000000"))
        .collect();
    let verifier = SolanaRailVerifier::new(
        Arc::new(ManySigs {
            paying: "sigReal00000000000000000000000000000000000000000000000000".to_owned(),
            noise,
            delivered: 5_000,
        }),
        terms(),
    );
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    let credit = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("the real payment is found and credits: {credit:?}");
    };
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1128: expiry is the chain's `blockTime`, not the server clock. A
/// payment on-chain before `expiresAt` credits even when confirm runs after
/// the TTL; one on-chain after never does — the two edges the documented
/// 'a payment after expiry is not credited' needs.
#[tokio::test]
async fn expiry_is_judged_on_block_time_not_the_confirm_clock() {
    let Some(db) = database("solana_expiry").await else {
        return;
    };
    let org = organization(&db, "expiry").await;
    // A 1-second-TTL intent: it is already lapsed by wall clock before either
    // credit runs, isolating the blockTime verdict from the server clock.
    let short = SolanaTerms {
        intent_ttl_secs: 1,
        ..terms()
    };
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    let expires_at = time::OffsetDateTime::now_utc().unix_timestamp() + 1;

    // Edge 1: on-chain before expiry (block_time < expiresAt), confirm after
    // the TTL — must credit.
    let on_time = Uuid::new_v4();
    db.open_solana_deposit(org, on_time, 1000, &short, None, &rate(&short, None))
        .await
        .unwrap()
        .expect("inserts");
    let early = SolanaRailVerifier::new(
        Arc::new(OneSig {
            reference: Mutex::new(String::new()),
            signature: "sigEarly00000000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            block_time: expires_at - 5,
        }),
        short.clone(),
    );
    let credit = db
        .credit_solana_deposit(&wallet, on_time, &early, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("a payment on-chain before expiry credits even when confirmed late: {credit:?}");
    };
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 1000);

    // Edge 2: on-chain after expiry (block_time >= expiresAt) — `late` on
    // the queue (issue #1142), never credited.
    let late = Uuid::new_v4();
    db.open_solana_deposit(org, late, 1000, &short, None, &rate(&short, None))
        .await
        .unwrap()
        .expect("inserts");
    let late_verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            reference: Mutex::new(String::new()),
            signature: "sigLate000000000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            block_time: expires_at + 60,
        }),
        short,
    );
    let verdict = db
        .credit_solana_deposit(&wallet, late, &late_verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(
        matches!(verdict, SolanaCredit::Late),
        "a payment on-chain after expiry must not credit: {verdict:?}"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(late)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, solana_status::LATE);
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// The cross-organization double-spend: one on-chain signature credits the
/// deposit it paid for and is refused — as `Forbidden`, never a storage
/// error — when a second organization's intent resolves to it. The failure
/// the shard caught is exactly this shape: a second confirm that should be
/// refused surfacing as INTERNAL_ERROR instead.
#[tokio::test]
async fn a_second_organizations_intent_on_a_spent_signature_is_forbidden() {
    let Some(db) = database("solana_dblspend").await else {
        return;
    };
    let org_a = organization(&db, "org-a").await;
    let org_b = organization(&db, "org-b").await;
    let signature = "sigSpent00000000000000000000000000000000000000000000000000";
    // Both organizations open an intent for 1000; one paying transaction
    // carries both references.
    let mut deposit_ids = Vec::new();
    let mut wallets = Vec::new();
    for (i, org) in [org_a, org_b].iter().enumerate() {
        let deposit_id = Uuid::new_v4();
        let (reference, _, _) = db
            .open_solana_deposit(
                *org,
                deposit_id,
                1000,
                &terms(),
                None,
                &rate(&terms(), None),
            )
            .await
            .unwrap()
            .expect("inserts");
        let _ = reference;
        deposit_ids.push(deposit_id);
        let tenant_id: String = sqlx::query_scalar(
            "SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1",
        )
        .bind(org)
        .fetch_one(db.pool())
        .await
        .unwrap();
        wallets.push(
            oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
                .await
                .unwrap(),
        );
        let _ = i;
    }
    // The verifier answers the same signature for either reference.
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            reference: Mutex::new(String::new()),
            signature: signature.to_owned(),
            delivered: 5_000,
            block_time: 0,
        }),
        terms(),
    );
    // The first organization's credit lands.
    let credit = db
        .credit_solana_deposit(&wallets[0], deposit_ids[0], &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("the first credit runs: {credit:?}");
    };
    assert!(receipt.is_new);
    // The second organization's confirm resolves to the same signature —
    // already spent — and must be a clean Forbidden, never a storage fault.
    let refusal = db
        .credit_solana_deposit(&wallets[1], deposit_ids[1], &verifier, time::Date::MAX)
        .await;
    assert!(
        matches!(refusal, Err(WalletError::Forbidden(_))),
        "a spent signature must refuse Forbidden, got {refusal:?}"
    );
    assert_eq!(wallets[1].available().await.unwrap(), 0);
}

/// Issue #1130: the open surface is bounded. An intent below the floor or
/// above the cap is refused, and an organization past its per-minute open
/// rate is throttled — both before a `payment_ref` is ever minted.
#[tokio::test]
async fn open_solana_deposit_enforces_amount_bounds_and_open_rate() {
    let Some(db) = database("solana_bounds").await else {
        return;
    };
    let org = organization(&db, "bounds").await;
    // A deployment that opens between 100 and 2_000 minor-credits, at most 2
    // intents per minute per organization.
    let bounded = SolanaTerms {
        min_amount_minor: 100,
        max_amount_minor: 2_000,
        opens_per_minute: 2,
        ..terms()
    };
    // Below the floor and above the cap are refused.
    for amount in [50_i64, 5_000] {
        let result = db
            .open_solana_deposit(
                org,
                Uuid::new_v4(),
                amount,
                &bounded,
                None,
                &rate(&bounded, None),
            )
            .await;
        assert!(
            matches!(result, Err(WalletError::InvalidInput(_))),
            "amount {amount} must be refused: {result:?}"
        );
    }
    // Two opens inside the bounds land; the third trips the per-minute cap.
    for _ in 0..2 {
        db.open_solana_deposit(
            org,
            Uuid::new_v4(),
            1000,
            &bounded,
            None,
            &rate(&bounded, None),
        )
        .await
        .unwrap()
        .expect("in-bounds opens insert");
    }
    let throttled = db
        .open_solana_deposit(
            org,
            Uuid::new_v4(),
            1000,
            &bounded,
            None,
            &rate(&bounded, None),
        )
        .await;
    assert!(
        matches!(throttled, Err(WalletError::Forbidden(_))),
        "the third open inside a minute must be throttled: {throttled:?}"
    );
}

#[tokio::test]
async fn verify_locked_settles_at_the_intents_terms_not_the_live_config() {
    let Some(db) = database("solana_locked").await else {
        return;
    };
    let org = organization(&db, "locked").await;
    // Open at the opening rate: 1000 minor-credits at 200_000_000 minor/SOL =
    // 5_000 lamports, locked into meta with payTo and the mint.
    let deposit_id = Uuid::new_v4();
    let (_reference, lamports, _ttl) = db
        .open_solana_deposit(org, deposit_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    assert_eq!(lamports, 5_000);
    // The deployment's live terms shift before the payment confirms: a
    // doubled rate, a different payTo, a different decimals. A verify that
    // re-read live terms would demand 10_000 lamports to the new address;
    // the locked intent must still settle at 5_000 to the locked address.
    let live = SolanaTerms::new(
        "aDifferentLivePayTo00000000000000000000000000".to_owned(),
        vec![AssetTerms {
            key: "usdc".to_owned(),
            spl_token_mint: Some("aLiveMintThatWasNeverLocked".to_owned()),
            asset_decimals: 6,
            rate_micro: 400_000_000,
        }],
        "usdc".to_owned(),
        600,
        1,
        i64::MAX,
        0,
    );
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            reference: Mutex::new(String::new()),
            signature: "sigLocked0000000000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            block_time: 0,
        }),
        live,
    );
    // Drive credit_solana_deposit through the verifier. The stub honors only
    // the locked 5_000-to-payTo figure; had verify used the live terms it
    // would have demanded the repriced recipient and dishonored.
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    let credit = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("the locked intent credits: {credit:?}");
    };
    assert!(receipt.is_new, "the credit ran fresh, not a replay");
    // The wallet now holds exactly the opened 1000 minor-credits — the locked
    // amount, not a repriced figure.
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1140: the customer picks the asset at open. `open_solana_deposit`
/// resolves the named whitelist key and locks *that asset's* mint, decimals
/// and rate into `meta` — the same `amount_minor` asks for 5_000 lamports
/// under `sol` but 1_000 USDC base units under `usdc`.
#[tokio::test]
async fn open_solana_deposit_locks_the_picked_asset() {
    let Some(db) = database("solana_asset_lock").await else {
        return;
    };
    let org = organization(&db, "asset-lock").await;
    let terms = multi_asset_terms();
    // USDC: 1000 minor-credits at parity = 1000 base units of the 6-decimal
    // mint — the meta locks the mint and the parity rate.
    let usdc_id = Uuid::new_v4();
    let (_, usdc_units, _) = db
        .open_solana_deposit(
            org,
            usdc_id,
            1000,
            &terms,
            Some("usdc"),
            &rate(&terms, Some("usdc")),
        )
        .await
        .unwrap()
        .expect("inserts");
    assert_eq!(usdc_units, 1_000);
    let meta: serde_json::Value =
        sqlx::query_scalar("SELECT meta FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(usdc_id)
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
    assert_eq!(meta["solLamports"].as_i64().unwrap(), 1_000);
    // SOL: the same credit amount asks for 5_000 lamports, `splToken` null.
    let sol_id = Uuid::new_v4();
    let (_, lamports, _) = db
        .open_solana_deposit(
            org,
            sol_id,
            1000,
            &terms,
            Some("sol"),
            &rate(&terms, Some("sol")),
        )
        .await
        .unwrap()
        .expect("inserts");
    assert_eq!(lamports, 5_000);
    let meta: serde_json::Value =
        sqlx::query_scalar("SELECT meta FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(sol_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(meta["asset"].as_str().unwrap(), "sol");
    assert!(meta["splToken"].is_null());
    // The deployment default (`usdc` here) resolves an `asset`-less open.
    let default_id = Uuid::new_v4();
    let (_, default_units, _) = db
        .open_solana_deposit(org, default_id, 1000, &terms, None, &rate(&terms, None))
        .await
        .unwrap()
        .expect("inserts");
    assert_eq!(default_units, 1_000);
    // An unknown key is InvalidInput — a whitelist miss never reprices
    // silently under another asset.
    let refused = db
        .open_solana_deposit(
            org,
            Uuid::new_v4(),
            1000,
            &terms,
            Some("doge"),
            // An off-whitelist open is refused before the rate is read — the
            // figure passed is inert.
            &LockedRate::Static { rate_micro: 1 },
        )
        .await;
    assert!(
        matches!(refused, Err(WalletError::InvalidInput(_))),
        "an off-whitelist asset must refuse: {refused:?}"
    );
}

/// Issue #1140: verify honors the asset the intent locked — a USDC intent
/// demands the USDC mint's base units on the USDC mint, not SOL lamports,
/// and a `meta.splToken` the whitelist does not carry is refused outright.
#[tokio::test]
async fn verify_locked_honors_the_locked_asset_and_refuses_off_whitelist() {
    let Some(db) = database("solana_asset_verify").await else {
        return;
    };
    let org = organization(&db, "asset-verify").await;
    let terms = multi_asset_terms();
    // A USDC intent: 1000 minor-credits at parity asks for 1_000 USDC base
    // units on the whitelisted mint.
    let deposit_id = Uuid::new_v4();
    let (reference, units, _) = db
        .open_solana_deposit(
            org,
            deposit_id,
            1000,
            &terms,
            Some("usdc"),
            &rate(&terms, Some("usdc")),
        )
        .await
        .unwrap()
        .expect("inserts");
    assert_eq!(units, 1_000);
    // The stub honors only a transfer that matches the locked mint — a SOL
    // lamport delta could never satisfy the SPL demand.
    #[derive(Debug)]
    struct UsdcSig {
        signature: String,
        delivered: i64,
    }
    impl ChainLookup for UsdcSig {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
            Box::pin(async move { vec![self.signature.clone()] })
        }
        fn validate_transfer<'a>(
            &'a self,
            signature: &'a str,
            expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>,
        > {
            Box::pin(async move {
                // Prove the payment — locked mint, located signature — and
                // report `delivered` unjudged for the verifier's per-asset
                // comparison (issue #1142).
                if signature != self.signature
                    || expected.spl_token_mint.as_deref()
                        != Some("usdcMint11111111111111111111111111111111")
                {
                    return None;
                }
                Some(ValidatedTransfer {
                    signature: signature.to_owned(),
                    delivered: self.delivered,
                    block_time: Some(0),
                    payers: Vec::new(),
                })
            })
        }
    }
    let verifier = SolanaRailVerifier::new(
        Arc::new(UsdcSig {
            signature: "sigUsdc000000000000000000000000000000000000000000000000000".to_owned(),
            delivered: 1_000,
        }),
        terms.clone(),
    );
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    let credit = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("the locked USDC intent credits: {credit:?}");
    };
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 1000);
    let _ = reference;

    // The defensive half: a stored meta whose `splToken` no configured asset
    // carries must refuse — the mint was never payable, so the intent cannot
    // settle. Written straight into a pending row's meta to simulate an
    // intent opened before the mint left the whitelist.
    let poisoned = Uuid::new_v4();
    db.open_solana_deposit(
        org,
        poisoned,
        1000,
        &terms,
        Some("usdc"),
        &rate(&terms, Some("usdc")),
    )
    .await
    .unwrap()
    .expect("inserts");
    sqlx::query(
        "UPDATE oxsum.deposits SET meta = meta || jsonb_build_object('splToken', 'aRogueMint') \
         WHERE deposit_id = $1",
    )
    .bind(poisoned)
    .execute(db.pool())
    .await
    .unwrap();
    let refused = db
        .credit_solana_deposit(&wallet, poisoned, &verifier, time::Date::MAX)
        .await;
    assert!(
        matches!(refused, Err(WalletError::Forbidden(_))),
        "an off-whitelist splToken must refuse: {refused:?}"
    );
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1139: an oracle-priced `sol` open locks the resolved `rateMicro`
/// into `meta` alongside the quote's own figures — `rateSource: "pyth"`, the
/// `pythPrice`/`pythPublishTime` — and `credit_solana_deposit` still settles
/// at the locked figure even when the verifier's live terms drift. The quote
/// conversion is `resolve_rate`'s job (unit-tested in core); this test feeds
/// the `LockedRate` `resolve_rate` would produce and proves the intent
/// records it and verifies under it.
#[tokio::test]
async fn oracle_priced_intent_locks_and_verifies_the_quote_rate() {
    let Some(db) = database("solana_oracle").await else {
        return;
    };
    let org = organization(&db, "oracle").await;
    // The `LockedRate::Oracle` resolve_rate answers for a USD 204.93/SOL
    // quote — the rate the open must lock, not the static 200_000_000.
    let oracle_rate = LockedRate::Oracle {
        rate_micro: 204_930_000,
        quote: oxsum_core::OracleQuote {
            price: 20_493_000_000,
            conf: 15_300_000,
            expo: -8,
            publish_time: 1_763_000_000,
        },
    };
    let deposit_id = Uuid::new_v4();
    let (_reference, lamports, _e) = db
        .open_solana_deposit(org, deposit_id, 1000, &terms(), None, &oracle_rate)
        .await
        .unwrap()
        .expect("inserts");
    // 1000 minor-credits at 204_930_000 minor/SOL = ceil(1000 * 1e9 /
    // 204_930_000) = 4_880 lamports — the oracle figure, not the static 5_000.
    assert_eq!(lamports, 4_880);
    let meta: serde_json::Value =
        sqlx::query_scalar("SELECT meta FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(meta["rateSource"].as_str().unwrap(), "pyth");
    assert_eq!(meta["rateMicro"].as_i64().unwrap(), 204_930_000);
    assert_eq!(meta["pythPrice"].as_str().unwrap(), "20493000000");
    assert_eq!(meta["pythPublishTime"].as_i64().unwrap(), 1_763_000_000);
    // The settle side reads only the locked rate: a verifier whose live terms
    // carry the static 200_000_000 still demands the oracle-priced 4_880
    // lamports the intent promised.
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            reference: Mutex::new(String::new()),
            signature: "sigOracle0000000000000000000000000000000000000000000000".to_owned(),
            delivered: 4_880,
            block_time: 0,
        }),
        terms(),
    );
    let wallet = wallet_for(&db, org).await;
    let verdict = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(
        matches!(verdict, SolanaCredit::Credited(_)),
        "the locked oracle rate credits: {verdict:?}"
    );
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1142's fixture: a wallet on the test org — the piece every
/// exception test needs.
async fn wallet_for(db: &Db, org: Uuid) -> oxsum_core::Wallet {
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap()
}

/// Opens a 1000-credit intent under `terms`/`asset` and runs
/// `credit_solana_deposit` against a `OneSig` answering
/// `delivered`/`block_time`. Returns the deposit id and the verdict.
async fn credit_with(
    db: &Db,
    wallet: &oxsum_core::Wallet,
    org: Uuid,
    terms: &SolanaTerms,
    asset: Option<&str>,
    delivered: i64,
    block_time: i64,
) -> (Uuid, SolanaCredit) {
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, terms, asset, &rate(terms, asset))
        .await
        .unwrap()
        .expect("inserts");
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            reference: Mutex::new(String::new()),
            signature: format!("sig{}", &Uuid::new_v4().simple().to_string()[..24]),
            delivered,
            block_time,
        }),
        terms.clone(),
    );
    let verdict = db
        .credit_solana_deposit(wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    (deposit_id, verdict)
}

/// Issue #1142 × #1140: a located transfer that delivers under the asked
/// amount marks `underpaid` with the verification figures on `meta` — and the
/// under/over verdict is in *the locked asset's* base units. Run for a `sol`
/// intent (lamports: 5_000 asked, 3_000 delivered) and a `usdc` one (1_000
/// mint units asked, 600 delivered): the same underpayment shape, two
/// different denominators — the per-asset expected figure is what makes a
/// short USDC payment `underpaid` rather than a SOL-denominated verdict.
#[tokio::test]
async fn an_under_delivered_transfer_marks_underpaid_with_the_figures() {
    let Some(db) = database("solana_underpaid").await else {
        return;
    };
    // (asset, delivered, expected base units, expected received_minor)
    for (asset, delivered, expected_units, expected_minor) in [
        (Some("sol"), 3_000_i64, 5_000_i64, 600_i64),
        (Some("usdc"), 600_i64, 1_000_i64, 600_i64),
    ] {
        let org = organization(&db, "underpaid").await;
        let wallet = wallet_for(&db, org).await;
        let terms = multi_asset_terms();
        let (deposit_id, verdict) =
            credit_with(&db, &wallet, org, &terms, asset, delivered, 0).await;
        assert!(matches!(verdict, SolanaCredit::Underpaid), "{verdict:?}");
        assert_eq!(wallet.available().await.unwrap(), 0);
        let row = sqlx::query(
            "SELECT status, received_minor, facilitator_tx_hash, meta \
             FROM oxsum.deposits WHERE deposit_id = $1",
        )
        .bind(deposit_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("status"), solana_status::UNDERPAID);
        // `received_minor` is the delivered figure converted back at the
        // locked rate: 3_000 lamports at 200M minor/SOL = 600 credits;
        // 600 USDC units at parity = 600 credits.
        assert_eq!(
            row.get::<Option<i64>, _>("received_minor"),
            Some(expected_minor)
        );
        assert!(
            row.get::<Option<String>, _>("facilitator_tx_hash")
                .is_some()
        );
        let meta = row.get::<serde_json::Value, _>("meta");
        assert_eq!(meta["delivered"].as_i64().unwrap(), delivered);
        assert_eq!(meta["expected"].as_i64().unwrap(), expected_units);
        assert_eq!(meta["exception"].as_str().unwrap(), "underpaid");
        // The row is on the exception queue the admin list reads — carrying
        // the locked asset so the operator knows the denomination.
        let queue = db
            .solana_exception_deposits(Some(solana_status::UNDERPAID), 10, None)
            .await
            .unwrap();
        let row = queue
            .iter()
            .find(|row| row.deposit_id == deposit_id)
            .expect("the marked deposit is on the queue");
        assert_eq!(row.status, solana_status::UNDERPAID);
        assert_eq!(row.asset.as_deref(), asset);
        assert_eq!(row.delivered, Some(delivered));
        assert_eq!(row.expected, Some(expected_units));
        assert_eq!(row.received_minor, Some(expected_minor));
        assert!(row.signature.is_some());
        // A second credit run is the settled no-op — the verdict stands, the
        // row does not re-mark.
        let verifier = SolanaRailVerifier::new(
            Arc::new(OneSig {
                reference: Mutex::new(String::new()),
                signature: "sigNeverConsulted000000000000000000".to_owned(),
                delivered: 9_000,
                block_time: 0,
            }),
            multi_asset_terms(),
        );
        let replay = db
            .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
            .await
            .unwrap();
        assert!(matches!(replay, SolanaCredit::Settled), "{replay:?}");
        assert_eq!(wallet.available().await.unwrap(), 0);
    }
}

/// Issue #1142: an over-delivery credits the opened amount once — `entry_id`
/// proves the credit — and the row marks `overpaid` so the surplus is visible
/// on the queue for a refund. For a `usdc` intent the surplus is judged in
/// USDC base units, not lamports.
#[tokio::test]
async fn an_over_delivered_transfer_credits_once_and_marks_overpaid() {
    let Some(db) = database("solana_overpaid").await else {
        return;
    };
    let org = organization(&db, "overpaid").await;
    let wallet = wallet_for(&db, org).await;
    // A USDC intent: 1000 credits at parity asks 1_000 mint units; the
    // transfer delivers 2_500.
    let terms = multi_asset_terms();
    let (deposit_id, verdict) =
        credit_with(&db, &wallet, org, &terms, Some("usdc"), 2_500, 0).await;
    let SolanaCredit::Credited(receipt) = verdict else {
        panic!("the over-delivery credits: {verdict:?}");
    };
    assert!(receipt.is_new);
    // The opened amount credits — never the surplus.
    assert_eq!(wallet.available().await.unwrap(), 1000);
    let row =
        sqlx::query("SELECT status, entry_id, meta FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(row.get::<String, _>("status"), solana_status::OVERPAID);
    assert!(row.get::<Option<Uuid>, _>("entry_id").is_some());
    let meta = row.get::<serde_json::Value, _>("meta");
    assert!(meta["overpaid"].as_bool().unwrap());
    assert_eq!(meta["delivered"].as_i64().unwrap(), 2_500);
    assert_eq!(meta["expected"].as_i64().unwrap(), 1_000);
    // The queue reads it as an exception with the entry that proves credit.
    let queue = db
        .solana_exception_deposits(Some(solana_status::OVERPAID), 10, None)
        .await
        .unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].asset.as_deref(), Some("usdc"));
    assert!(queue[0].entry_id.is_some());
    assert_eq!(queue[0].delivered, Some(2_500));
}

/// Issue #1186's stub: a `OneSig` whose validated transfer carries the payer
/// wallet the rail attributes — the shape a credit or exception mark needs to
/// merge `payerAddress` from.
#[derive(Debug)]
struct AttributedSig {
    signature: String,
    delivered: i64,
    payer: String,
}

impl ChainLookup for AttributedSig {
    fn find_reference<'a>(
        &'a self,
        _reference: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
        Box::pin(async move { vec![self.signature.clone()] })
    }
    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a ExpectedTransfer,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>>
    {
        Box::pin(async move {
            if signature != self.signature {
                return None;
            }
            // The transfer is the intent's payment; `delivered` rides up
            // unjudged so the verifier classifies an under/over-delivery the
            // way #1142's marks do.
            if expected.recipient != "oxsumDepositAddr111111111111111111111111111" {
                return None;
            }
            Some(ValidatedTransfer {
                signature: signature.to_owned(),
                delivered: self.delivered,
                block_time: Some(0),
                payers: vec![self.payer.clone()],
            })
        })
    }
}

/// Issue #1186: every terminal state merges the full verification record —
/// `payerAddress`, `delivered`, `expected`, `receivedCredits` — into `meta`
/// and sets `received_minor`, so the queue row and the CSV's `payer` column
/// attribute what the chain sent. An exact-pay `credited` row and a short
/// `underpaid` one prove the credit and exception paths write it alike.
#[tokio::test]
async fn every_terminal_meta_records_the_payer_and_figures() {
    let Some(db) = database("solana_terminal_meta").await else {
        return;
    };
    let org = organization(&db, "terminal-meta").await;
    let wallet = wallet_for(&db, org).await;
    let terms = terms();
    let payer = "attributedPayerWallet777777777777777777777";

    // The credit path: an exact 5_000-lamport pay on the 1000-credit intent
    // lands `credited` with the payer and figures on `meta`.
    let credited_id = Uuid::new_v4();
    db.open_solana_deposit(org, credited_id, 1000, &terms, None, &rate(&terms, None))
        .await
        .unwrap()
        .expect("inserts");
    let verifier = SolanaRailVerifier::new(
        Arc::new(AttributedSig {
            signature: "sigCreditMeta000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payer: payer.to_owned(),
        }),
        terms.clone(),
    );
    let verdict = db
        .credit_solana_deposit(&wallet, credited_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(matches!(verdict, SolanaCredit::Credited(_)), "{verdict:?}");
    let row = sqlx::query(
        "SELECT status, received_minor, meta FROM oxsum.deposits WHERE deposit_id = $1",
    )
    .bind(credited_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "credited");
    assert_eq!(row.get::<Option<i64>, _>("received_minor"), Some(1_000));
    let meta = row.get::<serde_json::Value, _>("meta");
    assert_eq!(meta["payerAddress"].as_str().unwrap(), payer);
    assert_eq!(meta["delivered"].as_i64().unwrap(), 5_000);
    assert_eq!(meta["expected"].as_i64().unwrap(), 5_000);
    assert_eq!(meta["receivedCredits"].as_i64().unwrap(), 1_000);

    // The exception path: a 3_000-lamport delivery on the same intent marks
    // `underpaid` — the same `meta` record plus `exception`, and the queue
    // row carries the payer the CSV exports.
    let underpaid_id = Uuid::new_v4();
    db.open_solana_deposit(org, underpaid_id, 1000, &terms, None, &rate(&terms, None))
        .await
        .unwrap()
        .expect("inserts");
    let short = SolanaRailVerifier::new(
        Arc::new(AttributedSig {
            signature: "sigUnderMeta0000000000000000000000000000000000000".to_owned(),
            delivered: 3_000,
            payer: payer.to_owned(),
        }),
        terms,
    );
    let verdict = db
        .credit_solana_deposit(&wallet, underpaid_id, &short, time::Date::MAX)
        .await
        .unwrap();
    assert!(matches!(verdict, SolanaCredit::Underpaid), "{verdict:?}");
    let row = sqlx::query(
        "SELECT status, received_minor, meta FROM oxsum.deposits WHERE deposit_id = $1",
    )
    .bind(underpaid_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), solana_status::UNDERPAID);
    assert_eq!(row.get::<Option<i64>, _>("received_minor"), Some(600));
    let meta = row.get::<serde_json::Value, _>("meta");
    assert_eq!(meta["payerAddress"].as_str().unwrap(), payer);
    assert_eq!(meta["delivered"].as_i64().unwrap(), 3_000);
    assert_eq!(meta["expected"].as_i64().unwrap(), 5_000);
    assert_eq!(meta["receivedCredits"].as_i64().unwrap(), 600);
    assert_eq!(meta["exception"].as_str().unwrap(), "underpaid");
    // The queue row surfaces `payer` — the field the CSV's `payer` column
    // serializes — on the exception deposits (`credited` is terminal, not an
    // exception, so the windowed CSV export is what reads its `payer`).
    let queue = db.solana_exception_deposits(None, 10, None).await.unwrap();
    let underpaid = queue
        .iter()
        .find(|row| row.deposit_id == underpaid_id)
        .expect("the underpaid deposit is on the queue");
    assert_eq!(underpaid.payer.as_deref(), Some(payer));
    assert_eq!(underpaid.delivered, Some(3_000));
    assert_eq!(underpaid.received_minor, Some(600));
}

/// Issue #1142: the refund state machine — `underpaid → refund_pending →
/// (address) → approved → refunded` — with each order violation refused and
/// `refunded` terminal. Driven at the Db layer where the transitions live.
#[tokio::test]
async fn the_refund_walks_pending_address_approve_complete() {
    let Some(db) = database("solana_refund").await else {
        return;
    };
    let org = organization(&db, "refund").await;
    let wallet = wallet_for(&db, org).await;
    let terms = terms();
    let (deposit_id, _) = credit_with(&db, &wallet, org, &terms, None, 3_000, 0).await;
    // Open: underpaid → refund_pending, recording who/why.
    let opened = db
        .solana_request_refund(deposit_id, "operator", "under-delivered")
        .await
        .unwrap();
    assert_eq!(opened.status, solana_status::REFUND_PENDING);
    let refund = opened.refund.expect("meta.refund is recorded");
    assert_eq!(refund["requestedBy"].as_str().unwrap(), "operator");
    assert_eq!(refund["reason"].as_str().unwrap(), "under-delivered");
    assert!(refund["requestedAt"].is_string());
    // Approving with no address is refused.
    let early = db.solana_approve_refund(deposit_id, "operator", None).await;
    assert!(matches!(early, Err(WalletError::Conflict(_))), "{early:?}");
    // The customer names the refund address — validated base58-32.
    let bad = db
        .solana_set_refund_address(org, deposit_id, "not-base58!!")
        .await;
    assert!(matches!(bad, Err(WalletError::InvalidInput(_))), "{bad:?}");
    let addressed = db
        .solana_set_refund_address(
            org,
            deposit_id,
            "4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM",
        )
        .await
        .unwrap();
    assert_eq!(
        addressed.refund_address.as_deref(),
        Some("4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM")
    );
    // Another organization's write is a `NotFound`, not a leak.
    let other = organization(&db, "refund-other").await;
    let foreign = db
        .solana_set_refund_address(
            other,
            deposit_id,
            "4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM",
        )
        .await;
    assert!(
        matches!(foreign, Err(WalletError::NotFound(_))),
        "{foreign:?}"
    );
    // Completing before approval is refused.
    let payout =
        "299xJhoRJCHgYo6xxU78Joi8sY7uXrsp7kzXMxbVFZVLKHqVAfV1R3dA864pwEhe6z1wYWy1BXDzyvzNZDKHfdMd";
    let unapproved = db.solana_complete_refund(deposit_id, payout).await;
    assert!(
        matches!(unapproved, Err(WalletError::Conflict(_))),
        "{unapproved:?}"
    );
    // Approve, then the address freezes.
    let approved = db
        .solana_approve_refund(deposit_id, "operator", None)
        .await
        .unwrap();
    assert!(approved.refund.unwrap()["approvedAt"].is_string());
    let frozen = db
        .solana_set_refund_address(org, deposit_id, "11111111111111111111111111111111")
        .await;
    assert!(
        matches!(frozen, Err(WalletError::Conflict(_))),
        "{frozen:?}"
    );
    // Complete: refunded with the payout signature — terminal.
    let closed = db.solana_complete_refund(deposit_id, payout).await.unwrap();
    assert_eq!(closed.status, solana_status::REFUNDED);
    assert_eq!(closed.refund_signature.as_deref(), Some(payout));
    // A replayed complete with the standing signature answers the row; a
    // contradicting signature is the audit-protecting Conflict.
    let replay = db.solana_complete_refund(deposit_id, payout).await.unwrap();
    assert_eq!(replay.status, solana_status::REFUNDED);
    let contradiction = db
        .solana_complete_refund(
            deposit_id,
            "1111111111111111111111111111111111111111111111111111111111111111",
        )
        .await;
    assert!(
        matches!(contradiction, Err(WalletError::Conflict(_))),
        "{contradiction:?}"
    );
    // And no credit ever ran — the customer's money returns on-chain.
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// Issue #1142: a refund opens only on the refundable statuses. `pending`,
/// `credited`, `expired`, `refund_pending`-replays and `refunded` all refuse —
/// `Conflict` (or a replay for an already-open refund), never a reset.
#[tokio::test]
async fn a_refund_opens_only_on_the_refundable_statuses() {
    let Some(db) = database("solana_refund_gate").await else {
        return;
    };
    let org = organization(&db, "refund-gate").await;
    let wallet = wallet_for(&db, org).await;
    let terms = terms();
    // `pending` refuses.
    let pending = Uuid::new_v4();
    db.open_solana_deposit(org, pending, 1000, &terms, None, &rate(&terms, None))
        .await
        .unwrap()
        .expect("inserts");
    let refused = db.solana_request_refund(pending, "op", "no").await;
    assert!(
        matches!(refused, Err(WalletError::Conflict(_))),
        "{refused:?}"
    );
    // `credited` (exact pay) refuses — nothing is owed.
    let (credited_id, verdict) = credit_with(&db, &wallet, org, &terms, None, 5_000, 0).await;
    assert!(matches!(verdict, SolanaCredit::Credited(_)));
    let refused = db.solana_request_refund(credited_id, "op", "no").await;
    assert!(
        matches!(refused, Err(WalletError::Conflict(_))),
        "{refused:?}"
    );
    // An open refund replays rather than re-entering — and `refunded` is a
    // terminal `Conflict`.
    let (underpaid_id, _) = credit_with(&db, &wallet, org, &terms, None, 3_000, 0).await;
    db.solana_request_refund(underpaid_id, "op", "first")
        .await
        .unwrap();
    let replay = db
        .solana_request_refund(underpaid_id, "op", "again")
        .await
        .unwrap();
    assert_eq!(replay.status, solana_status::REFUND_PENDING);
    assert_eq!(
        replay.refund.unwrap()["reason"].as_str().unwrap(),
        "first",
        "the replay answers the standing refund, not a rewrite"
    );
    // `flagged` — the #1144 screening status — is refundable: the refund
    // lifecycle predates the writer that lands rows there.
    let flagged = Uuid::new_v4();
    db.open_solana_deposit(org, flagged, 1000, &terms, None, &rate(&terms, None))
        .await
        .unwrap()
        .expect("inserts");
    sqlx::query("UPDATE oxsum.deposits SET status = 'flagged' WHERE deposit_id = $1")
        .bind(flagged)
        .execute(db.pool())
        .await
        .unwrap();
    let opened = db
        .solana_request_refund(flagged, "op", "screening hold")
        .await
        .unwrap();
    assert_eq!(opened.status, solana_status::REFUND_PENDING);
    // Unknown deposits are `NotFound`.
    let missing = db.solana_request_refund(Uuid::new_v4(), "op", "x").await;
    assert!(
        matches!(missing, Err(WalletError::NotFound(_))),
        "{missing:?}"
    );
}

/// Issue #1142: the queue filters and the org-scoped list only sees its own
/// rows — the two reads the admin table and the customer Topup surface make.
#[tokio::test]
async fn the_exception_queue_filters_by_status_and_scopes_by_org() {
    let Some(db) = database("solana_queue").await else {
        return;
    };
    let org = organization(&db, "queue").await;
    let wallet = wallet_for(&db, org).await;
    let terms = terms();
    let (underpaid_id, _) = credit_with(&db, &wallet, org, &terms, None, 3_000, 0).await;
    let (overpaid_id, _) = credit_with(&db, &wallet, org, &terms, None, 9_000, 0).await;
    // `None` lists every exception status; the filter narrows to one.
    let all = db.solana_exception_deposits(None, 10, None).await.unwrap();
    assert_eq!(all.len(), 2);
    let only_overpaid = db
        .solana_exception_deposits(Some(solana_status::OVERPAID), 10, None)
        .await
        .unwrap();
    assert_eq!(only_overpaid.len(), 1);
    assert_eq!(only_overpaid[0].deposit_id, overpaid_id);
    // The org-scoped list is the same rows through the customer's lens.
    let mine = db.solana_exception_deposits_for(org, 10).await.unwrap();
    assert_eq!(mine.len(), 2);
    // Another org sees none of them.
    let other = organization(&db, "queue-other").await;
    let theirs = db.solana_exception_deposits_for(other, 10).await.unwrap();
    assert!(theirs.is_empty());
    // `solana_deposit_for` answers one org-scoped row and refuses the leak.
    let row = db.solana_deposit_for(org, underpaid_id).await.unwrap();
    assert!(row.is_some());
    assert!(
        db.solana_deposit_for(other, underpaid_id)
            .await
            .unwrap()
            .is_none()
    );
}

/// Issue #1144: a `Flagged` verdict parks the deposit — `pending → flagged`,
/// `meta.screening` records the case, the wallet never credits, and the row
/// lands on the operator's `admin_flagged_solana_deposits` queue. The stub
/// screen names the verdict by address the way the production composite does.
#[derive(Debug)]
struct FixedScreen {
    verdict: ScreenVerdict,
}

impl SanctionsScreen for FixedScreen {
    fn screen<'a>(
        &'a self,
        _address: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ScreenVerdict> + Send + 'a>> {
        let verdict = self.verdict.clone();
        Box::pin(async move { verdict })
    }
}

/// A per-address screen (issue #1178): `verdicts` maps a payer address to the
/// verdict `screen` answers, an unmapped address answers `Clear` — the shape
/// a multi-party transfer needs, where the screen flags the second debited
/// signer while the fee payer is clean.
#[derive(Debug)]
struct MapScreen {
    verdicts: std::collections::HashMap<String, ScreenVerdict>,
    /// The addresses `screen` was asked, in order — the test asserts the walk
    /// covered every debited party.
    asked: Mutex<Vec<String>>,
}

impl SanctionsScreen for MapScreen {
    fn screen<'a>(
        &'a self,
        address: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ScreenVerdict> + Send + 'a>> {
        let verdict = self
            .verdicts
            .get(address)
            .cloned()
            .unwrap_or(ScreenVerdict::Clear);
        Box::pin(async move {
            self.asked.lock().unwrap().push(address.to_owned());
            verdict
        })
    }
}

/// A `OneSig` that answers a `ValidatedTransfer` carrying the debited funding
/// parties the screening path runs on (issues #1144, #1178).
/// `validate_transfer` honors the same terms the deposit's stub does.
#[derive(Debug)]
struct PayingSig {
    signature: String,
    delivered: i64,
    payers: Vec<String>,
}

impl ChainLookup for PayingSig {
    fn find_reference<'a>(
        &'a self,
        _reference: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
        Box::pin(async move { vec![self.signature.clone()] })
    }
    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a ExpectedTransfer,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>>
    {
        Box::pin(async move {
            if signature != self.signature || expected.amount != self.delivered {
                return None;
            }
            Some(ValidatedTransfer {
                signature: signature.to_owned(),
                delivered: self.delivered,
                block_time: Some(0),
                payers: self.payers.clone(),
            })
        })
    }
}

#[tokio::test]
async fn a_flagged_payer_parks_the_deposit_for_review() {
    let Some(db) = database("solana_flagged").await else {
        return;
    };
    let org = organization(&db, "flagged").await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    let verifier = SolanaRailVerifier::new(
        Arc::new(PayingSig {
            signature: "sigFlagCore000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: vec!["sanctionedPayerAddr444444444444444444444444".to_owned()],
        }),
        terms(),
    )
    .with_sanctions(Arc::new(FixedScreen {
        verdict: ScreenVerdict::Flagged {
            source: "ofac-sdn".to_owned(),
            reason: "the payer is on the OFAC SDN list".to_owned(),
        },
    }));
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    // The flag parks: no credit, and the verdict names itself — `Flagged`,
    // not the `Settled` that would fold the hold into "nothing to answer"
    // and leave the confirm a misleading 404 (issue #1184).
    let result = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(
        matches!(result, SolanaCredit::Flagged),
        "a flagged deposit never credits: {result:?}"
    );
    // A replayed walk over the parked row answers the same named verdict.
    let replay = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(
        matches!(replay, SolanaCredit::Flagged),
        "a flagged deposit stays a named answer: {replay:?}"
    );
    assert_eq!(wallet.available().await.unwrap(), 0);
    let row = sqlx::query(
        "SELECT status, facilitator_tx_hash, received_minor, meta \
         FROM oxsum.deposits WHERE deposit_id = $1",
    )
    .bind(deposit_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "flagged");
    assert_eq!(
        row.get::<Option<String>, _>("facilitator_tx_hash")
            .as_deref(),
        Some("sigFlagCore000000000000000000000000000000000000000000")
    );
    let meta = row.get::<serde_json::Value, _>("meta");
    assert_eq!(meta["screening"]["verdict"].as_str().unwrap(), "flagged");
    assert_eq!(meta["screening"]["source"].as_str().unwrap(), "ofac-sdn");
    assert_eq!(
        meta["screening"]["address"].as_str().unwrap(),
        "sanctionedPayerAddr444444444444444444444444"
    );
    // The flagged row records the same terminal figures every other mark
    // does (issue #1186): the parked transfer's delivered/expected in the
    // locked asset's base units, `received_minor` its worth in minor-credits,
    // and `payerAddress` the attributed wallet the queue reads back.
    assert_eq!(meta["delivered"].as_i64().unwrap(), 5_000);
    assert_eq!(meta["expected"].as_i64().unwrap(), 5_000);
    assert_eq!(meta["receivedCredits"].as_i64().unwrap(), 1_000);
    assert_eq!(
        meta["payerAddress"].as_str().unwrap(),
        "sanctionedPayerAddr444444444444444444444444"
    );
    assert_eq!(row.get::<Option<i64>, _>("received_minor"), Some(1_000));
    // The operator queue reads it back — the DB-level half #1142's fuller
    // exception queue lands on.
    let queue = db.admin_flagged_solana_deposits(50).await.unwrap();
    let listed = queue.iter().find(|d| d.deposit_id == deposit_id);
    let listed = listed.expect("the flagged row is on the queue");
    assert_eq!(listed.status, "flagged");
    let screening = listed
        .screening
        .as_ref()
        .expect("the row carries its verdict");
    assert_eq!(
        screening["address"].as_str().unwrap(),
        "sanctionedPayerAddr444444444444444444444444"
    );
    // The queue never leaks a pending or credited row.
    assert!(
        queue
            .iter()
            .all(|d| d.status == "flagged" && d.rail == "solana")
    );
    // The exception-queue row carries the attributed payer the CSV exports
    // (issue #1186) — `meta.payerAddress` merged by the flag write.
    let exceptions = db
        .solana_exception_deposits(Some(solana_status::FLAGGED), 10, None)
        .await
        .unwrap();
    let flagged = exceptions
        .iter()
        .find(|row| row.deposit_id == deposit_id)
        .expect("the flagged deposit is on the exception queue");
    assert_eq!(
        flagged.payer.as_deref(),
        Some("sanctionedPayerAddr444444444444444444444444")
    );
    assert_eq!(flagged.delivered, Some(5_000));
    assert_eq!(flagged.expected, Some(5_000));
    assert_eq!(flagged.received_minor, Some(1_000));
}

/// Issue #1178: screening covers *every* debited funding party, not just the
/// first the transaction names. A clean fee-paying relayer plus a second
/// debited signer on the SDN — the laundering shape — still parks the
/// deposit: the screen clears the relayer and flags the source.
#[tokio::test]
async fn a_flagged_funding_source_parks_the_deposit() {
    let Some(db) = database("solana_flag_source").await else {
        return;
    };
    let org = organization(&db, "flag-source").await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    // The validated transfer debited two parties: the fee-paying relayer the
    // old code screened (clean) and the funding source the fix adds — the
    // sanctioned wallet the relayer was hiding.
    let screen = Arc::new(MapScreen {
        verdicts: [(
            "sdnSourceAddr7777777777777777777777777777".to_owned(),
            ScreenVerdict::Flagged {
                source: "ofac-sdn".to_owned(),
                reason: "the funding source is on the OFAC SDN list".to_owned(),
            },
        )]
        .into_iter()
        .collect(),
        asked: Mutex::new(Vec::new()),
    });
    let verifier = SolanaRailVerifier::new(
        Arc::new(PayingSig {
            signature: "sigLaunder000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: vec![
                "cleanRelayerAddr6666666666666666666666666".to_owned(),
                "sdnSourceAddr7777777777777777777777777777".to_owned(),
            ],
        }),
        terms(),
    )
    .with_sanctions(screen.clone());
    let wallet = wallet_for(&db, org).await;
    let result = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(
        matches!(result, SolanaCredit::Flagged),
        "a flagged funding party never credits: {result:?}"
    );
    assert_eq!(wallet.available().await.unwrap(), 0);
    // The walk asked the screen about *both* debited parties — the relayer
    // could not launder the source out of the check.
    let asked = screen.asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        vec![
            "cleanRelayerAddr6666666666666666666666666".to_owned(),
            "sdnSourceAddr7777777777777777777777777777".to_owned(),
        ]
    );
    let row = sqlx::query("SELECT status, meta FROM oxsum.deposits WHERE deposit_id = $1")
        .bind(deposit_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("status"), "flagged");
    let meta = row.get::<serde_json::Value, _>("meta");
    assert_eq!(meta["screening"]["verdict"].as_str().unwrap(), "flagged");
    // `screening.address` keeps the representative (the fee payer — first in
    // key order); `screening.addresses` carries the whole screened set.
    assert_eq!(
        meta["screening"]["addresses"][1].as_str().unwrap(),
        "sdnSourceAddr7777777777777777777777777777"
    );
}

/// Issue #1178: a `Flagged` on one party wins over a `TemporarilyUnavailable`
/// on another — the deposit parks rather than deferring — while an
/// unavailable-only walk still defers (never credits unscreened money).
#[tokio::test]
async fn a_flagged_party_wins_over_an_unavailable_one() {
    let Some(db) = database("solana_flag_over_unavail").await else {
        return;
    };
    let org = organization(&db, "flag-unavail").await;
    let wallet = wallet_for(&db, org).await;
    // The fee payer's screen is unreachable; the source is flagged — the
    // flag parks, the deferral does not shield it.
    let flagged = Uuid::new_v4();
    db.open_solana_deposit(org, flagged, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    let screen = Arc::new(MapScreen {
        verdicts: [
            (
                "unavailPayerAddr88888888888888888888888888".to_owned(),
                ScreenVerdict::TemporarilyUnavailable,
            ),
            (
                "flaggedSourceAddr9999999999999999999999999".to_owned(),
                ScreenVerdict::Flagged {
                    source: "chainalysis".to_owned(),
                    reason: "chainalysis listed the address".to_owned(),
                },
            ),
        ]
        .into_iter()
        .collect(),
        asked: Mutex::new(Vec::new()),
    });
    let verifier = SolanaRailVerifier::new(
        Arc::new(PayingSig {
            signature: "sigFlagUnav000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: vec![
                "unavailPayerAddr88888888888888888888888888".to_owned(),
                "flaggedSourceAddr9999999999999999999999999".to_owned(),
            ],
        }),
        terms(),
    )
    .with_sanctions(screen);
    let result = db
        .credit_solana_deposit(&wallet, flagged, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(
        matches!(result, SolanaCredit::Flagged),
        "a flagged party parks even when another cannot be screened: {result:?}"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(flagged)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "flagged");
    assert_eq!(wallet.available().await.unwrap(), 0);

    // An unavailable answer on one party with no flag anywhere defers — the
    // row stays pending and no credit runs.
    let deferred = Uuid::new_v4();
    db.open_solana_deposit(org, deferred, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    let deferred_verifier = SolanaRailVerifier::new(
        Arc::new(PayingSig {
            signature: "sigDefer000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: vec![
                "cleanPayerAddr0000000000000000000000000000".to_owned(),
                "unavailPayerAddr88888888888888888888888888".to_owned(),
            ],
        }),
        terms(),
    )
    .with_sanctions(Arc::new(MapScreen {
        verdicts: [(
            "unavailPayerAddr88888888888888888888888888".to_owned(),
            ScreenVerdict::TemporarilyUnavailable,
        )]
        .into_iter()
        .collect(),
        asked: Mutex::new(Vec::new()),
    }));
    let result = db
        .credit_solana_deposit(&wallet, deferred, &deferred_verifier, time::Date::MAX)
        .await;
    assert!(
        matches!(result, Err(WalletError::Forbidden(_))),
        "an unavailable-only walk defers: {result:?}"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deferred)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(wallet.available().await.unwrap(), 0);
}

#[tokio::test]
async fn an_unavailable_screen_leaves_the_deposit_pending() {
    let Some(db) = database("solana_screen_unavail").await else {
        return;
    };
    let org = organization(&db, "unavail").await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    let verifier = SolanaRailVerifier::new(
        Arc::new(PayingSig {
            signature: "sigUnavCore00000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: vec!["retryPayerAddr55555555555555555555555555".to_owned()],
        }),
        terms(),
    )
    .with_sanctions(Arc::new(FixedScreen {
        verdict: ScreenVerdict::TemporarilyUnavailable,
    }));
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    // Deny-until-cleared: the deposit refuses Forbidden and stays pending —
    // the next pass retries the screen rather than crediting unscreened.
    let result = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await;
    assert!(
        matches!(result, Err(WalletError::Forbidden(_))),
        "an unavailable screen refuses the credit: {result:?}"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// Issue #1174: the expiry sweep's "unpaid" verdict can go stale — the row
/// flips `expired` while its payment was already on-chain before
/// `expiresAt` (the 257th intent past a pass's `LIMIT 256`, a row a second
/// unleadered instance was mid-verify on, or a transfer that landed between
/// the verify and the sweep). `credit_solana_deposit` must still see the
/// row: an on-time payment rescues into the ordinary credit walk, a late one
/// queues `late`, and a still-unpaid row keeps `expired`.
#[tokio::test]
async fn an_expired_row_still_credits_an_on_time_payment() {
    let Some(db) = database("solana_expired_rescue").await else {
        return;
    };
    let org = organization(&db, "expired-rescue").await;
    let wallet = wallet_for(&db, org).await;
    // A lapsed intent (block_time 0 is always before any real expiresAt) the
    // sweep already expired — the stale-verdict shape.
    let short = SolanaTerms {
        intent_ttl_secs: 1,
        ..terms()
    };
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &short, None, &rate(&short, None))
        .await
        .unwrap()
        .expect("inserts");
    db.expire_solana_deposit(deposit_id).await.unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "expired");
    // The paying transaction's blockTime precedes expiresAt — the credit
    // books through the normal walk, the row lands `credited`.
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            reference: Mutex::new(String::new()),
            signature: "sigRescued00000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            block_time: 0,
        }),
        short,
    );
    let credit = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    let SolanaCredit::Credited(receipt) = credit else {
        panic!("an on-time payment credits out of `expired`: {credit:?}");
    };
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 1000);
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "credited");
}

/// Issue #1184: the review-then-credit path — a `flagged` deposit the
/// operator cleared credits under the already-verified terms, the same
/// `solana:<signature>` key the ordinary credit would have booked, and a
/// replayed clear answers the standing credited row instead of crediting
/// twice or refusing.
#[tokio::test]
async fn a_cleared_flagged_deposit_credits_exactly_once() {
    let Some(db) = database("solana_clear").await else {
        return;
    };
    let org = organization(&db, "clear").await;
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let wallet = wallet_for(&db, org).await;
    // Park a deposit the way the screen does: a flagged row carrying the
    // verified payment's signature and figures.
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    let signature = "sigClearCore0000000000000000000000000000000000000000000";
    sqlx::query(
        "UPDATE oxsum.deposits \
         SET status = 'flagged', facilitator_tx_hash = $2, \
             meta = meta || jsonb_build_object( \
                 'screening', jsonb_build_object( \
                     'verdict', 'flagged', 'source', 'ofac-sdn', \
                     'reason', 'hit', 'address', 'flaggedPayer', 'at', 'now'), \
                 'signature', $2::text, \
                 'delivered', 5000::bigint, \
                 'expected', 5000::bigint, \
                 'receivedCredits', 1000::bigint) \
         WHERE deposit_id = $1",
    )
    .bind(deposit_id)
    .bind(signature)
    .execute(db.pool())
    .await
    .unwrap();
    // The clear credits: status `credited`, the entry is set, and
    // `meta.screening` attributes the operator's review.
    let cleared = db
        .solana_screening_clear(
            &tenants,
            deposit_id,
            "operator",
            "manual review passed",
            time::Date::MAX,
        )
        .await
        .unwrap();
    assert_eq!(cleared.status, "credited");
    assert!(cleared.entry_id.is_some(), "{cleared:?}");
    assert_eq!(wallet.available().await.unwrap(), 1000);
    let meta: serde_json::Value =
        sqlx::query_scalar("SELECT meta FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
/// is at/after `expiresAt` marks `late` on the exception queue (the verdict
/// belongs to the chain's clock even out of `expired`), and a still-unpaid
/// row keeps `expired` — the sweep's verdict stands.
#[tokio::test]
async fn an_expired_row_marks_late_and_stays_expired_unpaid() {
    let Some(db) = database("solana_expired_verdicts").await else {
        return;
    };
    let org = organization(&db, "expired-verdicts").await;
    let wallet = wallet_for(&db, org).await;
    let short = SolanaTerms {
        intent_ttl_secs: 1,
        ..terms()
    };
    // Late payment on an expired row → `late` on the queue, no credit.
    let late_id = Uuid::new_v4();
    let (_, _, expires_at) = db
        .open_solana_deposit(org, late_id, 1000, &short, None, &rate(&short, None))
        .await
        .unwrap()
        .expect("inserts");
    let expiry =
        time::OffsetDateTime::parse(&expires_at, &time::format_description::well_known::Rfc3339)
            .unwrap()
            .unix_timestamp();
    db.expire_solana_deposit(late_id).await.unwrap();
    let late_verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            reference: Mutex::new(String::new()),
            signature: "sigLateExpired000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            block_time: expiry + 60,
        }),
        short.clone(),
    );
    let verdict = db
        .credit_solana_deposit(&wallet, late_id, &late_verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(
        matches!(verdict, SolanaCredit::Late),
        "a late payment on an expired row queues late: {verdict:?}"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(late_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, solana_status::LATE);
    assert_eq!(wallet.available().await.unwrap(), 0);
    // Still unpaid on an expired row → Forbidden, `expired` stands.
    let unpaid_id = Uuid::new_v4();
    db.open_solana_deposit(org, unpaid_id, 1000, &short, None, &rate(&short, None))
        .await
        .unwrap()
        .expect("inserts");
    db.expire_solana_deposit(unpaid_id).await.unwrap();
    #[derive(Debug)]
    struct NoSigs;
    impl ChainLookup for NoSigs {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
            Box::pin(async { Vec::new() })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<ValidatedTransfer>> + Send + 'a>,
        > {
            Box::pin(async { None })
        }
    }
    let unpaid_verifier = SolanaRailVerifier::new(Arc::new(NoSigs), short);
    let refused = db
        .credit_solana_deposit(&wallet, unpaid_id, &unpaid_verifier, time::Date::MAX)
        .await;
    assert!(
        matches!(refused, Err(WalletError::Forbidden(_))),
        "a still-unpaid expired row refuses: {refused:?}"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(unpaid_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "expired", "the sweep's unpaid verdict stands");
    let screening = &meta["screening"];
    // The flag's own verdict stays for the audit trail; the clear appends.
    assert_eq!(screening["verdict"].as_str().unwrap(), "flagged");
    assert_eq!(screening["clearedBy"].as_str().unwrap(), "operator");
    assert_eq!(
        screening["clearReason"].as_str().unwrap(),
        "manual review passed"
    );
    assert!(screening["cleared"].as_bool().unwrap());
    // A replayed clear answers the standing credited row — never a second
    // credit, never a refusal.
    let replay = db
        .solana_screening_clear(
            &tenants,
            deposit_id,
            "operator",
            "manual review passed",
            time::Date::MAX,
        )
        .await
        .unwrap();
    assert_eq!(replay.status, "credited");
    assert_eq!(replay.entry_id, cleared.entry_id);
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1184: an over-delivered flagged transfer still lands `overpaid`
/// on its clear — the recorded `delivered > expected` verdict the ordinary
/// credit path would have reached on the same day.
#[tokio::test]
async fn a_cleared_flagged_deposit_keeps_the_overpaid_verdict() {
    let Some(db) = database("solana_clear_over").await else {
        return;
    };
    let org = organization(&db, "clear-over").await;
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let wallet = wallet_for(&db, org).await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    sqlx::query(
        "UPDATE oxsum.deposits \
         SET status = 'flagged', facilitator_tx_hash = $2, \
             meta = meta || jsonb_build_object( \
                 'signature', $2::text, \
                 'delivered', 9000::bigint, \
                 'expected', 5000::bigint, \
                 'receivedCredits', 1800::bigint) \
         WHERE deposit_id = $1",
    )
    .bind(deposit_id)
    .bind("sigClearOver000000000000000000000000000000000000000000")
    .execute(db.pool())
    .await
    .unwrap();
    let cleared = db
        .solana_screening_clear(
            &tenants,
            deposit_id,
            "operator",
            "reviewed",
            time::Date::MAX,
        )
        .await
        .unwrap();
    assert_eq!(cleared.status, "overpaid");
    // The credit books the opened amount — the surplus stays the queue's
    // concern, exactly as the same-day overpaid credit does.
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1184: the clear is `flagged`-only — a `pending`, `credited` or
/// `underpaid` row refuses `Conflict`, an unknown id a `NotFound`, and an
/// empty reason an `InvalidInput`: a clear with no recorded reasoning is
/// not a review.
#[tokio::test]
async fn screening_clear_refuses_a_non_flagged_deposit() {
    let Some(db) = database("solana_clear_refuse").await else {
        return;
    };
    let org = organization(&db, "clear-refuse").await;
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let wallet = wallet_for(&db, org).await;
    // A pending intent has no hold to clear.
    let pending_id = Uuid::new_v4();
    db.open_solana_deposit(org, pending_id, 1000, &terms(), None, &rate(&terms(), None))
        .await
        .unwrap()
        .expect("inserts");
    let pending = db
        .solana_screening_clear(&tenants, pending_id, "operator", "x", time::Date::MAX)
        .await;
    assert!(
        matches!(pending, Err(WalletError::Conflict(_))),
        "{pending:?}"
    );
    // A credited row without `screening.cleared` is not this path's replay.
    let (credited_id, _) = credit_with(&db, &wallet, org, &terms(), None, 5_000, 0).await;
    let credited = db
        .solana_screening_clear(&tenants, credited_id, "operator", "x", time::Date::MAX)
        .await;
    assert!(
        matches!(credited, Err(WalletError::Conflict(_))),
        "{credited:?}"
    );
    // An unknown deposit is a NotFound.
    let missing = db
        .solana_screening_clear(&tenants, Uuid::new_v4(), "operator", "x", time::Date::MAX)
        .await;
    assert!(
        matches!(missing, Err(WalletError::NotFound(_))),
        "{missing:?}"
    );
    // And a clear always carries its reason.
    let reasonless = db
        .solana_screening_clear(&tenants, pending_id, "operator", "  ", time::Date::MAX)
        .await;
    assert!(
        matches!(reasonless, Err(WalletError::InvalidInput(_))),
        "{reasonless:?}"
    );
}

/// Issue #1184: the approval binds to the address the caller screened — a
/// customer edit that lands between the screen and the approval refuses
/// `Conflict`, and a matching screen records its verdict on `meta.refund`.
#[tokio::test]
async fn solana_approve_refund_binds_to_the_screened_address() {
    let Some(db) = database("solana_approve_screen").await else {
        return;
    };
    let org = organization(&db, "approve-screen").await;
    let wallet = wallet_for(&db, org).await;
    let (deposit_id, _) = credit_with(&db, &wallet, org, &terms(), None, 3_000, 0).await;
    db.solana_request_refund(deposit_id, "operator", "under-delivered")
        .await
        .unwrap();
    let address = "4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM";
    db.solana_set_refund_address(org, deposit_id, address)
        .await
        .unwrap();
    // A screen against a different address does not approve the standing
    // one — the bind refuses rather than paying an unscreened destination.
    let stale = db
        .solana_approve_refund(
            deposit_id,
            "operator",
            Some("11111111111111111111111111111111"),
        )
        .await;
    assert!(matches!(stale, Err(WalletError::Conflict(_))), "{stale:?}");
    // The matching screen approves and records its verdict.
    let approved = db
        .solana_approve_refund(deposit_id, "operator", Some(address))
        .await
        .unwrap();
    let refund = approved.refund.expect("meta.refund is recorded");
    assert!(refund["approvedAt"].is_string());
    assert_eq!(
        refund["addressScreening"]["address"].as_str().unwrap(),
        address
    );
    assert_eq!(
        refund["addressScreening"]["verdict"].as_str().unwrap(),
        "clear"
    );
}

//! The Solana rail's intent bookkeeping against the scratch database
//! (issue #1117): `open_solana_deposit` inserts `pending` with the declared
//! terms in `meta`, `expire_solana_deposit` walks `pending`/`confirmed` to
//! `expired`, and neither misbehaves on a replayed or mis-priced open.
//! Skips without DATABASE_URL.
use oxsum_core::{
    AssetTerms, ChainLookup, Db, ExpectedTransfer, LockedRate, NewUser, SanctionsScreen,
    ScreenVerdict, SolanaRailVerifier, SolanaTerms, USD_PEGGED_RATE_MICRO, ValidatedTransfer,
    WalletError,
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
async fn expire_solana_deposit_walks_pending_and_confirmed_only() {
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
    // A `confirmed` row expires too; a `credited` one is untouched.
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
        let expected = if st == "confirmed" {
            "expired"
        } else {
            "credited"
        };
        assert_eq!(status, expected, "{st} row after expire");
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
            // Honor only the terms the intent locked — the live-rate figure a
            // repriced verify would demand is deliberately not this.
            if expected.amount != self.delivered || expected.recipient != self.pay_to() {
                return None;
            }
            Some(ValidatedTransfer {
                signature: signature.to_owned(),
                delivered: self.delivered,
                block_time: Some(self.block_time),
                payer: None,
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
            if signature != self.paying {
                return None; // every noise signature dishonors
            }
            if expected.amount != self.delivered {
                return None;
            }
            Some(ValidatedTransfer {
                signature: signature.to_owned(),
                delivered: self.delivered,
                block_time: Some(0),
                payer: None,
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
    let receipt = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap()
        .expect("the real payment is found and credits");
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
    let receipt = db
        .credit_solana_deposit(&wallet, on_time, &early, time::Date::MAX)
        .await
        .unwrap()
        .expect("a payment on-chain before expiry credits even when confirmed late");
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 1000);

    // Edge 2: on-chain after expiry (block_time >= expiresAt) — refused,
    // never credited.
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
    let refused = db
        .credit_solana_deposit(&wallet, late, &late_verifier, time::Date::MAX)
        .await;
    assert!(
        matches!(refused, Err(WalletError::Forbidden(_))),
        "a payment on-chain after expiry must not credit: {refused:?}"
    );
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
    let receipt = db
        .credit_solana_deposit(&wallets[0], deposit_ids[0], &verifier, time::Date::MAX)
        .await
        .unwrap()
        .expect("the first credit runs");
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
    // would have demanded the repriced amount and returned None.
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    let receipt = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap()
        .expect("the locked intent credits");
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
                if signature != self.signature
                    || expected.spl_token_mint.as_deref()
                        != Some("usdcMint11111111111111111111111111111111")
                    || expected.amount != self.delivered
                {
                    return None;
                }
                Some(ValidatedTransfer {
                    signature: signature.to_owned(),
                    delivered: self.delivered,
                    block_time: Some(0),
                    payer: None,
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
    let receipt = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap()
        .expect("the locked USDC intent credits");
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
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let wallet = oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
        .await
        .unwrap();
    let receipt = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap()
        .expect("the locked oracle rate credits");
    assert!(receipt.is_new);
    assert_eq!(wallet.available().await.unwrap(), 1000);
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

/// A `OneSig` that answers a `ValidatedTransfer` carrying a payer — the shape
/// the screening path needs. `validate_transfer` honors the same terms the
/// deposit's stub does.
#[derive(Debug)]
struct PayingSig {
    signature: String,
    delivered: i64,
    payer: Option<String>,
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
                payer: self.payer.clone(),
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
            payer: Some("sanctionedPayerAddr444444444444444444444444".to_owned()),
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
    // The flag parks: no credit, `Ok(None)` — the same shape "already
    // settled" takes, because the caller never learns the screening detail.
    let result = db
        .credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
        .await
        .unwrap();
    assert!(result.is_none(), "a flagged deposit never credits");
    assert_eq!(wallet.available().await.unwrap(), 0);
    let row = sqlx::query(
        "SELECT status, facilitator_tx_hash, meta FROM oxsum.deposits WHERE deposit_id = $1",
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
            payer: Some("retryPayerAddr55555555555555555555555555".to_owned()),
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

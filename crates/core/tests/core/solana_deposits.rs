//! The Solana rail's intent bookkeeping against the scratch database
//! (issue #1117): `open_solana_deposit` inserts `pending` with the declared
//! terms in `meta`, `expire_solana_deposit` walks `pending`/`confirmed` to
//! `expired`, and neither misbehaves on a replayed or mis-priced open.
//! Skips without DATABASE_URL.
use oxsum_core::{
    ChainLookup, Db, ExpectedTransfer, LockedRate, NewUser, SolanaRailVerifier, SolanaTerms,
    ValidatedTransfer, WalletError,
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
    SolanaTerms {
        pay_to: "oxsumDepositAddr111111111111111111111111111".to_owned(),
        spl_token_mint: None,
        asset_decimals: 9,
        rate_micro: 200_000_000,
        intent_ttl_secs: 600,
        min_amount_minor: 1,
        max_amount_minor: i64::MAX,
        opens_per_minute: 0,
    }
}

/// The static rate every test's open resolves at — the `LockedRate` an
/// `open_solana_deposit` now carries, matching `terms().rate_micro`.
fn rate() -> LockedRate {
    LockedRate::Static {
        rate_micro: 200_000_000,
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
        .open_solana_deposit(org, deposit_id, 1000, &terms(), &rate())
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
            .open_solana_deposit(org, Uuid::new_v4(), amount, &terms(), &rate())
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
        .open_solana_deposit(org, deposit_id, 100, &terms(), &rate())
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
    db.open_solana_deposit(org, pending, 100, &terms(), &rate())
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
    db.open_solana_deposit(org, deposit_id, 1000, &terms(), &rate())
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
    db.open_solana_deposit(org, on_time, 1000, &short, &rate())
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
    db.open_solana_deposit(org, late, 1000, &short, &rate())
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
            .open_solana_deposit(*org, deposit_id, 1000, &terms(), &rate())
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
            .open_solana_deposit(org, Uuid::new_v4(), amount, &bounded, &rate())
            .await;
        assert!(
            matches!(result, Err(WalletError::InvalidInput(_))),
            "amount {amount} must be refused: {result:?}"
        );
    }
    // Two opens inside the bounds land; the third trips the per-minute cap.
    for _ in 0..2 {
        db.open_solana_deposit(org, Uuid::new_v4(), 1000, &bounded, &rate())
            .await
            .unwrap()
            .expect("in-bounds opens insert");
    }
    let throttled = db
        .open_solana_deposit(org, Uuid::new_v4(), 1000, &bounded, &rate())
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
        .open_solana_deposit(org, deposit_id, 1000, &terms(), &rate())
        .await
        .unwrap()
        .expect("inserts");
    assert_eq!(lamports, 5_000);
    // The deployment's live terms shift before the payment confirms: a
    // doubled rate, a different payTo, a different decimals. A verify that
    // re-read live terms would demand 10_000 lamports to the new address;
    // the locked intent must still settle at 5_000 to the locked address.
    let live = SolanaTerms {
        pay_to: "aDifferentLivePayTo00000000000000000000000000".to_owned(),
        spl_token_mint: Some("aLiveMintThatWasNeverLocked".to_owned()),
        asset_decimals: 6,
        rate_micro: 400_000_000,
        intent_ttl_secs: 600,
        min_amount_minor: 1,
        max_amount_minor: i64::MAX,
        opens_per_minute: 0,
    };
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

/// Issue #1139: an oracle-priced open locks the resolved `rateMicro` into
/// `meta` alongside the quote's own figures — `rateSource: "pyth"`, the
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
        .open_solana_deposit(org, deposit_id, 1000, &terms(), &oracle_rate)
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

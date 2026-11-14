//! Issue #1153: concurrency and rate-limit stress on the Solana top-up rail.
//!
//! The rail's two race surfaces — many opens on one organization, many
//! confirms on one deposit — must still hold the invariants the sequential
//! tests in `solana_deposits.rs` prove one call at a time: one signature
//! credits one deposit ever (`deposits_solana_tx_hash` + the in-tx re-check),
//! an organization mints at most `opens_per_minute` intents a minute, and the
//! chain-RPC section of `credit_solana_deposit` never runs inside the row's
//! `FOR UPDATE` window (issue #1128) — the "no long-held row locks"
//! acceptance criterion these tests prove rather than assume.
//!
//! Needs DATABASE_URL; skips without it.
use oxsum_core::{
    AssetTerms, ChainLookup, Db, ExpectedTransfer, LockedRate, LookupError, NewUser, SolanaCredit,
    SolanaRailVerifier, SolanaTerms, ValidatedTransfer, WalletError,
};
use sqlx::Row;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

async fn database(slug: &str) -> Option<Db> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url(slug)?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    Some(db)
}

/// The deployment's terms — a single `sol` asset at the devnet rate.
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

/// The static `LockedRate` `resolve_rate` answers for the `sol` asset —
/// `open_solana_deposit` carries it.
fn static_rate(terms: &SolanaTerms) -> LockedRate {
    LockedRate::Static {
        rate_micro: terms.terms_for_asset(None).unwrap().rate_micro,
    }
}

/// A fresh organization's id; `deposits.organization_id` references it.
async fn organization(db: &Db, slug: &str) -> Uuid {
    let registration = db
        .register(NewUser {
            email: format!(
                "solana-conc-{slug}-{}@example.test",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            password: "solana-private-password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    registration.organization.id
}

/// The org's wallet — the ledger `credit_solana_deposit` books into.
async fn wallet_for(db: &Db, org: Uuid) -> Arc<oxsum_core::Wallet> {
    let tenant_id: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id = $1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    Arc::new(
        oxsum_core::Wallet::open(db.pool().clone(), &tenant_id)
            .await
            .unwrap(),
    )
}

/// A `ChainLookup` armed with one honoring transfer: `find_reference`
/// answers the signature for every reference it is asked, `validate_transfer`
/// reports `delivered`/`block_time`/`payers`. `block_time: 0` is always inside
/// the intent's TTL (the expiry judgment is the transaction's own clock).
#[derive(Debug)]
struct OneSig {
    signature: String,
    delivered: i64,
    payers: Vec<String>,
}

impl ChainLookup for OneSig {
    fn find_reference<'a>(
        &'a self,
        _reference: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
    > {
        Box::pin(async move { Ok(vec![self.signature.clone()]) })
    }
    fn validate_transfer<'a>(
        &'a self,
        signature: &'a str,
        expected: &'a ExpectedTransfer,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            // The transfer must name the intent's marker and recipient — a
            // raced confirm on a different reference still resolves to the
            // same signature the way a two-reference transaction does.
            if signature != self.signature
                || expected.recipient != "oxsumDepositAddr111111111111111111111111111"
            {
                return Ok(None);
            }
            Ok(Some(ValidatedTransfer {
                signature: signature.to_owned(),
                delivered: self.delivered,
                block_time: Some(0),
                payers: self.payers.clone(),
            }))
        })
    }
}

/// The opens-per-minute bound is per-organization: with the cap unset every
/// concurrent open lands its own pending row under its own `payment_ref`
/// (the reference is minted inside the open, so two raced opens can never
/// collide on the anchor), and a capped org stops at exactly K — the
/// serialized count+insert under the org's advisory lock (issue #1169)
/// refuses the rest Forbidden, and the cap itself reads the org's own rows
/// only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_opens_land_distinct_intents_and_the_cap_holds() {
    let Some(db) = database("solana_conc_open").await else {
        return;
    };
    let org = organization(&db, "open").await;
    const N: usize = 12;
    // (i) Unbounded: every open lands; every reference is distinct.
    let unlimited = terms();
    let rate = static_rate(&unlimited);
    let mut opens = Vec::with_capacity(N);
    for _ in 0..N {
        let db = db.clone();
        let unlimited = unlimited.clone();
        let rate = rate.clone();
        opens.push(tokio::spawn(async move {
            db.open_solana_deposit(org, Uuid::new_v4(), 1000, &unlimited, None, &rate)
                .await
        }));
    }
    let mut references = std::collections::HashSet::new();
    for open in opens {
        let opened = open.await.unwrap().unwrap().expect("inserts");
        assert!(
            references.insert(opened.0),
            "a second open answered the same reference"
        );
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.deposits \
         WHERE rail = 'solana' AND organization_id = $1 AND status = 'pending'",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(pending, N as i64);

    // (ii) A second org under `opens_per_minute = K < N`: the count-and-insert
    // serializes under the org's advisory lock (issue #1169), so the burst
    // stops exactly at K — N-K losers refuse Forbidden. The cap is per-org,
    // so the first org's unbounded burst never counted against it.
    const K: i64 = 4;
    let capped = SolanaTerms {
        opens_per_minute: K,
        ..terms()
    };
    let capped_rate = static_rate(&capped);
    let capped_org = organization(&db, "open-capped").await;
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let mut opens = Vec::with_capacity(N);
    for _ in 0..N {
        let db = db.clone();
        let terms = capped.clone();
        let rate = capped_rate.clone();
        let barrier = barrier.clone();
        opens.push(tokio::spawn(async move {
            barrier.wait().await;
            db.open_solana_deposit(capped_org, Uuid::new_v4(), 1000, &terms, None, &rate)
                .await
        }));
    }
    let mut landed = 0;
    let mut refused = 0;
    for open in opens {
        match open.await.unwrap() {
            Ok(Some(_)) => landed += 1,
            Err(WalletError::Forbidden(_)) => refused += 1,
            other => panic!("a capped open is a landing or a Forbidden: {other:?}"),
        }
    }
    assert_eq!(
        landed, K,
        "the serialized count+insert stops the burst at K"
    );
    assert_eq!(refused, N as i64 - K, "the rest refused Forbidden");
    // The cap still bites after the burst: the org sits at its bound, so a
    // further sequential open refuses.
    let throttled = db
        .open_solana_deposit(
            capped_org,
            Uuid::new_v4(),
            1000,
            &capped,
            None,
            &capped_rate,
        )
        .await;
    assert!(
        matches!(throttled, Err(WalletError::Forbidden(_))),
        "an org at its per-minute count refuses the next open: {throttled:?}"
    );
    // The bound is per-org: the first org's unbounded burst never counted —
    // every row the capped org landed is its own.
    let capped_pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.deposits \
         WHERE rail = 'solana' AND organization_id = $1 AND status = 'pending'",
    )
    .bind(capped_org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(capped_pending, K);
}

/// The per-minute budget is the organization's own: one capped org racing a
/// burst stops at its K while a second capped org — same terms, same
/// instant — still lands its own full K. Two bursts fanned onto one barrier
/// prove neither org's lock or count eats the other's allowance.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_open_cap_is_per_organization() {
    let Some(db) = database("solana_conc_per_org").await else {
        return;
    };
    const N: usize = 12;
    const K: i64 = 4;
    let capped = SolanaTerms {
        opens_per_minute: K,
        ..terms()
    };
    let capped_rate = static_rate(&capped);
    let org_a = organization(&db, "cap-a").await;
    let org_b = organization(&db, "cap-b").await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2 * N));
    let mut opens = Vec::with_capacity(2 * N);
    for org in [org_a, org_b] {
        for _ in 0..N {
            let db = db.clone();
            let terms = capped.clone();
            let rate = capped_rate.clone();
            let barrier = barrier.clone();
            opens.push(tokio::spawn(async move {
                barrier.wait().await;
                (
                    org,
                    db.open_solana_deposit(org, Uuid::new_v4(), 1000, &terms, None, &rate)
                        .await,
                )
            }));
        }
    }
    let mut landed_a = 0;
    let mut landed_b = 0;
    let mut refused = 0;
    for open in opens {
        let (org, result) = open.await.unwrap();
        match result {
            Ok(Some(_)) if org == org_a => landed_a += 1,
            Ok(Some(_)) => landed_b += 1,
            Err(WalletError::Forbidden(_)) => refused += 1,
            other => panic!("a capped open is a landing or a Forbidden: {other:?}"),
        }
    }
    assert_eq!(landed_a, K, "org A's own burst stops at its own K");
    assert_eq!(landed_b, K, "org B keeps a full budget under A's burst");
    assert_eq!(refused, 2 * (N as i64 - K), "every loser refused Forbidden");
    for (org, expected) in [(org_a, K), (org_b, K)] {
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.deposits \
             WHERE rail = 'solana' AND organization_id = $1 AND status = 'pending'",
        )
        .bind(org)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(pending, expected);
    }
}

/// N concurrent confirms on the SAME deposit resolving the SAME signature:
/// exactly one `Credited`, the rest `Settled` (the status re-check under
/// `FOR UPDATE`) — the wallet books once, `facilitator_tx_hash` is written
/// once, and no pass deadlocks on another's lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_confirms_on_one_deposit_credit_once() {
    let Some(db) = database("solana_conc_credit").await else {
        return;
    };
    let org = organization(&db, "credit").await;
    let wallet = wallet_for(&db, org).await;
    let terms = terms();
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    let signature = "sigRaceCredit00000000000000000000000000000000000000000000000";
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: signature.to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    const N: usize = 12;
    // A barrier fans the confirms out onto the same instant so the FOR UPDATE
    // re-check is the arbiter, not arrival order.
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let mut confirms = Vec::with_capacity(N);
    for _ in 0..N {
        let db = db.clone();
        let wallet = wallet.clone();
        let verifier = verifier.clone();
        let barrier = barrier.clone();
        confirms.push(tokio::spawn(async move {
            barrier.wait().await;
            db.credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
                .await
        }));
    }
    let mut credited = 0;
    let mut settled = 0;
    for confirm in confirms {
        match confirm.await.unwrap() {
            Ok(SolanaCredit::Credited(_)) => credited += 1,
            Ok(SolanaCredit::Settled) => settled += 1,
            other => panic!("a raced confirm credits or settles: {other:?}"),
        }
    }
    assert_eq!(credited, 1, "exactly one confirm books the credit");
    assert_eq!(settled, N - 1, "the rest re-check settled under the lock");
    assert_eq!(wallet.available().await.unwrap(), 1000);
    let row = sqlx::query(
        "SELECT status, facilitator_tx_hash, entry_id FROM oxsum.deposits WHERE deposit_id = $1",
    )
    .bind(deposit_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("status"), "credited");
    assert_eq!(
        row.get::<Option<String>, _>("facilitator_tx_hash")
            .as_deref(),
        Some(signature)
    );
    assert!(row.get::<Option<Uuid>, _>("entry_id").is_some());
}

/// Two deposits — different organizations — racing on the SAME signature:
/// exactly one credits, the loser gets `Forbidden` (the #1124 double-spend
/// refusal). The loser's refusal proves the in-transaction `already_used`
/// check or the `deposits_solana_tx_hash` backstop fired — the error kind is
/// the assertion, not the count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_orgs_racing_one_signature_credit_once() {
    let Some(db) = database("solana_conc_dblspend").await else {
        return;
    };
    let terms = terms();
    let signature = "sigShared0000000000000000000000000000000000000000000000000";
    let mut deposit_ids = Vec::new();
    let mut wallets = Vec::new();
    for slug in ["shared-a", "shared-b"] {
        let org = organization(&db, slug).await;
        let deposit_id = Uuid::new_v4();
        db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
            .await
            .unwrap()
            .expect("inserts");
        deposit_ids.push(deposit_id);
        wallets.push(wallet_for(&db, org).await);
    }
    // One paying transaction "carries both references" — the stub resolves
    // either to the same signature, the double-spend shape.
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: signature.to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut confirms = Vec::new();
    for (deposit_id, wallet) in deposit_ids.iter().zip(wallets.iter()) {
        let db = db.clone();
        let wallet = wallet.clone();
        let verifier = verifier.clone();
        let barrier = barrier.clone();
        let deposit_id = *deposit_id;
        confirms.push(tokio::spawn(async move {
            barrier.wait().await;
            db.credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
                .await
        }));
    }
    let mut credited = 0;
    let mut forbidden = 0;
    for confirm in confirms {
        match confirm.await.unwrap() {
            Ok(SolanaCredit::Credited(_)) => credited += 1,
            // The loser's own row is still `pending` — its refusal is the
            // double-spend check firing (the `already_used` re-check or the
            // `deposits_solana_tx_hash` backstop's 23505), never a settle.
            Err(WalletError::Forbidden(_)) => forbidden += 1,
            other => panic!("a double-spend race credits or refuses: {other:?}"),
        }
    }
    assert_eq!(credited, 1, "one signature credits one deposit ever");
    assert_eq!(
        forbidden, 1,
        "the losing org's confirm is the Forbidden double-spend refusal"
    );
    let wallets_seen: Vec<i64> = {
        let mut out = Vec::new();
        for wallet in &wallets {
            out.push(wallet.available().await.unwrap());
        }
        out
    };
    assert_eq!(
        wallets_seen.iter().sum::<i64>(),
        1000,
        "the losing organization's ledger books nothing"
    );
    // The loser's own row stays `pending` — its confirm refused, never wrote.
    // A replay confirms the refusal kind again: the signature is spent, so
    // the re-confirm is the same Forbidden, not a silent settle.
    let mut replay_refusals = 0;
    for (deposit_id, wallet) in deposit_ids.iter().zip(wallets.iter()) {
        let retry = db
            .credit_solana_deposit(wallet, *deposit_id, &verifier, time::Date::MAX)
            .await;
        match retry {
            Err(WalletError::Forbidden(_)) => replay_refusals += 1,
            // The winner's row is `credited` — a replay on it reads Settled.
            Ok(SolanaCredit::Settled) => {}
            other => panic!("a re-confirm on a spent signature refuses: {other:?}"),
        }
    }
    assert_eq!(replay_refusals, 1, "the loser refuses again on replay");
    let spent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.deposits WHERE rail = 'solana' AND facilitator_tx_hash = $1",
    )
    .bind(signature)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(spent, 1, "one row holds the spent signature");
}

/// A confirm racing the `solana-deposits` poll pass: the poll's own
/// `credit_solana_deposit` and the route's share one walk, so a barrier-fanned
/// pair still books exactly one credit — the poll sees `Settled` for the
/// already-walked row and the confirm credits, or the reverse.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_confirm_racing_the_poll_credits_once() {
    let Some(db) = database("solana_conc_poll").await else {
        return;
    };
    let org = organization(&db, "poll").await;
    let wallet = wallet_for(&db, org).await;
    let terms = terms();
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: "sigPollRace0000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    // The poll pass — one full `poll_solana_deposits` over the pending row.
    let poll = {
        let db = db.clone();
        let verifier = verifier.clone();
        let tenants = tenants.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            db.poll_solana_deposits(&verifier, &tenants, time::Date::MAX)
                .await
        })
    };
    // The on-demand confirm on the same deposit.
    let confirm = {
        let db = db.clone();
        let wallet = wallet.clone();
        let verifier = verifier.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            db.credit_solana_deposit(&wallet, deposit_id, &verifier, time::Date::MAX)
                .await
        })
    };
    let (poll_result, confirm_result) = tokio::join!(poll, confirm);
    let (candidates, credited, _expired, _queued) = poll_result.unwrap().unwrap();
    // The poll's work-list read races the confirm too: a fast confirm leaves
    // no `pending` row for the pass to walk, so `candidates` is 0 or 1.
    assert!(candidates <= 1, "the poll walks at most the one deposit");
    let confirm = confirm_result.unwrap().unwrap();
    // One of the two booked the credit; the other's pass read `Settled` — the
    // poll's own count and the confirm's verdict between them name the single
    // credit either way around the race lands.
    let confirm_credited = matches!(confirm, SolanaCredit::Credited(_));
    assert_eq!(
        credited + u64::from(confirm_credited),
        1,
        "exactly one of the poll and the confirm credits: \
         poll credited={credited}, confirm={confirm:?}"
    );
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// The "no long-held row locks" acceptance criterion (issues #1128, #1153): a
/// `credit_solana_deposit` parked inside `find_reference` — the chain RPC —
/// holds no row lock another deposit's work could queue behind. `BlockingSig`
/// parks its `find_reference` on a oneshot release; while the first credit
/// waits inside the RPC, a *different* organization's open and credit must
/// complete. If the RPC ran inside the row's `FOR UPDATE` window the second
/// credit would stall behind the parked one — the assert it finishes is the
/// proof the lock window never covers the network call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_chain_rpc_section_holds_no_row_lock() {
    let Some(db) = database("solana_conc_locks").await else {
        return;
    };
    let terms = terms();
    /// Parks its one `find_reference` until `release` fires; `entered` tells
    /// the test the RPC section is reached. `validate_transfer` honors the
    /// parked deposit once released, so the credit completes.
    struct BlockingSig {
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        signature: String,
        delivered: i64,
    }
    impl std::fmt::Debug for BlockingSig {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BlockingSig").finish_non_exhaustive()
        }
    }
    impl ChainLookup for BlockingSig {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async move {
                let entered = self.entered.lock().unwrap().take();
                if let Some(entered) = entered {
                    let _ = entered.send(());
                }
                let release = self.release.lock().unwrap().take();
                if let Some(release) = release {
                    let _ = release.await;
                }
                Ok(vec![self.signature.clone()])
            })
        }
        fn validate_transfer<'a>(
            &'a self,
            signature: &'a str,
            expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                if signature != self.signature
                    || expected.recipient != "oxsumDepositAddr111111111111111111111111111"
                {
                    return Ok(None);
                }
                Ok(Some(ValidatedTransfer {
                    signature: signature.to_owned(),
                    delivered: self.delivered,
                    block_time: Some(0),
                    payers: Vec::new(),
                }))
            })
        }
    }

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let blocking = SolanaRailVerifier::new(
        Arc::new(BlockingSig {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(Some(release_rx)),
            signature: "sigParked000000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
        }),
        terms.clone(),
    );
    // Deposit A parks inside the RPC; deposit B belongs to a different
    // organization so its work never touches A's row.
    let org_a = organization(&db, "parked").await;
    let wallet_a = wallet_for(&db, org_a).await;
    let parked_id = Uuid::new_v4();
    db.open_solana_deposit(org_a, parked_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    let parked = {
        let db = db.clone();
        let wallet = wallet_a.clone();
        let verifier = blocking.clone();
        tokio::spawn(async move {
            db.credit_solana_deposit(&wallet, parked_id, &verifier, time::Date::MAX)
                .await
        })
    };
    // Wait until the parked credit is inside its RPC — the window where a
    // held row lock would serialize every other deposit's work.
    tokio::time::timeout(std::time::Duration::from_secs(10), entered_rx)
        .await
        .expect("the parked credit reached its RPC section")
        .unwrap();
    // While it waits: a second organization opens and credits a deposit of
    // its own. A row lock held across the RPC would stall B's work — the
    // timeout turning red is the failure this test exists to catch.
    let org_b = organization(&db, "unblocked").await;
    let wallet_b = wallet_for(&db, org_b).await;
    let b_id = Uuid::new_v4();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        db.open_solana_deposit(org_b, b_id, 1000, &terms, None, &static_rate(&terms)),
    )
    .await
    .expect("a second org's open does not wait on the parked RPC")
    .unwrap()
    .expect("inserts");
    let free_verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: "sigFree000000000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let b_credit = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        db.credit_solana_deposit(&wallet_b, b_id, &free_verifier, time::Date::MAX),
    )
    .await
    .expect("a second org's credit does not wait on the parked RPC")
    .unwrap();
    assert!(
        matches!(b_credit, SolanaCredit::Credited(_)),
        "the second credit completes while the first is parked: {b_credit:?}"
    );
    // Release the parked RPC; its own credit completes under the short
    // transaction the row's lock actually covers.
    release_tx.send(()).unwrap();
    let parked_result = tokio::time::timeout(std::time::Duration::from_secs(10), parked)
        .await
        .expect("the parked credit finishes once released")
        .unwrap()
        .unwrap();
    assert!(
        matches!(parked_result, SolanaCredit::Credited(_)),
        "the parked deposit credits after release: {parked_result:?}"
    );
    assert_eq!(wallet_a.available().await.unwrap(), 1000);
    assert_eq!(wallet_b.available().await.unwrap(), 1000);
}

/// Issue #1174: the poll's expiry sweep may expire only the rows the same
/// pass verified `Unpaid`. 300 lapsed intents exceed the pass's 256-row work
/// list, so intent 257 is never verified — the old blanket
/// `UPDATE ... expiresAt <= now()` expired it anyway, and a payment that had
/// landed on-chain before `expiresAt` sat in the vault uncredited with no
/// queue row. Now the unverified row stays `pending`, the next pass walks it,
/// and `credit_solana_deposit` credits it even if a race did flip it
/// `expired` first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sweep_never_expires_a_row_it_did_not_verify() {
    let Some(db) = database("solana_sweep_verified").await else {
        return;
    };
    // A one-second TTL: a real expiresAt is stored (0 resolves the 600 s
    // default), and backdating it below makes every row already lapsed —
    // deterministic, never a wall-clock race with the inserts.
    let terms = SolanaTerms {
        intent_ttl_secs: 1,
        ..terms()
    };
    // A lapsed backlog the work-list bound truncates: 300 intents whose
    // created_at spread guarantees the 257th lands past the LIMIT. Opened
    // sequentially so created_at strictly increases with the index.
    let org = organization(&db, "sweep").await;
    let wallet = wallet_for(&db, org).await;
    let mut refs = Vec::new();
    for i in 0..300_usize {
        let deposit_id = Uuid::new_v4();
        let (reference, _, _) = db
            .open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
            .await
            .unwrap()
            .expect("inserts");
        if i == 256 {
            refs.push((deposit_id, reference));
        }
    }
    // Backdate every intent's expiresAt into the past: 0 resolves the
    // default TTL rather than a zero lapse, so the test sets the lapse
    // directly instead of racing a 1 s wall clock.
    sqlx::query(
        "UPDATE oxsum.deposits SET meta = jsonb_set(meta, '{expiresAt}', $1::jsonb) \
         WHERE rail = 'solana' AND organization_id = $2",
    )
    .bind("\"2000-01-01T00:00:00Z\"")
    .bind(org)
    .execute(db.pool())
    .await
    .unwrap();
    let (target_id, target_ref) = refs.pop().expect("the 257th intent is recorded");
    // Every intent is lapsed (the backdated expiresAt), and the stub
    // pays only the 257th's marker — block_time 0, before its expiresAt.
    /// Answers `vec![sig]` for the one armed reference, nothing for any
    /// other — the chain state where only intent 257 paid.
    struct PaysOne {
        reference: String,
        signature: String,
        delivered: i64,
    }
    impl std::fmt::Debug for PaysOne {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PaysOne").finish_non_exhaustive()
        }
    }
    impl ChainLookup for PaysOne {
        fn find_reference<'a>(
            &'a self,
            reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async move {
                if reference == self.reference {
                    Ok(vec![self.signature.clone()])
                } else {
                    Ok(Vec::new())
                }
            })
        }
        fn validate_transfer<'a>(
            &'a self,
            signature: &'a str,
            expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                if signature != self.signature
                    || expected.recipient != "oxsumDepositAddr111111111111111111111111111"
                {
                    return Ok(None);
                }
                Ok(Some(ValidatedTransfer {
                    signature: signature.to_owned(),
                    delivered: self.delivered,
                    block_time: Some(0),
                    payers: Vec::new(),
                }))
            })
        }
    }
    let verifier = SolanaRailVerifier::new(
        Arc::new(PaysOne {
            reference: target_ref,
            signature: "sigIntent25700000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
        }),
        terms,
    );
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let (checked, _credited, expired, _queued) = db
        .poll_solana_deposits(&verifier, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(checked, 256, "one pass walks at most its work-list bound");
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(target_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        status, "pending",
        "the 257th intent was never verified — it must not expire: {status}"
    );
    let expired_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.deposits \
         WHERE rail = 'solana' AND status = 'expired' AND organization_id = $1",
    )
    .bind(org)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(expired, expired_rows as u64);
    assert_eq!(
        expired_rows, 256,
        "every verified-unpaid lapsed row the pass walked expired, nothing more"
    );
    // The next pass reaches it: the payment verifies on-time and credits.
    let (_checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&verifier, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 1, "the second pass credits the paid intent");
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: two unleadered poll instances racing one lapsed intent must
/// not strand its payment. Instance A parks inside its verify on the armed
/// intent; instance B walks the same work list, verifies the armed intent
/// unpaid (its own genuine verdict — the chain stub sees no paying transfer
/// for B), and its keyed sweep expires the row under A's in-flight verify.
/// Before the fix A's `credit_solana_deposit` then hit the
/// `status != 'pending'` gate and returned `Settled` — money in the vault,
/// no customer credit, no queue row. Now the `expired`-rescue re-walks the
/// row and the on-time payment credits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_polls_racing_one_intent_still_credit_its_payment() {
    let Some(db) = database("solana_poll_race").await else {
        return;
    };
    // A one-second TTL: a real expiresAt is stored (0 resolves the 600 s
    // default), backdated below so every intent is lapsed when either poll
    // runs — deterministic, not a wall-clock race.
    let terms = SolanaTerms {
        intent_ttl_secs: 1,
        ..terms()
    };
    let org = organization(&db, "poll-race").await;
    let wallet = wallet_for(&db, org).await;
    // Filler intents — unpaid under either stub, so they are each pass's
    // verified-unpaid sweep candidates alongside the armed one.
    for _ in 0..3 {
        db.open_solana_deposit(
            org,
            Uuid::new_v4(),
            1000,
            &terms,
            None,
            &static_rate(&terms),
        )
        .await
        .unwrap()
        .expect("inserts");
    }
    let deposit_id = Uuid::new_v4();
    let (reference, _, _) = db
        .open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    // Backdate every intent's expiresAt into the past so both passes read a
    // genuinely lapsed intent: instance B's sweep is what flips the armed row.
    sqlx::query(
        "UPDATE oxsum.deposits SET meta = jsonb_set(meta, '{expiresAt}', $1::jsonb) \
         WHERE rail = 'solana' AND organization_id = $2",
    )
    .bind("\"2000-01-01T00:00:00Z\"")
    .bind(org)
    .execute(db.pool())
    .await
    .unwrap();
    /// `Blocker` — instance A's chain — parks its `find_reference` on the
    /// armed reference until released, then answers the paying signature.
    /// Any other reference is unpaid to it. Instance B's stub is the same
    /// shape minus the gate: it finds nothing, so B's verdict on every row —
    /// the armed one included — is a genuine `Unpaid`.
    struct Blocker {
        reference: String,
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        signature: String,
        delivered: i64,
    }
    impl std::fmt::Debug for Blocker {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Blocker").finish_non_exhaustive()
        }
    }
    impl ChainLookup for Blocker {
        fn find_reference<'a>(
            &'a self,
            reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async move {
                if reference == self.reference {
                    if let Some(entered) = self.entered.lock().unwrap().take() {
                        let _ = entered.send(());
                    }
                    let release = self.release.lock().unwrap().take();
                    if let Some(release) = release {
                        let _ = release.await;
                    }
                    return Ok(vec![self.signature.clone()]);
                }
                Ok(Vec::new())
            })
        }
        fn validate_transfer<'a>(
            &'a self,
            signature: &'a str,
            expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                if signature != self.signature
                    || expected.recipient != "oxsumDepositAddr111111111111111111111111111"
                {
                    return Ok(None);
                }
                Ok(Some(ValidatedTransfer {
                    signature: signature.to_owned(),
                    delivered: self.delivered,
                    block_time: Some(0),
                    payers: Vec::new(),
                }))
            })
        }
    }
    struct FindsNothing;
    impl std::fmt::Debug for FindsNothing {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FindsNothing").finish_non_exhaustive()
        }
    }
    impl ChainLookup for FindsNothing {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(None) })
        }
    }
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    // Instance A parks inside its verify on the armed intent — the window in
    // which a second instance's sweep can expire the row under it.
    let blocker = SolanaRailVerifier::new(
        Arc::new(Blocker {
            reference,
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(Some(release_rx)),
            signature: "sigParkedPay000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
        }),
        terms.clone(),
    );
    let blind = SolanaRailVerifier::new(Arc::new(FindsNothing), terms);
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let poll_a = {
        let db = db.clone();
        let tenants = tenants.clone();
        tokio::spawn(async move {
            db.poll_solana_deposits(&blocker, &tenants, time::Date::MAX)
                .await
        })
    };
    // Wait until instance A is inside its chain RPC on the armed intent, then
    // run instance B: it verifies every row unpaid — the armed one's verdict
    // is genuine to B — and its sweep expires all four under A's parked
    // verify.
    tokio::time::timeout(std::time::Duration::from_secs(10), entered_rx)
        .await
        .expect("instance A reached its RPC section")
        .unwrap();
    let poll_b = {
        let db = db.clone();
        let tenants = tenants.clone();
        tokio::spawn(async move {
            db.poll_solana_deposits(&blind, &tenants, time::Date::MAX)
                .await
        })
    };
    let (b_checked, _, b_expired, _) =
        tokio::time::timeout(std::time::Duration::from_secs(10), poll_b)
            .await
            .expect("instance B's pass does not wait on A's parked verify")
            .unwrap()
            .unwrap();
    assert_eq!(b_checked, 4, "instance B walked every pending row");
    assert_eq!(
        b_expired, 4,
        "instance B expired every row it verified unpaid — the armed one included"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        status, "expired",
        "B's sweep flipped the armed row mid-verify"
    );
    // Release A's verify: the payment locates, the `expired`-rescue re-walks
    // the row, and the ordinary credit path books it.
    release_tx.send(()).unwrap();
    let (a_checked, a_credited, _, _) =
        tokio::time::timeout(std::time::Duration::from_secs(10), poll_a)
            .await
            .expect("instance A's pass finishes once released")
            .unwrap()
            .unwrap();
    assert_eq!(a_checked, 4, "instance A's work list covered every row");
    assert_eq!(a_credited, 1, "instance A's released verify credited");
    let row =
        sqlx::query("SELECT status, facilitator_tx_hash FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        row.get::<String, _>("status"),
        "credited",
        "a raced expiry must never strand an on-time payment"
    );
    assert_eq!(
        row.get::<Option<String>, _>("facilitator_tx_hash")
            .as_deref(),
        Some("sigParkedPay000000000000000000000000000000000000000000")
    );
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: an `expired` row inside the rescue window whose on-time
/// payment the sweep never saw credits through the poll's own rescue pass —
/// no manual confirm, no operator action. This is the end-to-end half of the
/// rescue invariant `an_expired_row_still_credits_an_on_time_payment` proves
/// at the `credit_solana_deposit` seam: the poll picks the row up itself.
#[tokio::test]
async fn the_poll_rescues_an_expired_row_whose_payment_landed_on_time() {
    let Some(db) = database("solana_poll_rescue").await else {
        return;
    };
    let terms = terms();
    let org = organization(&db, "poll-rescue").await;
    let wallet = wallet_for(&db, org).await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    // The stale-verdict shape: the row is `expired` while its expiresAt is
    // still recent — inside the rescue pass's 24 h window (block_time 0 is
    // before any real expiresAt, so the payment is on-time).
    db.expire_solana_deposit(deposit_id).await.unwrap();
    let verifier = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: "sigPollRescue00000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let (checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&verifier, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(checked, 1, "the rescue pass walked the expired row");
    assert_eq!(
        credited, 1,
        "the on-time payment credited through the rescue"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "credited");
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: a lookup that fails — `find_reference` answering `Err` — is
/// NOT an unpaid verdict. The row is already `expired` and past `expiresAt`,
/// exactly the shape the old code swept into oblivion; now the deferred
/// verdict skips the row, and the pass that runs after the outage clears
/// credits the on-time payment.
#[tokio::test]
async fn a_lookup_failure_defers_the_row_and_the_next_pass_credits() {
    let Some(db) = database("solana_rpc_fail").await else {
        return;
    };
    let terms = terms();
    let org = organization(&db, "rpc-fail").await;
    let wallet = wallet_for(&db, org).await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    db.expire_solana_deposit(deposit_id).await.unwrap();

    /// The failing half: `find_reference` answers `Err` — an unreachable
    /// endpoint, not a signature list. `validate_transfer` is never reached.
    struct FailingLookup;
    impl std::fmt::Debug for FailingLookup {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FailingLookup").finish_non_exhaustive()
        }
    }
    impl ChainLookup for FailingLookup {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async { Err(LookupError("endpoint unreachable".into())) })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Err(LookupError("endpoint unreachable".into())) })
        }
    }
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let failing = SolanaRailVerifier::new(Arc::new(FailingLookup), terms.clone());
    let (checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&failing, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(checked, 1, "the rescue pass walked the row and deferred");
    assert_eq!(credited, 0, "a lookup failure never credits — or expires");
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        status, "expired",
        "a deferred verify leaves the row exactly where it was — never swept, never queued"
    );
    // The outage clears: the same row, the same rescue pass, the payment it
    // could not see credits.
    let healed = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: "sigHealedRpc000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let (_checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&healed, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 1, "the recovered pass credits the paid row");
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: the expiry grace covers finality lag — a payment that landed
/// just before `expiresAt` is not yet finalized-visible when the verify runs,
/// so an `Unpaid` verdict whose verify began inside
/// `expiresAt + SOLANA_EXPIRY_GRACE_SECS` must NOT expire the row. Intent A's
/// expiresAt sits inside the grace window: the pass verifies it unpaid and
/// leaves it `pending`. Intent B is long past grace: it expires. Once finality
/// catches up the next pass credits A — the whole point of the grace.
#[tokio::test]
async fn the_expiry_grace_keeps_a_freshly_lapsed_row_pending() {
    let Some(db) = database("solana_expiry_grace").await else {
        return;
    };
    let terms = terms();
    let org = organization(&db, "grace").await;
    let wallet = wallet_for(&db, org).await;
    // Both intents verify Unpaid — the chain stub finds nothing. A's
    // expiresAt is 30 s in the past (inside the 120 s grace), B's is ancient.
    let grace_id = Uuid::new_v4();
    let past_id = Uuid::new_v4();
    for id in [grace_id, past_id] {
        db.open_solana_deposit(org, id, 1000, &terms, None, &static_rate(&terms))
            .await
            .unwrap()
            .expect("inserts");
    }
    let within_grace = time::OffsetDateTime::now_utc() - time::Duration::seconds(30);
    let beyond_grace = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    for (id, at) in [(grace_id, within_grace), (past_id, beyond_grace)] {
        let stamp = at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        sqlx::query(
            "UPDATE oxsum.deposits SET meta = jsonb_set(meta, '{expiresAt}', $1::jsonb) \
             WHERE deposit_id = $2",
        )
        .bind(serde_json::Value::String(stamp))
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
    }
    /// Finds no signature for any reference — the genuine unpaid answer.
    struct FindsNothing;
    impl std::fmt::Debug for FindsNothing {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FindsNothing").finish_non_exhaustive()
        }
    }
    impl ChainLookup for FindsNothing {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(None) })
        }
    }
    let verifier = SolanaRailVerifier::new(Arc::new(FindsNothing), terms.clone());
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let (_checked, _credited, expired, _queued) = db
        .poll_solana_deposits(&verifier, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(expired, 1, "only the past-grace row expired");
    let grace_status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(grace_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        grace_status, "pending",
        "inside the grace window an Unpaid verdict stays pending — finality may still land the payment"
    );
    let past_status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(past_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(past_status, "expired");
    // Finality catches up: the honoring transfer becomes visible, the next
    // pass over the still-pending row credits it — the outcome the grace
    // exists to preserve.
    let healed = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: "sigGraceHealed0000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let (_checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&healed, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(
        credited, 1,
        "the once-deferred intent credits on the next pass"
    );
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: a `find_reference` that fails on a still-pending row is NOT
/// an unpaid verdict — the old code folded every lookup `Err` into "not
/// paid", pushed the row into the sweep and expired a deposit whose payment
/// may already sit in a block the endpoint never returned. The poll now
/// defers the row untouched; once the endpoint heals the paying transfer
/// verifies and credits.
#[tokio::test]
async fn a_pending_row_deferred_by_an_rpc_failure_credits_once_healed() {
    let Some(db) = database("solana_rpc_defer").await else {
        return;
    };
    let terms = terms();
    let org = organization(&db, "rpc-defer").await;
    let wallet = wallet_for(&db, org).await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    // Lapse the intent so the row is inside the sweep's reach — the shape
    // where the old "Err is unpaid" folded it straight to `expired`.
    sqlx::query(
        "UPDATE oxsum.deposits SET meta = jsonb_set(meta, '{expiresAt}', $1::jsonb) \
         WHERE deposit_id = $2",
    )
    .bind("\"2000-01-01T00:00:00Z\"")
    .bind(deposit_id)
    .execute(db.pool())
    .await
    .unwrap();
    /// `find_reference` errors — a transport failure, not an answer.
    struct RpcDown;
    impl std::fmt::Debug for RpcDown {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RpcDown").finish_non_exhaustive()
        }
    }
    impl ChainLookup for RpcDown {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async { Err(LookupError("503 from the RPC".into())) })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Ok(None) })
        }
    }
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let down = SolanaRailVerifier::new(Arc::new(RpcDown), terms.clone());
    let (_checked, credited, expired, _queued) = db
        .poll_solana_deposits(&down, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 0);
    assert_eq!(expired, 0, "a lookup failure must never feed the sweep");
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        status, "pending",
        "a deferred verify leaves the lapsed row pending for the next pass"
    );
    // The endpoint heals; the payment the RPC never returned is on-time and
    // honors — the row credits instead of having died `expired`.
    let healed = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: "sigAfterOutage0000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let (_checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&healed, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 1);
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "credited");
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: an unconfirmable candidate — `validate_transfer` answering
/// `Err` — defers the verify the same way a `find_reference` failure does.
/// The old code flattened every failure into "not paid" and let the sweep
/// expire the row; now the lapsed intent stays `pending` through the outage
/// and credits once the transfer can be confirmed.
#[tokio::test]
async fn an_unconfirmable_candidate_defers_rather_than_expires() {
    let Some(db) = database("solana_validate_err").await else {
        return;
    };
    let terms = terms();
    let org = organization(&db, "validate-err").await;
    let wallet = wallet_for(&db, org).await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    sqlx::query(
        "UPDATE oxsum.deposits SET meta = jsonb_set(meta, '{expiresAt}', $1::jsonb) \
         WHERE deposit_id = $2",
    )
    .bind("\"2000-01-01T00:00:00Z\"")
    .bind(deposit_id)
    .execute(db.pool())
    .await
    .unwrap();
    /// Locates one signature but cannot confirm it — the cross-confirmation
    /// disagreed or its fallback endpoint never answered.
    struct Unconfirmable;
    impl std::fmt::Debug for Unconfirmable {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Unconfirmable").finish_non_exhaustive()
        }
    }
    impl ChainLookup for Unconfirmable {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async {
                Ok(vec![
                    "sigCantConfirm0000000000000000000000000000000000000000".to_owned(),
                ])
            })
        }
        fn validate_transfer<'a>(
            &'a self,
            _signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async {
                Err(LookupError(
                    "cross-confirmation failed: the fallback endpoint disagreed".into(),
                ))
            })
        }
    }
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let unconfirmable = SolanaRailVerifier::new(Arc::new(Unconfirmable), terms.clone());
    let (_checked, credited, expired, _queued) = db
        .poll_solana_deposits(&unconfirmable, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 0);
    assert_eq!(
        expired, 0,
        "an unconfirmable candidate never feeds the sweep"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "pending");
    // The confirmation endpoint recovers: the transfer confirms, honors and
    // credits — the intent the sweep would otherwise have buried.
    let healed = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: "sigNowConfirmed00000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let (_checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&healed, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 1);
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: an honoring transfer whose `blockTime` the chain did not
/// report is unverifiable *timing*, not a missing payment — the old code
/// refused it `Forbidden`, the poll read that as unpaid, and the sweep
/// expired the row over money already in the vault. Now the verdict defers:
/// the row stays `pending`, and a later pass that sees the `blockTime`
/// credits the on-time payment.
#[tokio::test]
async fn a_missing_block_time_defers_and_the_next_pass_credits() {
    let Some(db) = database("solana_no_blocktime").await else {
        return;
    };
    let terms = terms();
    let org = organization(&db, "no-blocktime").await;
    let wallet = wallet_for(&db, org).await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    // Past the grace window — the row would expire the moment a pass reached
    // a real Unpaid verdict; a Deferred verdict must never produce one.
    sqlx::query(
        "UPDATE oxsum.deposits SET meta = jsonb_set(meta, '{expiresAt}', $1::jsonb) \
         WHERE deposit_id = $2",
    )
    .bind("\"2000-01-01T00:00:00Z\"")
    .bind(deposit_id)
    .execute(db.pool())
    .await
    .unwrap();
    /// Honors the transfer but reports no `blockTime` — timing unverifiable.
    struct NoBlockTime;
    impl std::fmt::Debug for NoBlockTime {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("NoBlockTime").finish_non_exhaustive()
        }
    }
    impl ChainLookup for NoBlockTime {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async {
                Ok(vec![
                    "sigNoTime0000000000000000000000000000000000000000".to_owned(),
                ])
            })
        }
        fn validate_transfer<'a>(
            &'a self,
            signature: &'a str,
            _expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            let signature = signature.to_owned();
            Box::pin(async move {
                // The chain answered the transaction without a `blockTime` —
                // timing it cannot prove is timing the credit cannot judge.
                Ok(Some(ValidatedTransfer {
                    signature,
                    delivered: 5_000,
                    block_time: None,
                    payers: Vec::new(),
                }))
            })
        }
    }
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let timeless = SolanaRailVerifier::new(Arc::new(NoBlockTime), terms.clone());
    let (_checked, credited, expired, _queued) = db
        .poll_solana_deposits(&timeless, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 0, "unverifiable timing is not a credit");
    assert_eq!(expired, 0, "unverifiable timing is not unpaid either");
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "pending");
    // A later pass reads the `blockTime` — the transfer lands on-time and
    // credits.
    let healed = SolanaRailVerifier::new(
        Arc::new(OneSig {
            signature: "sigTimedNow000000000000000000000000000000000000000000".to_owned(),
            delivered: 5_000,
            payers: Vec::new(),
        }),
        terms,
    );
    let (_checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&healed, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 1);
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: a screening service that cannot answer is not a clear — but
/// its non-answer is not "unpaid" either. The old code refused `Forbidden`,
/// the poll read that as unpaid, and a past-TTL intent expired over a payment
/// the vault already held. Now `TemporarilyUnavailable` defers: the row stays
/// `pending` through the outage, and the first pass after the screen recovers
/// credits the payment.
#[tokio::test]
async fn an_unavailable_screen_defers_and_the_recovery_credits() {
    let Some(db) = database("solana_screen_defer").await else {
        return;
    };
    let terms = terms();
    let org = organization(&db, "screen-defer").await;
    let wallet = wallet_for(&db, org).await;
    let deposit_id = Uuid::new_v4();
    db.open_solana_deposit(org, deposit_id, 1000, &terms, None, &static_rate(&terms))
        .await
        .unwrap()
        .expect("inserts");
    sqlx::query(
        "UPDATE oxsum.deposits SET meta = jsonb_set(meta, '{expiresAt}', $1::jsonb) \
         WHERE deposit_id = $2",
    )
    .bind("\"2000-01-01T00:00:00Z\"")
    .bind(deposit_id)
    .execute(db.pool())
    .await
    .unwrap();
    /// Honors the transfer; the payer screens clean. The verdict the screen
    /// answers is swapped out from under the verifier — unavailable first,
    /// clear once the service recovers.
    struct PayingSig {
        signature: String,
        payers: Vec<String>,
    }
    impl std::fmt::Debug for PayingSig {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PayingSig").finish_non_exhaustive()
        }
    }
    impl ChainLookup for PayingSig {
        fn find_reference<'a>(
            &'a self,
            _reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async move { Ok(vec![self.signature.clone()]) })
        }
        fn validate_transfer<'a>(
            &'a self,
            signature: &'a str,
            expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                if signature != self.signature
                    || expected.recipient != "oxsumDepositAddr111111111111111111111111111"
                {
                    return Ok(None);
                }
                Ok(Some(ValidatedTransfer {
                    signature: signature.to_owned(),
                    delivered: 5_000,
                    block_time: Some(0),
                    payers: self.payers.clone(),
                }))
            })
        }
    }
    struct SwappableScreen {
        verdict: Mutex<oxsum_core::ScreenVerdict>,
    }
    impl std::fmt::Debug for SwappableScreen {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SwappableScreen").finish_non_exhaustive()
        }
    }
    impl oxsum_core::SanctionsScreen for SwappableScreen {
        fn screen<'a>(
            &'a self,
            _address: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = oxsum_core::ScreenVerdict> + Send + 'a>,
        > {
            let verdict = self.verdict.lock().unwrap().clone();
            Box::pin(async move { verdict })
        }
    }
    let screen = Arc::new(SwappableScreen {
        verdict: Mutex::new(oxsum_core::ScreenVerdict::TemporarilyUnavailable),
    });
    let verifier = SolanaRailVerifier::new(
        Arc::new(PayingSig {
            signature: "sigScreenOut00000000000000000000000000000000000000000".to_owned(),
            payers: vec!["payerScreened555555555555555555555555555555".to_owned()],
        }),
        terms,
    )
    .with_sanctions(screen.clone());
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let (_checked, credited, expired, _queued) = db
        .poll_solana_deposits(&verifier, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(
        credited, 0,
        "deny-until-cleared never books unscreened money"
    );
    assert_eq!(expired, 0, "a screening non-answer is never unpaid");
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        status, "pending",
        "the row survives the outage pending — the old code expired it"
    );
    // The screen recovers: the verified payment screens clear and credits.
    *screen.verdict.lock().unwrap() = oxsum_core::ScreenVerdict::Clear;
    let (_checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&verifier, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(credited, 1);
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "credited");
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

/// Issue #1174: the rescue pass only reaches `expired` rows whose
/// `expiresAt` fell within the last 24 h — an intent can only rescue a
/// payment that landed before its own deadline, so anything older is done.
/// The 25 h-old row stays `expired` untouched even with its payment armed;
/// the 1 h-old row rescues through the same poll.
#[tokio::test]
async fn the_rescue_pass_ignores_expired_rows_older_than_a_day() {
    let Some(db) = database("solana_rescue_window").await else {
        return;
    };
    let terms = terms();
    let org = organization(&db, "rescue-window").await;
    let wallet = wallet_for(&db, org).await;
    let old_id = Uuid::new_v4();
    let fresh_id = Uuid::new_v4();
    let mut refs = std::collections::HashMap::new();
    for id in [old_id, fresh_id] {
        let (reference, _, _) = db
            .open_solana_deposit(org, id, 1000, &terms, None, &static_rate(&terms))
            .await
            .unwrap()
            .expect("inserts");
        refs.insert(id, reference);
        db.expire_solana_deposit(id).await.unwrap();
    }
    // `expired` writes preserve meta.expiresAt — backdate it directly: one
    // row inside the 24 h rescue window, one outside it.
    let old_stamp = (time::OffsetDateTime::now_utc() - time::Duration::hours(25))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let fresh_stamp = (time::OffsetDateTime::now_utc() - time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    for (id, stamp) in [(old_id, old_stamp), (fresh_id, fresh_stamp)] {
        sqlx::query(
            "UPDATE oxsum.deposits SET meta = jsonb_set(meta, '{expiresAt}', $1::jsonb) \
             WHERE deposit_id = $2",
        )
        .bind(serde_json::Value::String(stamp))
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
    }
    /// Pays the fresh row's reference; the old row's marker finds nothing —
    /// even so, the old row must never be walked at all.
    struct PaysFresh {
        fresh_reference: String,
        signature: String,
    }
    impl std::fmt::Debug for PaysFresh {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PaysFresh").finish_non_exhaustive()
        }
    }
    impl ChainLookup for PaysFresh {
        fn find_reference<'a>(
            &'a self,
            reference: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<String>, LookupError>> + Send + 'a>,
        > {
            Box::pin(async move {
                if reference == self.fresh_reference {
                    Ok(vec![self.signature.clone()])
                } else {
                    Ok(Vec::new())
                }
            })
        }
        fn validate_transfer<'a>(
            &'a self,
            signature: &'a str,
            expected: &'a ExpectedTransfer,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<ValidatedTransfer>, LookupError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                if signature != self.signature
                    || expected.recipient != "oxsumDepositAddr111111111111111111111111111"
                {
                    return Ok(None);
                }
                Ok(Some(ValidatedTransfer {
                    signature: signature.to_owned(),
                    delivered: 5_000,
                    block_time: Some(0),
                    payers: Vec::new(),
                }))
            })
        }
    }
    let verifier = SolanaRailVerifier::new(
        Arc::new(PaysFresh {
            fresh_reference: refs[&fresh_id].clone(),
            signature: "sigFreshRescue0000000000000000000000000000000000000000".to_owned(),
        }),
        terms,
    );
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let (checked, credited, _expired, _queued) = db
        .poll_solana_deposits(&verifier, &tenants, time::Date::MAX)
        .await
        .unwrap();
    assert_eq!(
        checked, 1,
        "only the in-window expired row is walked — the day-old one is out of scope"
    );
    assert_eq!(credited, 1);
    let old_status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(old_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        old_status, "expired",
        "a deposit whose deadline passed over a day ago is genuinely done"
    );
    let fresh_status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id = $1")
            .bind(fresh_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(fresh_status, "credited");
    assert_eq!(wallet.available().await.unwrap(), 1000);
}

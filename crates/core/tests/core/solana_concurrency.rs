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
    AssetTerms, ChainLookup, Db, ExpectedTransfer, LockedRate, NewUser, SolanaCredit,
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
            // The transfer must name the intent's marker and recipient — a
            // raced confirm on a different reference still resolves to the
            // same signature the way a two-reference transaction does.
            if signature != self.signature
                || expected.recipient != "oxsumDepositAddr111111111111111111111111111"
            {
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
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
            Box::pin(async move {
                let entered = self.entered.lock().unwrap().take();
                if let Some(entered) = entered {
                    let _ = entered.send(());
                }
                let release = self.release.lock().unwrap().take();
                if let Some(release) = release {
                    let _ = release.await;
                }
                vec![self.signature.clone()]
            })
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
                    || expected.recipient != "oxsumDepositAddr111111111111111111111111111"
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

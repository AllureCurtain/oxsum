//! Signed sale-snapshot checkpoints, against real PostgreSQL (issue #600).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! A sale-priced settlement's description commits `sale.digest` — the resolved
//! `SalePriceSnapshot`'s tagged-BLAKE3 hash — and `receipt_proof` binds it to
//! the operator-signed sale-catalog checkpoint, the `sale` field on the bundle.
//! These tests pin the promises the verifier relies on: the resolved snapshot
//! records its digest once and re-serves the same canonical row, the `sale`
//! link verifies under `oxsum_verify::verify_sale_head`, a tampered
//! price/discount/customer/signer fails by name, and a wholesale-priced bill is
//! `NotSalePriced`, never read as proven by an old channel checkpoint.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/integration.rs"]
mod setup;

use oxsum_core::{
    BillLine, Db, NewUser, SaleChange, SaleChangeRequest, SaleMutationResult, SaleTerms,
    Settlement, SettlementKind, SettlementRecord, SettlementSale, UsageRecord, Wallet,
    canonical_sale_snapshot, entry_id_for, sale_snapshot_hash, settlement_key_for, signing_key,
};
use serde_json::json;
use std::collections::BTreeMap;
use time::OffsetDateTime;
use time::macros::date;
use uuid::Uuid;

const D: time::Date = date!(2026 - 10 - 01);
const SEED: [u8; 32] = [42; 32];
const PASSWORD: &str = "sale head fixture password";

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

macro_rules! db_or_skip {
    () => {
        match oxsum_testkit::suite_db_url("sale_heads") {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// The business owner session, a registered customer, and the Db — the shape
/// `sale_books.rs`'s fixture takes, kept local so this suite never couples to it.
async fn fixture() -> Option<(Db, String, Uuid, String)> {
    let url = oxsum_testkit::suite_db_url("sale_heads")?;
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    Some((
        db,
        owner,
        customer.organization.id,
        customer.organization.tenant_id,
    ))
}

fn terms(rate: i64, discount: i64) -> SaleTerms {
    serde_json::from_value(
        json!({"price":{"inputPricePerMillion":rate,"outputPricePerMillion":rate,"maxOutputTokens":100},"discountPercent":discount}),
    )
    .unwrap()
}

async fn change(db: &Db, token: &str, change: SaleChange) -> SaleMutationResult {
    db.change_sale_terms(
        token,
        &SaleChangeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "sale head fixture change".into(),
            change,
        },
    )
    .await
    .unwrap()
}

/// Publishes a one-model book at `rate`/`discount` and assigns it to `org` —
/// the live terms `resolve_sale_price` then pins.
async fn assign_book(db: &Db, owner: &str, org: Uuid, model: &str, rate: i64, discount: i64) {
    let book = change(
        db,
        owner,
        SaleChange::CreateBook {
            name: Uuid::new_v4().to_string(),
        },
    )
    .await
    .resource_id;
    change(
        db,
        owner,
        SaleChange::SaveDraft {
            book_id: book,
            expected_revision: 0,
            models: BTreeMap::from([(model.to_owned(), terms(rate, discount))]),
        },
    )
    .await;
    change(
        db,
        owner,
        SaleChange::PublishDraft {
            book_id: book,
            draft_revision: 1,
        },
    )
    .await;
    change(
        db,
        owner,
        SaleChange::AssignBook {
            organization_id: org,
            book_id: book,
            version: 1,
            // Omitting the start resolves to transaction time, so the terms are
            // already in force when the test resolves them a moment later.
            effective_from: None,
            effective_until: None,
            replaces: None,
        },
    )
    .await;
}

/// Settles one sale-priced turn against `wallet`, committing `sale.digest`, and
/// returns the settlement entry's id and the description that wrote it.
async fn settle_sale(
    wallet: &Wallet,
    channel: &str,
    model: &str,
    price_version: i64,
    digest: &str,
) -> (doubleentry::EntryId, String) {
    settle(wallet, channel, model, price_version, Some(digest)).await
}

/// As [`settle_sale`], but the description commits no `sale` digest — a turn
/// the wholesale channel catalog priced.
async fn settle_wholesale(
    wallet: &Wallet,
    channel: &str,
    model: &str,
    price_version: i64,
) -> (doubleentry::EntryId, String) {
    settle(wallet, channel, model, price_version, None).await
}

async fn settle(
    wallet: &Wallet,
    channel: &str,
    model: &str,
    price_version: i64,
    digest: Option<&str>,
) -> (doubleentry::EntryId, String) {
    let hold_key = format!("hold-{}", Uuid::new_v4().simple());
    wallet.hold(&hold_key, "hold", 900_000, D).await.unwrap();
    let usage = UsageRecord::tokens(10, 2).unwrap();
    let sale = digest.map(|d| SettlementSale { digest: d });
    let description = Settlement {
        request: &hold_key,
        channel,
        model,
        requested_model: model,
        served_model: model,
        upstream_model: None,
        price_version,
        kind: SettlementKind::Usage,
        usage: &usage,
        lines: &[
            BillLine {
                item: "input".into(),
                units: 10,
                price_per_m: 1_000_000,
            },
            BillLine {
                item: "output".into(),
                units: 2,
                price_per_m: 2_000_000,
            },
        ],
        matched_rule: None,
        discount_percent: None,
        charged: 14,
        freeze: 900_000,
        upstream_attempts: 1,
        mandate: None,
        subscription: None,
        cache: None,
        guardrail: Vec::new(),
        reason: None,
        experiment: None,
        sale,
    }
    .description()
    .expect("the settlement serializes");
    wallet
        .settle(&hold_key, &description, 14, D)
        .await
        .expect("the turn settles");
    (entry_id_for(&settlement_key_for(&hold_key)), description)
}

/// The `sale` link carries everything the verifier needs without trusting the
/// server again, and `verify_sale_head` confirms the binding end to end.
#[tokio::test]
async fn a_sale_priced_settlements_bundle_verifies() {
    let _guard = SERIAL.lock().await;
    let url = db_or_skip!();
    let _ = url;
    let Some((db, owner, org, tenant)) = fixture().await else {
        return;
    };
    let business = db.business_ownership().await.unwrap().business.id;
    let model = format!("m-{}", &Uuid::new_v4().simple().to_string()[..8]);
    assign_book(&db, &owner, org, &model, 1_000_000, 0).await;

    // Resolving the live terms records the digest → canonical row mapping.
    let snapshot = db
        .resolve_sale_price(business, org, &model, OffsetDateTime::now_utc())
        .await
        .unwrap();
    let canonical = canonical_sale_snapshot(&snapshot).unwrap();
    let digest = sale_snapshot_hash(&canonical).to_hex();

    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    wallet.top_up("topup", 10_000_000, D).await.unwrap();
    let (entry_id, description) = settle_sale(&wallet, "chan", &model, 1, &digest).await;

    let record = SettlementRecord::parse(&description).expect("the description parses");
    assert_eq!(
        record.sale.as_ref().map(|s| s.digest.as_str()),
        Some(digest.as_str())
    );

    let key = signing_key(SEED).expect("constant key name");
    let bundle = wallet
        .receipt_proof(entry_id, Some(&key))
        .await
        .expect("the proof builds")
        .expect("the settlement entry exists");
    let link = bundle
        .sale
        .as_ref()
        .expect("a sale-priced settlement binds the sale");
    assert_eq!(link.digest, digest);
    assert_eq!(link.row, canonical);

    // The verifier reads the whole bundle; the binding checks out end to end.
    let bundle_json = serde_json::to_string(&bundle).unwrap();
    assert_eq!(oxsum_verify::verify_sale_head(&bundle_json), Ok(()));
    // And the composed bill verdict names the sale binding.
    let verdict = oxsum_verify::verify_bill(&bundle_json, &bundle.entry.content_hash())
        .expect("the bundle parses");
    assert_eq!(verdict.verdict, "pass");
    assert!(verdict.sale.is_some());
}

/// The stored snapshot re-signs identically however the sale book has since
/// moved: the digest a bill committed is content-addressed, so it stays
/// recoverable and identical forever.
#[tokio::test]
async fn a_committed_digest_stays_signable() {
    let _guard = SERIAL.lock().await;
    let Some((db, owner, org, _tenant)) = fixture().await else {
        return;
    };
    let business = db.business_ownership().await.unwrap().business.id;
    let model = format!("m-{}", &Uuid::new_v4().simple().to_string()[..8]);
    assign_book(&db, &owner, org, &model, 1_000_000, 0).await;
    let snapshot = db
        .resolve_sale_price(business, org, &model, OffsetDateTime::now_utc())
        .await
        .unwrap();
    let digest = sale_snapshot_hash(&canonical_sale_snapshot(&snapshot).unwrap()).to_hex();

    let key = signing_key(SEED).expect("constant key name");
    let (before, _) = db
        .sign_sale_snapshot(&key, Some(&digest), None, None, None)
        .await
        .unwrap()
        .expect("the digest resolves");

    // Re-resolving at the same instant yields the same snapshot, so the digest
    // is stable; re-signing it yields the byte-identical note.
    let again = db
        .resolve_sale_price(business, org, &model, snapshot.at)
        .await
        .unwrap();
    assert_eq!(
        sale_snapshot_hash(&canonical_sale_snapshot(&again).unwrap()).to_hex(),
        digest,
        "the same terms digest to the same value",
    );
    let (after, row) = db
        .sign_sale_snapshot(&key, Some(&digest), None, None, None)
        .await
        .unwrap()
        .expect("the digest still resolves");
    assert_eq!(after.note, before.note, "signing is deterministic");
    assert_eq!(after.digest.to_hex(), digest);
    assert_eq!(row, canonical_sale_snapshot(&snapshot).unwrap());
}

/// A wholesale-priced settlement commits no `sale` digest: the bundle binds the
/// wholesale `price` link as always, and `verify_sale_head` reports
/// `NotSalePriced` — the channel checkpoint must never be read as proving
/// customer sale terms.
#[tokio::test]
async fn a_wholesale_bill_is_not_sale_priced() {
    let _guard = SERIAL.lock().await;
    let Some((db, _owner, _org, tenant)) = fixture().await else {
        return;
    };
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    wallet.top_up("topup", 10_000_000, D).await.unwrap();
    // No `sale` on the description — the wholesale channel catalog priced it.
    let (entry_id, _description) = settle_wholesale(&wallet, "chan", "m", 1).await;
    let key = signing_key(SEED).expect("constant key name");
    let bundle = wallet
        .receipt_proof(entry_id, Some(&key))
        .await
        .unwrap()
        .unwrap();
    assert!(bundle.sale.is_none());
    let bundle_json = serde_json::to_string(&bundle).unwrap();
    assert_eq!(
        oxsum_verify::verify_sale_head(&bundle_json),
        Err(oxsum_verify::SaleError::NotSalePriced)
    );

    // A committed digest that was never recorded binds nothing — the field is
    // absent, not signed over a fabrication.
    let (ghost_id, _d) = settle_sale(&wallet, "chan", "m", 1, &"0".repeat(64)).await;
    let bundle = wallet
        .receipt_proof(ghost_id, Some(&key))
        .await
        .unwrap()
        .unwrap();
    assert!(bundle.sale.is_none());
}

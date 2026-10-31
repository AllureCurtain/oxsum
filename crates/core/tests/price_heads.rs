//! Signed price catalog snapshots, against real PostgreSQL (issue #204).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! A settlement description names the `(channel, model, priceVersion)` it
//! charged under; `receipt_proof` binds that triple to the operator-signed
//! catalog checkpoint — the `price` field on the bundle. These tests pin the
//! promises the verifier relies on: `price_row` reads a pinned version or the
//! latest, an old version stays signable after the catalog moves on, and the
//! field is absent exactly when there is nothing to attest (a top-up, no
//! signing key, or a price row that no longer exists).
//!
//! Channel and model names carry a random suffix — the database is shared
//! between tests and a shared name would couple them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use doubleentry::witness::signing::VerifyingKey;
use doubleentry::witness::{KeyName, SignedTreeHead};
use oxsum_core::{
    Db, Price, SecretKey, Settlement, SettlementKind, SettlementRecord, UsageRecord, Wallet,
    canonical_price_row, entry_id_for, price_row, price_row_hash, settlement_key_for, signing_key,
};
use sqlx::PgPool;
use time::macros::date;

const D: time::Date = date!(2026 - 10 - 01);
const SEED: [u8; 32] = [42; 32];

fn url() -> Option<String> {
    // `.env` is searched for in the current directory and its parents, see docs/development.md.
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("price_heads")
}

macro_rules! db_or_skip {
    () => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

/// A database handle with oxsum's own tables applied, over the pool both the
/// `Db` and the tenant `Wallet` share.
async fn db(url: &str) -> (Db, PgPool) {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool.clone());
    db.migrate().await.expect("migrates");
    (db, pool)
}

/// Each test uses its own channel and model names, so tests never interfere
/// and reruns never collide.
fn fresh(name: &str) -> String {
    format!("{name}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// `fresh` for a tenant id: the same uniqueness, spelled with the underscore a
/// tenant id requires (`Wallet::open` rejects dashes).
fn fresh_tenant(name: &str) -> String {
    format!("{name}_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

fn secret() -> SecretKey {
    SecretKey::from_bytes([7; 32])
}

fn price(input: i64, output: i64, max: i64) -> Price {
    Price {
        input_price_per_million: input,
        output_price_per_million: output,
        max_output_tokens: max,
        cache_read_price_per_million: None,
        cache_write_5m_price_per_million: None,
        cache_write_1h_price_per_million: None,
        reasoning_price_per_million: None,
        cost_per_request: None,
        upstream: None,
        mode: Default::default(),
        rules: Vec::new(),
    }
}

/// Registers a channel with one priced model and returns the channel name.
async fn priced_channel(db: &Db, model: &str, input: i64) -> String {
    let channel = fresh("chan");
    db.set_channel(
        &channel,
        "https://upstream.example/v1",
        "sk-secret-1234",
        "openai",
        &secret(),
    )
    .await
    .unwrap();
    db.append_price(&channel, model, price(input, 2_000_000, 8_000), 100)
        .await
        .unwrap();
    channel
}

/// Settles one turn against `wallet` under `(channel, model, price_version)`
/// and returns the settlement entry's id and the description that wrote it.
async fn settle_one(
    wallet: &Wallet,
    channel: &str,
    model: &str,
    price_version: i64,
) -> (doubleentry::EntryId, String) {
    let hold_key = format!("hold-{}", fresh("price"));
    wallet.hold(&hold_key, "hold", 900_000, D).await.unwrap();
    let usage = UsageRecord::tokens(10, 2).unwrap();
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
            oxsum_core::BillLine {
                item: "input".into(),
                units: 10,
                price_per_m: 1_000_000,
            },
            oxsum_core::BillLine {
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
    }
    .description()
    .expect("the settlement serializes");
    wallet
        .settle(&hold_key, &description, 14, D)
        .await
        .expect("the turn settles");
    (entry_id_for(&settlement_key_for(&hold_key)), description)
}

/// A pinned version reads back exactly that version; an absent pin reads the
/// newest — the two shapes `/api/v1/log/price-head` selects by.
#[tokio::test]
async fn price_row_reads_a_pinned_version_or_the_latest() {
    let url = db_or_skip!();
    let (db, pool) = db(&url).await;
    let model = fresh("m");
    let channel = priced_channel(&db, &model, 1_000_000).await;
    db.append_price(&channel, &model, price(3_000_000, 4_000_000, 9_000), 100)
        .await
        .unwrap();

    let latest = price_row(&pool, &channel, &model, None)
        .await
        .unwrap()
        .expect("a priced model");
    assert_eq!(latest.version, 2);
    assert_eq!(latest.price.input_price_per_million, 3_000_000);

    let pinned = price_row(&pool, &channel, &model, Some(1))
        .await
        .unwrap()
        .expect("v1 stays readable");
    assert_eq!(pinned.version, 1);
    assert_eq!(pinned.price.input_price_per_million, 1_000_000);

    assert!(
        price_row(&pool, &channel, &model, Some(99))
            .await
            .unwrap()
            .is_none(),
        "an unwritten version is absent, not latest"
    );
    assert!(
        price_row(&pool, &channel, &fresh("m"), None)
            .await
            .unwrap()
            .is_none(),
        "an unpriced model is absent"
    );
}

/// The `price` field carries everything the verifier needs without trusting
/// the server again: the channel/model/version the entry names, the canonical
/// row bytes, the note whose root commits to their hash, and the key that
/// signed it — and the note checks out against `entry.description`.
#[tokio::test]
async fn a_settlements_bundle_carries_the_signed_price_link() {
    let url = db_or_skip!();
    let (db, pool) = db(&url).await;
    let tenant = fresh_tenant("price_bundle");
    let wallet = Wallet::open(pool.clone(), &tenant).await.unwrap();
    wallet.top_up("topup", 10_000_000, D).await.unwrap();

    let model = fresh("m");
    let channel = priced_channel(&db, &model, 1_000_000).await;
    let (entry_id, description) = settle_one(&wallet, &channel, &model, 1).await;

    let key = signing_key(SEED).expect("constant key name");
    let bundle = wallet
        .receipt_proof(entry_id, Some(&key))
        .await
        .expect("the proof builds")
        .expect("the settlement entry exists");
    let link = bundle.price.expect("a settlement names a signed price");

    // The binding names exactly what the entry's description names.
    let record = SettlementRecord::parse(&description).expect("the description parses");
    assert_eq!(link.channel, record.channel);
    assert_eq!(link.model, record.model);
    assert_eq!(link.version, record.price_version);

    // The note attests the catalog origin, the version, and the row hash —
    // verifiable under the key the bundle publishes.
    let parsed = SignedTreeHead::parse(&link.note).expect("the note parses");
    assert_eq!(
        parsed.origin.as_str(),
        format!("oxsum/price-catalog/{channel}")
    );
    assert_eq!(parsed.head.size, 1);
    assert_eq!(parsed.head.root, price_row_hash(&link.row));

    let raw: Vec<u8> = (0..link.public_key.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&link.public_key[i..i + 2], 16).unwrap())
        .collect();
    let bytes: [u8; 32] = raw.try_into().expect("a 32-byte hex public key");
    let verifying = VerifyingKey::from_bytes(KeyName::new(&link.key_name).expect("valid"), bytes)
        .expect("a real public key");
    assert_eq!(
        verifying
            .note_key_hash()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        link.key_hash
    );
    let signature = parsed
        .signatures
        .iter()
        .find(|s| s.key_hash == verifying.note_key_hash())
        .expect("the operator's signature line is on the note");
    assert!(verifying.verify(&parsed.body(), signature));

    // The canonical row the bundle carries is what the stored row renders.
    let stored = price_row(&pool, &channel, &model, Some(1))
        .await
        .unwrap()
        .expect("v1 exists");
    assert_eq!(
        link.row,
        canonical_price_row(&channel, &stored).expect("renders")
    );
}

/// v1 stays verifiable after v2 lands: the same stored row re-signs to the
/// identical note, so a bundle issued before the catalog moved still checks.
#[tokio::test]
async fn an_old_version_stays_signable_after_the_catalog_moves_on() {
    let url = db_or_skip!();
    let (db, pool) = db(&url).await;
    let tenant = fresh_tenant("price_versions");
    let wallet = Wallet::open(pool.clone(), &tenant).await.unwrap();
    wallet.top_up("topup", 10_000_000, D).await.unwrap();

    let model = fresh("m");
    let channel = priced_channel(&db, &model, 1_000_000).await;
    let (entry_id, _description) = settle_one(&wallet, &channel, &model, 1).await;

    let key = signing_key(SEED).expect("constant key name");
    let before = wallet
        .receipt_proof(entry_id, Some(&key))
        .await
        .unwrap()
        .unwrap()
        .price
        .expect("the v1 bundle binds the price");

    // The catalog moves on: v2 replaces what `latest` serves.
    db.append_price(&channel, &model, price(9_000_000, 9_000_000, 9_000), 100)
        .await
        .unwrap();

    let after = wallet
        .receipt_proof(entry_id, Some(&key))
        .await
        .unwrap()
        .unwrap()
        .price
        .expect("v1 still binds");
    assert_eq!(after.version, 1);
    assert_eq!(after.note, before.note, "signing is deterministic");
    assert_eq!(after.row, before.row);
}

/// `None` is a state, not a failure: a top-up names no price, a missing
/// signing key binds nothing, and a settlement whose price row is gone binds
/// nothing — each still yields a bundle.
#[tokio::test]
async fn the_price_link_is_absent_when_there_is_nothing_to_attest() {
    let url = db_or_skip!();
    let (db, pool) = db(&url).await;
    let tenant = fresh_tenant("price_absent");
    let wallet = Wallet::open(pool.clone(), &tenant).await.unwrap();
    wallet.top_up("topup", 10_000_000, D).await.unwrap();

    // A top-up entry is not a settlement: nothing to sign it against.
    let key = signing_key(SEED).expect("constant key name");
    let topup_id = entry_id_for("topup");
    let bundle = wallet
        .receipt_proof(topup_id, Some(&key))
        .await
        .unwrap()
        .expect("the top-up entry exists");
    assert!(bundle.price.is_none());

    // No key configured: the entry proves, the field stays absent.
    let model = fresh("m");
    let channel = priced_channel(&db, &model, 1_000_000).await;
    let (entry_id, _description) = settle_one(&wallet, &channel, &model, 1).await;
    let bundle = wallet.receipt_proof(entry_id, None).await.unwrap().unwrap();
    assert!(bundle.price.is_none(), "unsigned bundles stay provable");

    // A settlement that names a price the catalog never held: absent, not
    // signed over a fabrication.
    let ghost = fresh("ghost");
    let (ghost_id, _description) = settle_one(&wallet, &ghost, &model, 1).await;
    let bundle = wallet
        .receipt_proof(ghost_id, Some(&key))
        .await
        .unwrap()
        .unwrap();
    assert!(bundle.price.is_none());
}

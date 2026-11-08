//! Real PostgreSQL regressions for connection ownership and canonical source verification.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use doubleentry::account::AccountRegistry;
use doubleentry::storage::postgres::{PostgresError, PostgresStore};
use doubleentry::{
    Amount, Balanced, Currency, Description, Dimensions, DocumentRef, Draft, Entry, EntryBatch,
    EntryId, Hash, IdempotencyKey, Label, Layer, LedgerId, LedgerPolicy, LedgerStore,
    PeriodCalendar, Posting, Provenance, SealContext,
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use time::macros::date;
use uuid::Uuid;

use crate::{Db, NewUser, SCALE, Wallet, WalletError};

const DAY: time::Date = date!(2026 - 10 - 01);
struct World {
    pool: PgPool,
    wallet: Wallet,
    store: PostgresStore<SCALE>,
    tenant: String,
}

async fn world(connections: u32) -> Option<World> {
    let url = oxsum_testkit::suite_db_url("borrowed_ledger")?;
    let pool = PgPoolOptions::new()
        .max_connections(connections)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&url)
        .await
        .unwrap();
    Db::from_pool(pool.clone()).migrate().await.unwrap();
    let tenant = format!("source_{}", Uuid::new_v4().simple());
    let wallet = Wallet::open(pool.clone(), &tenant).await.unwrap();
    let store = PostgresStore::new(
        pool.clone(),
        LedgerId::new(format!("tenant-{tenant}")).unwrap(),
    )
    .in_schema(&format!("ledger_{tenant}"));
    Some(World {
        pool,
        wallet,
        store,
        tenant,
    })
}

async fn complete_entry(w: &World, verifiable: bool) -> Entry<Balanced, SCALE> {
    let registry = AccountRegistry::from_records(w.store.accounts().await.unwrap()).unwrap();
    let records = registry.records();
    let cash = records
        .iter()
        .find(|r| r.account.path.to_string() == "Assets:Cash")
        .unwrap()
        .id;
    let income = records
        .iter()
        .find(|r| r.account.path.to_string() == "Income:Usage")
        .unwrap()
        .id;
    let mut dimensions = Dimensions::none();
    dimensions
        .set(
            Label::new("customer").unwrap(),
            Label::new("fixture").unwrap(),
        )
        .unwrap();
    dimensions
        .set(Label::new("region").unwrap(), Label::new("test").unwrap())
        .unwrap();
    let document = if verifiable {
        DocumentRef::new("fixture-document", Hash::from_bytes([7; 32])).unwrap()
    } else {
        DocumentRef::unverified("fixture-document").unwrap()
    };
    let draft = Entry::<Draft, SCALE>::new(
        EntryId::generate(),
        IdempotencyKey::new(vec![0, 255, 128, 42, u8::from(verifiable)]).unwrap(),
        DAY,
    )
    .with_value_date(date!(2026 - 10 - 02))
    .with_description(Description::new("full canonical fixture\nsecond line").unwrap())
    .with_kind(Label::new("fixture-kind").unwrap())
    .with_provenance(
        Provenance::none()
            .with_actor("actor")
            .unwrap()
            .with_source("source")
            .unwrap()
            .with_correlation("correlation")
            .unwrap(),
    )
    .with_document(document)
    .post(
        Posting::debit(cash, Amount::from_minor(70), Currency::USD)
            .with_dimensions(dimensions.clone()),
    )
    .post(
        Posting::credit(income, Amount::from_minor(70), Currency::USD)
            .with_dimensions(dimensions.clone()),
    )
    .post(
        Posting::debit(cash, Amount::from_minor(30), Currency::USD)
            .in_layer(Layer::Pending)
            .with_dimensions(dimensions.clone()),
    )
    .post(
        Posting::credit(income, Amount::from_minor(30), Currency::USD)
            .in_layer(Layer::Pending)
            .with_dimensions(dimensions),
    );
    let context = SealContext {
        accounts: &registry,
        calendar: &PeriodCalendar::new(),
        policy: &LedgerPolicy::default(),
    };
    let original_id = EntryId::generate();
    let mut original = Entry::<Draft, SCALE>::new(
        original_id,
        IdempotencyKey::new(format!("original-{original_id}").into_bytes()).unwrap(),
        date!(2026 - 09 - 30),
    );
    for posting in draft.postings() {
        original = original.post(posting.inverted());
    }
    w.store
        .append(&EntryBatch::single(original.seal(&context).unwrap()))
        .await
        .unwrap();
    draft
        .reversing(original_id, date!(2026 - 09 - 30))
        .seal(&context)
        .unwrap()
}

#[tokio::test]
async fn canonical_roundtrip_preserves_raw_keys_metadata_dimensions_and_unsequenced_rows() {
    let Some(w) = world(1).await else {
        return;
    };
    for verifiable in [false, true] {
        let entry = complete_entry(&w, verifiable).await;
        let id = entry.id();
        w.store
            .append(&EntryBatch::single(entry.clone()))
            .await
            .unwrap();
        let official = w.store.get(id).await.unwrap().unwrap();
        let mut tx = w.pool.begin().await.unwrap();
        let captured = super::entry_in(&mut tx, &w.tenant, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(captured, official);
        assert_eq!(captured.entry, entry);
        assert_eq!(
            captured.entry.idempotency_key().as_bytes(),
            &[0, 255, 128, 42, u8::from(verifiable)]
        );
        sqlx::query(&format!(
            "UPDATE ledger_{}.entries SET log_index=NULL WHERE entry_id=$1",
            w.tenant
        ))
        .bind(id.as_uuid())
        .execute(&mut *tx)
        .await
        .unwrap();
        let deferred = super::entry_in(&mut tx, &w.tenant, id)
            .await
            .unwrap()
            .unwrap();
        assert!(deferred.index.is_none());
        assert_eq!(deferred.entry, official.entry);
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn each_hashed_field_rejects_tampering_and_malformed_rows_keep_their_identity() {
    let Some(w) = world(1).await else {
        return;
    };
    let entry = complete_entry(&w, true).await;
    let id = entry.id();
    w.store.append(&EntryBatch::single(entry)).await.unwrap();
    let changes = [
        ("entries", "idempotency_key=decode('01','hex')"),
        ("entries", "content_hash=decode(repeat('00',32),'hex')"),
        ("entries", "booking_date=booking_date+1"),
        ("entries", "value_date=value_date+1"),
        ("entries", "description='changed'"),
        ("entries", "kind='changed'"),
        ("entries", "provenance_actor='changed'"),
        ("entries", "provenance_source='changed'"),
        ("entries", "provenance_correlation='changed'"),
        ("entries", "document_id='changed'"),
        (
            "entries",
            "document_content_hash=decode(repeat('08',32),'hex')",
        ),
        ("entries", "reverses='00000000-0000-0000-0000-000000000001'"),
        ("entries", "original_booking_date=original_booking_date+1"),
        ("postings", "amount_minor=amount_minor+1"),
        ("postings", "account_index=0"),
        (
            "postings",
            "direction=CASE direction WHEN 'D' THEN 'C' ELSE 'D' END",
        ),
        (
            "postings",
            "layer=CASE layer WHEN 'pending' THEN 'settled' ELSE 'pending' END",
        ),
        ("postings", "currency='EUR'"),
        ("posting_dimensions", "value='changed'"),
        ("entries", "description=chr(1)"),
        ("entries", "document_content_hash=decode('ff','hex')"),
        ("entries", "idempotency_key=decode('','hex')"),
        ("posting_dimensions", "axis=axis || chr(1)"),
    ];
    for (table, change) in changes {
        let mut tx = w.pool.begin().await.unwrap();
        sqlx::query(&format!(
            "UPDATE ledger_{}.{table} SET {change} WHERE entry_id=$1",
            w.tenant
        ))
        .bind(id.as_uuid())
        .execute(&mut *tx)
        .await
        .unwrap();
        let mut entries = super::entries_in(&mut tx, &w.tenant, &[*id.as_uuid()])
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        let source = entries.remove(0);
        assert_eq!(source.id, *id.as_uuid());
        assert!(
            matches!(
                source.verification,
                Err(PostgresError::Integrity(_) | PostgresError::Malformed(_))
            ),
            "{table}: {change}"
        );
        assert_eq!(
            w.wallet
                .verified_source_hash_in(&mut tx, *id.as_uuid())
                .await
                .unwrap(),
            None
        );
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn missing_foreign_and_broken_storage_remain_distinct() {
    let Some(w) = world(1).await else {
        return;
    };
    let other = format!("source_{}", Uuid::new_v4().simple());
    Wallet::open(w.pool.clone(), &other).await.unwrap();
    let receipt = w.wallet.top_up("only-this-tenant", 100, DAY).await.unwrap();
    let mut tx = w.pool.begin().await.unwrap();
    assert!(
        super::entry_in(&mut tx, &other, receipt.entry_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        super::entry_in(&mut tx, "absent_fixture", receipt.entry_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        super::entry_in(&mut tx, &w.tenant, EntryId::generate())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        super::entries_in(&mut tx, "x; DROP SCHEMA", &[*receipt.entry_id.as_uuid()])
            .await
            .is_err()
    );
    sqlx::query(&format!(
        "ALTER TABLE ledger_{}.postings RENAME TO unavailable_postings",
        w.tenant
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    assert!(matches!(
        w.wallet
            .verified_source_hash_in(&mut tx, *receipt.entry_id.as_uuid())
            .await,
        Err(WalletError::Storage(PostgresError::Database(_)))
    ));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn repeatable_read_keeps_one_snapshot_and_sees_its_own_uncommitted_entries() {
    let Some(w) = world(2).await else {
        return;
    };
    let initial = w.wallet.top_up("initial", 100, DAY).await.unwrap();
    let mut tx = w.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await
        .unwrap();
    let before = super::entry_in(&mut tx, &w.tenant, initial.entry_id)
        .await
        .unwrap()
        .unwrap();
    let later = w.wallet.top_up("later", 200, DAY).await.unwrap();
    sqlx::query(&format!(
        "UPDATE ledger_{}.entries SET description='tampered after snapshot' WHERE entry_id=$1",
        w.tenant
    ))
    .bind(initial.entry_id.as_uuid())
    .execute(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        super::entry_in(&mut tx, &w.tenant, initial.entry_id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
    assert!(
        super::entry_in(&mut tx, &w.tenant, later.entry_id)
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
    let mut tx = w.pool.begin().await.unwrap();
    let order = Uuid::new_v4();
    let first = w
        .wallet
        .plan_prepay_in(&mut tx, &order, 30, DAY)
        .await
        .unwrap();
    let replay = w
        .wallet
        .plan_prepay_in(&mut tx, &order, 30, DAY)
        .await
        .unwrap();
    assert_eq!(first.entry_id, replay.entry_id);
    assert!(!replay.is_new);
    assert_eq!(
        w.wallet
            .verified_source_hash_in(&mut tx, *first.entry_id.as_uuid())
            .await
            .unwrap(),
        Some(first.content_hash.to_hex())
    );
    tx.rollback().await.unwrap();
    assert!(w.store.get(first.entry_id).await.unwrap().is_none());
}

#[tokio::test]
async fn one_connection_completes_credit_hold_settlement_and_plan_replays() {
    let Some(w) = world(1).await else {
        return;
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        w.wallet.set_credit_limit("credit", 100, DAY).await.unwrap();
        w.wallet
            .hold("credit-call", "reserved", 70, DAY)
            .await
            .unwrap();
        let mut tx = w.pool.begin().await.unwrap();
        w.wallet
            .settle_in(&mut tx, "credit-call", "settled", 60, DAY)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(w.wallet.credit_used().await.unwrap(), 60);
        w.wallet.repay_credit("repayment", 20, DAY).await.unwrap();
        w.wallet.top_up("repaying-top-up", 50, DAY).await.unwrap();
        assert_eq!(w.wallet.credit_used().await.unwrap(), 0);
        assert_eq!(w.wallet.available().await.unwrap(), 110);
        let mut tx = w.pool.begin().await.unwrap();
        let journal = Uuid::new_v4();
        let first = w
            .wallet
            .plan_journal_accrual_in(&mut tx, &journal, 100, 0, 0, DAY)
            .await
            .unwrap()
            .unwrap();
        let replay = w
            .wallet
            .plan_journal_accrual_in(&mut tx, &journal, 100, 0, 0, DAY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.entry_id, replay.entry_id);
        assert!(!replay.is_new);
        w.wallet
            .usage_receipt_in(
                &mut tx,
                "free",
                "zero-cost",
                Some("actor"),
                Some("mandate"),
                DAY,
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_borrowed_operations_complete_when_all_pool_slots_are_occupied() {
    let Some(w) = world(2).await else {
        return;
    };
    let mut a = w.pool.begin().await.unwrap();
    let mut b = w.pool.begin().await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        let first = async {
            let result = w
                .wallet
                .plan_prepay_in(&mut a, &Uuid::new_v4(), 10, DAY)
                .await;
            a.commit().await.unwrap();
            result.unwrap()
        };
        let second = async {
            let result = w
                .wallet
                .plan_prepay_in(&mut b, &Uuid::new_v4(), 20, DAY)
                .await;
            b.commit().await.unwrap();
            result.unwrap()
        };
        let (one, two) = tokio::join!(first, second);
        assert_ne!(one.entry_id, two.entry_id);
        assert_ne!(one.log_index, two.log_index);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn borrowed_calendar_and_day_seals_refuse_writes_in_the_callers_transaction() {
    let Some(w) = world(1).await else {
        return;
    };
    let mut tx = w.pool.begin().await.unwrap();
    sqlx::query(&format!("INSERT INTO ledger_{}.periods(period_id,starts_on,ends_on,state) VALUES('october','2026-10-01','2026-10-31','sealed')", w.tenant))
        .execute(&mut *tx).await.unwrap();
    assert!(
        w.wallet
            .plan_prepay_in(&mut tx, &Uuid::new_v4(), 10, DAY)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let db = Db::from_pool(w.pool.clone());
    let user = db
        .register(NewUser {
            email: format!("{}@example.test", Uuid::new_v4()),
            password: "borrowed ledger test password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let wallet = Wallet::open(w.pool.clone(), &user.organization.tenant_id)
        .await
        .unwrap();
    wallet.top_up("source", 100, DAY).await.unwrap();
    wallet.hold("hold", "reserved", 50, DAY).await.unwrap();
    let mut tx = w.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO oxsum.usage_anchors(organization_id,day,entry_id,map_root,enduser_count) VALUES($1,$2,$3,$4,0)")
        .bind(user.organization.id).bind(DAY).bind(Uuid::new_v4()).bind(vec![0u8; 32]).execute(&mut *tx).await.unwrap();
    assert!(matches!(
        wallet.settle_in(&mut tx, "hold", "settle", 30, DAY).await,
        Err(WalletError::DaySealed)
    ));
    assert!(matches!(
        wallet
            .usage_receipt_in(&mut tx, "free", "zero-cost", None, None, DAY)
            .await,
        Err(WalletError::DaySealed)
    ));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn one_connection_handles_promo_grant_replay_and_reserved_lots() {
    let Some(w) = world(1).await else {
        return;
    };
    let db = Db::from_pool(w.pool.clone());
    let user = db
        .register(NewUser {
            email: format!("{}@example.test", Uuid::new_v4()),
            password: "promo borrowed test password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let wallet = Wallet::open(w.pool.clone(), &user.organization.tenant_id)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut tx = w.pool.begin().await.unwrap();
        let first = wallet
            .grant_promo_lot_in(
                &mut tx,
                user.organization.id,
                "admin",
                "fixture",
                "promo-grant",
                50,
                None,
                DAY,
            )
            .await
            .unwrap();
        let replay = wallet
            .grant_promo_lot_in(
                &mut tx,
                user.organization.id,
                "admin",
                "fixture",
                "promo-grant",
                50,
                None,
                DAY,
            )
            .await
            .unwrap();
        assert_eq!(first.0, replay.0);
        assert_eq!(first.1.entry_id, replay.1.entry_id);
        tx.commit().await.unwrap();
        wallet
            .hold("promo-hold", "reserved promo", 40, DAY)
            .await
            .unwrap();
        let mut tx = w.pool.begin().await.unwrap();
        wallet
            .settle_in(&mut tx, "promo-hold", "settled promo", 30, DAY)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(wallet.available().await.unwrap(), 20);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn own_uncommitted_hold_preserves_attribution_and_wrong_raw_key_is_refused() {
    let Some(w) = world(1).await else {
        return;
    };
    w.wallet.top_up("funds", 100, DAY).await.unwrap();
    let registry = AccountRegistry::from_records(w.store.accounts().await.unwrap()).unwrap();
    let records = registry.records();
    let account = records
        .iter()
        .find(|r| r.account.path.to_string() == "Liabilities:Wallet")
        .unwrap()
        .id;
    let revenue = records
        .iter()
        .find(|r| r.account.path.to_string() == "Income:Usage")
        .unwrap()
        .id;
    let draft = |id, key| {
        Entry::<Draft, SCALE>::new(id, IdempotencyKey::new(key).unwrap(), DAY)
            .with_provenance(
                Provenance::none()
                    .with_actor("fixture-actor")
                    .unwrap()
                    .with_correlation("fixture-mandate")
                    .unwrap(),
            )
            .post(
                Posting::debit(account, Amount::from_minor(40), Currency::USD)
                    .in_layer(Layer::Pending),
            )
            .post(
                Posting::credit(revenue, Amount::from_minor(40), Currency::USD)
                    .in_layer(Layer::Pending),
            )
            .seal(&SealContext {
                accounts: &registry,
                calendar: &PeriodCalendar::new(),
                policy: &LedgerPolicy::default(),
            })
            .unwrap()
    };
    let mut tx = w.pool.begin().await.unwrap();
    w.store
        .append_in_tx(
            &mut tx,
            &EntryBatch::single(draft(crate::entry_id_for("own-hold"), b"own-hold".to_vec())),
        )
        .await
        .unwrap();
    let settled = w
        .wallet
        .settle_in(&mut tx, "own-hold", "same transaction", 30, DAY)
        .await
        .unwrap();
    let entry = super::entry_in(&mut tx, &w.tenant, settled.entry_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        entry.entry.provenance().actor.as_ref().unwrap().as_str(),
        "fixture-actor"
    );
    assert_eq!(
        entry
            .entry
            .provenance()
            .correlation
            .as_ref()
            .unwrap()
            .as_str(),
        "fixture-mandate"
    );
    tx.rollback().await.unwrap();
    let mut tx = w.pool.begin().await.unwrap();
    w.store
        .append_in_tx(
            &mut tx,
            &EntryBatch::single(draft(
                crate::entry_id_for("claimed-hold"),
                b"different-hold".to_vec(),
            )),
        )
        .await
        .unwrap();
    assert!(matches!(
        w.wallet
            .settle_in(&mut tx, "claimed-hold", "must refuse", 30, DAY)
            .await,
        Err(WalletError::HoldNotFound(_))
    ));
    tx.rollback().await.unwrap();
}

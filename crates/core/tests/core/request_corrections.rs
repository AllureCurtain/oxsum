//! Source correction concurrency, exact replay and financial rollback.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{Db, NewUser, RequestCorrectionInput, Wallet, WalletError};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

async fn world(bonus: bool) -> (Db, String, Uuid, Wallet, Uuid) {
    let url =
        oxsum_testkit::suite_url!().expect("request correction acceptance requires PostgreSQL");
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "request correction customer".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let wallet = Wallet::open(db.pool().clone(), &user.organization.tenant_id)
        .await
        .unwrap();
    let day = OffsetDateTime::now_utc().date();
    if bonus {
        wallet.adjust("fund", "bonus", 1000, day).await.unwrap();
    } else {
        wallet.top_up("fund", 1000, day).await.unwrap();
    }
    let id = Uuid::new_v4();
    let key = format!("req-{id}:hold");
    wallet
        .hold(
            &key,
            &oxsum_core::hold_description(&id.to_string(), "model", 100).unwrap(),
            100,
            day,
        )
        .await
        .unwrap();
    let description=json!({"v":3,"request":id.to_string(),"channel":"fixture","model":"model","priceVersion":1,"kind":"usage","usage":{"inputTokens":100},"lines":[["input",100,1000000],["output",0,1000000]],"charged":100,"freeze":100}).to_string();
    let source = *wallet
        .settle(&key, &description, 100, day)
        .await
        .unwrap()
        .entry_id
        .as_uuid();
    (db, owner, user.organization.id, wallet, source)
}
fn input(
    source: Uuid,
    hash: &str,
    key: &str,
    amount: i64,
    corrected: i64,
) -> RequestCorrectionInput {
    RequestCorrectionInput {
        idempotency_key: key.into(),
        source_entry_id: source,
        source_hash: hash.into(),
        expected_corrected_minor: corrected,
        amount_minor: amount,
        reason: "duplicate billable component".into(),
    }
}
#[tokio::test]
async fn competing_credits_are_bounded_and_exact_retries_do_not_repost() {
    let (db, owner, org, wallet, source) = world(false).await;
    let preview = db
        .preview_request_correction(&owner, org, source)
        .await
        .unwrap();
    assert_eq!(preview.remaining_minor, 100);
    let a = input(source, &preview.source_hash, "a", 70, 0);
    let b = input(source, &preview.source_hash, "b", 70, 0);
    let (first, second) = tokio::join!(
        db.apply_request_correction(&owner, org, &a),
        db.apply_request_correction(&owner, org, &b)
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let request = if first.is_ok() { &a } else { &b };
    let winner = first.or(second).unwrap();
    let again = db
        .apply_request_correction(&owner, org, request)
        .await
        .unwrap();
    assert_eq!(winner.entry_id, again.entry_id);
    assert_eq!(wallet.available().await.unwrap(), 970);
    let preview = db
        .preview_request_correction(&owner, org, source)
        .await
        .unwrap();
    assert_eq!(preview.remaining_minor, 30);
    let changed = input(
        source,
        &preview.source_hash,
        &request.idempotency_key,
        60,
        0,
    );
    assert!(matches!(
        db.apply_request_correction(&owner, org, &changed).await,
        Err(WalletError::Conflict(_))
    ));
    db.apply_request_correction(
        &owner,
        org,
        &input(source, &preview.source_hash, "rest", 30, 70),
    )
    .await
    .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 1000);
    assert_eq!(
        db.preview_request_correction(&owner, org, source)
            .await
            .unwrap()
            .remaining_minor,
        0
    );
    assert_eq!(
        wallet
            .settled_record(&format!("req-{}:hold", preview.request_id))
            .await
            .unwrap()
            .unwrap()
            .charged,
        100
    );
}
#[tokio::test]
async fn unsupported_funding_and_invalid_source_never_credit_purchased_wallet() {
    let (db, owner, org, wallet, source) = world(true).await;
    let before = wallet.available().await.unwrap();
    assert!(matches!(
        db.preview_request_correction(&owner, org, source).await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.preview_request_correction(&owner, org, Uuid::new_v4())
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert_eq!(wallet.available().await.unwrap(), before);
}
#[tokio::test]
async fn audit_failure_rolls_back_credit_and_retry_identity() {
    let (db, owner, org, wallet, source) = world(false).await;
    let preview = db
        .preview_request_correction(&owner, org, source)
        .await
        .unwrap();
    sqlx::query(&format!("CREATE FUNCTION oxsum.reject_correction_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='business.request.correct' AND NEW.detail->'request'->>'sourceEntryId'='{source}' THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$")).execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TRIGGER correction_audit_fail BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_correction_audit()").execute(db.pool()).await.unwrap();
    let request = input(source, &preview.source_hash, "rollback", 50, 0);
    assert!(
        db.apply_request_correction(&owner, org, &request)
            .await
            .is_err()
    );
    assert_eq!(wallet.available().await.unwrap(), 900);
    sqlx::query("DROP TRIGGER correction_audit_fail ON oxsum.admin_audit")
        .execute(db.pool())
        .await
        .unwrap();
    db.apply_request_correction(&owner, org, &request)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 950);
}

#[tokio::test]
async fn correction_is_visible_in_bills_receipt_audit_and_period_evidence_but_scoped_entries_refuse()
 {
    let (db, owner, org, wallet, source) = world(false).await;
    let preview = db
        .preview_request_correction(&owner, org, source)
        .await
        .unwrap();
    let credit = db
        .apply_request_correction(
            &owner,
            org,
            &input(source, &preview.source_hash, "integrations", 40, 0),
        )
        .await
        .unwrap();
    let row = wallet
        .transactions_page(None, 100)
        .await
        .unwrap()
        .rows
        .into_iter()
        .find(|r| r.id == credit.entry_id.to_string())
        .unwrap();
    assert_eq!(row.kind, oxsum_core::TransactionKind::Adjustment);
    assert_eq!(row.amount_minor, 40);
    for include_unattributed in [false, true] {
        let scoped = oxsum_core::CustomerRecordScope {
            key_ids: Some(vec!["sibling".into()]),
            include_unattributed,
        };
        assert!(
            wallet
                .scoped_transactions_page(None, 100, &scoped)
                .await
                .unwrap()
                .rows
                .iter()
                .all(|r| r.id != credit.entry_id.to_string())
        );
        assert!(
            wallet
                .scoped_receipt_proof(
                    doubleentry::EntryId::from_uuid(credit.entry_id),
                    None,
                    &scoped
                )
                .await
                .unwrap()
                .is_none()
        );
    }
    let scope = oxsum_core::CustomerRecordScope {
        key_ids: None,
        include_unattributed: true,
    };
    let receipt = wallet
        .charge_receipt(&db, org, &source.to_string(), &scope)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.charged_minor, Some(100));
    assert_eq!(receipt.corrected_minor, 40);
    assert_eq!(receipt.net_charged_minor, Some(60));
    assert_eq!(receipt.corrections[0].entry_id, credit.entry_id);
    assert!(
        receipt
            .ledger
            .iter()
            .any(|r| r.entry_id == credit.entry_id.to_string() && r.settled_effect_minor == 40)
    );
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.finance_audit WHERE action='request.correct' AND target=$1",
    )
    .bind(source.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(audit, 1);
    let facts = db
        .financial_facts(&oxsum_core::FactFilter {
            organization_id: Some(org),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(facts.request_credits_minor, 40);
    assert_eq!(facts.refund_minor, 0);
    assert_eq!(facts.billed_minor, 0);
    let yesterday = OffsetDateTime::now_utc().date() - time::Duration::days(1);
    let bundle = db
        .mint_period_evidence(
            &wallet,
            org,
            wallet.tenant_id(),
            yesterday,
            yesterday,
            false,
            None,
        )
        .await
        .unwrap();
    let body = db
        .evidence_bundle_json(bundle.bundle_id)
        .await
        .unwrap()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["artifacts"].get("requestCorrection").is_none());
    let yesterday_facts = db
        .financial_facts(&oxsum_core::FactFilter {
            organization_id: Some(org),
            from: Some(yesterday),
            to: Some(yesterday),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(yesterday_facts.request_credits_minor, 0);
}

#[tokio::test]
async fn manual_key_and_corrupt_sources_refuse() {
    let (db, owner, org, wallet, source) = world(false).await;
    let day = OffsetDateTime::now_utc().date();
    let request = Uuid::new_v4();
    wallet
        .hold("manual-hold", "manual", 100, day)
        .await
        .unwrap();
    let description=json!({"v":3,"request":request.to_string(),"channel":"fixture","model":"model","priceVersion":1,"kind":"usage","usage":{"inputTokens":100},"lines":[["input",100,1000000],["output",0,1000000]],"charged":100,"freeze":100}).to_string();
    let manual = wallet
        .settle("manual-hold", &description, 100, day)
        .await
        .unwrap();
    assert!(matches!(
        db.preview_request_correction(&owner, org, *manual.entry_id.as_uuid())
            .await,
        Err(WalletError::Conflict(_))
    ));
    sqlx::query(&format!(
        "UPDATE ledger_{}.entries SET content_hash=decode(repeat('00',32),'hex') WHERE entry_id=$1",
        wallet.tenant_id()
    ))
    .bind(source)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        db.preview_request_correction(&owner, org, source)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn finance_audit_failure_rolls_back_credit() {
    let (db, owner, org, wallet, source) = world(false).await;
    let p = db
        .preview_request_correction(&owner, org, source)
        .await
        .unwrap();
    // Only this source fails, so concurrent tests on this suite's database remain independent.
    sqlx::query(&format!("CREATE FUNCTION oxsum.reject_credit_finance() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.target='{source}' THEN RAISE EXCEPTION 'injected finance failure'; END IF; RETURN NEW; END $$")).execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TRIGGER credit_finance_fail BEFORE INSERT ON oxsum.finance_audit FOR EACH ROW EXECUTE FUNCTION oxsum.reject_credit_finance()").execute(db.pool()).await.unwrap();
    let request = input(source, &p.source_hash, "finance-rollback", 50, 0);
    assert!(
        db.apply_request_correction(&owner, org, &request)
            .await
            .is_err()
    );
    assert_eq!(wallet.available().await.unwrap(), 900);
    sqlx::query("DROP TRIGGER credit_finance_fail ON oxsum.finance_audit")
        .execute(db.pool())
        .await
        .unwrap();
    db.apply_request_correction(&owner, org, &request)
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 950);
}

#[tokio::test]
async fn sealed_posting_day_refuses_without_rewriting_original() {
    let (db, owner, org, wallet, source) = world(false).await;
    let p = db
        .preview_request_correction(&owner, org, source)
        .await
        .unwrap();
    sqlx::query("INSERT INTO oxsum.usage_anchors (organization_id,day,entry_id,map_root,enduser_count) VALUES ($1,$2,$3,$4,0)")
        .bind(org).bind(OffsetDateTime::now_utc().date()).bind(source).bind(vec![0_u8;32]).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.apply_request_correction(&owner, org, &input(source, &p.source_hash, "sealed", 20, 0))
            .await,
        Err(WalletError::DaySealed)
    ));
    assert_eq!(wallet.available().await.unwrap(), 900);
}

//! Exact historical chargeback compensation preserves cash, source and debt history.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;

use std::sync::Arc;

use oxsum_core::{
    BundlePublishRequest, BundleTerms, Db, DisputeResolution, MerchantEventKind,
    MerchantSigningContext, NewPaymentOrder, NewUser, NonplanChargebackRecoveryRequest,
    OrderDispute, PaymentOrder, PaymentOrderPurpose, VerifiedMerchantEvent, Wallet, WalletError,
};
use serde_json::json;
use time::{Date, OffsetDateTime};
use tokio::time::{Duration, timeout};
use uuid::Uuid;

const TOTAL: i64 = 2_500_000;

struct World {
    db: Db,
    owner: String,
    org: Uuid,
    wallet: Arc<Wallet>,
    context: MerchantSigningContext,
}

fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

async fn world() -> Option<World> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url("legacy_chargebacks")?;
    let db = Db::connect(&url, 20).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: format!("{}@example.test", Uuid::new_v4()),
            password: "non-plan source test password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = customer.organization.id;
    let wallet = Arc::new(
        Wallet::open(db.pool().clone(), &customer.organization.tenant_id)
            .await
            .unwrap(),
    );
    let context = payment::merchant_context(&db, &owner).await;
    Some(World {
        db,
        owner,
        org,
        wallet,
        context,
    })
}

async fn paid(w: &World, purpose: PaymentOrderPurpose, fulfill: bool) -> PaymentOrder {
    let purpose_ref = match purpose {
        PaymentOrderPurpose::Bundle => Some(
            w.db.publish_bundle(
                &w.owner,
                &BundlePublishRequest {
                    idempotency_key: Uuid::new_v4().to_string(),
                    reason: "cash loss fixture".into(),
                    bundle_id: None,
                    expected_version: 0,
                    terms: BundleTerms {
                        name: Uuid::new_v4().to_string(),
                        display_currency: "USD".into(),
                        display_amount_minor: 250,
                        purchased_minor: TOTAL,
                        promo_minor: 1_000_000,
                        promo_lifetime_days: Some(30),
                    },
                },
            )
            .await
            .unwrap()
            .offer
            .bundle_version_id,
        ),
        PaymentOrderPurpose::InvoiceRepayment => Some(Uuid::new_v4()),
        _ => None,
    };
    let order =
        w.db.create_payment_order(
            &w.context,
            w.org,
            &NewPaymentOrder {
                purpose,
                purpose_ref,
                display_amount_minor: 250,
                display_currency: "USD".into(),
                idempotency_key: Uuid::new_v4().to_string(),
                promo_minor: 0,
                expected_plan_quote: None,
                expected_collection_revision: None,
            },
        )
        .await
        .unwrap();
    payment::mark_paid(&w.db, &w.context, &order).await;
    if fulfill {
        w.db.fulfill_payment_order(order.order_id, &w.wallet, today())
            .await
            .unwrap();
    }
    order
}

fn event(w: &World, order: &PaymentOrder, fiat: Option<i64>) -> VerifiedMerchantEvent {
    VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: MerchantEventKind::Dispute,
        payment_id: Some(order.order_id),
        provider_payment_id: Some(format!("pay_{}", order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(oxsum_core::MerchantMode::Test),
        amount_minor: fiat,
        currency: Some("USD".into()),
        refund_id: Some(Uuid::new_v4().to_string()),
    }
}

async fn open(w: &World, order: &PaymentOrder, fiat: i64) -> OrderDispute {
    let event = event(w, order, Some(fiat));
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[84; 32])
            .await
            .unwrap();
    assert_eq!(receipt.state, "accepted", "{:?}", receipt.reason);
    w.db.apply_refund_event(&w.wallet, &receipt, today())
        .await
        .unwrap();
    w.db.business_disputes(&w.owner, None, 100)
        .await
        .unwrap()
        .disputes
        .into_iter()
        .find(|d| Some(d.provider_dispute_id.as_str()) == event.refund_id.as_deref())
        .unwrap()
}

async fn lose(w: &World, dispute: Uuid) -> OrderDispute {
    w.db.resolve_order_dispute(
        &w.wallet,
        &w.owner,
        dispute,
        DisputeResolution::Lost,
        Some("proven source loss"),
        &format!("loss-{dispute}"),
        today(),
    )
    .await
    .unwrap()
}

async fn detail(w: &World, dispute: Uuid) -> serde_json::Value {
    sqlx::query_scalar(
        "SELECT a.detail FROM oxsum.finance_audit a JOIN oxsum.disputes d ON d.audit_ref=a.audit_id WHERE a.action='dispute.resolve' AND a.target=$1",
    )
    .bind(dispute.to_string())
    .fetch_one(w.db.pool())
    .await
    .unwrap()
}

async fn legs(w: &World, id: Uuid) -> serde_json::Value {
    sqlx::query_scalar(&format!("SELECT jsonb_object_agg(path,minor) FROM (SELECT a.path,SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END)::bigint minor FROM ledger_{}.postings p JOIN ledger_{}.accounts a USING(account_index) WHERE entry_id=$1 AND layer='settled' GROUP BY a.path) q",w.wallet.tenant_id(),w.wallet.tenant_id()))
        .bind(id).fetch_one(w.db.pool()).await.unwrap()
}

struct Legacy {
    order: PaymentOrder,
    dispute: OrderDispute,
    request: NonplanChargebackRecoveryRequest,
    old_entry: Uuid,
    old_hash: String,
    old_audit: serde_json::Value,
}

async fn net(w: &World, path: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a USING(account_index) WHERE a.path=$1 AND p.layer='settled'",w.wallet.tenant_id(),w.wallet.tenant_id()))
        .bind(path).fetch_one(w.db.pool()).await.unwrap()
}
async fn hash(w: &World, id: Uuid) -> String {
    sqlx::query_scalar(&format!(
        "SELECT encode(content_hash,'hex') FROM ledger_{}.entries WHERE entry_id=$1",
        w.wallet.tenant_id()
    ))
    .bind(id)
    .fetch_one(w.db.pool())
    .await
    .unwrap()
}
async fn audit_count(w: &World, id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM oxsum.finance_audit WHERE target=$1")
        .bind(id.to_string())
        .fetch_one(w.db.pool())
        .await
        .unwrap()
}
async fn legacy(w: &World, wrong: i64, fiat: i64, terminal: &str) -> Legacy {
    let order = paid(w, PaymentOrderPurpose::WalletTopup, true).await;
    legacy_order(w, order, wrong, fiat, terminal).await
}

async fn legacy_order(
    w: &World,
    order: PaymentOrder,
    wrong: i64,
    fiat: i64,
    terminal: &str,
) -> Legacy {
    legacy_order_with_evidence(w, order, wrong, fiat, terminal, false).await
}

async fn legacy_order_with_evidence(
    w: &World,
    order: PaymentOrder,
    wrong: i64,
    fiat: i64,
    terminal: &str,
    earlier_evidence: bool,
) -> Legacy {
    let original_wallet = net(w, "Liabilities:Wallet").await
        + net(w, "Equity:Promo").await
        + net(w, "Equity:Bonus").await;
    // Reserve the original Wallet while drawing a separate credit hold. The
    // legitimate old wrong writer repays that draw, then the original hold is
    // released. No ledger trigger is disabled or money leg hand-edited.
    w.wallet
        .set_credit_limit("old-credit-limit", TOTAL, today())
        .await
        .unwrap();
    if original_wallet > 0 {
        w.wallet
            .hold("original-pending", "", original_wallet, today())
            .await
            .unwrap();
    }
    w.wallet
        .hold("old-credit-draw", "", wrong, today())
        .await
        .unwrap();
    w.wallet
        .settle("old-credit-draw", "", wrong, today())
        .await
        .unwrap();
    let dispute = open(w, &order, fiat).await;
    if earlier_evidence {
        w.db.resolve_order_dispute(
            &w.wallet,
            &w.owner,
            dispute.dispute_id,
            DisputeResolution::EvidenceDue,
            Some("historical evidence requested"),
            &format!("legacy-evidence/{}", dispute.dispute_id),
            today(),
        )
        .await
        .unwrap();
    }
    let key = format!("dispute-recharge/{}", dispute.dispute_id);
    w.wallet.repay_credit(&key, wrong, today()).await.unwrap();
    if original_wallet > 0 {
        w.wallet
            .settle("original-pending", "", 0, today())
            .await
            .unwrap();
    }
    // Close the fully repaid fixture facility so available() measures the
    // retained purchased/promotional pools rather than unused credit headroom.
    w.wallet
        .set_credit_limit("close-fixture-credit-limit", 0, today())
        .await
        .unwrap();
    let old_entry = *oxsum_core::entry_id_for(&key).as_uuid();
    let receipt: Uuid =
        sqlx::query_scalar("SELECT inbox_receipt_id FROM oxsum.disputes WHERE dispute_id=$1")
            .bind(dispute.dispute_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let actor: Uuid =
        sqlx::query_scalar("SELECT user_id FROM oxsum.business_staff WHERE role='owner'")
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let business: Uuid =
        sqlx::query_scalar("SELECT business_id FROM oxsum.businesses WHERE singleton")
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let old_audit = json!({"orderId":order.order_id,"resolution":terminal,"contestedMinor":dispute.amount_minor,"rechargedMinor":wrong,"payableMinor":0,"unallocatedMinor":0,"requiresReview":false,"currency":"USD","rechargeEntryId":old_entry,"reason":"retained historical loss"});
    let audit = Uuid::new_v4();
    if terminal != "opened" {
        sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) VALUES($1,$2,$3,'dispute.resolve',$4,$5,$6)")
            .bind(audit).bind(business).bind(actor).bind(dispute.dispute_id.to_string()).bind(&old_audit).bind(format!("legacy-loss/{}",dispute.dispute_id)).execute(w.db.pool()).await.unwrap();
        sqlx::query("UPDATE oxsum.disputes SET state=$2,recharge_entry_id=$3,resolved_by=$4,audit_ref=$5 WHERE dispute_id=$1")
            .bind(dispute.dispute_id).bind(terminal).bind(old_entry).bind(actor).bind(audit).execute(w.db.pool()).await.unwrap();
    }
    Legacy {
        order,
        request: NonplanChargebackRecoveryRequest {
            dispute_id: dispute.dispute_id,
            event_receipt_id: receipt,
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "reviewed exact legacy case".into(),
            expected_revision: None,
        },
        dispute,
        old_entry,
        old_hash: hash(w, old_entry).await,
        old_audit,
    }
}

#[tokio::test]
async fn earlier_exact_resolution_replays_current_corrected_standing() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let mut old = legacy_order_with_evidence(&w, order, 500_000, 250, "lost", true).await;
    old.request.expected_revision = Some(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap()
            .source_revision,
    );
    let correction =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap()
            .effect
            .unwrap();
    let before = w.wallet.log_size().await.unwrap();
    let audits = audit_count(&w, old.dispute.dispute_id).await;
    for (resolution, reason, key) in [
        (
            DisputeResolution::EvidenceDue,
            "historical evidence requested",
            format!("legacy-evidence/{}", old.dispute.dispute_id),
        ),
        (
            DisputeResolution::Lost,
            "retained historical loss",
            format!("legacy-loss/{}", old.dispute.dispute_id),
        ),
        (
            DisputeResolution::Lost,
            "read current terminal outcome",
            Uuid::new_v4().to_string(),
        ),
    ] {
        let replay =
            w.db.resolve_order_dispute(
                &w.wallet,
                &w.owner,
                old.dispute.dispute_id,
                resolution,
                Some(reason),
                &key,
                today(),
            )
            .await
            .unwrap();
        assert_eq!(replay.state, "lost");
        assert_eq!(replay.recharge_entry_id, Some(old.old_entry));
        assert_eq!(
            replay.cash_correction.unwrap().correction_entry_id,
            correction.correction_entry_id
        );
    }
    for (resolution, reason, key) in [
        (
            DisputeResolution::EvidenceDue,
            "changed request",
            format!("legacy-evidence/{}", old.dispute.dispute_id),
        ),
        (
            DisputeResolution::Won,
            "historical evidence requested",
            format!("legacy-evidence/{}", old.dispute.dispute_id),
        ),
        (
            DisputeResolution::EvidenceDue,
            "new backward transition",
            Uuid::new_v4().to_string(),
        ),
        (
            DisputeResolution::Won,
            "new terminal transition",
            Uuid::new_v4().to_string(),
        ),
    ] {
        assert!(matches!(
            w.db.resolve_order_dispute(
                &w.wallet,
                &w.owner,
                old.dispute.dispute_id,
                resolution,
                Some(reason),
                &key,
                today()
            )
            .await,
            Err(WalletError::Conflict(_))
        ));
    }
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    assert_eq!(audit_count(&w, old.dispute.dispute_id).await, audits);
    assert_eq!(detail(&w, old.dispute.dispute_id).await, old.old_audit);
}

#[tokio::test]
async fn complete_and_clamped_legacy_cash_is_compensated_once_without_reopening_debt() {
    for wrong in [TOTAL, 500_000] {
        let Some(w) = world().await else { return };
        let mut old = legacy(&w, wrong, 250, "lost").await;
        let before = w.wallet.log_size().await.unwrap();
        let audit = audit_count(&w, old.dispute.dispute_id).await;
        let cash = net(&w, "Assets:Cash").await;
        let credit = net(&w, "Liabilities:CreditLine").await;
        let preview =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                .await
                .unwrap();
        assert_eq!(preview.status, "proposed", "{:?}", preview.refusal);
        assert_eq!(preview.cash_to_book_minor, wrong + TOTAL);
        assert_eq!(preview.wallet_minor, TOTAL);
        assert_eq!(preview.suspense_minor, wrong);
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
        assert_eq!(audit_count(&w, old.dispute.dispute_id).await, audit);
        old.request.expected_revision = Some(preview.source_revision);
        let applied =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
                .await
                .unwrap();
        assert_eq!(applied.status, "applied");
        let effect = applied.effect.unwrap();
        assert_eq!(net(&w, "Assets:Cash").await, cash + wrong + TOTAL);
        assert_eq!(net(&w, "Liabilities:CreditLine").await, credit);
        assert_eq!(effect.preserved_credit_benefit_minor, wrong);
        assert!(effect.requires_review);
        assert_eq!(w.wallet.available().await.unwrap(), 0);
        assert_eq!(hash(&w, old.old_entry).await, old.old_hash);
        assert_eq!(detail(&w, old.dispute.dispute_id).await, old.old_audit);
        assert_eq!(
            legs(&w, effect.correction_entry_id).await,
            json!({"Assets:Cash":wrong+TOTAL,"Liabilities:Wallet":-TOTAL,"Equity:Adjustments":-wrong})
        );
        let after = w.wallet.log_size().await.unwrap();
        let replay =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
                .await
                .unwrap();
        assert!(replay.already_applied);
        assert_eq!(replay.cash_to_book_minor, 0);
        assert_eq!(w.wallet.log_size().await.unwrap(), after);
        let standing =
            w.db.business_disputes(&w.owner, None, 100)
                .await
                .unwrap()
                .disputes
                .into_iter()
                .find(|d| d.dispute_id == old.dispute.dispute_id)
                .unwrap();
        assert_eq!(standing.recharge_entry_id, Some(old.old_entry));
        assert!(standing.cash_correction.is_some());
        let counts: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.plan_statement_lines WHERE source_ref=$1",
        )
        .bind(old.dispute.dispute_id)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        assert_eq!(counts, 0);
    }
}

#[tokio::test]
async fn spent_held_replacement_and_later_returns_cannot_replenish_original_source() {
    let Some(w) = world().await else { return };
    let mut old = legacy(&w, 500_000, 250, "lost").await;
    w.wallet
        .hold("later-spend", "", 1_000_000, today())
        .await
        .unwrap();
    w.wallet
        .settle("later-spend", "", 1_000_000, today())
        .await
        .unwrap();
    w.wallet
        .hold("later-held", "", 500_000, today())
        .await
        .unwrap();
    w.wallet
        .top_up("unrelated-replacement", 4_000_000, today())
        .await
        .unwrap();
    let preview =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap();
    assert_eq!(preview.wallet_minor, 1_000_000);
    old.request.expected_revision = Some(preview.source_revision);
    let effect =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap()
            .effect
            .unwrap();
    assert_eq!(effect.suspense_minor, 2_000_000);
    let second = open(&w, &old.order, 250).await;
    let second = lose(&w, second.dispute_id).await;
    assert_eq!(
        legs(&w, second.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Equity:Adjustments":-TOTAL})
    );
    assert_eq!(
        detail(&w, second.dispute_id).await["source"]["priorChargebackCashMinor"],
        TOTAL
    );
    assert_eq!(w.wallet.available().await.unwrap(), 4_000_000);
}

#[tokio::test]
async fn exact_revision_preview_and_concurrent_apply_preserve_one_compensation() {
    let Some(w) = world().await else { return };
    let mut old = legacy(&w, 500_000, 250, "lost").await;
    let preview =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap();
    old.request.expected_revision = Some(preview.source_revision);
    w.wallet
        .hold("revision-drift", "", 100_000, today())
        .await
        .unwrap();
    let before = w.wallet.log_size().await.unwrap();
    assert!(matches!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    let fresh =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap();
    old.request.expected_revision = Some(fresh.source_revision);
    let (a, b) = tokio::join!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today()),
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.already_applied, b.already_applied);
    assert_eq!(w.wallet.log_size().await.unwrap(), before + 1);
    assert_eq!(
        a.effect.unwrap().correction_entry_id,
        b.effect.unwrap().correction_entry_id
    );
    old.request.reason = "changed terms".into();
    assert!(matches!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
}

#[tokio::test]
async fn unsupported_terminal_and_orphan_history_refuses_without_preview_writes() {
    for terminal in ["opened", "evidence_due", "won", "closed"] {
        let Some(w) = world().await else { return };
        let mut old = legacy(&w, 500_000, 250, terminal).await;
        let before = w.wallet.log_size().await.unwrap();
        let audit = audit_count(&w, old.dispute.dispute_id).await;
        let preview =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                .await
                .unwrap();
        assert_eq!(preview.status, "refused");
        assert_eq!(audit_count(&w, old.dispute.dispute_id).await, audit);
        old.request.expected_revision = Some(preview.source_revision);
        let refused =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
                .await
                .unwrap();
        assert_eq!(refused.status, "refused");
        assert_eq!(audit_count(&w, old.dispute.dispute_id).await, audit + 1);
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
        assert_eq!(hash(&w, old.old_entry).await, old.old_hash);
    }
}

#[tokio::test]
async fn wrong_wallet_receipt_and_customer_authority_cannot_compensate() {
    let Some(w) = world().await else { return };
    let old = legacy(&w, 500_000, 250, "lost").await;
    let other =
        w.db.register(NewUser {
            email: format!("{}@example.test", Uuid::new_v4()),
            password: "another customer password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let foreign = Wallet::open(w.db.pool().clone(), &other.organization.tenant_id)
        .await
        .unwrap();
    let before = w.wallet.log_size().await.unwrap();
    assert!(matches!(
        w.db.recover_nonplan_chargeback(&w.owner, &foreign, &old.request, false, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    let customer =
        w.db.login(&other.user.email, "another customer password")
            .await
            .unwrap()
            .into_session();
    assert!(
        w.db.recover_nonplan_chargeback(&customer.token, &w.wallet, &old.request, false, today())
            .await
            .is_err()
    );
    let mut request = old.request;
    request.event_receipt_id = Uuid::new_v4();
    assert_eq!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap()
            .status,
        "refused"
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
}

#[tokio::test]
async fn one_connection_recovery_and_replay_share_all_source_reads() {
    let Some(w) = world().await else { return };
    let mut old = legacy(&w, 500_000, 250, "lost").await;
    let db = Db::connect(
        &oxsum_testkit::suite_db_url("legacy_chargebacks").unwrap(),
        1,
    )
    .await
    .unwrap();
    let wallet = Wallet::open_existing(db.pool().clone(), w.wallet.tenant_id())
        .await
        .unwrap();
    let preview = timeout(
        Duration::from_secs(10),
        db.recover_nonplan_chargeback(&w.owner, &wallet, &old.request, false, today()),
    )
    .await
    .unwrap()
    .unwrap();
    old.request.expected_revision = Some(preview.source_revision);
    let applied = timeout(
        Duration::from_secs(10),
        db.recover_nonplan_chargeback(&w.owner, &wallet, &old.request, true, today()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(applied.status, "applied");
    let replay = timeout(
        Duration::from_secs(10),
        db.recover_nonplan_chargeback(&w.owner, &wallet, &old.request, true, today()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(replay.already_applied);
}

#[tokio::test]
async fn recovery_audit_failure_rolls_back_cash_effect_and_original_history() {
    let Some(w) = world().await else { return };
    let mut old = legacy(&w, 500_000, 250, "lost").await;
    old.request.expected_revision = Some(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap()
            .source_revision,
    );
    let name = format!("fail_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE FUNCTION oxsum.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='nonplan_chargeback.recover' AND NEW.target='{}' THEN RAISE EXCEPTION 'controlled chargeback audit failure'; END IF; RETURN NEW; END $$",old.dispute.dispute_id)).execute(w.db.pool()).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {name} BEFORE INSERT ON oxsum.finance_audit FOR EACH ROW EXECUTE FUNCTION oxsum.{name}()")).execute(w.db.pool()).await.unwrap();
    let before = w.wallet.log_size().await.unwrap();
    let cash = net(&w, "Assets:Cash").await;
    assert!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .is_err()
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    assert_eq!(net(&w, "Assets:Cash").await, cash);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.nonplan_chargeback_corrections WHERE dispute_id=$1",
    )
    .bind(old.dispute.dispute_id)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    assert_eq!(detail(&w, old.dispute.dispute_id).await, old.old_audit);
    assert_eq!(hash(&w, old.old_entry).await, old.old_hash);
    sqlx::query(&format!("DROP TRIGGER {name} ON oxsum.finance_audit"))
        .execute(w.db.pool())
        .await
        .unwrap();
    assert_eq!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap()
            .status,
        "applied"
    );
}

#[tokio::test]
async fn altered_original_and_funding_bytes_refuse_without_new_cash() {
    for source in ["original", "funding", "correction"] {
        let Some(w) = world().await else { return };
        let mut old = legacy(&w, 500_000, 250, "lost").await;
        let id = match source {
            "funding" => {
                *oxsum_core::entry_id_for(&format!("payment-order:{}", old.order.order_id))
                    .as_uuid()
            }
            "correction" => {
                old.request.expected_revision = Some(
                    w.db.recover_nonplan_chargeback(
                        &w.owner,
                        &w.wallet,
                        &old.request,
                        false,
                        today(),
                    )
                    .await
                    .unwrap()
                    .source_revision,
                );
                w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
                    .await
                    .unwrap()
                    .effect
                    .unwrap()
                    .correction_entry_id
            }
            _ => old.old_entry,
        };
        sqlx::query(&format!("UPDATE ledger_{}.entries SET description='controlled historical corruption' WHERE entry_id=$1",w.wallet.tenant_id())).bind(id).execute(w.db.pool()).await.unwrap();
        let before = w.wallet.log_size().await.unwrap();
        let result =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                .await;
        if source == "correction" {
            assert!(matches!(result, Err(WalletError::Conflict(_))));
        } else {
            assert_eq!(result.unwrap().status, "refused");
        }
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
    }
}

#[tokio::test]
async fn original_audit_must_independently_match_amount_loss_currency_and_pointer() {
    for field in [
        "missing",
        "rechargedMinor",
        "contestedMinor",
        "currency",
        "rechargeEntryId",
    ] {
        let Some(w) = world().await else { return };
        let old = legacy(&w, 500_000, 250, "lost").await;
        if field == "missing" {
            sqlx::query("UPDATE oxsum.disputes SET audit_ref=NULL WHERE dispute_id=$1")
                .bind(old.dispute.dispute_id)
                .execute(w.db.pool())
                .await
                .unwrap();
        } else {
            let mut detail = old.old_audit.clone();
            detail[field] = match field {
                "rechargedMinor" => json!(500_001),
                "contestedMinor" => json!(TOTAL - 1),
                "currency" => json!("EUR"),
                _ => json!(Uuid::new_v4()),
            };
            let replacement = Uuid::new_v4();
            sqlx::query("INSERT INTO oxsum.finance_audit(audit_id,business_id,actor_user_id,action,target,detail,idempotency_key) SELECT $1,a.business_id,a.actor_user_id,a.action,a.target,$2,$3 FROM oxsum.finance_audit a JOIN oxsum.disputes d ON d.audit_ref=a.audit_id WHERE d.dispute_id=$4")
                .bind(replacement).bind(detail).bind(Uuid::new_v4().to_string()).bind(old.dispute.dispute_id).execute(w.db.pool()).await.unwrap();
            sqlx::query("UPDATE oxsum.disputes SET audit_ref=$2 WHERE dispute_id=$1")
                .bind(old.dispute.dispute_id)
                .bind(replacement)
                .execute(w.db.pool())
                .await
                .unwrap();
        }
        let before = w.wallet.log_size().await.unwrap();
        let preview =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                .await
                .unwrap();
        assert_eq!(preview.status, "refused", "{field}");
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
        assert_eq!(hash(&w, old.old_entry).await, old.old_hash);
    }
}

#[tokio::test]
async fn accepted_receipt_still_requires_exact_case_payment_amount_and_merchant_binding() {
    for field in [
        "refundId",
        "providerPaymentId",
        "amountMinor",
        "currency",
        "organizationId",
        "providerAccountId",
        "mode",
    ] {
        let Some(w) = world().await else { return };
        let mut old = legacy(&w, 500_000, 250, "lost").await;
        let mut normalized: serde_json::Value = sqlx::query_scalar(
            "SELECT normalized FROM oxsum.merchant_event_inbox WHERE receipt_id=$1",
        )
        .bind(old.request.event_receipt_id)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        normalized[field] = match field {
            "amountMinor" => json!(100),
            "currency" => json!("EUR"),
            "mode" => json!("live"),
            _ => json!(Uuid::new_v4().to_string()),
        };
        let event_id = Uuid::new_v4().to_string();
        normalized["eventId"] = json!(event_id);
        let receipt = Uuid::new_v4();
        // Retain a deliberately inconsistent accepted historical row, without
        // bypassing any immutable guards; current intake would reject it.
        sqlx::query("INSERT INTO oxsum.merchant_event_inbox(receipt_id,merchant_id,version_id,event_id,payload_digest,normalized,state) SELECT $1,merchant_id,version_id,$2,payload_digest,$3,'accepted' FROM oxsum.merchant_event_inbox WHERE receipt_id=$4")
            .bind(receipt).bind(event_id).bind(normalized).bind(old.request.event_receipt_id).execute(w.db.pool()).await.unwrap();
        sqlx::query("UPDATE oxsum.disputes SET inbox_receipt_id=$2 WHERE dispute_id=$1")
            .bind(old.dispute.dispute_id)
            .bind(receipt)
            .execute(w.db.pool())
            .await
            .unwrap();
        old.request.event_receipt_id = receipt;
        let before = w.wallet.log_size().await.unwrap();
        let preview =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                .await
                .unwrap();
        assert_eq!(preview.status, "refused", "{field}");
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
    }
}

#[tokio::test]
async fn historical_bundle_promo_and_existing_statement_bytes_are_retained() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::Bundle, true).await;
    let mut old = legacy_order(&w, order, 500_000, 250, "lost").await;
    let statement = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.plan_statements(statement_id,organization_id,business_id,state,total_minor,paid_minor,currency,issued_at,due_at,past_due_at) SELECT $1,$2,business_id,'past_due',700000,200000,'USD',clock_timestamp()-interval '30 days',clock_timestamp()-interval '16 days',clock_timestamp()-interval '15 days' FROM oxsum.businesses WHERE singleton")
        .bind(statement).bind(w.org).execute(w.db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'usage',700000,'Retained earlier usage',NULL)").bind(Uuid::new_v4()).bind(statement).execute(w.db.pool()).await.unwrap();
    let old_statement: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM oxsum.plan_statements s WHERE statement_id=$1")
            .bind(statement)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let old_lines:serde_json::Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(l) ORDER BY line_id) FROM oxsum.plan_statement_lines l WHERE statement_id=$1").bind(statement).fetch_one(w.db.pool()).await.unwrap();
    let old_promo = serde_json::to_value(w.db.promo_lots(w.org, true).await.unwrap()).unwrap();
    let preview =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap();
    assert_eq!(preview.wallet_minor, TOTAL);
    old.request.expected_revision = Some(preview.source_revision);
    let effect =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap()
            .effect
            .unwrap();
    assert!(
        effect
            .review_reasons
            .iter()
            .any(|r| r == "bundlePromoRetained")
    );
    let current_statement: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM oxsum.plan_statements s WHERE statement_id=$1")
            .bind(statement)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let current_lines:serde_json::Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(l) ORDER BY line_id) FROM oxsum.plan_statement_lines l WHERE statement_id=$1").bind(statement).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(current_statement, old_statement);
    assert_eq!(current_lines, old_lines);
    assert_eq!(
        serde_json::to_value(w.db.promo_lots(w.org, true).await.unwrap()).unwrap(),
        old_promo
    );
    assert_eq!(net(&w, "Liabilities:Wallet").await, 0);
    assert_eq!(net(&w, "Equity:Promo").await, 1_000_000);
    assert!(sqlx::query("UPDATE oxsum.nonplan_chargeback_corrections SET source=jsonb_set(source,'{originalFunding,priorChargebackCashMinor}','999') WHERE dispute_id=$1").bind(old.dispute.dispute_id).execute(w.db.pool()).await.is_err());
    assert!(
        sqlx::query("DELETE FROM oxsum.nonplan_chargeback_corrections WHERE dispute_id=$1")
            .bind(old.dispute.dispute_id)
            .execute(w.db.pool())
            .await
            .is_err()
    );
    let log = w.wallet.log_size().await.unwrap();
    w.wallet
        .top_up("later-unrelated-funding", TOTAL, today())
        .await
        .unwrap();
    let replay =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap();
    assert!(replay.already_applied);
    assert_eq!(
        replay.effect.unwrap().source_revision,
        effect.source_revision
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), log + 1);
}

#[tokio::test]
async fn finance_must_have_current_business_context_and_retained_actor_for_replay() {
    use oxsum_core::{BusinessRole, BusinessStaffRequest, SessionContext, SessionContextRequest};
    let Some(w) = world().await else { return };
    let mut old = legacy(&w, 500_000, 250, "lost").await;
    let staff =
        w.db.register(NewUser {
            email: format!("{}@example.test", Uuid::new_v4()),
            password: "historical finance password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let token =
        w.db.login(&staff.user.email, "historical finance password")
            .await
            .unwrap()
            .into_session()
            .token;
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            user_id: staff.user.id,
            role: BusinessRole::Finance,
            reason: "inspect historical source".into(),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    assert!(
        w.db.recover_nonplan_chargeback(&token, &w.wallet, &old.request, false, today())
            .await
            .is_err()
    );
    w.db.switch_session_context(
        &token,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    let preview =
        w.db.recover_nonplan_chargeback(&token, &w.wallet, &old.request, false, today())
            .await
            .unwrap();
    old.request.expected_revision = Some(preview.source_revision);
    assert_eq!(
        w.db.recover_nonplan_chargeback(&token, &w.wallet, &old.request, true, today())
            .await
            .unwrap()
            .status,
        "applied"
    );
    assert!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .is_err()
    );
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            user_id: staff.user.id,
            role: BusinessRole::Viewer,
            reason: "remove financial authority".into(),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    assert!(
        w.db.recover_nonplan_chargeback(&token, &w.wallet, &old.request, true, today())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn partial_case_preserves_exact_integral_loss_and_source() {
    let Some(w) = world().await else { return };
    let mut old = legacy(&w, 500_000, 100, "lost").await;
    let preview =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap();
    assert_eq!(preview.actual_loss_minor, 1_000_000);
    assert_eq!(preview.cash_to_book_minor, 1_500_000);
    old.request.expected_revision = Some(preview.source_revision);
    let result =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap();
    assert_eq!(result.effect.unwrap().wallet_minor, 1_000_000);
    assert_eq!(w.wallet.available().await.unwrap(), 1_500_000);
}

#[tokio::test]
async fn repayment_origin_cash_stays_suspense_and_original_credit_benefit_stays_retained() {
    let Some(w) = world().await else { return };
    w.wallet
        .set_credit_limit("prior-credit-limit", TOTAL * 2, today())
        .await
        .unwrap();
    w.wallet
        .hold("prior-credit-spend", "", TOTAL, today())
        .await
        .unwrap();
    w.wallet
        .settle("prior-credit-spend", "", TOTAL, today())
        .await
        .unwrap();
    let order = paid(&w, PaymentOrderPurpose::InvoiceRepayment, true).await;
    let mut old = legacy_order(&w, order, 500_000, 250, "lost").await;
    let credit = net(&w, "Liabilities:CreditLine").await;
    let preview =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap();
    assert_eq!(preview.wallet_minor, 0);
    assert_eq!(preview.suspense_minor, 3_000_000);
    old.request.expected_revision = Some(preview.source_revision);
    assert_eq!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap()
            .status,
        "applied"
    );
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    assert_eq!(net(&w, "Liabilities:CreditLine").await, credit);
}

#[tokio::test]
async fn later_refund_cash_does_not_replace_historical_chargeback_source() {
    use oxsum_core::{NewRefundRequest, RefundDecision};
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: Some(500_000),
                reason: "preauthorized partial return".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    let refund =
        w.db.resolve_order_refund(
            &w.owner,
            refund.refund_id,
            RefundDecision::Approve,
            None,
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap()
        .0;
    let mut old = legacy_order(&w, order, 500_000, 250, "lost").await;
    let mut event = event(&w, &old.order, Some(refund.display_amount_minor));
    event.kind = MerchantEventKind::RefundSucceeded;
    event.refund_id = Some(refund.refund_id.to_string());
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[47; 32])
            .await
            .unwrap();
    w.db.apply_refund_event(&w.wallet, &receipt, today())
        .await
        .unwrap();
    // A currently uncorrected wrong chargeback makes the forward refund's source
    // uncertain, so Cash is retained in suspense. It still consumes the source.
    let preview =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap();
    assert_eq!(preview.wallet_minor, 2_000_000);
    old.request.expected_revision = Some(preview.source_revision);
    assert_eq!(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap()
            .status,
        "applied"
    );
    assert_eq!(w.wallet.available().await.unwrap(), 500_000);
}

#[tokio::test]
async fn corrected_partial_loss_consumes_only_actual_loss_before_late_refund_cash() {
    use oxsum_core::{NewRefundRequest, RefundDecision};
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: Some(2_000_000),
                reason: "preauthorized late partial cash".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    let refund =
        w.db.resolve_order_refund(
            &w.owner,
            refund.refund_id,
            RefundDecision::Approve,
            None,
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap()
        .0;
    let mut old = legacy_order(&w, order, 500_000, 100, "lost").await;
    old.request.expected_revision = Some(
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
            .await
            .unwrap()
            .source_revision,
    );
    let corrected =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap();
    assert_eq!(corrected.actual_loss_minor, 1_000_000);
    assert_eq!(corrected.cash_to_book_minor, 1_500_000);
    let correction = corrected.effect.unwrap();
    let mut event = event(&w, &old.order, Some(refund.display_amount_minor));
    event.kind = MerchantEventKind::RefundSucceeded;
    event.refund_id = Some(refund.refund_id.to_string());
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[65; 32])
            .await
            .unwrap();
    let cash = net(&w, "Assets:Cash").await;
    w.db.apply_refund_event(&w.wallet, &receipt, today())
        .await
        .unwrap();
    assert_eq!(net(&w, "Assets:Cash").await, cash + 2_000_000);
    let source: serde_json::Value =
        sqlx::query_scalar("SELECT source FROM oxsum.nonplan_refund_effects WHERE refund_id=$1")
            .bind(refund.refund_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(source["original"]["priorChargebackCashMinor"], 1_000_000);
    assert_eq!(source["original"]["walletMinor"], 1_500_000);
    assert_eq!(source["original"]["suspenseMinor"], 500_000);
    let prior = &source["original"]["priorReturns"][0];
    assert_eq!(prior["cashMinor"], 1_000_000);
    assert_eq!(prior["falseReceiptMinor"], 500_000);
    assert_eq!(prior["originalWrongEntryId"], json!(old.old_entry));
    assert_eq!(
        prior["correctionEntryId"],
        json!(correction.correction_entry_id)
    );
    assert_eq!(prior["sourceRevision"], correction.source_revision);
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    let log = w.wallet.log_size().await.unwrap();
    let replay =
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap();
    assert!(replay.already_applied);
    assert_eq!(replay.wallet_minor, 1_000_000);
    assert_eq!(replay.source, corrected.source);
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
}

async fn blocked_by(pool: &sqlx::PgPool, owner: i32) -> i32 {
    timeout(Duration::from_secs(10),async {
        loop {
            let pid:Option<i32>=sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) LIMIT 1")
                .bind(owner).fetch_optional(pool).await.unwrap();
            if let Some(pid)=pid { return pid; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("controlled source writer reaches its lock")
}

#[tokio::test]
async fn approved_refund_and_correction_serialize_in_both_commit_orders() {
    use doubleentry::LedgerId;
    use doubleentry::storage::postgres::PostgresStore;
    use oxsum_core::{NewRefundRequest, RefundDecision};
    for recovery_first in [false, true] {
        let Some(w) = world().await else { return };
        let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
        let refund =
            w.db.request_order_refund(
                w.org,
                order.order_id,
                &NewRefundRequest {
                    amount_minor: Some(2_000_000),
                    reason: "concurrent confirmed cash".into(),
                    request_key: Uuid::new_v4().to_string(),
                    requested_by: None,
                },
            )
            .await
            .unwrap();
        let refund =
            w.db.resolve_order_refund(
                &w.owner,
                refund.refund_id,
                RefundDecision::Approve,
                None,
                &Uuid::new_v4().to_string(),
            )
            .await
            .unwrap()
            .0;
        let mut old = legacy_order(&w, order, 500_000, 100, "lost").await;
        old.request.expected_revision = Some(
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                .await
                .unwrap()
                .source_revision,
        );
        let mut event = event(&w, &old.order, Some(refund.display_amount_minor));
        event.kind = MerchantEventKind::RefundSucceeded;
        event.refund_id = Some(refund.refund_id.to_string());
        let receipt =
            w.db.receive_merchant_event(&w.context, &event, &[71; 32])
                .await
                .unwrap();
        let before_cash = net(&w, "Assets:Cash").await;
        let store = PostgresStore::<6>::new(
            w.db.pool().clone(),
            LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
        )
        .in_schema(&format!("ledger_{}", w.wallet.tenant_id()));
        let mut pin = w.db.pool().begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(store.append_lock())
            .execute(&mut *pin)
            .await
            .unwrap();
        let pin_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *pin)
            .await
            .unwrap();
        let spawn = |recovery: bool| {
            let db = w.db.clone();
            let wallet = w.wallet.clone();
            let owner = w.owner.clone();
            let request: NonplanChargebackRecoveryRequest =
                serde_json::from_value(json!(old.request)).unwrap();
            let receipt = receipt.clone();
            tokio::spawn(async move {
                if recovery {
                    db.recover_nonplan_chargeback(&owner, &wallet, &request, true, today())
                        .await
                        .map(|_| ())
                } else {
                    db.apply_refund_event(&wallet, &receipt, today()).await
                }
            })
        };
        let first = spawn(recovery_first);
        let first_pid = blocked_by(w.db.pool(), pin_pid).await;
        let second = spawn(!recovery_first);
        blocked_by(w.db.pool(), first_pid).await;
        pin.rollback().await.unwrap();
        timeout(Duration::from_secs(10), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let second = timeout(Duration::from_secs(10), second)
            .await
            .unwrap()
            .unwrap();
        if recovery_first {
            second.unwrap();
        } else {
            assert!(matches!(second, Err(WalletError::Conflict(_))));
            old.request.expected_revision = Some(
                w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                    .await
                    .unwrap()
                    .source_revision,
            );
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
                .await
                .unwrap();
        }
        assert_eq!(net(&w, "Assets:Cash").await, before_cash + 3_500_000);
        assert_eq!(hash(&w, old.old_entry).await, old.old_hash);
        let corrections: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.nonplan_chargeback_corrections WHERE dispute_id=$1",
        )
        .bind(old.dispute.dispute_id)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        let refunds: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.nonplan_refund_effects WHERE refund_id=$1",
        )
        .bind(refund.refund_id)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        assert_eq!((corrections, refunds), (1, 1));
        let total_taken:i64=sqlx::query_scalar("SELECT (SELECT wallet_minor FROM oxsum.nonplan_chargeback_corrections WHERE dispute_id=$1)+(SELECT wallet_minor FROM oxsum.nonplan_refund_effects WHERE refund_id=$2)").bind(old.dispute.dispute_id).bind(refund.refund_id).fetch_one(w.db.pool()).await.unwrap();
        assert!(total_taken <= TOTAL);
        assert_eq!(w.wallet.available().await.unwrap(), TOTAL - total_taken);
        let log = w.wallet.log_size().await.unwrap();
        w.db.apply_refund_event(&w.wallet, &receipt, today())
            .await
            .unwrap();
        w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
            .await
            .unwrap();
        assert_eq!(w.wallet.log_size().await.unwrap(), log);
    }
}

#[tokio::test]
async fn official_reversals_of_original_or_correction_refuse_replay() {
    use doubleentry::storage::postgres::PostgresStore;
    use doubleentry::{
        AccountRegistry, EntryBatch, EntryId, IdempotencyKey, LedgerId, LedgerPolicy, LedgerStore,
        SealContext,
    };
    for correction in [false, true] {
        let Some(w) = world().await else { return };
        let mut old = legacy(&w, 500_000, 250, "lost").await;
        let target = if correction {
            old.request.expected_revision = Some(
                w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                    .await
                    .unwrap()
                    .source_revision,
            );
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, true, today())
                .await
                .unwrap()
                .effect
                .unwrap()
                .correction_entry_id
        } else {
            old.old_entry
        };
        let store = PostgresStore::<6>::new(
            w.db.pool().clone(),
            LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
        )
        .in_schema(&format!("ledger_{}", w.wallet.tenant_id()));
        let original = store
            .get(EntryId::from_uuid(target))
            .await
            .unwrap()
            .unwrap();
        let registry = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
        let key = Uuid::new_v4().to_string();
        let reverse = original
            .entry
            .reverse(
                oxsum_core::entry_id_for(&key),
                IdempotencyKey::new(key.as_bytes().to_vec()).unwrap(),
                today(),
            )
            .seal(&SealContext {
                accounts: &registry,
                calendar: &store.calendar().await.unwrap(),
                policy: &LedgerPolicy::default(),
            })
            .unwrap();
        store.append(&EntryBatch::single(reverse)).await.unwrap();
        let before = w.wallet.log_size().await.unwrap();
        let result =
            w.db.recover_nonplan_chargeback(&w.owner, &w.wallet, &old.request, false, today())
                .await;
        if correction {
            assert!(matches!(result, Err(WalletError::Conflict(_))));
        } else {
            assert_eq!(result.unwrap().status, "refused");
        }
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
    }
}

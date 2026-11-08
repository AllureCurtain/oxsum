//! Proven-source non-plan chargebacks retain every Cash minor without inventing debt.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;

use std::sync::Arc;

use doubleentry::LedgerId;
use doubleentry::storage::postgres::PostgresStore;
use oxsum_core::{
    BundlePublishRequest, BundleTerms, Db, DisputeResolution, MerchantEventKind,
    MerchantSigningContext, NewPaymentOrder, NewRefundRequest, NewUser, OrderDispute, PaymentOrder,
    PaymentOrderPurpose, RefundDecision, VerifiedMerchantEvent, Wallet, WalletError,
};
use serde_json::json;
use sqlx::PgPool;
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
    let url = oxsum_testkit::suite_db_url("nonplan_chargebacks")?;
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
        "SELECT detail FROM oxsum.finance_audit WHERE action='dispute.resolve' AND target=$1",
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

async fn totals(w: &World) -> serde_json::Value {
    sqlx::query_scalar(&format!("SELECT jsonb_object_agg(path,minor) FROM (SELECT a.path,COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint minor FROM ledger_{}.accounts a LEFT JOIN ledger_{}.postings p USING(account_index) WHERE p.layer='settled' GROUP BY a.path) q",w.wallet.tenant_id(),w.wallet.tenant_id()))
        .fetch_one(w.db.pool()).await.unwrap()
}

#[tokio::test]
async fn unspent_cash_is_reclaimed_once_and_new_refunds_are_fenced() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Liabilities:Wallet":-TOTAL})
    );
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    let audit = detail(&w, dispute.dispute_id).await;
    assert_eq!(audit["requiresReview"], false);
    assert_eq!(audit["walletTakenMinor"], TOTAL);
    assert!(audit["source"]["contentHash"].as_str().unwrap().len() == 64);
    let before = totals(&w).await;
    lose(&w, dispute.dispute_id).await;
    assert_eq!(totals(&w).await, before);
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: None,
                reason: "after chargeback".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await;
    assert!(matches!(refund, Err(WalletError::Conflict(_))));
    assert_eq!(
        w.db.order_refundable_minor(w.org, order.order_id)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn consumed_and_repayment_funding_never_takes_later_wallet_credits() {
    let Some(w) = world().await else { return };
    w.wallet
        .set_credit_limit("source-line", 5_000_000, today())
        .await
        .unwrap();
    w.wallet
        .hold("before-source", "", 1_000_000, today())
        .await
        .unwrap();
    w.wallet
        .settle("before-source", "", 1_000_000, today())
        .await
        .unwrap();
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    w.wallet
        .hold("spend-source", "", 500_000, today())
        .await
        .unwrap();
    w.wallet
        .settle("spend-source", "", 500_000, today())
        .await
        .unwrap();
    w.wallet
        .top_up("unrelated-later-funds", 3_000_000, today())
        .await
        .unwrap();
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Liabilities:Wallet":-1_000_000,"Equity:Adjustments":-1_500_000})
    );
    assert_eq!(w.wallet.credit_used().await.unwrap(), 0);
    let audit = detail(&w, dispute.dispute_id).await;
    assert_eq!(audit["source"]["sourceRepaymentMinor"], 1_000_000);
    assert_eq!(audit["source"]["subsequentWalletDebitMinor"], 500_000);
    assert_eq!(audit["requiresReview"], true);
    assert_eq!(audit["unallocatedMinor"], 1_500_000);
}

#[tokio::test]
async fn pending_wallet_reservations_are_preserved_and_later_release_is_safe() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    w.wallet
        .hold("pending-source", "", 700_000, today())
        .await
        .unwrap();
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Liabilities:Wallet":-1_800_000,"Equity:Adjustments":-700_000})
    );
    assert_eq!(
        detail(&w, dispute.dispute_id).await["source"]["pendingWalletMinor"],
        700_000
    );
    w.wallet
        .settle("pending-source", "", 0, today())
        .await
        .unwrap();
    assert_eq!(w.wallet.available().await.unwrap(), 700_000);
}

#[tokio::test]
async fn invoice_repayment_loss_does_not_invent_new_credit_draws() {
    let Some(w) = world().await else { return };
    w.wallet
        .set_credit_limit("repayment-line", 5_000_000, today())
        .await
        .unwrap();
    w.wallet
        .hold("original-credit-draw", "", TOTAL, today())
        .await
        .unwrap();
    w.wallet
        .settle("original-credit-draw", "", TOTAL, today())
        .await
        .unwrap();
    let order = paid(&w, PaymentOrderPurpose::InvoiceRepayment, true).await;
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Equity:Adjustments":-TOTAL})
    );
    assert_eq!(w.wallet.credit_used().await.unwrap(), 0);
    assert_eq!(
        detail(&w, dispute.dispute_id).await["source"]["sourceRepaymentMinor"],
        TOTAL
    );
}

#[tokio::test]
async fn exact_partial_cases_preserve_every_loss_but_cannot_reclaim_source_twice() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let first = open(&w, &order, 150).await;
    assert_eq!(first.amount_minor, 1_500_000);
    lose(&w, first.dispute_id).await;
    w.wallet
        .top_up("unrelated-after-first", 4_000_000, today())
        .await
        .unwrap();
    let second = open(&w, &order, 250).await;
    let lost = lose(&w, second.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Liabilities:Wallet":-1_000_000,"Equity:Adjustments":-1_500_000})
    );
    assert_eq!(
        detail(&w, second.dispute_id).await["source"]["priorChargebackCashMinor"],
        1_500_000
    );
}

#[tokio::test]
async fn invalid_case_terms_are_durable_rejections_without_rewriting_the_first_case() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    for (kind, reason) in [
        (0, "missingDisputeAmount"),
        (1, "disputeCurrencyMismatch"),
        (2, "disputeAmountMismatch"),
        (3, "missingDisputeId"),
    ] {
        let mut bad = event(&w, &order, Some(250));
        match kind {
            0 => bad.amount_minor = None,
            1 => bad.currency = Some("EUR".into()),
            2 => bad.amount_minor = Some(251),
            _ => bad.refund_id = None,
        };
        let receipt =
            w.db.receive_merchant_event(&w.context, &bad, &[86; 32])
                .await
                .unwrap();
        assert_eq!(receipt.state, "rejected");
        assert_eq!(receipt.reason.as_deref(), Some(reason));
    }
    let good = event(&w, &order, Some(125));
    let receipt =
        w.db.receive_merchant_event(&w.context, &good, &[87; 32])
            .await
            .unwrap();
    w.db.apply_refund_event(&w.wallet, &receipt, today())
        .await
        .unwrap();
    let mut changed = good.clone();
    changed.event_id = Uuid::new_v4().to_string();
    changed.amount_minor = Some(250);
    let receipt =
        w.db.receive_merchant_event(&w.context, &changed, &[88; 32])
            .await
            .unwrap();
    assert_eq!(receipt.state, "rejected");
    assert_eq!(receipt.reason.as_deref(), Some("disputeTermsMismatch"));
    let amount: i64 =
        sqlx::query_scalar("SELECT amount_minor FROM oxsum.disputes WHERE order_id=$1")
            .bind(order.order_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(amount, 1_250_000);
}

#[tokio::test]
async fn prior_refund_history_requires_suspense_and_cannot_be_approved_after_loss() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: Some(500_000),
                reason: "prior request".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Equity:Adjustments":-TOTAL})
    );
    assert_eq!(
        detail(&w, dispute.dispute_id).await["source"]["reviewReasons"],
        json!(["priorRefundHistory"])
    );
    assert!(matches!(
        w.db.resolve_order_refund(
            &w.owner,
            refund.refund_id,
            RefundDecision::Approve,
            None,
            "approve-after-loss"
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
}

#[tokio::test]
async fn unperformed_fulfillment_is_fenced_after_complete_suspense_loss() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, false).await;
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Equity:Adjustments":-TOTAL})
    );
    assert_eq!(
        detail(&w, dispute.dispute_id).await["source"]["reviewReasons"],
        json!(["originalFulfillmentUnproven"])
    );
    assert!(matches!(
        w.db.fulfill_payment_order(order.order_id, &w.wallet, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    let healthy = paid(&w, PaymentOrderPurpose::WalletTopup, false).await;
    let queued = w.db.unfulfilled_payment_orders(100).await.unwrap();
    assert!(!queued.iter().any(|o| o.order_id == order.order_id));
    assert!(queued.iter().any(|o| o.order_id == healthy.order_id));
    let report = w.db.payment_reconciliation(&w.owner).await.unwrap();
    assert!(report.paid_unfulfilled.contains(&order.order_id));
}

#[tokio::test]
async fn bundle_promo_cancel_and_cash_rollback_together_then_reserved_promo_burns() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::Bundle, true).await;
    let lot =
        w.db.promo_lots(w.org, true)
            .await
            .unwrap()
            .into_iter()
            .find(|l| l.source_ref == order.order_id.to_string())
            .unwrap();
    w.wallet
        .hold("bundle-promo-hold", "", 400_000, today())
        .await
        .unwrap();
    let dispute = open(&w, &order, 250).await;
    let before = totals(&w).await;
    let function = format!("fail_cash_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dispute.resolve' AND NEW.target='{}' THEN RAISE EXCEPTION 'injected source cash audit failure'; END IF; RETURN NEW; END $$",dispute.dispute_id)).execute(w.db.pool()).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON oxsum.finance_audit FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()" )).execute(w.db.pool()).await.unwrap();
    assert!(
        w.db.resolve_order_dispute(
            &w.wallet,
            &w.owner,
            dispute.dispute_id,
            DisputeResolution::Lost,
            None,
            "fail-bundle-loss",
            today()
        )
        .await
        .is_err()
    );
    assert_eq!(totals(&w).await, before);
    assert!(
        w.db.promo_lot(lot.lot_id)
            .await
            .unwrap()
            .unwrap()
            .closed_at
            .is_none()
    );
    sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.finance_audit"))
        .execute(w.db.pool())
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION oxsum.{function}()"))
        .execute(w.db.pool())
        .await
        .unwrap();
    lose(&w, dispute.dispute_id).await;
    let closed = w.db.promo_lot(lot.lot_id).await.unwrap().unwrap();
    assert_eq!(closed.remaining_minor, 0);
    assert!(closed.closed_at.is_some());
    assert_eq!(closed.reserved_minor, 400_000);
    w.wallet
        .settle("bundle-promo-hold", "", 0, today())
        .await
        .unwrap();
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    assert_eq!(
        w.db.promo_lot(lot.lot_id)
            .await
            .unwrap()
            .unwrap()
            .reserved_minor,
        0
    );
}

async fn blocked(pool: &PgPool, owner: i32) -> i32 {
    timeout(Duration::from_secs(10),async {
        loop {
            let pid:Option<i32>=sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) LIMIT 1")
                .bind(owner).fetch_optional(pool).await.unwrap();
            if let Some(pid)=pid{return pid}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("backend waits on controlled lock")
}

#[tokio::test]
async fn loss_retries_without_pinning_pool_until_fulfillment_finishes_its_source() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, false).await;
    let dispute = open(&w, &order, 250).await;
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
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *pin)
        .await
        .unwrap();
    let db = w.db.clone();
    let wallet = w.wallet.clone();
    let grant = tokio::spawn(async move {
        db.fulfill_payment_order(order.order_id, &wallet, today())
            .await
    });
    blocked(w.db.pool(), pid).await;
    let db = w.db.clone();
    let wallet = w.wallet.clone();
    let owner = w.owner.clone();
    let loss = tokio::spawn(async move {
        db.resolve_order_dispute(
            &wallet,
            &owner,
            dispute.dispute_id,
            DisputeResolution::Lost,
            None,
            "concurrent-source-loss",
            today(),
        )
        .await
    });
    assert!(matches!(
        timeout(Duration::from_secs(2), loss)
            .await
            .unwrap()
            .unwrap(),
        Err(WalletError::PoolBusy)
    ));
    pin.rollback().await.unwrap();
    timeout(Duration::from_secs(10), grant)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Liabilities:Wallet":-TOTAL})
    );
}

#[tokio::test]
async fn fractional_partial_amount_is_rejected_and_an_integral_full_case_survives() {
    let Some(w) = world().await else { return };
    let bundle =
        w.db.publish_bundle(
            &w.owner,
            &BundlePublishRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "fractional partial dispute fixture".into(),
                bundle_id: None,
                expected_version: 0,
                terms: BundleTerms {
                    name: Uuid::new_v4().to_string(),
                    display_currency: "USD".into(),
                    display_amount_minor: 300,
                    purchased_minor: 2_000_000,
                    promo_minor: 0,
                    promo_lifetime_days: None,
                },
            },
        )
        .await
        .unwrap()
        .offer;
    let order =
        w.db.create_payment_order(
            &w.context,
            w.org,
            &NewPaymentOrder {
                purpose: PaymentOrderPurpose::Bundle,
                purpose_ref: Some(bundle.bundle_version_id),
                display_amount_minor: 300,
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
    w.db.fulfill_payment_order(order.order_id, &w.wallet, today())
        .await
        .unwrap();
    let receipt =
        w.db.receive_merchant_event(&w.context, &event(&w, &order, Some(1)), &[89; 32])
            .await
            .unwrap();
    assert_eq!(receipt.state, "rejected");
    assert_eq!(receipt.reason.as_deref(), Some("disputeAmountNotIntegral"));
    let dispute = open(&w, &order, 300).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":2_000_000,"Liabilities:Wallet":-2_000_000})
    );
}

#[tokio::test]
async fn incompatible_fulfillment_source_preserves_cash_without_taking_other_wallet_money() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, false).await;
    // Seed an incompatible real ledger entry under the historical order key.
    // This does not forge ledger rows or bypass immutable guards.
    w.wallet
        .top_up(
            &format!("payment-order:{}", order.order_id),
            TOTAL - 1,
            today(),
        )
        .await
        .unwrap();
    assert!(
        w.db.fulfill_payment_order(order.order_id, &w.wallet, today())
            .await
            .is_err()
    );
    w.wallet
        .top_up("unrelated-source-funds", 4_000_000, today())
        .await
        .unwrap();
    let available = w.wallet.available().await.unwrap();
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Equity:Adjustments":-TOTAL})
    );
    assert_eq!(w.wallet.available().await.unwrap(), available);
    assert_eq!(detail(&w, dispute.dispute_id).await["requiresReview"], true);
}

#[tokio::test]
async fn prior_legacy_wrong_direction_and_orphan_effects_require_review_without_repair() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    w.wallet
        .set_credit_limit("legacy-source-line", 1_000_000, today())
        .await
        .unwrap();
    w.wallet
        .hold("legacy-source-draw", "", TOTAL + 500_000, today())
        .await
        .unwrap();
    w.wallet
        .settle("legacy-source-draw", "", TOTAL + 500_000, today())
        .await
        .unwrap();
    let first = open(&w, &order, 250).await;
    // Reproduce the old DrCash/CrCreditLine allocation using the official
    // repayment API and the old deterministic case key, retaining its bytes.
    w.wallet
        .repay_credit(
            &format!("dispute-recharge/{}", first.dispute_id),
            500_000,
            today(),
        )
        .await
        .unwrap();
    let before = totals(&w).await;
    assert!(matches!(
        w.db.resolve_order_dispute(
            &w.wallet,
            &w.owner,
            first.dispute_id,
            DisputeResolution::Lost,
            None,
            "legacy-source-review",
            today()
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(totals(&w).await, before);
    w.wallet
        .top_up("after-legacy-source", 4_000_000, today())
        .await
        .unwrap();
    let second = open(&w, &order, 250).await;
    let lost = lose(&w, second.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Equity:Adjustments":-TOTAL})
    );
    assert_eq!(
        detail(&w, second.dispute_id).await["source"]["reviewReasons"],
        json!(["priorChargebackUnproven"])
    );
}

#[tokio::test]
async fn three_connection_bursts_queue_same_and_distinct_order_grants_off_pool() {
    let Some(w) = world().await else { return };
    for same in [true, false] {
        let mut orders = Vec::new();
        for _ in 0..if same { 1 } else { 8 } {
            orders.push(paid(&w, PaymentOrderPurpose::WalletTopup, false).await);
        }
        let low = Db::connect(
            &oxsum_testkit::suite_db_url("nonplan_chargebacks").unwrap(),
            3,
        )
        .await
        .unwrap();
        let wallet = Arc::new(
            Wallet::open_existing(low.pool().clone(), w.wallet.tenant_id())
                .await
                .unwrap(),
        );
        let barrier = Arc::new(tokio::sync::Barrier::new(17));
        let mut tasks = Vec::new();
        for i in 0..16 {
            let db = low.clone();
            let wallet = wallet.clone();
            let barrier = barrier.clone();
            let id = orders[i % orders.len()].order_id;
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                db.fulfill_payment_order(id, &wallet, today()).await
            }));
        }
        barrier.wait().await;
        for task in tasks {
            timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.payment_fulfillments WHERE order_id=ANY($1)",
        )
        .bind(orders.iter().map(|o| o.order_id).collect::<Vec<_>>())
        .fetch_one(low.pool())
        .await
        .unwrap();
        assert_eq!(count, orders.len() as i64);
        low.pool().close().await;
    }
}

#[tokio::test]
async fn three_connection_grant_remains_live_with_contending_dispute() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, false).await;
    let dispute = open(&w, &order, 250).await;
    let low = Db::connect(
        &oxsum_testkit::suite_db_url("nonplan_chargebacks").unwrap(),
        3,
    )
    .await
    .unwrap();
    let wallet = Arc::new(
        Wallet::open_existing(low.pool().clone(), w.wallet.tenant_id())
            .await
            .unwrap(),
    );
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
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *pin)
        .await
        .unwrap();
    let db = low.clone();
    let grant_wallet = wallet.clone();
    let grant = tokio::spawn(async move {
        db.fulfill_payment_order(order.order_id, &grant_wallet, today())
            .await
    });
    blocked(w.db.pool(), pid).await;
    let refused = timeout(
        Duration::from_secs(2),
        low.resolve_order_dispute(
            &wallet,
            &w.owner,
            dispute.dispute_id,
            DisputeResolution::Lost,
            None,
            "small-pool-contender",
            today(),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(refused, Err(WalletError::PoolBusy)));
    pin.rollback().await.unwrap();
    timeout(Duration::from_secs(5), grant)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let lost = low
        .resolve_order_dispute(
            &wallet,
            &w.owner,
            dispute.dispute_id,
            DisputeResolution::Lost,
            None,
            "small-pool-contender",
            today(),
        )
        .await
        .unwrap();
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Liabilities:Wallet":-TOTAL})
    );
    low.pool().close().await;
}

#[tokio::test]
async fn a_reversed_chargeback_cannot_replay_as_retained_cash() {
    use doubleentry::{
        AccountRegistry, EntryBatch, IdempotencyKey, LedgerPolicy, LedgerStore, SealContext,
    };
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    let store = PostgresStore::<6>::new(
        w.db.pool().clone(),
        LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
    )
    .in_schema(&format!("ledger_{}", w.wallet.tenant_id()));
    let id = oxsum_core::entry_id_for(&format!("dispute-recharge/{}", dispute.dispute_id));
    assert_eq!(Some(*id.as_uuid()), lost.recharge_entry_id);
    let original = store.get(id).await.unwrap().unwrap();
    let registry = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
    let key = format!("historical-chargeback-reversal:{}", dispute.dispute_id);
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
    let before = totals(&w).await;
    assert!(matches!(
        w.db.resolve_order_dispute(
            &w.wallet,
            &w.owner,
            dispute.dispute_id,
            DisputeResolution::Lost,
            Some("proven source loss"),
            &format!("loss-{}", dispute.dispute_id),
            today()
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(totals(&w).await, before);
}

#[tokio::test]
async fn cancellation_releases_source_guard_and_queue_and_tiny_pool_fails_before_grant() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, false).await;
    let url = oxsum_testkit::suite_db_url("nonplan_chargebacks").unwrap();
    let tiny = Db::connect(&url, 2).await.unwrap();
    let tiny_wallet = Wallet::open_existing(tiny.pool().clone(), w.wallet.tenant_id())
        .await
        .unwrap();
    assert!(matches!(
        timeout(
            Duration::from_secs(1),
            tiny.fulfill_payment_order(order.order_id, &tiny_wallet, today())
        )
        .await
        .unwrap(),
        Err(WalletError::PoolBusy)
    ));
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    tiny.pool().close().await;
    let low = Db::connect(&url, 3).await.unwrap();
    let wallet = Arc::new(
        Wallet::open_existing(low.pool().clone(), w.wallet.tenant_id())
            .await
            .unwrap(),
    );
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
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *pin)
        .await
        .unwrap();
    let db = low.clone();
    let grant_wallet = wallet.clone();
    let grant = tokio::spawn(async move {
        db.fulfill_payment_order(order.order_id, &grant_wallet, today())
            .await
    });
    blocked(w.db.pool(), pid).await;
    grant.abort();
    assert!(grant.await.unwrap_err().is_cancelled());
    pin.rollback().await.unwrap();
    timeout(
        Duration::from_secs(5),
        low.fulfill_payment_order(order.order_id, &wallet, today()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(w.wallet.available().await.unwrap(), TOTAL);
    low.pool().close().await;
}

#[tokio::test]
async fn reversed_original_source_never_uses_replacement_wallet_credit() {
    use doubleentry::{
        AccountRegistry, EntryBatch, IdempotencyKey, LedgerPolicy, LedgerStore, SealContext,
    };
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let store = PostgresStore::<6>::new(
        w.db.pool().clone(),
        LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
    )
    .in_schema(&format!("ledger_{}", w.wallet.tenant_id()));
    let original = store
        .get(oxsum_core::entry_id_for(&format!(
            "payment-order:{}",
            order.order_id
        )))
        .await
        .unwrap()
        .unwrap();
    let registry = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
    let key = format!("historical-source-reversal:{}", order.order_id);
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
    w.wallet
        .top_up(
            "replacement-wallet-after-source-reversal",
            4_000_000,
            today(),
        )
        .await
        .unwrap();
    let dispute = open(&w, &order, 250).await;
    let lost = lose(&w, dispute.dispute_id).await;
    assert_eq!(
        legs(&w, lost.recharge_entry_id.unwrap()).await,
        json!({"Assets:Cash":TOTAL,"Equity:Adjustments":-TOTAL})
    );
    assert_eq!(
        detail(&w, dispute.dispute_id).await["source"]["reviewReasons"],
        json!(["originalEntryReversed"])
    );
    assert_eq!(w.wallet.available().await.unwrap(), 4_000_000);
}

#[tokio::test]
async fn conflicting_case_that_raced_classification_keeps_review_reason_and_original_terms() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let original = event(&w, &order, Some(125));
    let first =
        w.db.receive_merchant_event(&w.context, &original, &[90; 32])
            .await
            .unwrap();
    let mut changed = original.clone();
    changed.event_id = Uuid::new_v4().to_string();
    changed.amount_minor = Some(250);
    let second =
        w.db.receive_merchant_event(&w.context, &changed, &[91; 32])
            .await
            .unwrap();
    assert_eq!(first.state, "accepted");
    assert_eq!(second.state, "accepted");
    w.db.apply_refund_event(&w.wallet, &first, today())
        .await
        .unwrap();
    assert!(matches!(
        w.db.apply_refund_event(&w.wallet, &second, today()).await,
        Err(WalletError::Conflict(_))
    ));
    let retained:serde_json::Value=sqlx::query_scalar("SELECT jsonb_build_object('state',state,'reason',reason,'normalized',normalized) FROM oxsum.merchant_event_inbox WHERE receipt_id=$1")
        .bind(second.id).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(retained["state"], "accepted");
    assert_eq!(retained["reason"], "disputeTermsMismatch");
    assert_eq!(retained["normalized"]["amountMinor"], 250);
    let amount: i64 =
        sqlx::query_scalar("SELECT amount_minor FROM oxsum.disputes WHERE order_id=$1")
            .bind(order.order_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(amount, 1_250_000);
    let duplicate =
        w.db.receive_merchant_event(&w.context, &original, &[90; 32])
            .await
            .unwrap();
    assert_eq!(duplicate.state, "applied");
    w.db.apply_refund_event(&w.wallet, &duplicate, today())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.disputes WHERE order_id=$1")
        .bind(order.order_id)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

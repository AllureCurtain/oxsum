//! Complete non-plan refund Cash, source bounds and explicit historical recovery.
#[path = "../support/plan_payment.rs"]
mod payment;
#[path = "../support/integration.rs"]
mod setup;

use std::sync::Arc;

use oxsum_core::{
    BundlePublishRequest, BundleTerms, BusinessRole, BusinessStaffRequest, Db, DisputeResolution,
    FactFilter, MerchantEventKind, MerchantEventReceipt, MerchantSigningContext, NewPaymentOrder,
    NewRefundRequest, NewUser, NonplanRefundRecoveryRequest, OrderRefund, PaymentOrder,
    PaymentOrderPurpose, RefundDecision, SessionContext, SessionContextRequest,
    VerifiedMerchantEvent, Wallet, WalletError,
};
use time::{Date, Duration, OffsetDateTime};
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
    let url = oxsum_testkit::suite_db_url("nonplan_refunds")?;
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
                expected_statement_quote: None,
                statement_amount_minor: None,
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

fn event(w: &World, order: &PaymentOrder, refund: &OrderRefund) -> VerifiedMerchantEvent {
    VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind: MerchantEventKind::RefundSucceeded,
        payment_id: Some(order.order_id),
        provider_payment_id: Some(format!("pay_{}", order.order_id)),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(oxsum_core::MerchantMode::Test),
        amount_minor: Some(refund.display_amount_minor),
        currency: Some("USD".into()),
        refund_id: Some(refund.refund_id.to_string()),
    }
}

async fn reserve(
    w: &World,
    order: &PaymentOrder,
    amount: Option<i64>,
    approve: bool,
) -> OrderRefund {
    let refund =
        w.db.request_order_refund(
            w.org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: amount,
                reason: "refund fixture".into(),
                request_key: Uuid::new_v4().to_string(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    if approve {
        w.db.resolve_order_refund(
            &w.owner,
            refund.refund_id,
            RefundDecision::Approve,
            Some("approved fixture"),
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap()
        .0
    } else {
        refund
    }
}
async fn signed_receipt(
    w: &World,
    order: &PaymentOrder,
    refund: &OrderRefund,
) -> MerchantEventReceipt {
    let received =
        w.db.receive_merchant_event(&w.context, &event(w, order, refund), &[19; 32])
            .await
            .unwrap();
    assert_eq!(received.state, "accepted", "{:?}", received.reason);
    received
}
async fn apply(w: &World, receipt: &MerchantEventReceipt) {
    w.db.apply_refund_event(&w.wallet, receipt, today())
        .await
        .unwrap();
}
async fn current(w: &World, id: Uuid) -> OrderRefund {
    w.db.business_refund(&w.owner, id).await.unwrap().unwrap()
}
async fn net(w: &World, path: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a USING(account_index) WHERE a.path=$1 AND p.layer='settled'",w.wallet.tenant_id(),w.wallet.tenant_id())).bind(path).fetch_one(w.db.pool()).await.unwrap()
}
async fn reported_cash(w: &World, on: Option<Date>) -> i64 {
    w.db.financial_facts(&FactFilter {
        organization_id: Some(w.org),
        from: on,
        to: on,
        ..FactFilter::default()
    })
    .await
    .unwrap()
    .refund_minor
}
fn recovery(refund: Uuid, event: Uuid) -> NonplanRefundRecoveryRequest {
    NonplanRefundRecoveryRequest {
        refund_id: refund,
        event_receipt_id: event,
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "reviewed exact historical evidence".into(),
        expected_revision: None,
    }
}

#[tokio::test]
async fn partial_and_full_returns_conserve_cash_once_and_show_customer_effect() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    for amount in [1_000_000, 1_500_000] {
        let refund = reserve(&w, &order, Some(amount), true).await;
        let event = signed_receipt(&w, &order, &refund).await;
        apply(&w, &event).await;
        apply(&w, &event).await;
        let result = current(&w, refund.refund_id).await;
        assert_eq!(result.state, "succeeded");
        let effect = result.cash_effect.unwrap();
        assert_eq!(
            (
                effect.cash_minor,
                effect.wallet_minor,
                effect.suspense_minor
            ),
            (amount, amount, 0)
        );
        assert!(!effect.requires_review);
        let customer = w.db.order_refunds(w.org, order.order_id).await.unwrap();
        assert!(
            customer
                .iter()
                .any(|r| r.refund_id == refund.refund_id && r.cash_effect.is_some())
        );
        let notes =
            w.db.business_order_credit_notes(&w.owner, None, 100)
                .await
                .unwrap();
        assert!(
            notes
                .notes
                .iter()
                .any(|n| n.refund_id == refund.refund_id && n.cash_effect.is_some())
        );
    }
    assert_eq!(net(&w, "Assets:Cash").await, 0);
    assert_eq!(reported_cash(&w, None).await, TOTAL);
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    assert_eq!(
        w.db.order_refundable_minor(w.org, order.order_id)
            .await
            .unwrap(),
        Some(0)
    );
}

#[tokio::test]
async fn spent_held_and_replacement_wallet_cannot_replace_original_source() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    w.wallet
        .hold("spent", "", 1_500_000, today())
        .await
        .unwrap();
    w.wallet
        .settle("spent", "", 1_500_000, today())
        .await
        .unwrap();
    w.wallet
        .hold("pending", "", 500_000, today())
        .await
        .unwrap();
    w.wallet
        .top_up("replacement", 4_000_000, today())
        .await
        .unwrap();
    let refund = reserve(&w, &order, None, true).await;
    apply(&w, &signed_receipt(&w, &order, &refund).await).await;
    let effect = current(&w, refund.refund_id).await.cash_effect.unwrap();
    assert_eq!(
        (
            effect.cash_minor,
            effect.wallet_minor,
            effect.suspense_minor
        ),
        (TOTAL, 500_000, 2_000_000)
    );
    assert_eq!(w.wallet.available().await.unwrap(), 4_000_000);
    assert_eq!(w.wallet.reserved().await.unwrap(), 500_000);
    assert_eq!(net(&w, "Equity:Adjustments").await, -2_000_000);
}

#[tokio::test]
async fn repayment_origin_return_never_changes_current_credit_debt() {
    let Some(w) = world().await else { return };
    w.wallet
        .set_credit_limit("line", 5_000_000, today())
        .await
        .unwrap();
    w.wallet
        .hold("credit-use", "", TOTAL, today())
        .await
        .unwrap();
    w.wallet
        .settle("credit-use", "", TOTAL, today())
        .await
        .unwrap();
    let order = paid(&w, PaymentOrderPurpose::InvoiceRepayment, true).await;
    let before = net(&w, "Liabilities:CreditLine").await;
    let refund = reserve(&w, &order, None, true).await;
    apply(&w, &signed_receipt(&w, &order, &refund).await).await;
    let effect = current(&w, refund.refund_id).await.cash_effect.unwrap();
    assert_eq!((effect.wallet_minor, effect.suspense_minor), (0, TOTAL));
    assert_eq!(net(&w, "Liabilities:CreditLine").await, before);
    assert_eq!(w.wallet.available().await.unwrap(), 5_000_000);
}

#[tokio::test]
async fn unapproved_and_declined_cash_stays_suspense_and_cannot_authorize_another_payout() {
    for decline in [false, true] {
        let Some(w) = world().await else { return };
        let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
        let refund = reserve(&w, &order, None, false).await;
        if decline {
            w.db.resolve_order_refund(
                &w.owner,
                refund.refund_id,
                RefundDecision::Decline,
                Some("no approval"),
                &Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        }
        let before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM oxsum.finance_audit WHERE target=$1")
                .bind(refund.refund_id.to_string())
                .fetch_one(w.db.pool())
                .await
                .unwrap();
        apply(&w, &signed_receipt(&w, &order, &refund).await).await;
        let result = current(&w, refund.refund_id).await;
        assert_eq!(result.state, if decline { "declined" } else { "requested" });
        let effect = result.cash_effect.unwrap();
        assert_eq!((effect.wallet_minor, effect.suspense_minor), (0, TOTAL));
        assert!(effect.review_reasons.contains(&"unapprovedRefund".into()));
        assert!(effect.review_reasons.contains(&"missingCreditNote".into()));
        assert_eq!(result.note_id, None);
        let (_, call) =
            w.db.resolve_order_refund(
                &w.owner,
                refund.refund_id,
                RefundDecision::Approve,
                Some("cannot repay"),
                &Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        assert!(!call);
        assert!(
            w.db.claim_refund_attempt(refund.refund_id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            w.db.order_refundable_minor(w.org, order.order_id)
                .await
                .unwrap(),
            Some(0)
        );
        assert_eq!(w.wallet.available().await.unwrap(), TOTAL);
        assert_eq!(reported_cash(&w, None).await, TOTAL);
        let after: i64 =
            sqlx::query_scalar("SELECT count(*) FROM oxsum.finance_audit WHERE target=$1")
                .bind(refund.refund_id.to_string())
                .fetch_one(w.db.pool())
                .await
                .unwrap();
        assert_eq!(
            after, before,
            "external cash never fabricates a staff actor"
        );
    }
}

#[tokio::test]
async fn late_released_success_counts_bound_and_preserves_reversed_note() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, None, true).await;
    assert!(
        w.db.claim_refund_attempt(refund.refund_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(w.db.refuse_claimed_refund(refund.refund_id).await.unwrap());
    apply(&w, &signed_receipt(&w, &order, &refund).await).await;
    let result = current(&w, refund.refund_id).await;
    assert_eq!(result.state, "released");
    assert_eq!(result.cash_effect.unwrap().cash_minor, TOTAL);
    let state: String =
        sqlx::query_scalar("SELECT state FROM oxsum.credit_notes WHERE refund_id=$1")
            .bind(refund.refund_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(state, "reversed");
    assert_eq!(
        w.db.order_refundable_minor(w.org, order.order_id)
            .await
            .unwrap(),
        Some(0)
    );
}

#[tokio::test]
async fn return_before_fulfillment_fences_grant_and_remains_reconcilable() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, false).await;
    let refund = reserve(&w, &order, Some(1_000_000), true).await;
    apply(&w, &signed_receipt(&w, &order, &refund).await).await;
    assert_eq!(
        current(&w, refund.refund_id)
            .await
            .cash_effect
            .unwrap()
            .suspense_minor,
        1_000_000
    );
    assert!(matches!(
        w.db.fulfill_payment_order(order.order_id, &w.wallet, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        !w.db
            .unfulfilled_payment_orders(100)
            .await
            .unwrap()
            .iter()
            .any(|o| o.order_id == order.order_id)
    );
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    assert_eq!(
        w.db.payment_order(w.org, order.order_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "paid"
    );
}

async fn old_cash(w: &World, refund: Uuid, amount: i64) -> String {
    old_cash_on(w, refund, amount, today()).await
}

async fn old_cash_on(w: &World, refund: Uuid, amount: i64, on: Date) -> String {
    use doubleentry::storage::postgres::PostgresStore;
    use doubleentry::{
        AccountRegistry, Amount, Currency, Draft, Entry, EntryBatch, IdempotencyKey, LedgerId,
        LedgerPolicy, LedgerStore, SealContext,
    };
    let store = PostgresStore::<6>::new(
        w.db.pool().clone(),
        LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
    )
    .in_schema(&format!("ledger_{}", w.wallet.tenant_id()));
    let accounts = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
    let find = |path: &str| {
        accounts
            .records()
            .iter()
            .find(|r| r.account.path.to_string() == path)
            .unwrap()
            .id
    };
    let key = format!("order-refund/{refund}");
    let entry = Entry::<Draft, 6>::new(
        oxsum_core::entry_id_for(&key),
        IdempotencyKey::new(key.as_bytes().to_vec()).unwrap(),
        on,
    )
    .debit(
        find("Liabilities:Wallet"),
        Amount::from_minor(amount),
        Currency::USD,
    )
    .credit(
        find("Assets:Cash"),
        Amount::from_minor(amount),
        Currency::USD,
    )
    .seal(&SealContext {
        accounts: &accounts,
        calendar: &store.calendar().await.unwrap(),
        policy: &LedgerPolicy::default(),
    })
    .unwrap();
    let hash = entry.content_hash().to_hex();
    store.append(&EntryBatch::single(entry)).await.unwrap();
    hash
}

#[tokio::test]
async fn historical_wrong_source_restores_wallet_without_repeating_cash() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    w.wallet.hold("spent", "", TOTAL, today()).await.unwrap();
    w.wallet.settle("spent", "", TOTAL, today()).await.unwrap();
    w.wallet
        .top_up("replacement", 4_000_000, today())
        .await
        .unwrap();
    let refund = reserve(&w, &order, None, true).await;
    let receipt = signed_receipt(&w, &order, &refund).await;
    let original_cash_day = today() - Duration::days(1);
    let hash = old_cash_on(&w, refund.refund_id, TOTAL, original_cash_day).await;
    let id = *oxsum_core::entry_id_for(&format!("order-refund/{}", refund.refund_id)).as_uuid();
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(refund.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.credit_notes SET state='applied',entry_id=$2 WHERE refund_id=$1")
        .bind(refund.refund_id)
        .bind(id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let cash = net(&w, "Assets:Cash").await;
    assert_eq!(reported_cash(&w, Some(original_cash_day)).await, TOTAL);
    assert_eq!(reported_cash(&w, Some(today())).await, 0);
    let count = w.wallet.log_size().await.unwrap();
    let mut request = recovery(refund.refund_id, receipt.id);
    let preview =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    assert_eq!(preview.status, "proposed");
    assert_eq!(
        (preview.cash_to_book_minor, preview.wallet_to_restore_minor),
        (0, TOTAL)
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), count);
    request.expected_revision = Some(preview.source_revision);
    for _ in 0..2 {
        let result =
            w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
                .await
                .unwrap();
        assert_eq!(result.status, "applied");
        assert_eq!(result.effect.unwrap().cash_entry_hash, hash);
    }
    assert_eq!(net(&w, "Assets:Cash").await, cash);
    assert_eq!(reported_cash(&w, None).await, TOTAL);
    assert_eq!(reported_cash(&w, Some(original_cash_day)).await, TOTAL);
    assert_eq!(reported_cash(&w, Some(today())).await, 0);
    assert_eq!(w.wallet.available().await.unwrap(), 4_000_000);
    assert_eq!(w.wallet.log_size().await.unwrap(), count + 1);
    let note: Uuid =
        sqlx::query_scalar("SELECT entry_id FROM oxsum.credit_notes WHERE refund_id=$1")
            .bind(refund.refund_id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(note, id);
}

#[tokio::test]
async fn historical_bound_ignores_later_spend_and_missing_cash_requires_fresh_revision() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, Some(1_000_000), true).await;
    let receipt = signed_receipt(&w, &order, &refund).await;
    old_cash(&w, refund.refund_id, 1_000_000).await;
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(refund.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    w.wallet
        .hold("later", "", 1_500_000, today())
        .await
        .unwrap();
    w.wallet
        .settle("later", "", 1_500_000, today())
        .await
        .unwrap();
    let mut request = recovery(refund.refund_id, receipt.id);
    let preview =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    assert_eq!(
        (
            preview.status.as_str(),
            preview.wallet_minor,
            preview.wallet_to_restore_minor
        ),
        ("proposed", 1_000_000, 0)
    );
    request.expected_revision = Some(preview.source_revision);
    let log = w.wallet.log_size().await.unwrap();
    w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
        .await
        .unwrap();
    assert_eq!(w.wallet.log_size().await.unwrap(), log);

    let another = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let missing = reserve(&w, &another, None, true).await;
    let missing_event = signed_receipt(&w, &another, &missing).await;
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(missing.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let mut request = recovery(missing.refund_id, missing_event.id);
    let first =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    request.expected_revision = Some(first.source_revision);
    w.wallet
        .hold("revision-drift", "", 500_000, today())
        .await
        .unwrap();
    assert!(matches!(
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    let fresh =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    assert_eq!(
        (
            fresh.cash_to_book_minor,
            fresh.wallet_minor,
            fresh.suspense_minor
        ),
        (TOTAL, 2_000_000, 500_000)
    );
    request.expected_revision = Some(fresh.source_revision);
    let result =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
            .await
            .unwrap();
    assert_eq!(result.effect.unwrap().cash_minor, TOTAL);
}

#[tokio::test]
async fn missing_external_receipt_is_refused_without_ledger_or_preview_audit() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, None, true).await;
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(refund.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let log = w.wallet.log_size().await.unwrap();
    let mut request = recovery(refund.refund_id, Uuid::new_v4());
    let report =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    assert_eq!(report.status, "refused");
    assert!(report.refusal.unwrap().contains("receipt is missing"));
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.finance_audit WHERE action='nonplan_refund.recover' AND target=$1").bind(refund.refund_id.to_string()).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(count, 0);
    request.expected_revision = Some(report.source_revision);
    let applied =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
            .await
            .unwrap();
    assert_eq!(applied.status, "refused");
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
}

#[tokio::test]
async fn callback_cash_promo_note_and_effect_roll_back_when_audit_fails() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::Bundle, true).await;
    let refund = reserve(&w, &order, None, true).await;
    let receipt = signed_receipt(&w, &order, &refund).await;
    let log = w.wallet.log_size().await.unwrap();
    let cash = net(&w, "Assets:Cash").await;
    let function = format!("fail_nonplan_refund_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.idempotency_key='nonplan-refund-cash:{}' THEN RAISE EXCEPTION 'controlled refund audit failure'; END IF; RETURN NEW; END $$",refund.refund_id)).execute(w.db.pool()).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON oxsum.finance_audit FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()" )).execute(w.db.pool()).await.unwrap();
    assert!(
        w.db.apply_refund_event(&w.wallet, &receipt, today())
            .await
            .is_err()
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
    assert_eq!(net(&w, "Assets:Cash").await, cash);
    let standing = current(&w, refund.refund_id).await;
    assert_eq!(standing.state, "provider_pending");
    assert!(standing.cash_effect.is_none());
    let closed: Option<OffsetDateTime> = sqlx::query_scalar(
        "SELECT closed_at FROM oxsum.promo_lots WHERE source='bundle_promo' AND source_ref=$1",
    )
    .bind(order.order_id.to_string())
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert!(closed.is_none());
    sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.finance_audit"))
        .execute(w.db.pool())
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION oxsum.{function}()"))
        .execute(w.db.pool())
        .await
        .unwrap();
    apply(&w, &receipt).await;
    assert_eq!(
        current(&w, refund.refund_id)
            .await
            .cash_effect
            .unwrap()
            .cash_minor,
        TOTAL
    );
}

#[tokio::test]
async fn concurrent_loss_and_refund_retain_both_cash_facts_once() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, None, true).await;
    let receipt = signed_receipt(&w, &order, &refund).await;
    let mut disputed = event(&w, &order, &refund);
    disputed.kind = MerchantEventKind::Dispute;
    disputed.event_id = Uuid::new_v4().to_string();
    disputed.refund_id = Some(Uuid::new_v4().to_string());
    let case =
        w.db.receive_merchant_event(&w.context, &disputed, &[21; 32])
            .await
            .unwrap();
    apply(&w, &case).await;
    let dispute: Uuid =
        sqlx::query_scalar("SELECT dispute_id FROM oxsum.disputes WHERE inbox_receipt_id=$1")
            .bind(case.id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let key = Uuid::new_v4().to_string();
    let (loss, returned) = tokio::join!(
        w.db.resolve_order_dispute(
            &w.wallet,
            &w.owner,
            dispute,
            DisputeResolution::Lost,
            Some("lost"),
            &key,
            today()
        ),
        w.db.apply_refund_event(&w.wallet, &receipt, today())
    );
    if matches!(loss, Err(WalletError::PoolBusy)) {
        w.db.resolve_order_dispute(
            &w.wallet,
            &w.owner,
            dispute,
            DisputeResolution::Lost,
            Some("lost"),
            &key,
            today(),
        )
        .await
        .unwrap();
    } else {
        loss.unwrap();
    }
    if matches!(returned, Err(WalletError::PoolBusy)) {
        apply(&w, &receipt).await;
    } else {
        returned.unwrap();
    }
    assert_eq!(net(&w, "Assets:Cash").await, TOTAL);
    let retained = w.wallet.available().await.unwrap();
    // If the loss wins, the preapproved refund conservatively fences its Wallet
    // allocation and both actual cash facts remain in suspense. If the refund
    // wins, it takes the original Wallet once and the loss uses suspense.
    assert!(retained == 0 || retained == TOTAL);
    assert_eq!(net(&w, "Equity:Adjustments").await, -TOTAL - retained);
    apply(&w, &receipt).await;
    assert_eq!(net(&w, "Assets:Cash").await, TOTAL);
}

#[tokio::test]
async fn two_preapproved_partials_share_original_source_and_never_repeat_effects() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let first = reserve(&w, &order, Some(1_000_000), true).await;
    let second = reserve(&w, &order, Some(1_500_000), true).await;
    let a = signed_receipt(&w, &order, &first).await;
    let b = signed_receipt(&w, &order, &second).await;
    let (one, two) = tokio::join!(
        w.db.apply_refund_event(&w.wallet, &a, today()),
        w.db.apply_refund_event(&w.wallet, &b, today())
    );
    for (result, event) in [(one, &a), (two, &b)] {
        if matches!(result, Err(WalletError::PoolBusy)) {
            apply(&w, event).await;
        } else {
            result.unwrap();
        }
    }
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    assert_eq!(net(&w, "Assets:Cash").await, 0);
    assert_eq!(net(&w, "Equity:Adjustments").await, 0);
}

#[tokio::test]
async fn approval_racing_signed_success_cannot_authorize_payout_after_effect() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, None, false).await;
    let event = signed_receipt(&w, &order, &refund).await;
    let key = Uuid::new_v4().to_string();
    let (decision, cash) = tokio::join!(
        w.db.resolve_order_refund(
            &w.owner,
            refund.refund_id,
            RefundDecision::Approve,
            Some("approve"),
            &key
        ),
        w.db.apply_refund_event(&w.wallet, &event, today())
    );
    if matches!(cash, Err(WalletError::PoolBusy)) {
        apply(&w, &event).await;
    } else {
        cash.unwrap();
    }
    if matches!(decision, Err(WalletError::PoolBusy)) {
        assert!(
            !w.db
                .resolve_order_refund(
                    &w.owner,
                    refund.refund_id,
                    RefundDecision::Approve,
                    Some("approve"),
                    &key
                )
                .await
                .unwrap()
                .1
        );
    } else {
        decision.unwrap();
    }
    assert!(current(&w, refund.refund_id).await.cash_effect.is_some());
    assert!(
        w.db.claim_refund_attempt(refund.refund_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !w.db
            .due_refund_retries(100)
            .await
            .unwrap()
            .iter()
            .any(|r| r.refund_id == refund.refund_id)
    );
}

#[tokio::test]
async fn wrong_wallet_forged_receipt_and_customer_session_cannot_apply_recovery() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, None, true).await;
    let event = signed_receipt(&w, &order, &refund).await;
    let another = Wallet::open(w.db.pool().clone(), &Uuid::new_v4().simple().to_string())
        .await
        .unwrap();
    let request = recovery(refund.refund_id, event.id);
    assert!(matches!(
        w.db.recover_nonplan_refund(&w.owner, &another, &request, false, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    let mut forged = event.clone();
    forged.id = Uuid::new_v4();
    assert!(
        w.db.apply_refund_event(&w.wallet, &forged, today())
            .await
            .is_err()
    );
    for forged in [
        MerchantEventReceipt {
            kind: MerchantEventKind::RefundFailed,
            ..event.clone()
        },
        MerchantEventReceipt {
            payment_id: None,
            ..event.clone()
        },
        MerchantEventReceipt {
            merchant_id: Uuid::new_v4(),
            ..event.clone()
        },
    ] {
        assert!(
            w.db.apply_refund_event(&w.wallet, &forged, today())
                .await
                .is_err()
        );
    }
    assert_eq!(
        current(&w, refund.refund_id).await.state,
        "provider_pending"
    );
    let email = format!("{}@example.test", Uuid::new_v4());
    w.db.register(NewUser {
        email: email.clone(),
        password: "customer only password".into(),
        organization_name: None,
    })
    .await
    .unwrap();
    let customer =
        w.db.login(&email, "customer only password")
            .await
            .unwrap()
            .into_session();
    assert!(matches!(
        w.db.recover_nonplan_refund(&customer.token, &w.wallet, &request, false, today())
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(w.wallet.available().await.unwrap(), TOTAL);
}

#[tokio::test]
async fn mixed_credit_repayment_and_wallet_refund_only_reclaims_original_wallet() {
    let Some(w) = world().await else { return };
    w.wallet
        .set_credit_limit("mixed-line", 5_000_000, today())
        .await
        .unwrap();
    w.wallet
        .hold("credit", "", 1_000_000, today())
        .await
        .unwrap();
    w.wallet
        .settle("credit", "", 1_000_000, today())
        .await
        .unwrap();
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let credit = net(&w, "Liabilities:CreditLine").await;
    let refund = reserve(&w, &order, None, true).await;
    apply(&w, &signed_receipt(&w, &order, &refund).await).await;
    let effect = current(&w, refund.refund_id).await.cash_effect.unwrap();
    assert_eq!(
        (effect.wallet_minor, effect.suspense_minor),
        (1_500_000, 1_000_000)
    );
    assert_eq!(net(&w, "Liabilities:CreditLine").await, credit);
}

#[tokio::test]
async fn reversed_original_and_reversed_retained_return_cannot_justify_recovery() {
    use doubleentry::storage::postgres::PostgresStore;
    use doubleentry::{
        AccountRegistry, EntryBatch, IdempotencyKey, LedgerId, LedgerPolicy, LedgerStore,
        SealContext,
    };
    for reverse_cash in [false, true] {
        let Some(w) = world().await else { return };
        let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
        let refund = reserve(&w, &order, None, true).await;
        let event = signed_receipt(&w, &order, &refund).await;
        if reverse_cash {
            old_cash(&w, refund.refund_id, TOTAL).await;
        }
        let store = PostgresStore::<6>::new(
            w.db.pool().clone(),
            LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
        )
        .in_schema(&format!("ledger_{}", w.wallet.tenant_id()));
        let original = store
            .get(oxsum_core::entry_id_for(&if reverse_cash {
                format!("order-refund/{}", refund.refund_id)
            } else {
                format!("payment-order:{}", order.order_id)
            }))
            .await
            .unwrap()
            .unwrap();
        let accounts = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
        let key = Uuid::new_v4().to_string();
        let reverse = original
            .entry
            .reverse(
                oxsum_core::entry_id_for(&key),
                IdempotencyKey::new(key.as_bytes().to_vec()).unwrap(),
                today(),
            )
            .seal(&SealContext {
                accounts: &accounts,
                calendar: &store.calendar().await.unwrap(),
                policy: &LedgerPolicy::default(),
            })
            .unwrap();
        store.append(&EntryBatch::single(reverse)).await.unwrap();
        if reverse_cash {
            let request = recovery(refund.refund_id, event.id);
            let log = w.wallet.log_size().await.unwrap();
            let report =
                w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
                    .await
                    .unwrap();
            assert_eq!(report.status, "refused");
            assert_eq!(w.wallet.log_size().await.unwrap(), log);
        } else {
            w.wallet
                .top_up("replacement", 4_000_000, today())
                .await
                .unwrap();
            apply(&w, &event).await;
            let effect = current(&w, refund.refund_id).await.cash_effect.unwrap();
            assert_eq!((effect.wallet_minor, effect.suspense_minor), (0, TOTAL));
            assert_eq!(w.wallet.available().await.unwrap(), 4_000_000);
        }
    }
}

#[tokio::test]
async fn historical_holds_and_later_refunds_use_the_exclusive_cash_boundary() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, Some(1_000_000), true).await;
    let receipt = signed_receipt(&w, &order, &refund).await;
    w.wallet
        .hold("historical-held", "", 2_000_000, today())
        .await
        .unwrap();
    // The old policy debited pooled replacements while original credit remained
    // reserved. Releasing the hold later cannot change that historical boundary.
    w.wallet
        .top_up("held-replacement", 1_000_000, today())
        .await
        .unwrap();
    old_cash(&w, refund.refund_id, 1_000_000).await;
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(refund.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    w.wallet
        .settle("historical-held", "", 0, today())
        .await
        .unwrap();
    let later = reserve(&w, &order, Some(500_000), true).await;
    let later_receipt = signed_receipt(&w, &order, &later).await;
    apply(&w, &later_receipt).await;
    let mut request = recovery(refund.refund_id, receipt.id);
    let preview =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    assert_eq!(preview.status, "proposed");
    assert_eq!(
        (preview.wallet_minor, preview.wallet_to_restore_minor),
        (500_000, 500_000)
    );
    assert_eq!(preview.source["original"]["pendingWalletMinor"], 2_000_000);
    request.expected_revision = Some(preview.source_revision);
    let before = net(&w, "Assets:Cash").await;
    w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
        .await
        .unwrap();
    assert_eq!(net(&w, "Assets:Cash").await, before);
    assert_eq!(reported_cash(&w, None).await, 1_500_000);
}

#[tokio::test]
async fn recovery_rechecks_finance_context_role_and_exact_actor_request() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, None, true).await;
    let receipt = signed_receipt(&w, &order, &refund).await;
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(refund.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let user =
        w.db.register(NewUser {
            email: format!("{}@example.test", Uuid::new_v4()),
            password: "finance recovery password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let token =
        w.db.login(&user.user.email, "finance recovery password")
            .await
            .unwrap()
            .into_session()
            .token;
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            user_id: user.user.id,
            role: BusinessRole::Finance,
            reason: "recover exact refunds".into(),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    let mut request = recovery(refund.refund_id, receipt.id);
    assert!(
        w.db.recover_nonplan_refund(&token, &w.wallet, &request, false, today())
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
    let report =
        w.db.recover_nonplan_refund(&token, &w.wallet, &request, false, today())
            .await
            .unwrap();
    request.expected_revision = Some(report.source_revision);
    let report =
        w.db.recover_nonplan_refund(&token, &w.wallet, &request, true, today())
            .await
            .unwrap();
    assert_eq!(report.status, "applied");
    assert!(
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
            .await
            .is_err(),
        "same key cannot change actor"
    );
    request.reason.push_str(" changed");
    assert!(
        w.db.recover_nonplan_refund(&token, &w.wallet, &request, true, today())
            .await
            .is_err(),
        "same key cannot change reason"
    );
    request.reason = "reviewed exact historical evidence".into();
    assert!(
        w.db.recover_nonplan_refund(&token, &w.wallet, &request, true, today())
            .await
            .unwrap()
            .already_applied
    );
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            user_id: user.user.id,
            role: BusinessRole::Viewer,
            reason: "remove finance authority".into(),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    assert!(
        w.db.recover_nonplan_refund(&token, &w.wallet, &request, true, today())
            .await
            .is_err(),
        "committed replay still requires current funds authority"
    );
}

#[tokio::test]
async fn historical_restitution_rolls_back_when_recovery_audit_fails() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    w.wallet
        .hold("original-spent", "", TOTAL, today())
        .await
        .unwrap();
    w.wallet
        .settle("original-spent", "", TOTAL, today())
        .await
        .unwrap();
    w.wallet
        .top_up("other-payment", TOTAL, today())
        .await
        .unwrap();
    let refund = reserve(&w, &order, None, true).await;
    let receipt = signed_receipt(&w, &order, &refund).await;
    let hash = old_cash(&w, refund.refund_id, TOTAL).await;
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(refund.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let mut request = recovery(refund.refund_id, receipt.id);
    let report =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    request.expected_revision = Some(report.source_revision);
    let log = w.wallet.log_size().await.unwrap();
    let cash = net(&w, "Assets:Cash").await;
    let function = format!("fail_refund_recovery_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='nonplan_refund.recover' AND NEW.target='{}' THEN RAISE EXCEPTION 'controlled recovery audit failure'; END IF; RETURN NEW; END $$",refund.refund_id)).execute(w.db.pool()).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON oxsum.finance_audit FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()" )).execute(w.db.pool()).await.unwrap();
    assert!(
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
            .await
            .is_err()
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
    assert_eq!(w.wallet.available().await.unwrap(), 0);
    assert_eq!(net(&w, "Assets:Cash").await, cash);
    assert!(current(&w, refund.refund_id).await.cash_effect.is_none());
    sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.finance_audit"))
        .execute(w.db.pool())
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION oxsum.{function}()"))
        .execute(w.db.pool())
        .await
        .unwrap();
    let report =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
            .await
            .unwrap();
    assert_eq!(report.effect.unwrap().cash_entry_hash, hash);
    assert_eq!(w.wallet.available().await.unwrap(), TOTAL);
    assert_eq!(w.wallet.log_size().await.unwrap(), log + 1);
}

#[tokio::test]
async fn callbacks_and_historical_recovery_complete_with_one_shared_connection() {
    let Some(w) = world().await else { return };
    let live_order = paid(&w, PaymentOrderPurpose::Bundle, true).await;
    let live_refund = reserve(&w, &live_order, None, true).await;
    let live_receipt = signed_receipt(&w, &live_order, &live_refund).await;
    let old_order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    // Spend both original purchased sources and the bundle promotion, then
    // reproduce the legacy take-back from unrelated replacement funds.
    w.wallet
        .hold("all-original-spent", "", TOTAL * 2 + 1_000_000, today())
        .await
        .unwrap();
    w.wallet
        .settle("all-original-spent", "", TOTAL * 2 + 1_000_000, today())
        .await
        .unwrap();
    w.wallet
        .top_up("later-replacement", TOTAL, today())
        .await
        .unwrap();
    let old_refund = reserve(&w, &old_order, None, true).await;
    let old_receipt = signed_receipt(&w, &old_order, &old_refund).await;
    let old_hash = old_cash(&w, old_refund.refund_id, TOTAL).await;
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(old_refund.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let url = oxsum_testkit::suite_db_url("nonplan_refunds").unwrap();
    let small = Db::connect(&url, 1).await.unwrap();
    let wallet = Wallet::open_existing(small.pool().clone(), w.wallet.tenant_id())
        .await
        .unwrap();
    let mut request = recovery(old_refund.refund_id, old_receipt.id);
    let preview = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        small.recover_nonplan_refund(&w.owner, &wallet, &request, false, today()),
    )
    .await
    .expect("preview must use its one borrowed connection")
    .unwrap();
    assert_eq!(preview.wallet_to_restore_minor, TOTAL);
    request.expected_revision = Some(preview.source_revision);
    let (callback, recovered) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            small.apply_refund_event(&wallet, &live_receipt, today()),
            small.recover_nonplan_refund(&w.owner, &wallet, &request, true, today())
        )
    })
    .await
    .expect("distinct refund operations must not request nested pool connections");
    callback.unwrap();
    assert_eq!(recovered.unwrap().effect.unwrap().cash_entry_hash, old_hash);
    let replay = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        small.recover_nonplan_refund(&w.owner, &wallet, &request, true, today()),
    )
    .await
    .expect("replay verification must use the borrowed connection")
    .unwrap();
    assert!(replay.already_applied);
    assert_eq!(reported_cash(&w, None).await, TOTAL * 2);
    assert_eq!(wallet.available().await.unwrap(), TOTAL);
}

#[tokio::test]
async fn corrupted_original_or_historical_activity_cannot_authorize_wallet_changes() {
    for historical in [false, true] {
        let Some(w) = world().await else { return };
        let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
        let refund = reserve(&w, &order, None, true).await;
        let receipt = signed_receipt(&w, &order, &refund).await;
        let tampered = if historical {
            w.wallet
                .hold("historical-draw", "", 500_000, today())
                .await
                .unwrap();
            let spent = w
                .wallet
                .settle("historical-draw", "", 500_000, today())
                .await
                .unwrap();
            w.wallet
                .top_up("another-source", 500_000, today())
                .await
                .unwrap();
            old_cash(&w, refund.refund_id, TOTAL).await;
            sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
                .bind(refund.refund_id)
                .execute(w.db.pool())
                .await
                .unwrap();
            spent.entry_id
        } else {
            oxsum_core::entry_id_for(&format!("payment-order:{}", order.order_id))
        };
        // Isolated test-only corruption retains the original content hash.
        sqlx::query(&format!(
            "UPDATE ledger_{}.entries SET description='tampered fixture' WHERE entry_id=$1",
            w.wallet.tenant_id()
        ))
        .bind(tampered.as_uuid())
        .execute(w.db.pool())
        .await
        .unwrap();
        if historical {
            let count = w.wallet.log_size().await.unwrap();
            let report =
                w.db.recover_nonplan_refund(
                    &w.owner,
                    &w.wallet,
                    &recovery(refund.refund_id, receipt.id),
                    false,
                    today(),
                )
                .await
                .unwrap();
            assert_eq!(report.status, "refused");
            assert!(
                report
                    .refusal
                    .unwrap()
                    .contains("original-source bound is unproven")
            );
            assert_eq!(w.wallet.log_size().await.unwrap(), count);
        } else {
            apply(&w, &receipt).await;
            let effect = current(&w, refund.refund_id).await.cash_effect.unwrap();
            assert_eq!(
                (
                    effect.cash_minor,
                    effect.wallet_minor,
                    effect.suspense_minor
                ),
                (TOTAL, 0, TOTAL)
            );
            assert!(
                effect
                    .review_reasons
                    .contains(&"originalEntryHashUnproven".into())
            );
            assert_eq!(w.wallet.available().await.unwrap(), TOTAL);
        }
    }
}

#[tokio::test]
async fn migration_default_preserves_legacy_policy_and_ambiguous_receipt_refuses_recovery() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.refunds(refund_id,order_id,organization_id,business_id,amount_minor,display_amount_minor,display_currency,reason,state,provider_payment_id,request_key,hyperswitch_body_id) SELECT $1,order_id,organization_id,business_id,amount_minor,display_amount_minor,display_currency,'legacy source fixture','requested',provider_payment_id,$2,true FROM oxsum.payment_orders WHERE order_id=$3")
        .bind(id).bind(Uuid::new_v4().to_string()).bind(order.order_id).execute(w.db.pool()).await.unwrap();
    let version: i16 =
        sqlx::query_scalar("SELECT cash_policy_version FROM oxsum.refunds WHERE refund_id=$1")
            .bind(id)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    assert_eq!(version, 0);
    let refund =
        w.db.resolve_order_refund(
            &w.owner,
            id,
            RefundDecision::Approve,
            Some("approve old source"),
            &Uuid::new_v4().to_string(),
        )
        .await
        .unwrap()
        .0;
    let mut body = event(&w, &order, &refund);
    body.refund_id = None;
    let payment_only =
        w.db.receive_merchant_event(&w.context, &body, &[21; 32])
            .await
            .unwrap();
    assert_eq!(payment_only.state, "accepted");
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(id)
        .execute(w.db.pool())
        .await
        .unwrap();
    let log = w.wallet.log_size().await.unwrap();
    let report =
        w.db.recover_nonplan_refund(
            &w.owner,
            &w.wallet,
            &recovery(id, payment_only.id),
            false,
            today(),
        )
        .await
        .unwrap();
    assert_eq!(report.status, "refused");
    assert!(report.refusal.unwrap().contains("payment-only receipt"));
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
    let exact = signed_receipt(&w, &order, &refund).await;
    let mut request = recovery(id, exact.id);
    let report =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    assert_eq!(report.status, "proposed");
    request.expected_revision = Some(report.source_revision);
    let report =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
            .await
            .unwrap();
    assert_eq!(report.effect.unwrap().wallet_minor, TOTAL);
    assert_eq!(w.wallet.available().await.unwrap(), 0);
}

#[tokio::test]
async fn unique_forward_payment_only_receipt_replays_its_retained_identity() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    let refund = reserve(&w, &order, Some(1_000_000), true).await;
    let mut body = event(&w, &order, &refund);
    body.refund_id = None;
    let receipt =
        w.db.receive_merchant_event(&w.context, &body, &[29; 32])
            .await
            .unwrap();
    apply(&w, &receipt).await;
    let next = reserve(&w, &order, Some(1_500_000), true).await;
    let log = w.wallet.log_size().await.unwrap();
    apply(&w, &receipt).await;
    assert_eq!(w.wallet.log_size().await.unwrap(), log);
    assert!(current(&w, next.refund_id).await.cash_effect.is_none());
    apply(&w, &signed_receipt(&w, &order, &next).await).await;
    assert_eq!(reported_cash(&w, None).await, TOTAL);
    assert_eq!(w.wallet.available().await.unwrap(), 0);
}

#[tokio::test]
async fn callback_claims_parent_rows_before_holding_the_source_or_append_lock() {
    use doubleentry::LedgerId;
    use doubleentry::storage::postgres::PostgresStore;
    for parent in ["business", "organization"] {
        let Some(w) = world().await else { return };
        let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
        let refund = reserve(&w, &order, None, true).await;
        let receipt = signed_receipt(&w, &order, &refund).await;
        let store = PostgresStore::<6>::new(
            w.db.pool().clone(),
            LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
        )
        .in_schema(&format!("ledger_{}", w.wallet.tenant_id()));
        let mut pin = w.db.pool().begin().await.unwrap();
        if parent == "business" {
            sqlx::query("SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE")
                .fetch_one(&mut *pin)
                .await
                .unwrap();
        } else {
            sqlx::query("SELECT organization_id FROM oxsum.organizations WHERE organization_id=$1 FOR UPDATE").bind(w.org).fetch_one(&mut *pin).await.unwrap();
        }
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *pin)
            .await
            .unwrap();
        let db = w.db.clone();
        let wallet = w.wallet.clone();
        let callback =
            tokio::spawn(async move { db.apply_refund_event(&wallet, &receipt, today()).await });
        tokio::time::timeout(std::time::Duration::from_secs(10),async {
            loop {
                let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)))").bind(pid).fetch_one(w.db.pool()).await.unwrap();
                if blocked {break;}
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("callback reaches the controlled parent lock");
        let source: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("oxsum.nonplan-return:{}", order.order_id))
                .fetch_one(&mut *pin)
                .await
                .unwrap();
        let append: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(store.append_lock())
            .fetch_one(&mut *pin)
            .await
            .unwrap();
        assert!(
            source && append,
            "waiting for {parent} must not hold source or append ownership"
        );
        pin.rollback().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), callback)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            current(&w, refund.refund_id)
                .await
                .cash_effect
                .unwrap()
                .wallet_minor,
            TOTAL
        );
    }
}

#[tokio::test]
async fn successful_fulfillment_releases_source_before_return_even_when_drop_cleanup_is_paused() {
    use sqlx::Connection;
    use sqlx::postgres::PgPoolOptions;
    use tokio::sync::Semaphore;

    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, false).await;
    let url = oxsum_testkit::suite_db_url("nonplan_refunds").unwrap();
    for replay in [false, true] {
        let cleanup = Arc::new(Semaphore::new(0));
        let paused_cleanup = cleanup.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_release(move |connection, _| {
                let paused_cleanup = paused_cleanup.clone();
                Box::pin(async move {
                    // SQLx queues Drop's ROLLBACK before returning this connection.
                    // Freeze that cleanup before it can flush to PostgreSQL.
                    if connection.should_flush() {
                        paused_cleanup.acquire().await.unwrap().forget();
                    }
                    Ok(true)
                })
            })
            .connect(&url)
            .await
            .unwrap();
        let db = Db::from_pool(pool.clone());
        db.fulfill_payment_order(order.order_id, &w.wallet, today())
            .await
            .unwrap();
        let mut observer = w.db.pool().begin().await.unwrap();
        let available: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("oxsum.nonplan-return:{}", order.order_id))
                .fetch_one(&mut *observer)
                .await
                .unwrap();
        observer.rollback().await.unwrap();
        cleanup.add_permits(4);
        pool.close().await;
        assert!(
            available,
            "source must be unlocked on successful return; replay={replay}"
        );
    }
    let refund = reserve(&w, &order, None, true).await;
    assert_eq!(refund.state, "provider_pending");
    assert_eq!(w.wallet.available().await.unwrap(), TOTAL);
}

/// A retained historical correction must compare against its original Cash day
/// and its separate restitution entry, without inventing a second refund.
#[tokio::test]
async fn independent_reconciliation_observes_historical_cash_and_wallet_restitution() {
    let Some(w) = world().await else { return };
    let order = paid(&w, PaymentOrderPurpose::WalletTopup, true).await;
    w.wallet
        .hold("reconcile-spent", "", TOTAL, today())
        .await
        .unwrap();
    w.wallet
        .settle("reconcile-spent", "", TOTAL, today())
        .await
        .unwrap();
    w.wallet
        .top_up("reconcile-replacement", 4_000_000, today())
        .await
        .unwrap();
    let refund = reserve(&w, &order, None, true).await;
    let receipt = signed_receipt(&w, &order, &refund).await;
    let old_day = today() - Duration::days(1);
    old_cash_on(&w, refund.refund_id, TOTAL, old_day).await;
    let entry = *oxsum_core::entry_id_for(&format!("order-refund/{}", refund.refund_id)).as_uuid();
    sqlx::query("UPDATE oxsum.refunds SET state='succeeded' WHERE refund_id=$1")
        .bind(refund.refund_id)
        .execute(w.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.credit_notes SET state='applied',entry_id=$2 WHERE refund_id=$1")
        .bind(refund.refund_id)
        .bind(entry)
        .execute(w.db.pool())
        .await
        .unwrap();
    let mut request = recovery(refund.refund_id, receipt.id);
    let preview =
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, false, today())
            .await
            .unwrap();
    request.expected_revision = Some(preview.source_revision);
    assert_eq!(
        w.db.recover_nonplan_refund(&w.owner, &w.wallet, &request, true, today())
            .await
            .unwrap()
            .status,
        "applied"
    );
    let run =
        w.db.run_reconciliation("manual", old_day, old_day, None)
            .await
            .unwrap();
    let run =
        w.db.reconciliation_run(&w.owner, run.run_id)
            .await
            .unwrap()
            .unwrap();
    let snapshot =
        w.db.reconciliation_snapshot(&w.owner, run.run_id, run.latest_snapshot_id.unwrap())
            .await
            .unwrap()
            .unwrap();
    let evidence = snapshot.evidence.unwrap();
    let differences: Vec<_> = evidence["differences"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["organizationId"] == serde_json::json!(w.org))
        .collect();
    assert!(differences.is_empty(), "{differences:#?}");
    let source = evidence["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["sourceTable"] == "nonplan_refund_effects"
                && s["sourceId"] == serde_json::json!(refund.refund_id)
        })
        .unwrap();
    assert_eq!(source["expected"], source["actual"]);
    assert_eq!(source["actual"]["refundMinor"], TOTAL);
    assert_eq!(source["context"]["restoredWalletMinor"], TOTAL);
}

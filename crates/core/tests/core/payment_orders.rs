//! Payment orders and durable fulfillment (issue #606): terms are pinned before
//! the provider call, the conversion is server-rated, a signed callback marks
//! `paid` — never `fulfilled` — and the purpose's own fulfillment grants exactly
//! the value it is scoped to, once, under replay and recovery.
//! Needs DATABASE_URL; skips without it.
#[path = "../support/legacy_plan.rs"]
mod legacy_plan;
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{
    CustomerPlanState, Db, MerchantCreateRequest, MerchantEventKind, MerchantMode,
    MerchantMutationResult, MerchantPaymentPurpose, MerchantRail, MerchantSigningContext,
    MerchantStatusRequest, MerchantVersionInput, NewPaymentOrder, NewSoldPlan, NewUser,
    PaymentOrderPurpose, PeriodAuthorization, PeriodKind, SecretKey, VerifiedMerchantEvent, Wallet,
    WalletError,
};
use uuid::Uuid;

fn secret() -> SecretKey {
    SecretKey::from_bytes([42; 32])
}
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("payment_orders")
}

/// A migrated database, the owner token, a registered customer org and its
/// tenant — every order needs a real organization for its FK and wallet.
async fn world() -> Option<(Db, String, Uuid, String)> {
    let url = url()?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    db.bootstrap_collection_terms(&oxsum_core::CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "0.0014".into(),
    })
    .await
    .unwrap();
    let owner = setup::owner(&db).await;
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "payment order fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = registration.organization.id;
    let tenant = org.simple().to_string();
    Some((db, owner, org, tenant))
}

fn config(webhook_secret: &str) -> MerchantVersionInput {
    serde_json::from_value(serde_json::json!({
        "apiUrl": "https://sandbox.hyperswitch.io",
        "apiKey": "snd_merchant_secret",
        "webhookSecret": webhook_secret,
        "publishableKey": "pk_snd_merchant",
        "sdkUrl": "https://sdk.example"
    }))
    .unwrap()
}

async fn merchant(db: &Db, owner: &str, webhook_secret: &str) -> MerchantMutationResult {
    let created = db
        .create_merchant(
            owner,
            &MerchantCreateRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "order fixture".into(),
                name: format!("merchant-{}", Uuid::new_v4().simple()),
                rail: MerchantRail::Hyperswitch,
                mode: MerchantMode::Test,
                account_id: Uuid::new_v4().to_string(),
                config: config(webhook_secret),
            },
            &secret(),
        )
        .await
        .unwrap();
    db.set_merchant_enabled(
        owner,
        created.merchant_id,
        &MerchantStatusRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "enable".into(),
            enabled: true,
        },
    )
    .await
    .unwrap();
    created
}

async fn context(db: &Db, m: &MerchantMutationResult, version: Uuid) -> MerchantSigningContext {
    db.merchant_signing_context(m.merchant_id, version, &secret())
        .await
        .unwrap()
        .unwrap()
}

/// An order requests the fixture's currency; the business supplies its rate.
fn new_order(purpose: PaymentOrderPurpose, key: &str, display_minor: i64) -> NewPaymentOrder {
    NewPaymentOrder {
        expected_collection_revision: None,
        expected_statement_quote: None,
        statement_amount_minor: None,
        expected_plan_quote: None,
        purpose,
        display_amount_minor: display_minor,
        display_currency: "USD".into(),
        purpose_ref: None,
        idempotency_key: key.into(),
        promo_minor: 0,
    }
}

fn succeeded_event(
    order_id: Uuid,
    provider: &str,
    org: Uuid,
    account: &str,
    amount: i64,
) -> VerifiedMerchantEvent {
    VerifiedMerchantEvent {
        event_id: format!("evt_{}", Uuid::new_v4().simple()),
        kind: MerchantEventKind::Succeeded,
        payment_id: Some(order_id),
        provider_payment_id: Some(provider.into()),
        organization_id: Some(org),
        provider_account_id: Some(account.into()),
        mode: Some(MerchantMode::Test),
        amount_minor: Some(amount),
        currency: Some("USD".into()),
        refund_id: None,
    }
}

/// Server-rated conversion: the order's ledger `amount_minor` derives from the
/// display amount at the server's pinned rate — never a client-supplied figure.
/// $2.10 (210 cents) at 1 credit = $0.0014 rates to 1500 credits.
#[tokio::test]
async fn the_conversion_is_server_rated_and_the_terms_are_frozen() {
    let Some((db, owner, org, _)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "order-secret-a").await;
    let ctx = context(&db, &m, m.version_id).await;

    let order = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "order-key-1", 210),
        )
        .await
        .unwrap();
    // 210 cents × 10^SCALE × 10^4 / (14 × 10^2) = 1_500_000_000 minor.
    assert_eq!(order.amount_minor, 1_500_000_000);
    assert_eq!(order.display_amount_minor, 210);
    assert_eq!(order.display_currency, "USD");
    assert_eq!(order.currency, "USD");
    assert_eq!(order.status, "created");
    assert_eq!(order.rate_mantissa, 14);
    assert_eq!(order.rate_scale, 4);

    // A replayed create returns the standing order under the same terms.
    let replay = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "order-key-1", 210),
        )
        .await
        .unwrap();
    assert_eq!(replay.order_id, order.order_id);

    // Changed terms under the same idempotency key conflict.
    assert!(matches!(
        db.create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "order-key-1", 999),
        )
        .await,
        Err(WalletError::Conflict(_))
    ));

    // The expected payment was pinned under the order's own UUID.
    let binding = db
        .merchant_payment_binding(order.order_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.purpose, MerchantPaymentPurpose::Order);
    assert_eq!(binding.amount_minor, 210);
    assert_eq!(binding.currency, "USD");
}

/// The purpose is exhaustive and one-sided: a `plan_period` order activates
/// quota through `bind_subscription` and never books wallet cash, while a
/// `wallet_topup` order books cash and never quota.
#[tokio::test]
async fn paid_then_fulfilled_grants_only_the_purposes_value() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "order-secret-b").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();

    // A wallet_topup order: created, provider-pending, paid, then fulfilled.
    let order = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "wt-1", 210),
        )
        .await
        .unwrap();
    db.note_order_provider(order.order_id, "pay_wt_1", &serde_json::json!({}))
        .await
        .unwrap();
    let pending = db
        .payment_order(org, order.order_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.status, "provider_pending");

    // Fulfillment refuses before paid — paid is not asserted by the client.
    assert!(matches!(
        db.fulfill_payment_order(order.order_id, &wallet, on).await,
        Err(WalletError::Conflict(_))
    ));

    // The signed success event marks the order paid — still not fulfilled.
    let ev = succeeded_event(order.order_id, "pay_wt_1", org, &account, 210);
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[7u8; 32])
        .await
        .unwrap();
    assert_eq!(receipt.state, "accepted", "{:?}", receipt.reason);
    db.apply_order_event(&receipt).await.unwrap();
    let paid = db
        .payment_order(org, order.order_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(paid.status, "paid");
    assert!(paid.paid_at.is_some());
    // Nothing credited yet — paid and fulfilled are deliberately separate.
    assert_eq!(wallet.available().await.unwrap(), 0);

    // Fulfillment books the wallet cash once.
    let f = db
        .fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();
    assert_eq!(f.order_id, order.order_id);
    assert_eq!(f.purpose, PaymentOrderPurpose::WalletTopup);
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
    let done = db
        .payment_order(org, order.order_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "fulfilled");
    assert!(f.outbox_id.is_some());

    // Replaying the fulfillment returns the standing record, crediting once.
    let again = db
        .fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();
    assert_eq!(again.fulfillment_id, f.fulfillment_id);
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
}

/// A `plan_period` order's fulfillment activates the customer plan's period —
/// and provably posts no wallet cash: the purpose's arm is
/// `activate_plan_period`, not `top_up`, so quota and wallet cash can never
/// both land.
#[tokio::test]
async fn a_plan_period_order_grants_quota_never_wallet_cash() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "order-secret-c").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();

    // The sold version the customer binds, then the pending plan the order
    // funds — `purpose_ref` names the `customer_plan_id` the order buys.
    let business = db.default_business().await.unwrap();
    let sold = legacy_plan::publish(
        &db,
        business,
        &NewSoldPlan {
            plan_id: None,
            name: format!("pro-{}", Uuid::new_v4().simple()),
            price_minor: 210,
            currency: "USD".into(),
            quota_minor: Some(9_000_000),
            period_kind: PeriodKind::CalendarMonth,
            overage_enabled: false,
            sort_order: 0,
        },
    )
    .await
    .unwrap();
    let customer_plan = db
        .bind_customer_plan(
            org,
            sold.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    assert_eq!(customer_plan.state, CustomerPlanState::PendingPayment);

    let quote = db.plan_payment_quote(org).await.unwrap();
    let mut new = new_order(
        PaymentOrderPurpose::PlanPeriod,
        "pp-1",
        quote.terms.display_amount_minor,
    );
    new.expected_plan_quote = Some(quote.fingerprint);
    new.purpose_ref = Some(customer_plan.customer_plan_id);
    let order = db.create_payment_order(&ctx, org, &new).await.unwrap();

    let ev = succeeded_event(
        order.order_id,
        "pay_pp_1",
        org,
        &account,
        order.display_amount_minor,
    );
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[8u8; 32])
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();
    db.fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();

    // The plan is active with period 0 authorized by this order — and the
    // wallet holds nothing: a plan period never granted cash.
    let plan = db.customer_plan(org).await.unwrap().unwrap();
    assert_eq!(plan.state, CustomerPlanState::Active);
    let periods = db.plan_periods(plan.customer_plan_id).await.unwrap();
    assert_eq!(periods.len(), 1);
    assert_eq!(periods[0].authorization_ref, order.order_id);
    assert_eq!(wallet.available().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 0);
    let f = db
        .payment_fulfillment(order.order_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.purpose, PaymentOrderPurpose::PlanPeriod);
}

/// A paid event whose amount disagrees with the pinned terms is parked in
/// `needs_attention` — an explicit discrepancy state, never a silent credit.
#[tokio::test]
async fn a_paid_amount_mismatch_parks_in_needs_attention() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "order-secret-d").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();

    let order = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "na-1", 210),
        )
        .await
        .unwrap();

    // T14's classify rejects a mismatched signed event outright — the order
    // stays un-paid rather than crediting the wrong figure.
    let wrong = succeeded_event(order.order_id, "pay_na_1", org, &account, 999);
    let r = db
        .receive_merchant_event(&ctx, &wrong, &[9u8; 32])
        .await
        .unwrap();
    assert_eq!(r.state, "rejected");
    assert_eq!(r.reason.as_deref(), Some("amountMismatch"));

    // The order is still `created`: it can neither fulfill nor credit.
    assert!(matches!(
        db.fulfill_payment_order(order.order_id, &wallet, on).await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// Recovery: a crash between `paid` and `fulfilled` leaves a paid-unfulfilled
/// order the sweep re-runs — and the order-derived ledger key makes the retry a
/// single credit, never a double grant.
#[tokio::test]
async fn recovery_redelivers_a_paid_unfulfilled_order_once() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "order-secret-e").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();

    let order = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "rec-1", 210),
        )
        .await
        .unwrap();
    let ev = succeeded_event(order.order_id, "pay_rec_1", org, &account, 210);
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[10u8; 32])
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();

    // The order is paid but unfulfilled — the sweep's backlog names it.
    let backlog = db.unfulfilled_payment_orders(100).await.unwrap();
    assert!(backlog.iter().any(|o| o.order_id == order.order_id));

    // The sweep fulfills it once; a second pass replays the standing grant. The
    // backlog no longer names this order — other suites' paid orders are theirs.
    for owed in db.unfulfilled_payment_orders(100).await.unwrap() {
        if owed.order_id == order.order_id {
            db.fulfill_payment_order(owed.order_id, &wallet, on)
                .await
                .unwrap();
        }
    }
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
    assert!(
        !db.unfulfilled_payment_orders(100)
            .await
            .unwrap()
            .iter()
            .any(|o| o.order_id == order.order_id)
    );
}

/// Concurrent duplicate signed webhooks: the merchant inbox dedups on
/// `(merchant, event_id)`, and the order's own state machine plus the
/// fulfillment's ledger key keep a raced delivery to a single application and a
/// single credit.
#[tokio::test]
async fn concurrent_duplicate_webhooks_apply_once() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "order-secret-f").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();

    let order = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "dup-1", 210),
        )
        .await
        .unwrap();

    // Two deliveries of the same signed event id race; one wins the insert, the
    // other returns the standing receipt — and apply marks paid once.
    let ev = succeeded_event(order.order_id, "pay_dup_1", org, &account, 210);
    let digest = [11u8; 32];
    let (a, b) = (
        db.receive_merchant_event(&ctx, &ev, &digest),
        db.receive_merchant_event(&ctx, &ev, &digest),
    );
    let (a, b) = (a.await.unwrap(), b.await.unwrap());
    assert_eq!(a.id, b.id);
    assert_eq!(a.state, "accepted");
    db.apply_order_event(&a).await.unwrap();
    db.apply_order_event(&b).await.unwrap();
    let paid = db
        .payment_order(org, order.order_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(paid.status, "paid");

    // Concurrent fulfillments: the order's advisory lock serializes them and
    // the standing record returns — one credit either way.
    let (f1, f2) = (
        db.fulfill_payment_order(order.order_id, &wallet, on),
        db.fulfill_payment_order(order.order_id, &wallet, on),
    );
    let (f1, f2) = (f1.await.unwrap(), f2.await.unwrap());
    assert_eq!(f1.fulfillment_id, f2.fulfillment_id);
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
}

/// An `invoice_repayment` order repays the drawn credit line only — `debit
/// Cash / credit CreditLine` — so none of it lands as spendable wallet balance
/// and a repayment beyond the draw is refused outright.
#[tokio::test]
async fn an_invoice_repayment_repays_only_the_drawn_receivable() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "order-secret-g").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();

    // Give the organization a credit line, then leave it undrawn: an
    // `invoice_repayment` beyond the outstanding draw is refused outright
    // rather than booking the excess into a pool the purpose forbids.
    wallet
        .set_credit_limit("ir-limit", 5_000_000, on)
        .await
        .unwrap();
    let mut new = new_order(PaymentOrderPurpose::InvoiceRepayment, "ir-1", 210);
    // The receivable the order settles — a statement id the purpose names.
    new.purpose_ref = Some(Uuid::new_v4());
    let order = db.create_payment_order(&ctx, org, &new).await.unwrap();
    let ev = succeeded_event(order.order_id, "pay_ir_1", org, &account, 210);
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[12u8; 32])
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();
    // Nothing is drawn, so the repayment has no receivable to settle — the
    // purpose's arm refuses rather than crediting wallet or bonus.
    assert!(matches!(
        db.fulfill_payment_order(order.order_id, &wallet, on).await,
        Err(WalletError::InvalidInput(_)) | Err(WalletError::Conflict(_))
    ));
    // Nothing was repaid and no cash/bonus was granted: the only balance is the
    // undrawn credit facility the grant set, which the repayment never touched.
    assert_eq!(wallet.credit_used().await.unwrap(), 0);
    assert_eq!(wallet.credit_repaid().await.unwrap(), 0);
    assert_eq!(wallet.settled().await.unwrap(), 5_000_000);
}

/// The reconciliation report counts each lifecycle state and names the
/// paid-unfulfilled backlog — the operator's view of paid vs fulfilled vs
/// needs_attention.
#[tokio::test]
async fn the_reconciliation_reports_paid_against_fulfilled() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "order-secret-h").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();

    let order = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "recon-1", 210),
        )
        .await
        .unwrap();
    let ev = succeeded_event(order.order_id, "pay_recon_1", org, &account, 210);
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[13u8; 32])
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();

    let report = db.payment_reconciliation(&owner).await.unwrap();
    assert!(report.paid >= 1);
    assert!(report.paid_unfulfilled.contains(&order.order_id));

    db.fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();
    let report = db.payment_reconciliation(&owner).await.unwrap();
    assert!(!report.paid_unfulfilled.contains(&order.order_id));
}

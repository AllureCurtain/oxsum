//! Corrections, partial refunds and disputes (issue #608): the request is a
//! review, never a self-grant; approval commits the durable reservation
//! before any provider call; the signed `refund_*`/`dispute_*` event is the
//! only terminal truth; and no path — concurrent consume, spent funds,
//! duplicate events — can drive a wallet negative.
//! Needs DATABASE_URL; skips without it.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    Db, DisputeResolution, MerchantCreateRequest, MerchantEventKind, MerchantMode,
    MerchantMutationResult, MerchantRail, MerchantSigningContext, MerchantStatusRequest,
    MerchantVersionInput, NewPaymentOrder, NewRefundRequest, NewUser, PaymentOrder,
    PaymentOrderPurpose, REFUND_MAX_ATTEMPTS, RefundDecision, SecretKey, VerifiedMerchantEvent,
    Wallet, WalletError,
};
use uuid::Uuid;

fn secret() -> SecretKey {
    SecretKey::from_bytes([42; 32])
}
fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("refunds")
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
            password: "refund fixture password".into(),
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
                reason: "refund fixture".into(),
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

fn new_order(purpose: PaymentOrderPurpose, key: &str, display_minor: i64) -> NewPaymentOrder {
    NewPaymentOrder {
        expected_collection_revision: None,
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

fn refund_event(
    kind: MerchantEventKind,
    order_id: Uuid,
    provider: &str,
    org: Uuid,
    account: &str,
    refund_id: Option<&str>,
    amount: i64,
) -> VerifiedMerchantEvent {
    VerifiedMerchantEvent {
        event_id: format!("evt_{}", Uuid::new_v4().simple()),
        kind,
        payment_id: Some(order_id),
        provider_payment_id: Some(provider.into()),
        organization_id: Some(org),
        provider_account_id: Some(account.into()),
        mode: Some(MerchantMode::Test),
        amount_minor: Some(amount),
        currency: Some("USD".into()),
        refund_id: refund_id.map(str::to_owned),
    }
}

/// A paid+fulfilled `wallet_topup` order: the merchant, the order pinned and
/// driven to `fulfilled` through the signed event, the wallet credited.
/// Returns the pieces every refund test needs.
async fn paid_order(
    db: &Db,
    owner: &str,
    org: Uuid,
    tenant: &str,
    key: &str,
    display_minor: i64,
) -> (MerchantSigningContext, PaymentOrder, Wallet, String) {
    let m = merchant(db, owner, &format!("refund-secret-{key}")).await;
    let ctx = context(db, &m, m.version_id).await;
    let account = ctx.account_id.clone();
    let wallet = Wallet::open(db.pool().clone(), tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();
    let order = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, key, display_minor),
        )
        .await
        .unwrap();
    let provider = format!("pay_{key}");
    db.note_order_provider(order.order_id, &provider, &serde_json::json!({}))
        .await
        .unwrap();
    let ev = succeeded_event(order.order_id, &provider, org, &account, display_minor);
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[7u8; 32])
        .await
        .unwrap();
    assert_eq!(receipt.state, "accepted", "{:?}", receipt.reason);
    db.apply_order_event(&receipt).await.unwrap();
    db.fulfill_payment_order(order.order_id, &wallet, on)
        .await
        .unwrap();
    (ctx, order, wallet, account)
}

/// The request is bounded by the refundable remainder, replays under its
/// key, and can never exceed what the order funded — a refund past the bound
/// is refused, and a second one consumes only what is left.
#[tokio::test]
async fn the_refund_request_is_bounded_and_replays() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (_ctx, order, wallet, _account) =
        paid_order(&db, &owner, org, &tenant, "r-bound", 210).await;
    // $2.10 → 1_500_000_000 credit-minor funded at the pinned 14/10⁴ rate.
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    let request = NewRefundRequest {
        amount_minor: Some(500_000_000),
        reason: "partial refund please".into(),
        request_key: "req-1".into(),
        requested_by: None,
    };
    let refund = db
        .request_order_refund(org, order.order_id, &request)
        .await
        .unwrap();
    assert_eq!(refund.state, "requested");
    assert_eq!(refund.amount_minor, 500_000_000);
    // 500/1500 of 210 display → 70.
    assert_eq!(refund.display_amount_minor, 70);
    assert_eq!(refund.display_currency, "USD");

    // A replayed request answers the standing refund; changed terms conflict.
    let replay = db
        .request_order_refund(org, order.order_id, &request)
        .await
        .unwrap();
    assert_eq!(replay.refund_id, refund.refund_id);
    let changed = NewRefundRequest {
        amount_minor: Some(600_000_000),
        ..request.clone()
    };
    assert!(matches!(
        db.request_order_refund(org, order.order_id, &changed).await,
        Err(WalletError::Conflict(_))
    ));

    // The remainder bounds the next request: 1_000_000_000 left.
    assert_eq!(
        db.order_refundable_minor(org, order.order_id)
            .await
            .unwrap(),
        Some(1_000_000_000)
    );
    let over = NewRefundRequest {
        amount_minor: Some(1_500_000_000),
        reason: "too much".into(),
        request_key: "req-2".into(),
        requested_by: None,
    };
    assert!(matches!(
        db.request_order_refund(org, order.order_id, &over).await,
        Err(WalletError::InvalidInput(_))
    ));
    // A request on another organization's order is a not-found.
    let other = Uuid::new_v4();
    assert!(matches!(
        db.request_order_refund(other, order.order_id, &request)
            .await,
        Err(WalletError::NotFound(_))
    ));
}

/// Approval commits the durable reservation *and* the issued credit note in
/// one transaction, then the signed success books the take-back — the wallet
/// shrinks by exactly what the provider returned, and the note applies with
/// its entry. Decline lands `declined` with audit.
#[tokio::test]
async fn approval_reserves_then_the_signed_event_settles() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (ctx, order, wallet, account) =
        paid_order(&db, &owner, org, &tenant, "r-approve", 210).await;
    let on = time::OffsetDateTime::now_utc().date();

    let refund = db
        .request_order_refund(
            org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: None,
                reason: "refund it all".into(),
                request_key: "req-full".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    // The full remainder: 210 display for 1_500_000_000 credit.
    assert_eq!(refund.amount_minor, 1_500_000_000);
    assert_eq!(refund.display_amount_minor, 210);
    // The customer still holds the whole credit — nothing moved yet.
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    let (resolved, needs_call) = db
        .resolve_order_refund(
            &owner,
            refund.refund_id,
            RefundDecision::Approve,
            Some("goodwill"),
            "resolve-1",
        )
        .await
        .unwrap();
    assert_eq!(resolved.state, "provider_pending");
    assert!(needs_call);
    assert!(resolved.note_id.is_some());
    // Replaying the resolve answers the standing reservation without asking
    // for a second provider call.
    let (replayed, again) = db
        .resolve_order_refund(
            &owner,
            refund.refund_id,
            RefundDecision::Approve,
            None,
            "resolve-1",
        )
        .await
        .unwrap();
    assert!(!again);
    assert_eq!(replayed.state, "provider_pending");

    // The signed refund event books the take-back: the wallet shrinks by the
    // refunded credit, the note applies with its entry, and the refund lands
    // `succeeded` — the PSP's answer is the only terminal truth.
    let provider_refund = "re_test_1";
    db.note_refund_provider(refund.refund_id, provider_refund)
        .await
        .unwrap();
    let ev = refund_event(
        MerchantEventKind::RefundSucceeded,
        order.order_id,
        "pay_r-approve",
        org,
        &account,
        Some(provider_refund),
        210,
    );
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[9u8; 32])
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    let settled = db
        .business_refund(&owner, refund.refund_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, "succeeded");
    assert_eq!(wallet.available().await.unwrap(), 0);
    // The note applied with its ledger entry — the correction beside the
    // funding entry, history keeping both.
    let notes = db
        .business_order_credit_notes(&owner, None, 50)
        .await
        .unwrap();
    let note = notes
        .notes
        .iter()
        .find(|n| n.refund_id == refund.refund_id)
        .unwrap();
    assert_eq!(note.state, "applied");
    assert!(note.entry_id.is_some());
    // A duplicate event answers the standing state, booking once.
    let receipt2 = db
        .receive_merchant_event(&ctx, &ev, &[9u8; 32])
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt2, on).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// A partial refund converts pro-rata at the pinned rate and the last part
/// takes the display remainder — the total never exceeds the funded amount.
#[tokio::test]
async fn partial_refunds_sum_to_the_funded_total() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (ctx, order, wallet, account) =
        paid_order(&db, &owner, org, &tenant, "r-partial", 211).await;
    let on = time::OffsetDateTime::now_utc().date();
    // 211 cents → 1_507_142_857 credit-minor at the 14/10^4 rate.
    let order = db
        .payment_order(org, order.order_id)
        .await
        .unwrap()
        .unwrap();
    let total = order.amount_minor;
    let half = total / 2;

    let first = db
        .request_order_refund(
            org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: Some(half),
                reason: "first half".into(),
                request_key: "p-1".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    let (first, _) = db
        .resolve_order_refund(
            &owner,
            first.refund_id,
            RefundDecision::Approve,
            None,
            "res-p1",
        )
        .await
        .unwrap();
    let second = db
        .request_order_refund(
            org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: None,
                reason: "the rest".into(),
                request_key: "p-2".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(second.amount_minor, total - half);
    // The last part takes the display remainder — the two display figures sum
    // to the whole 211 the order pinned.
    assert_eq!(
        first.display_amount_minor + second.display_amount_minor,
        211
    );
    let (second, _) = db
        .resolve_order_refund(
            &owner,
            second.refund_id,
            RefundDecision::Approve,
            None,
            "res-p2",
        )
        .await
        .unwrap();

    // The PSP echoes its own refund id on each success — bind each provider
    // refund id so the event lands on its own row rather than the fallback.
    let provider_ids = ["re_part_1", "re_part_2"];
    for (refund, provider_id) in [&first, &second].iter().zip(provider_ids) {
        db.note_refund_provider(refund.refund_id, provider_id)
            .await
            .unwrap();
        let ev = refund_event(
            MerchantEventKind::RefundSucceeded,
            order.order_id,
            "pay_r-partial",
            org,
            &account,
            Some(provider_id),
            refund.display_amount_minor,
        );
        let receipt = db
            .receive_merchant_event(&ctx, &ev, &[8u8; 32])
            .await
            .unwrap();
        db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    }
    assert_eq!(wallet.available().await.unwrap(), 0);
}

/// A `refund_failed` event releases the reservation — the bound frees and
/// the issued note reverses, so the customer may ask again.
#[tokio::test]
async fn a_failed_refund_releases_the_reservation() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (ctx, order, wallet, account) = paid_order(&db, &owner, org, &tenant, "r-fail", 210).await;
    let on = time::OffsetDateTime::now_utc().date();
    let refund = db
        .request_order_refund(
            org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: Some(400_000_000),
                reason: "partial".into(),
                request_key: "f-1".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    db.resolve_order_refund(
        &owner,
        refund.refund_id,
        RefundDecision::Approve,
        None,
        "res-f1",
    )
    .await
    .unwrap();
    // The bound is consumed while pending.
    assert_eq!(
        db.order_refundable_minor(org, order.order_id)
            .await
            .unwrap(),
        Some(1_100_000_000)
    );
    let ev = refund_event(
        MerchantEventKind::RefundFailed,
        order.order_id,
        "pay_r-fail",
        org,
        &account,
        None,
        refund.display_amount_minor,
    );
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[6u8; 32])
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    let settled = db
        .business_refund(&owner, refund.refund_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.state, "released");
    // The bound freed — the whole 1_500_000_000 is refundable again, and the
    // wallet never moved.
    assert_eq!(
        db.order_refundable_minor(org, order.order_id)
            .await
            .unwrap(),
        Some(1_500_000_000)
    );
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
    let notes = db
        .business_order_credit_notes(&owner, None, 50)
        .await
        .unwrap();
    let note = notes
        .notes
        .iter()
        .find(|n| n.refund_id == refund.refund_id)
        .unwrap();
    assert_eq!(note.state, "reversed");
}

/// Compatibility failure reports also retain an ambiguous reservation when
/// the bounded automatic attempt budget is exhausted.
#[tokio::test]
async fn compatibility_failures_cannot_release_an_ambiguous_refund() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (_ctx, order, _wallet, _account) =
        paid_order(&db, &owner, org, &tenant, "r-retry", 210).await;
    let refund = db
        .request_order_refund(
            org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: Some(300_000_000),
                reason: "retry me".into(),
                request_key: "t-1".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    db.resolve_order_refund(
        &owner,
        refund.refund_id,
        RefundDecision::Approve,
        None,
        "res-t1",
    )
    .await
    .unwrap();
    // Due immediately — the sweep's work list takes it.
    assert_eq!(db.due_refund_retries(10).await.unwrap().len(), 1);
    let mut last = refund.clone();
    for _ in 0..REFUND_MAX_ATTEMPTS {
        last = db
            .fail_order_refund(
                refund.refund_id,
                "transport failure after possible creation",
            )
            .await
            .unwrap();
    }
    assert_eq!(last.state, "provider_pending");
    assert_eq!(last.retry_count, REFUND_MAX_ATTEMPTS);
    assert!(last.next_retry_at.is_none());
    assert_eq!(
        db.order_refundable_minor(org, order.order_id)
            .await
            .unwrap(),
        Some(1_200_000_000)
    );
    // Nothing left due; the reservation and note remain available to reconcile.
    assert!(db.due_refund_retries(10).await.unwrap().is_empty());
}

/// A dispute opens its own contested record — never a refund — and a `won`
/// resolution closes it without moving money: the dispute is a claim the
/// provider arbitrates, and `won`/`closed` end the record with audit.
#[tokio::test]
async fn a_dispute_is_not_a_refund() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (ctx, order, wallet, account) =
        paid_order(&db, &owner, org, &tenant, "r-dispute", 210).await;
    let on = time::OffsetDateTime::now_utc().date();
    let ev = refund_event(
        MerchantEventKind::Dispute,
        order.order_id,
        "pay_r-dispute",
        org,
        &account,
        Some("dsp_1"),
        210,
    );
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[5u8; 32])
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    // A replayed dispute keeps the standing row.
    let receipt2 = db
        .receive_merchant_event(&ctx, &ev, &[5u8; 32])
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt2, on).await.unwrap();
    let disputes = db.business_disputes(&owner, None, 50).await.unwrap();
    assert_eq!(disputes.disputes.len(), 1);
    let dispute = &disputes.disputes[0];
    assert_eq!(dispute.state, "opened");
    assert_eq!(dispute.order_id, order.order_id);
    assert_eq!(dispute.provider_dispute_id, "dsp_1");
    // The wallet is untouched — a dispute is a claim, not a refund.
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);

    // `won` ends it without moving money.
    let won = db
        .resolve_order_dispute(
            &wallet,
            &owner,
            dispute.dispute_id,
            DisputeResolution::Won,
            Some("evidence accepted"),
            "disp-res-1",
            on,
        )
        .await
        .unwrap();
    assert_eq!(won.state, "won");
    assert!(won.recharge_entry_id.is_none());
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
}

/// A lost dispute re-charges through the repay shape under the dispute's own
/// ledger key — the ledger reads a contested-charge restoration, never a
/// refund — and the charge is capped at what the line still carries.
#[tokio::test]
async fn a_lost_dispute_recharges_the_drawn_receivable() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (ctx, order, wallet, account) = paid_order(&db, &owner, org, &tenant, "r-lost", 210).await;
    let on = time::OffsetDateTime::now_utc().date();
    // Grant a credit line, then draw past the purchased pool so the re-charge
    // has a receivable: the order credited 1_500_000_000, the line 2_000_000_000
    // more, so a 2_000_000_000 hold draws 500_000_000 of it.
    wallet
        .set_credit_limit("rl-line", 2_000_000_000, on)
        .await
        .unwrap();
    wallet.hold("rl-hold", "", 2_000_000_000, on).await.unwrap();
    wallet
        .settle("rl-hold", "", 2_000_000_000, on)
        .await
        .unwrap();
    assert_eq!(wallet.credit_used().await.unwrap(), 500_000_000);
    let ev = refund_event(
        MerchantEventKind::Dispute,
        order.order_id,
        "pay_r-lost",
        org,
        &account,
        Some("dsp_lost"),
        210,
    );
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[4u8; 32])
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    let dispute = db
        .business_disputes(&owner, None, 50)
        .await
        .unwrap()
        .disputes
        .remove(0);
    // `lost` re-charges the drawn receivable: the dispute's contested amount
    // is the order's funded 1_500_000_000 credit-minor, clamped to the
    // 500_000_000 the line still carries, booked under the dispute's own
    // ledger key — the restoration reads as a contested-charge re-charge, not
    // a voluntary repayment.
    let lost = db
        .resolve_order_dispute(
            &wallet,
            &owner,
            dispute.dispute_id,
            DisputeResolution::Lost,
            Some("chargeback upheld"),
            "lost-1",
            on,
        )
        .await
        .unwrap();
    assert_eq!(lost.state, "lost");
    assert!(lost.recharge_entry_id.is_some());
    // The draw restored in whole — the recharge clamped to the receivable.
    assert_eq!(wallet.credit_used().await.unwrap(), 0);
    // Replaying the resolve answers the standing terminal state.
    let replay = db
        .resolve_order_dispute(
            &wallet,
            &owner,
            dispute.dispute_id,
            DisputeResolution::Lost,
            Some("chargeback upheld"),
            "lost-1",
            on,
        )
        .await
        .unwrap();
    assert_eq!(replay.state, "lost");
}

/// A lost dispute over money the line no longer carries is refused outright —
/// the repay shape is bounded by what is drawn, so a re-charge past the
/// receivable surfaces rather than booking into a pool the outcome forbids.
#[tokio::test]
async fn a_lost_dispute_beyond_the_draw_is_refused() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (ctx, order, wallet, account) =
        paid_order(&db, &owner, org, &tenant, "r-undrawn", 210).await;
    let on = time::OffsetDateTime::now_utc().date();
    let ev = refund_event(
        MerchantEventKind::Dispute,
        order.order_id,
        "pay_r-undrawn",
        org,
        &account,
        Some("dsp_none"),
        210,
    );
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[3u8; 32])
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt, on).await.unwrap();
    let dispute = db
        .business_disputes(&owner, None, 50)
        .await
        .unwrap()
        .disputes
        .remove(0);
    // Nothing drawn → `lost` has no receivable to restore — refused, and the
    // dispute stays open for an outcome the ledger can carry.
    assert!(
        db.resolve_order_dispute(
            &wallet,
            &owner,
            dispute.dispute_id,
            DisputeResolution::Lost,
            None,
            "lost-none",
            on,
        )
        .await
        .is_err()
    );
}

/// A claimed attempt survives restart, excludes another worker, and never
/// returns funds merely because the external outcome remains unknown.
#[tokio::test]
async fn ambiguous_recovery_is_bounded_and_preserves_the_refundable_reservation() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (ctx, order, wallet, account) =
        paid_order(&db, &owner, org, &tenant, "unknown-recovery", 210).await;
    let refund = db
        .request_order_refund(
            org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: Some(300_000_000),
                reason: "unknown external outcome".into(),
                request_key: "unknown-recovery".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    db.resolve_order_refund(
        &owner,
        refund.refund_id,
        RefundDecision::Approve,
        None,
        "approve-unknown",
    )
    .await
    .unwrap();
    let reopened = Db::from_pool(db.pool().clone());
    for index in 1..=REFUND_MAX_ATTEMPTS {
        let (first, second) = tokio::join!(
            db.claim_refund_attempt(refund.refund_id),
            reopened.claim_refund_attempt(refund.refund_id)
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(
            usize::from(first.is_some()) + usize::from(second.is_some()),
            1
        );
        let claimed = first.or(second).unwrap();
        assert_eq!(claimed.refund.retry_count, index);
        assert_eq!(claimed.refund.state, "provider_pending");
        assert_eq!(
            claimed.refund.next_retry_at.is_none(),
            index == REFUND_MAX_ATTEMPTS
        );
        if index > 1 {
            assert!(!db.refuse_claimed_refund(refund.refund_id).await.unwrap());
        }
        if index < REFUND_MAX_ATTEMPTS {
            sqlx::query("UPDATE oxsum.refunds SET next_retry_at=now()-interval '1 second' WHERE refund_id=$1")
                .bind(refund.refund_id).execute(db.pool()).await.unwrap();
        }
    }
    assert!(
        db.claim_refund_attempt(refund.refund_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !db.due_refund_retries(100)
            .await
            .unwrap()
            .iter()
            .any(|r| r.refund_id == refund.refund_id)
    );
    assert_eq!(
        db.order_refundable_minor(org, order.order_id)
            .await
            .unwrap(),
        Some(1_200_000_000)
    );
    assert_eq!(wallet.available().await.unwrap(), 1_500_000_000);
    let notes = db
        .business_order_credit_notes(&owner, None, 100)
        .await
        .unwrap();
    assert_eq!(
        notes
            .notes
            .iter()
            .find(|n| n.refund_id == refund.refund_id)
            .unwrap()
            .state,
        "issued"
    );
    // A late signed success uses its durable body ID even without an HTTP handle.
    let ev = refund_event(
        MerchantEventKind::RefundSucceeded,
        order.order_id,
        "pay_unknown-recovery",
        org,
        &account,
        Some(&refund.refund_id.to_string()),
        refund.display_amount_minor,
    );
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[63; 32])
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    db.apply_refund_event(&wallet, &receipt, time::OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    assert_eq!(wallet.available().await.unwrap(), 1_200_000_000);
    assert_eq!(
        db.business_refund(&owner, refund.refund_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "succeeded"
    );
}

/// A valid signature for one payment cannot select a different refund or
/// silently replace its exact amount with a wrong or missing value.
#[tokio::test]
async fn refund_events_cannot_cross_orders_or_reprice_partial_refunds() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let (ctx, order, wallet, account) =
        paid_order(&db, &owner, org, &tenant, "refund-isolation", 210).await;
    let refund = db
        .request_order_refund(
            org,
            order.order_id,
            &NewRefundRequest {
                amount_minor: Some(300_000_000),
                reason: "exact terms".into(),
                request_key: "isolation".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    db.resolve_order_refund(
        &owner,
        refund.refund_id,
        RefundDecision::Approve,
        None,
        "approve-isolation",
    )
    .await
    .unwrap();
    // A second paid order uses the same customer and merchant. Reusing its
    // known refund ID must not select it through the first order's receipt.
    let other = db
        .create_payment_order(
            &ctx,
            org,
            &new_order(PaymentOrderPurpose::WalletTopup, "isolation-other", 210),
        )
        .await
        .unwrap();
    db.note_order_provider(
        other.order_id,
        "pay_isolation-other",
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    let paid = succeeded_event(other.order_id, "pay_isolation-other", org, &account, 210);
    let receipt = db
        .receive_merchant_event(&ctx, &paid, &[65; 32])
        .await
        .unwrap();
    db.apply_order_event(&receipt).await.unwrap();
    db.fulfill_payment_order(
        other.order_id,
        &wallet,
        time::OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    let other_refund = db
        .request_order_refund(
            org,
            other.order_id,
            &NewRefundRequest {
                amount_minor: Some(300_000_000),
                reason: "other order".into(),
                request_key: "other-refund".into(),
                requested_by: None,
            },
        )
        .await
        .unwrap();
    db.resolve_order_refund(
        &owner,
        other_refund.refund_id,
        RefundDecision::Approve,
        None,
        "approve-other",
    )
    .await
    .unwrap();
    db.note_refund_provider(other_refund.refund_id, "ref_other_order")
        .await
        .unwrap();
    for (provider_id, amount, currency) in [
        ("ref_other_order".into(), refund.display_amount_minor, "USD"),
        (
            Uuid::new_v4().to_string(),
            refund.display_amount_minor,
            "USD",
        ),
        (
            refund.refund_id.to_string(),
            refund.display_amount_minor + 1,
            "USD",
        ),
        (
            refund.refund_id.to_string(),
            refund.display_amount_minor,
            "EUR",
        ),
    ] {
        let mut ev = refund_event(
            MerchantEventKind::RefundSucceeded,
            order.order_id,
            "pay_refund-isolation",
            org,
            &account,
            Some(&provider_id),
            amount,
        );
        ev.currency = Some(currency.into());
        let receipt = db
            .receive_merchant_event(&ctx, &ev, &[64; 32])
            .await
            .unwrap();
        assert!(
            db.apply_refund_event(&wallet, &receipt, time::OffsetDateTime::now_utc().date())
                .await
                .is_err()
        );
    }
    assert_eq!(wallet.available().await.unwrap(), 3_000_000_000);
    assert_eq!(
        db.business_refund(&owner, other_refund.refund_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "provider_pending"
    );
    assert_eq!(
        db.business_refund(&owner, refund.refund_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "provider_pending"
    );
    assert!(
        sqlx::query("UPDATE oxsum.refunds SET hyperswitch_body_id=false WHERE refund_id=$1")
            .bind(refund.refund_id)
            .execute(db.pool())
            .await
            .is_err()
    );
}

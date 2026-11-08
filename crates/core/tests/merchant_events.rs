//! Scoped durable merchant event intake (issue #649): the expected payment is
//! pinned before any provider call, a signed callback is classified inside the
//! merchant/version scope it was received under, and an event ID cannot be
//! replayed with different content. Needs DATABASE_URL; skips without it.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    Db, MerchantCreateRequest, MerchantEventKind, MerchantMode, MerchantMutationResult,
    MerchantPaymentBinding, MerchantPaymentPurpose, MerchantRail, MerchantSigningContext,
    MerchantStatusRequest, MerchantVersionInput, MerchantVersionRequest, NewUser, SecretKey,
    VerifiedMerchantEvent, Wallet, WalletError,
};
use uuid::Uuid;

fn secret() -> SecretKey {
    SecretKey::from_bytes([41; 32])
}

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("merchant_events")
}

/// A migrated database, the owner token and a registered customer org with its
/// tenant id — every binding needs a real organization for its FK and wallet.
async fn world() -> Option<(Db, String, Uuid, String)> {
    let url = url()?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "merchant event fixture password".into(),
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

/// An enabled Hyperswitch merchant pinned at revision 1 for `owner`.
async fn merchant(db: &Db, owner: &str, webhook_secret: &str) -> MerchantMutationResult {
    let created = db
        .create_merchant(
            owner,
            &MerchantCreateRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "event intake fixture".into(),
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

fn binding(
    payment_id: Uuid,
    m: &MerchantMutationResult,
    org: Uuid,
    amount: i64,
) -> MerchantPaymentBinding {
    MerchantPaymentBinding {
        payment_id,
        merchant_id: m.merchant_id,
        version_id: m.version_id,
        organization_id: org,
        amount_minor: amount,
        currency: "USD".into(),
        purpose: MerchantPaymentPurpose::Order,
        hyperswitch_body_id: true,
        provider_payment_id: None,
    }
}

fn event(
    payment_id: Uuid,
    provider: &str,
    org: Uuid,
    account: &str,
    amount: i64,
) -> VerifiedMerchantEvent {
    VerifiedMerchantEvent {
        event_id: format!("evt_{}", Uuid::new_v4().simple()),
        kind: MerchantEventKind::Succeeded,
        payment_id: Some(payment_id),
        provider_payment_id: Some(provider.into()),
        organization_id: Some(org),
        provider_account_id: Some(account.into()),
        mode: Some(MerchantMode::Test),
        amount_minor: Some(amount),
        currency: Some("USD".into()),
        refund_id: None,
    }
}

async fn context(db: &Db, m: &MerchantMutationResult, version: Uuid) -> MerchantSigningContext {
    db.merchant_signing_context(m.merchant_id, version, &secret())
        .await
        .unwrap()
        .unwrap()
}

/// A recognized, fully matching success event is accepted; replaying the same
/// delivery returns the same receipt instead of acting twice.
#[tokio::test]
async fn matching_event_accepts_and_replays_idempotently() {
    let Some((db, owner, org, _)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "signing-secret-a").await;
    let account = db
        .merchant_signing_context(m.merchant_id, m.version_id, &secret())
        .await
        .unwrap()
        .unwrap()
        .account_id;
    let ctx = context(&db, &m, m.version_id).await;

    let payment_id = Uuid::new_v4();
    db.pin_merchant_payment(&binding(payment_id, &m, org, 5000))
        .await
        .unwrap();
    // Re-pinning identical terms is a no-op recovery, not a conflict.
    db.pin_merchant_payment(&binding(payment_id, &m, org, 5000))
        .await
        .unwrap();
    // Changed terms under the same payment id conflict.
    assert!(matches!(
        db.pin_merchant_payment(&binding(payment_id, &m, org, 9999))
            .await,
        Err(WalletError::Conflict(_))
    ));

    let digest = [9u8; 32];
    let ev = event(payment_id, "pay_provider_1", org, &account, 5000);
    let receipt = db.receive_merchant_event(&ctx, &ev, &digest).await.unwrap();
    assert_eq!(receipt.state, "accepted", "{:?}", receipt.reason);
    assert_eq!(receipt.event_id, ev.event_id);
    assert_eq!(receipt.payment_id, Some(payment_id));

    // Same merchant + event id + same bytes: the stored receipt returns.
    let replay = db.receive_merchant_event(&ctx, &ev, &digest).await.unwrap();
    assert_eq!(replay.id, receipt.id);
    assert_eq!(replay.state, "accepted");

    // The provider reference is now bound and discoverable.
    let by_provider = db
        .merchant_payment_by_provider(m.merchant_id, "pay_provider_1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_provider.payment_id, payment_id);
}

/// Metadata-poor events wait for a stored reference, then recover only within
/// its exact signing scope. Contradictory supplied coordinates remain refused.
#[tokio::test]
async fn missing_metadata_recovers_only_from_the_pinned_merchant_version() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let first = merchant(&db, &owner, "correlation-first").await;
    let ctx = context(&db, &first, first.version_id).await;
    let payment = Uuid::new_v4();
    db.pin_merchant_payment(&binding(payment, &first, org, 5000))
        .await
        .unwrap();
    let mut ev = event(payment, "pay_correlation", org, &ctx.account_id, 1000);
    ev.kind = MerchantEventKind::RefundSucceeded;
    ev.refund_id = Some("refund_correlation".into());
    ev.payment_id = None;
    ev.organization_id = None;
    let waiting = db
        .receive_merchant_event(&ctx, &ev, &[51; 32])
        .await
        .unwrap();
    assert_eq!(waiting.state, "pending");
    assert_eq!(waiting.reason.as_deref(), Some("paymentReferencePending"));
    assert_eq!(waiting.payment_id, None);
    db.record_merchant_payment_reference(payment, "pay_correlation")
        .await
        .unwrap();
    let recovered = db.retry_merchant_event(waiting.id).await.unwrap();
    assert_eq!(recovered.state, "accepted");
    assert_eq!(recovered.payment_id, Some(payment));
    let stored = db.merchant_event_data(waiting.id).await.unwrap().unwrap();
    assert_eq!(stored.organization_id, Some(org));
    assert_eq!(stored.payment_id, Some(payment));
    let original: serde_json::Value =
        sqlx::query_scalar("SELECT normalized FROM oxsum.merchant_event_inbox WHERE receipt_id=$1")
            .bind(waiting.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(original["paymentId"].is_null());
    assert!(original["organizationId"].is_null());
    assert!(
        sqlx::query("UPDATE oxsum.merchant_event_inbox SET normalized='{}' WHERE receipt_id=$1")
            .bind(waiting.id)
            .execute(db.pool())
            .await
            .is_err()
    );
    let replay = db
        .receive_merchant_event(&ctx, &ev, &[51; 32])
        .await
        .unwrap();
    assert_eq!(replay.id, waiting.id);
    assert_eq!(replay.payment_id, Some(payment));
    assert!(matches!(
        db.receive_merchant_event(&ctx, &ev, &[52; 32]).await,
        Err(WalletError::Conflict(_))
    ));

    for (wrong_payment, wrong_customer, reason) in [
        (Some(Uuid::new_v4()), None, "providerPaymentMismatch"),
        (None, Some(Uuid::new_v4()), "customerMismatch"),
    ] {
        let mut altered = ev.clone();
        altered.event_id = Uuid::new_v4().to_string();
        altered.payment_id = wrong_payment;
        altered.organization_id = wrong_customer;
        let rejected = db
            .receive_merchant_event(&ctx, &altered, &[53; 32])
            .await
            .unwrap();
        assert_eq!(rejected.state, "rejected");
        assert_eq!(rejected.reason.as_deref(), Some(reason));
    }

    let rotated = db
        .rotate_merchant(
            &owner,
            first.merchant_id,
            &MerchantVersionRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "correlation rotation".into(),
                config: config("correlation-rotated"),
            },
            &secret(),
        )
        .await
        .unwrap();
    let rotated_ctx = context(&db, &first, rotated.version_id).await;
    let mut rotated_event = ev.clone();
    rotated_event.event_id = Uuid::new_v4().to_string();
    let rejected = db
        .receive_merchant_event(&rotated_ctx, &rotated_event, &[54; 32])
        .await
        .unwrap();
    assert_eq!(rejected.state, "rejected");
    assert_eq!(rejected.reason.as_deref(), Some("versionMismatch"));
    assert_eq!(rejected.payment_id, None);

    let second = merchant(&db, &owner, "correlation-second").await;
    let second_ctx = context(&db, &second, second.version_id).await;
    let mut other = ev.clone();
    other.event_id = Uuid::new_v4().to_string();
    other.provider_account_id = Some(second_ctx.account_id.clone());
    let pending = db
        .receive_merchant_event(&second_ctx, &other, &[55; 32])
        .await
        .unwrap();
    assert_eq!(pending.state, "pending");
    assert_eq!(pending.payment_id, None);
    let second_payment = Uuid::new_v4();
    db.pin_merchant_payment(&binding(second_payment, &second, org, 5000))
        .await
        .unwrap();
    db.record_merchant_payment_reference(second_payment, "pay_correlation")
        .await
        .unwrap();
    let own = db.retry_merchant_event(pending.id).await.unwrap();
    assert_eq!(own.state, "accepted");
    assert_eq!(own.payment_id, Some(second_payment));
    // Intake alone never creates a refund, note or ledger effect.
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    assert_eq!(wallet.available().await.unwrap(), 0);
    let refunds: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.refunds WHERE organization_id=$1")
            .bind(org)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(refunds, 0);
}

/// The same event ID delivered under a different digest is a conflict, never a
/// silent overwrite — the dedupe window is merchant-scoped across versions.
#[tokio::test]
async fn event_id_reuse_with_different_digest_conflicts() {
    let Some((db, owner, org, _)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "signing-secret-b").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let payment_id = Uuid::new_v4();
    db.pin_merchant_payment(&binding(payment_id, &m, org, 5000))
        .await
        .unwrap();
    let ev = event(payment_id, "pay_provider_2", org, &account, 5000);
    db.receive_merchant_event(&ctx, &ev, &[1u8; 32])
        .await
        .unwrap();
    // Same id, altered signed bytes: reject as a conflict.
    assert!(matches!(
        db.receive_merchant_event(&ctx, &ev, &[2u8; 32]).await,
        Err(WalletError::Conflict(_))
    ));
}

/// An event whose payment belongs to a different merchant or version is
/// rejected inside the scope it arrived under — identity cannot cross scopes.
#[tokio::test]
async fn cross_version_and_cross_merchant_scope_is_isolated() {
    let Some((db, owner, org, _)) = world().await else {
        return;
    };
    let first = merchant(&db, &owner, "scope-secret-1").await;
    let second = merchant(&db, &owner, "scope-secret-2").await;
    let first_account = context(&db, &first, first.version_id).await.account_id;
    let second_ctx = context(&db, &second, second.version_id).await;

    // A payment pinned to `first` cannot be claimed by an event arriving under
    // `second`'s scope — merchant mismatch rejects it.
    let payment_id = Uuid::new_v4();
    db.pin_merchant_payment(&binding(payment_id, &first, org, 5000))
        .await
        .unwrap();
    let ev = event(payment_id, "pay_provider_3", org, &first_account, 5000);
    let receipt = db
        .receive_merchant_event(&second_ctx, &ev, &[3u8; 32])
        .await
        .unwrap();
    assert_eq!(receipt.state, "rejected");
    assert_eq!(receipt.reason.as_deref(), Some("merchantMismatch"));

    // Pin a payment under `first`'s current version, then rotate. The payment
    // keeps the version it was pinned to; an event that arrives under the new
    // current version naming that payment rejects as a version mismatch.
    let payment_id2 = Uuid::new_v4();
    db.pin_merchant_payment(&binding(payment_id2, &first, org, 7000))
        .await
        .unwrap();
    let rotated = db
        .rotate_merchant(
            &owner,
            first.merchant_id,
            &MerchantVersionRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                reason: "rotation".into(),
                config: config("rotated-secret"),
            },
            &secret(),
        )
        .await
        .unwrap();
    assert_ne!(rotated.version_id, first.version_id);
    let rotated_ctx = context(&db, &first, rotated.version_id).await;
    let ev2 = event(payment_id2, "pay_provider_4", org, &first_account, 7000);
    let receipt2 = db
        .receive_merchant_event(&rotated_ctx, &ev2, &[4u8; 32])
        .await
        .unwrap();
    assert_eq!(receipt2.state, "rejected");
    assert_eq!(receipt2.reason.as_deref(), Some("versionMismatch"));
}

/// Terms that disagree with the pinned expectation — amount, currency,
/// customer, mode — are rejected; an unknown payment stays pending for retry.
#[tokio::test]
async fn mismatched_terms_reject_and_unknown_payment_stays_pending() {
    let Some((db, owner, org, _)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "terms-secret").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;

    // Unknown payment: not an error, retained pending for recovery.
    let unknown = event(Uuid::new_v4(), "pay_provider_5", org, &account, 5000);
    let pending = db
        .receive_merchant_event(&ctx, &unknown, &[5u8; 32])
        .await
        .unwrap();
    assert_eq!(pending.state, "pending");
    assert_eq!(pending.reason.as_deref(), Some("unknownPayment"));

    // Amount mismatch against a known payment rejects.
    let payment_id = Uuid::new_v4();
    db.pin_merchant_payment(&binding(payment_id, &m, org, 5000))
        .await
        .unwrap();
    let wrong_amount = event(payment_id, "pay_provider_6", org, &account, 9999);
    let r = db
        .receive_merchant_event(&ctx, &wrong_amount, &[6u8; 32])
        .await
        .unwrap();
    assert_eq!(r.state, "rejected");
    assert_eq!(r.reason.as_deref(), Some("amountMismatch"));

    // Customer mismatch rejects.
    let mut wrong_customer = event(payment_id, "pay_provider_7", org, &account, 5000);
    wrong_customer.organization_id = Some(Uuid::new_v4());
    let r = db
        .receive_merchant_event(&ctx, &wrong_customer, &[7u8; 32])
        .await
        .unwrap();
    assert_eq!(r.state, "rejected");
    assert_eq!(r.reason.as_deref(), Some("customerMismatch"));

    // Provider account mismatch rejects.
    let mut wrong_account = event(payment_id, "pay_provider_8", org, &account, 5000);
    wrong_account.provider_account_id = Some("other-account".into());
    let r = db
        .receive_merchant_event(&ctx, &wrong_account, &[8u8; 32])
        .await
        .unwrap();
    assert_eq!(r.state, "rejected");
    assert_eq!(r.reason.as_deref(), Some("merchantMismatch"));

    // Retry of the still-pending unknown payment now finds it bound — but to a
    // different payment id, so it remains pending, not silently applied.
    let recovered = db.retry_merchant_event(pending.id).await.unwrap();
    assert_eq!(recovered.state, "pending");
}

/// The legacy adapter only credits a validated success event whose terms match
/// the pinned deposit exactly — the credit key is merchant-scoped and the
/// wallet is topped up exactly once under replay.
#[tokio::test]
async fn legacy_deposit_credits_once_under_merchant_scope() {
    let Some((db, owner, org, tenant)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "legacy-secret").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let on = time::OffsetDateTime::now_utc().date();

    // The checkout path binds the deposit and pins expected terms in one commit.
    let checkout = db
        .begin_merchant_deposit(&ctx, org, "checkout-key-1", 8400)
        .await
        .unwrap();
    let payment_id = checkout.payment.payment_id;
    // Replaying the same idempotency key returns the same binding.
    let replay = db
        .begin_merchant_deposit(&ctx, org, "checkout-key-1", 8400)
        .await
        .unwrap();
    assert_eq!(replay.payment.payment_id, payment_id);
    // A different amount under the same key conflicts.
    assert!(matches!(
        db.begin_merchant_deposit(&ctx, org, "checkout-key-1", 100)
            .await,
        Err(WalletError::Conflict(_))
    ));

    // The signed success event matches the pinned deposit and is accepted.
    let ev = event(payment_id, "pay_legacy_1", org, &account, 8400);
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[11u8; 32])
        .await
        .unwrap();
    assert_eq!(receipt.state, "accepted", "{:?}", receipt.reason);

    // Credit is purpose-gated and exact; it lands once.
    db.apply_legacy_merchant_event(&wallet, &receipt, on)
        .await
        .unwrap();
    let status: String =
        sqlx::query_scalar("SELECT status FROM oxsum.deposits WHERE deposit_id=$1")
            .bind(payment_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(status, "credited");
    // The event is marked applied so the recovery sweep does not revisit it.
    let state: String =
        sqlx::query_scalar("SELECT state FROM oxsum.merchant_event_inbox WHERE receipt_id=$1")
            .bind(receipt.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(state, "applied");

    // Re-applying does not double-credit: the deposit is already credited.
    db.apply_legacy_merchant_event(&wallet, &receipt, on)
        .await
        .unwrap();
    let credited: i64 =
        sqlx::query_scalar("SELECT received_minor FROM oxsum.deposits WHERE deposit_id=$1")
            .bind(payment_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(credited, 8400);
}

/// A payment bound before the provider reference exists still accepts the
/// event once the reference is recorded — the early callback wins the race the
/// synchronous response later confirms.
#[tokio::test]
async fn binding_committed_before_provider_reference_accepts_later_event() {
    let Some((db, owner, org, _)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "race-secret").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let payment_id = Uuid::new_v4();
    db.pin_merchant_payment(&binding(payment_id, &m, org, 6100))
        .await
        .unwrap();

    // The signed callback arrives before the checkout's own reference write.
    let ev = event(payment_id, "pay_provider_9", org, &account, 6100);
    let receipt = db
        .receive_merchant_event(&ctx, &ev, &[12u8; 32])
        .await
        .unwrap();
    // It establishes the provider reference and accepts in one step.
    assert_eq!(receipt.state, "accepted", "{:?}", receipt.reason);
    let bound = db
        .merchant_payment_binding(payment_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bound.provider_payment_id.as_deref(), Some("pay_provider_9"));

    // A later event reusing that provider reference for another payment is
    // refused: one provider payment cannot fund two orders.
    let other_payment = Uuid::new_v4();
    db.pin_merchant_payment(&binding(other_payment, &m, org, 6100))
        .await
        .unwrap();
    let ev2 = event(other_payment, "pay_provider_9", org, &account, 6100);
    let r2 = db
        .receive_merchant_event(&ctx, &ev2, &[13u8; 32])
        .await
        .unwrap();
    assert_eq!(r2.state, "rejected");
}

/// `business_merchant_events` requires the reports capability and returns the
/// redacted intake surface — no raw payload or secret crosses the read path.
#[tokio::test]
async fn business_event_page_is_scoped_and_redacted() {
    let Some((db, owner, org, _)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "page-secret").await;
    let account = context(&db, &m, m.version_id).await.account_id;
    let ctx = context(&db, &m, m.version_id).await;
    let payment_id = Uuid::new_v4();
    db.pin_merchant_payment(&binding(payment_id, &m, org, 2200))
        .await
        .unwrap();
    let ev = event(payment_id, "pay_provider_10", org, &account, 2200);
    db.receive_merchant_event(&ctx, &ev, &[14u8; 32])
        .await
        .unwrap();

    let page = db
        .business_merchant_events(&owner, m.merchant_id, None, 50)
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
    let serialized = serde_json::to_string(&page.events[0]).unwrap();
    // The read surface carries no signing secret or raw provider body.
    for sensitive in ["page-secret", "snd_merchant_secret", "pay_provider_10"] {
        assert!(!serialized.contains(sensitive), "leaked {sensitive}");
    }
    // Unknown merchant in this business is a not-found, not an empty page.
    assert!(matches!(
        db.business_merchant_events(&owner, Uuid::new_v4(), None, 50)
            .await,
        Err(WalletError::NotFound(_))
    ));
    // The row the page returned is the receipt's durable identity.
    let stored: Uuid =
        sqlx::query_scalar("SELECT receipt_id FROM oxsum.merchant_event_inbox LIMIT 1")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(page.events[0].id, stored);
}

/// Rows inserted by an old writer without the new identity column stay fenced;
/// unresolved early events rotate behind untried work in the bounded sweep.
#[tokio::test]
async fn historical_defaults_and_recovery_paging_preserve_unknown_work() {
    let Some((db, owner, org, _)) = world().await else {
        return;
    };
    let m = merchant(&db, &owner, "recovery-paging").await;
    let context = context(&db, &m, m.version_id).await;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO oxsum.merchant_payment_bindings(payment_id,merchant_id,version_id,organization_id,amount_minor,currency,purpose) VALUES($1,$2,$3,$4,100,'USD','order')")
        .bind(id).bind(m.merchant_id).bind(m.version_id).bind(org).execute(db.pool()).await.unwrap();
    assert!(
        !db.merchant_payment_binding(id)
            .await
            .unwrap()
            .unwrap()
            .hyperswitch_body_id
    );
    let mut ids = Vec::new();
    for _ in 0..101 {
        let missing = Uuid::new_v4();
        let ev = event(
            missing,
            &format!("pay_{missing}"),
            org,
            &context.account_id,
            100,
        );
        let receipt = db
            .receive_merchant_event(&context, &ev, &[73; 32])
            .await
            .unwrap();
        assert_eq!(receipt.state, "pending");
        ids.push(receipt.id);
    }
    let first = db.recoverable_merchant_events(100).await.unwrap();
    assert!(!first.iter().any(|r| r.id == ids[100]));
    for receipt in first {
        db.retry_merchant_event(receipt.id).await.unwrap();
    }
    let next = db.recoverable_merchant_events(100).await.unwrap();
    assert!(next.iter().any(|r| r.id == ids[100]));
    let original = db.merchant_event_data(ids[0]).await.unwrap().unwrap();
    assert_eq!(original.kind, MerchantEventKind::Succeeded);
}

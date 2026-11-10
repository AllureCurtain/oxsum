//! Collection authority, exact conversion and history survive customer edits/retries.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{
    BusinessRole, BusinessStaffRequest, CollectionCurrency, CollectionTermsRequest, Db,
    DisplayCurrency, NewManualOrder, NewUser, PaymentOrderPurpose, SessionContext,
    SessionContextRequest, WalletError,
};
use uuid::Uuid;

fn currency(code: &str, rate: &str) -> CollectionCurrency {
    CollectionCurrency {
        code: code.into(),
        rate_per_credit: rate.into(),
    }
}
fn change(revision: i64, code: &str, rate: &str) -> CollectionTermsRequest {
    CollectionTermsRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "collection fixture".into(),
        expected_revision: revision,
        currency: currency(code, rate),
    }
}
fn receipt(org: Uuid, code: &str, revision: Option<i64>) -> NewManualOrder {
    NewManualOrder {
        organization: org,
        expected_collection_revision: revision,
        purpose: PaymentOrderPurpose::WalletTopup,
        display_amount_minor: 210,
        display_currency: code.into(),
        idempotency_key: Uuid::new_v4().to_string(),
        reference: Uuid::new_v4().to_string(),
        reason: "local fixture receipt".into(),
    }
}

#[tokio::test]
async fn operator_terms_are_authoritative_revisioned_and_preserve_payment_history() {
    let Some(url) = oxsum_testkit::suite_db_url("collection_terms") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let registration = db
        .register(NewUser {
            email: "collection-customer@example.test".into(),
            password: "collection fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = registration.organization.id;
    assert!(!db.collection_terms().await.unwrap().ready);
    assert!(matches!(
        db.create_manual_payment_order(&owner, &receipt(org, "USD", None))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let request = change(0, "USD", "0.0014");
    let first = db.set_collection_terms(&owner, &request).await.unwrap();
    assert_eq!(first.terms.revision, 1);
    assert!(first.terms.ready);
    assert_eq!(
        db.set_collection_terms(&owner, &request).await.unwrap().id,
        first.id
    );
    let mut mismatch = change(0, "EUR", "1");
    assert!(matches!(
        db.set_collection_terms(&owner, &mismatch).await,
        Err(WalletError::Conflict(_))
    ));
    mismatch.idempotency_key = request.idempotency_key.clone();
    assert!(matches!(
        db.set_collection_terms(&owner, &mismatch).await,
        Err(WalletError::Conflict(_))
    ));
    // Currency can change before any intent; two stale concurrent drafts cannot both win.
    let a = change(1, "USD", "1");
    let b = change(1, "USD", "2");
    let (left, right) = tokio::join!(
        db.set_collection_terms(&owner, &a),
        db.set_collection_terms(&owner, &b)
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    db.set_collection_terms(&owner, &change(2, "USD", "0.0014"))
        .await
        .unwrap();
    db.set_display_currency(
        org,
        Some(DisplayCurrency {
            code: "EUR".into(),
            rate_per_credit: "0.00000001".into(),
            decimals: 6,
        }),
    )
    .await
    .unwrap();
    let payment = receipt(org, "USD", Some(3));
    let order = db
        .create_manual_payment_order(&owner, &payment)
        .await
        .unwrap();
    assert_eq!(order.amount_minor, 1_500_000_000);
    assert_eq!(order.rate_mantissa, 14);
    assert_eq!(order.rate_scale, 4);
    let pinned = serde_json::to_value(&order).unwrap();
    assert_eq!(
        db.collection_terms().await.unwrap().historical_currencies,
        vec!["USD"]
    );
    assert!(matches!(
        db.set_collection_terms(&owner, &change(3, "EUR", "0.0014"))
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        db.create_manual_payment_order(&owner, &receipt(org, "EUR", None))
            .await,
        Err(WalletError::Conflict(_))
    ));
    db.set_collection_terms(&owner, &change(3, "USD", "1"))
        .await
        .unwrap();
    assert!(matches!(
        db.create_manual_payment_order(&owner, &receipt(org, "USD", Some(3)))
            .await,
        Err(WalletError::Conflict(_))
    ));
    let replay = db
        .create_manual_payment_order(&owner, &payment)
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(replay).unwrap(), pinned);
    assert_eq!(
        db.create_manual_payment_order(&owner, &receipt(org, "USD", Some(4)))
            .await
            .unwrap()
            .amount_minor,
        2_100_000
    );
    assert_eq!(
        db.set_collection_terms(&owner, &request)
            .await
            .unwrap()
            .terms
            .revision,
        1
    );
    db.bootstrap_collection_terms(&currency("EUR", "7.20"))
        .await
        .unwrap();
    assert_eq!(
        db.collection_terms().await.unwrap().revision,
        4,
        "environment restart must not overwrite saved terms"
    );
    assert!(
        sqlx::query("DELETE FROM oxsum.collection_term_revisions")
            .execute(db.pool())
            .await
            .is_err()
    );
    // Finance can record receipts and read terms, but cannot set their value.
    let finance = db
        .register(NewUser {
            email: "collection-finance@example.test".into(),
            password: "collection fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.set_business_staff(
        &owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            user_id: finance.user.id,
            role: BusinessRole::Finance,
            reason: "fixture finance".into(),
        },
    )
    .await
    .unwrap();
    let session = db
        .login(&finance.user.email, "collection fixture password")
        .await
        .unwrap()
        .into_session();
    db.switch_session_context(
        &session.token,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    assert!(
        db.business_collection_terms(&session.token)
            .await
            .unwrap()
            .ready
    );
    assert!(matches!(
        db.set_collection_terms(&session.token, &change(4, "USD", "0.01"))
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(
        db.create_manual_payment_order(&session.token, &receipt(org, "USD", Some(4)))
            .await
            .unwrap()
            .amount_minor,
        2_100_000
    );
    // A stale quote racing a rate update either commits its original revision,
    // or conflicts before creating any order. It can never take the new rate.
    let racing = receipt(org, "USD", Some(4));
    let update = change(4, "USD", "2");
    let (payment_result, changed) = tokio::join!(
        db.create_manual_payment_order(&owner, &racing),
        db.set_collection_terms(&owner, &update)
    );
    assert_eq!(changed.unwrap().terms.revision, 5);
    match payment_result {
        Ok(order) => assert_eq!(order.amount_minor, 2_100_000),
        Err(WalletError::Conflict(_)) => {}
        other => panic!("unexpected race result: {other:?}"),
    }
    db.set_collection_terms(&owner, &change(5, "USD", "9223372036854775807"))
        .await
        .unwrap();
    assert!(
        matches!(
            db.create_manual_payment_order(&owner, &receipt(org, "USD", Some(6)))
                .await,
            Err(WalletError::InvalidInput(_))
        ),
        "A positive fiat amount that rounds to zero credits must fail before insertion"
    );
}

#[test]
fn collection_decimal_boundaries_are_exact_and_fail_closed() {
    assert_eq!(
        currency("USD", "0.0014")
            .display()
            .unwrap()
            .to_credit_minor(210),
        Some(1_500_000_000)
    );
    assert_eq!(
        currency("USD", "3").display().unwrap().to_credit_minor(1),
        Some(3333)
    );
    for rate in [
        "0",
        "-1",
        "1e2",
        "1.",
        " 1",
        "1.000000001",
        "9223372036854775808",
    ] {
        assert!(currency("USD", rate).display().is_err(), "{rate}");
    }
    assert!(currency("usd", "1").display().is_err());
    assert!(
        currency("USD", "0.00000001")
            .display()
            .unwrap()
            .to_credit_minor(i64::MAX)
            .is_none()
    );
}

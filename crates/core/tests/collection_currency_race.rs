//! Currency adoption and a first payment share one serialization boundary.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/integration.rs"]
mod setup;
use oxsum_core::{
    CollectionCurrency, CollectionTermsRequest, Db, NewManualOrder, NewUser, PaymentOrderPurpose,
    WalletError,
};
use uuid::Uuid;

#[tokio::test]
async fn first_payment_and_currency_change_cannot_commit_contradictory_history() {
    let Some(url) = oxsum_testkit::suite_db_url("collection_currency_race") else {
        return;
    };
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: "currency-race@example.test".into(),
            password: "isolated fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    db.bootstrap_collection_terms(&CollectionCurrency {
        code: "USD".into(),
        rate_per_credit: "1".into(),
    })
    .await
    .unwrap();
    let edit = CollectionTermsRequest {
        idempotency_key: Uuid::new_v4().to_string(),
        reason: "currency race fixture".into(),
        expected_revision: 1,
        currency: CollectionCurrency {
            code: "EUR".into(),
            rate_per_credit: "1".into(),
        },
    };
    let payment = NewManualOrder {
        organization: customer.organization.id,
        expected_collection_revision: Some(1),
        purpose: PaymentOrderPurpose::WalletTopup,
        display_amount_minor: 100,
        display_currency: "USD".into(),
        idempotency_key: Uuid::new_v4().to_string(),
        reference: Uuid::new_v4().to_string(),
        reason: "synthetic receipt".into(),
    };
    let (edit_result, payment_result) = tokio::join!(
        db.set_collection_terms(&owner, &edit),
        db.create_manual_payment_order(&owner, &payment)
    );
    match (edit_result, payment_result) {
        (Ok(_), Err(WalletError::Conflict(_))) => {
            let terms = db.collection_terms().await.unwrap();
            assert_eq!(terms.currency.unwrap().code, "EUR");
            assert!(terms.historical_currencies.is_empty());
        }
        (Err(WalletError::Conflict(_)), Ok(order)) => {
            assert_eq!(order.display_currency, "USD");
            let terms = db.collection_terms().await.unwrap();
            assert_eq!(terms.currency.unwrap().code, "USD");
            assert_eq!(terms.historical_currencies, vec!["USD"]);
        }
        other => panic!("Unexpected currency race result: {other:?}"),
    }
}

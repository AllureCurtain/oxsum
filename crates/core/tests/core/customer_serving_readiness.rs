//! Scoped customer sale/funding selection without implicit account or ledger writes.
#[path = "../support/integration.rs"]
mod authority;
#[path = "../support/ready_receipt.rs"]
mod setup;

use oxsum_core::{
    Db, KeyConstraints, Principal, ResetPeriod, Role, SaleChange, SaleChangeRequest, SalePrice,
    SaleTerms, SecretKey, WalletError,
};
use serde_json::json;
use uuid::Uuid;

async fn supply(f: &setup::Fixture, count: usize) -> Vec<String> {
    let channel = format!("ready-{}", Uuid::new_v4().simple());
    f.db.set_channel(
        &channel,
        "https://provider.example/v1",
        "fixture-secret",
        "openai",
        &SecretKey::from_bytes([6; 32]),
    )
    .await
    .unwrap();
    let models = (0..count)
        .map(|i| format!("{channel}-{i:02}"))
        .collect::<Vec<_>>();
    let price: oxsum_core::Price = serde_json::from_value(json!({"inputPricePerMillion": 9000000, "outputPricePerMillion": 9000000,"maxOutputTokens":32})).unwrap();
    for model in &models {
        f.db.append_price(&channel, model, price.clone(), 100)
            .await
            .unwrap();
    }
    // A per-customer tier makes the shared fixture's routes irrelevant to this test.
    f.db.set_tier(&channel, None, Some(models.clone()))
        .await
        .unwrap();
    f.db.set_organization_tier(f.registration.organization.id, Some(&channel))
        .await
        .unwrap();
    models
}
async fn sale(f: &setup::Fixture, model: &str, rate: i64, discount: i64) {
    let price: oxsum_core::Price = serde_json::from_value(
        json!({"inputPricePerMillion":rate,"outputPricePerMillion":rate,"maxOutputTokens":32}),
    )
    .unwrap();
    f.db.change_sale_terms(
        &authority::owner(&f.db).await,
        &SaleChangeRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "readiness test".into(),
            change: SaleChange::OverrideModel {
                organization_id: f.registration.organization.id,
                model: model.into(),
                terms: Box::new(SaleTerms {
                    price: SalePrice::try_new(price).unwrap(),
                    discount_percent: discount,
                }),
                effective_from: None,
                effective_until: None,
                replaces: None,
            },
        },
    )
    .await
    .unwrap();
}
async fn facts(f: &setup::Fixture) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_build_object('digests',(SELECT count(*) FROM oxsum.sale_snapshot_digests WHERE organization_id=$1),'draws',(SELECT count(*) FROM oxsum.subscription_draws d JOIN oxsum.org_subscriptions s USING(subscription_id) WHERE s.organization_id=$1),'operations',(SELECT count(*) FROM oxsum.request_operations WHERE organization_id=$1),'holds',(SELECT count(*) FROM oxsum.open_holds WHERE tenant_id=$2),'quota',(SELECT jsonb_agg(to_jsonb(s)) FROM oxsum.org_subscriptions s WHERE organization_id=$1))").bind(f.registration.organization.id).bind(&f.registration.organization.tenant_id).fetch_one(f.db.pool()).await.unwrap()
}
#[tokio::test]
async fn zero_sale_and_full_quota_are_ready_without_a_positive_wallet() {
    let Some(f) = setup::fixture("customer_serving_readiness").await else {
        return;
    };
    let names = supply(&f, 2).await;
    sale(&f, &names[0], 1000000, 100).await;
    sale(&f, &names[1], 0, 0).await;
    let wallet = f
        .tenants
        .get(&f.registration.organization.tenant_id)
        .await
        .unwrap();
    let before = facts(&f).await;
    let size = wallet.log_size().await.unwrap();
    let result = f.db.customer_serving_readiness(&f.principal).await.unwrap();
    assert!(result.funding_ready, "{result:?}");
    let sample = result.sample.unwrap();
    assert_eq!(sample.model, names[1]);
    assert_eq!((sample.bound_minor, sample.wallet_minor), (0, Some(0)));
    // Revise the permitted model to the charged sample. A 100% settlement
    // discount still reserves the undiscounted bound, matching the gateway.
    f.db.update_key_constraints_as(
        &f.principal,
        f.registration.api_key.key.id,
        KeyConstraints {
            model_allowlist: Some(vec![names[0].clone()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let result = f.db.customer_serving_readiness(&f.principal).await.unwrap();
    assert_eq!(result.reason, Some("insufficientFunds"));
    let bound = result.sample.unwrap().bound_minor;
    assert_eq!(bound, oxsum_core::input_upper_bound(&["Hello"]) + 32);
    let plan =
        f.db.set_subscription_plan(
            &Uuid::new_v4().to_string(),
            Some(bound),
            ResetPeriod::Monthly,
            false,
            None,
            None,
            true,
            0,
        )
        .await
        .unwrap();
    f.db.bind_subscription(f.registration.organization.id, plan.plan_id, "test", None)
        .await
        .unwrap();
    let quota_before = facts(&f).await;
    let result = f.db.customer_serving_readiness(&f.principal).await.unwrap();
    assert!(result.funding_ready, "{result:?}");
    let sample = result.sample.unwrap();
    assert_eq!(
        (sample.quota_minor, sample.wallet_minor),
        (Some(bound), Some(0))
    );
    assert_eq!(quota_before, facts(&f).await);
    assert_eq!(before["digests"], facts(&f).await["digests"]);
    assert_eq!(size, wallet.log_size().await.unwrap());
    assert_eq!(wallet.available().await.unwrap(), 0);
}
#[tokio::test]
async fn missing_ledger_and_missing_sale_never_bootstrap_or_use_supplier_price() {
    let Some(f) = setup::fixture("customer_serving_readiness").await else {
        return;
    };
    let name = supply(&f, 1).await.remove(0);
    assert_eq!(
        f.db.customer_serving_readiness(&f.principal)
            .await
            .unwrap()
            .reason,
        Some("saleUnavailable")
    );
    sale(&f, &name, 0, 0).await;
    let before = facts(&f).await;
    let result = f.db.customer_serving_readiness(&f.principal).await.unwrap();
    assert!(result.key_ready && result.model_ready);
    assert!(!result.funding_ready);
    assert_eq!(result.reason, Some("ledgerUninitialized"));
    assert_eq!(result.sample.unwrap().wallet_minor, None);
    assert_eq!(before, facts(&f).await);
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)")
            .bind(format!("ledger_{}", f.registration.organization.tenant_id))
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert!(!exists);
}
#[tokio::test]
async fn member_visibility_current_keys_and_restrictions_are_not_raw_counts() {
    let Some(mut f) = setup::fixture("customer_serving_readiness").await else {
        return;
    };
    let name = supply(&f, 1).await.remove(0);
    sale(&f, &name, 0, 0).await;
    f.tenants
        .get(&f.registration.organization.tenant_id)
        .await
        .unwrap();
    if let Principal::Session(ref mut session) = f.principal {
        session.role = Role::Member;
    }
    sqlx::query("UPDATE oxsum.api_keys SET created_by=NULL WHERE key_id=$1")
        .bind(f.registration.api_key.key.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    assert_eq!(
        f.db.customer_serving_readiness(&f.principal)
            .await
            .unwrap()
            .reason,
        Some("noLiveKey")
    );
    sqlx::query("UPDATE oxsum.api_keys SET created_by=$2,expires_at=now()-interval '1 second' WHERE key_id=$1").bind(f.registration.api_key.key.id).bind(f.registration.user.id).execute(f.db.pool()).await.unwrap();
    assert_eq!(
        f.db.customer_serving_readiness(&f.principal)
            .await
            .unwrap()
            .reason,
        Some("noLiveKey")
    );
    sqlx::query("UPDATE oxsum.api_keys SET expires_at=NULL WHERE key_id=$1")
        .bind(f.registration.api_key.key.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    assert!(
        f.db.customer_serving_readiness(&f.principal)
            .await
            .unwrap()
            .funding_ready
    );
    sqlx::query("UPDATE oxsum.organizations SET suspended_at=now() WHERE organization_id=$1")
        .bind(f.registration.organization.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    assert_eq!(
        f.db.customer_serving_readiness(&f.principal)
            .await
            .unwrap()
            .reason,
        Some("restricted")
    );
    f.db.revoke_key_as(&f.principal, f.registration.api_key.key.id)
        .await
        .unwrap();
    let result = f.db.customer_serving_readiness(&f.principal).await.unwrap();
    assert!(!result.key_ready && !result.funding_ready);
}
#[tokio::test]
async fn bounded_scan_does_not_claim_exhaustive_model_absence() {
    let Some(f) = setup::fixture("customer_serving_readiness").await else {
        return;
    };
    let names = supply(&f, 17).await;
    sale(&f, &names[16], 0, 0).await;
    f.tenants
        .get(&f.registration.organization.tenant_id)
        .await
        .unwrap();
    let result = f.db.customer_serving_readiness(&f.principal).await.unwrap();
    assert!(result.scan_limited);
    assert!(!result.funding_ready && !result.model_ready);
    f.db.update_key_constraints_as(
        &f.principal,
        f.registration.api_key.key.id,
        KeyConstraints {
            model_allowlist: Some(vec![names[16].clone()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let result = f.db.customer_serving_readiness(&f.principal).await.unwrap();
    assert!(!result.scan_limited);
    assert!(result.funding_ready);
}
#[tokio::test]
async fn single_connection_read_and_machine_denial() {
    let Some(f) = setup::fixture("customer_serving_readiness").await else {
        return;
    };
    let name = supply(&f, 1).await.remove(0);
    sale(&f, &name, 0, 0).await;
    f.tenants
        .get(&f.registration.organization.tenant_id)
        .await
        .unwrap();
    let url = oxsum_testkit::suite_db_url("customer_serving_readiness").unwrap();
    let db = Db::connect(&url, 1).await.unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        db.customer_serving_readiness(&f.principal),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.funding_ready);
    let (organization, key) =
        f.db.authenticate(&f.registration.api_key.secret)
            .await
            .unwrap()
            .unwrap();
    assert!(matches!(
        f.db.customer_serving_readiness(&Principal::Key(oxsum_core::KeyPrincipal {
            organization,
            key
        }))
        .await,
        Err(WalletError::Forbidden(_))
    ));
}

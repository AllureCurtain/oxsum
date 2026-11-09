//! First-call readiness proves visible canonical receipts, never operational-log recency.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/ready_receipt.rs"]
mod setup;

use oxsum_core::{KeyConstraints, KeyPrincipal, Principal, SettlementKind, WalletError};
use serde_json::Value;
use uuid::Uuid;

async fn read(f: &setup::Fixture) -> Value {
    serde_json::to_value(f.db.customer_receipt_readiness(&f.principal).await.unwrap()).unwrap()
}

async fn counts(f: &setup::Fixture) -> Value {
    let wallet = f
        .tenants
        .get(&f.registration.organization.tenant_id)
        .await
        .unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.usage_records WHERE tenant_id=$1")
            .bind(&f.registration.organization.tenant_id)
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    serde_json::json!({"entries":wallet.log_size().await.unwrap(),"usage":count,
        "available":wallet.available().await.unwrap(),"reserved":wallet.reserved().await.unwrap(),
        "settled":wallet.settled().await.unwrap()})
}

#[tokio::test]
async fn a_success_survives_later_errors_and_reads_do_not_write() {
    let Some(f) = setup::fixture("customer_readiness").await else {
        return;
    };
    assert_eq!(read(&f).await["state"], "missing");
    let row = setup::turn(&f, SettlementKind::Usage, 3, None).await;
    for n in 0..13 {
        setup::turn(
            &f,
            if n % 2 == 0 {
                SettlementKind::Refused
            } else {
                SettlementKind::UpstreamError
            },
            0,
            Some("fixture refusal"),
        )
        .await;
    }
    let before = counts(&f).await;
    for _ in 0..2 {
        let ready = read(&f).await;
        assert_eq!(ready["state"], "ready", "{ready}");
        assert_eq!(ready["receipt"]["requestId"], row.request_id);
        assert_eq!(ready["receipt"]["entryId"], row.entry_id.to_string());
        assert_eq!(ready["receipt"]["chargedMinor"], 3);
        assert_eq!(ready["receipt"]["contentHash"].as_str().unwrap().len(), 64);
    }
    assert_eq!(before, counts(&f).await);
}

#[tokio::test]
async fn free_success_counts_and_blocked_or_inconsistent_evidence_does_not() {
    for altered in ["blocked", "arithmetic", "request", "missing", "projection"] {
        let Some(f) = setup::fixture("customer_readiness").await else {
            return;
        };
        let row = setup::turn_for(
            &f,
            SettlementKind::Usage,
            0,
            None,
            f.registration.api_key.key.id,
            if ["missing", "projection"].contains(&altered) {
                None
            } else {
                Some(altered)
            },
        )
        .await;
        if altered == "missing" {
            sqlx::query("UPDATE oxsum.usage_records SET entry_id=$1 WHERE request_id=$2")
                .bind(Uuid::new_v4())
                .bind(&row.request_id)
                .execute(f.db.pool())
                .await
                .unwrap();
        } else if altered == "projection" {
            sqlx::query("UPDATE oxsum.usage_records SET charged_minor=5 WHERE request_id=$1")
                .bind(&row.request_id)
                .execute(f.db.pool())
                .await
                .unwrap();
        }
        let result = read(&f).await;
        assert_eq!(result["state"], "incomplete", "{altered}: {result}");
        assert!(result["receipt"].is_null());
        let good = setup::turn(&f, SettlementKind::Usage, 0, None).await;
        let result = read(&f).await;
        assert_eq!(result["state"], "ready", "{result}");
        assert_eq!(result["receipt"]["entryId"], good.entry_id.to_string());
        assert_eq!(result["receipt"]["chargedMinor"], 0);
    }
}

#[tokio::test]
async fn member_scope_checks_the_receipt_actor_and_other_customers_are_isolated() {
    let Some(mut f) = setup::fixture("customer_readiness").await else {
        return;
    };
    let own =
        f.db.create_key_as(&f.principal, None, None, KeyConstraints::default())
            .await
            .unwrap();
    // Assign the sibling key to another real person; member scope excludes it.
    let sibling = setup::fixture("customer_readiness").await.unwrap();
    sqlx::query("UPDATE oxsum.api_keys SET created_by=$1 WHERE key_id=$2")
        .bind(sibling.registration.user.id)
        .bind(f.registration.api_key.key.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    let foreign = setup::turn(&f, SettlementKind::Usage, 0, None).await;
    sqlx::query(
        "UPDATE oxsum.memberships SET role='member' WHERE organization_id=$1 AND user_id=$2",
    )
    .bind(f.registration.organization.id)
    .bind(f.registration.user.id)
    .execute(f.db.pool())
    .await
    .unwrap();
    f.principal = Principal::Session(f.db.authenticate_session(&f.cookie).await.unwrap().unwrap());
    assert_eq!(read(&f).await["state"], "missing");
    // A tampered projection cannot make a sibling's canonical receipt visible.
    sqlx::query("UPDATE oxsum.usage_records SET key_id=$1 WHERE request_id=$2")
        .bind(own.key.id)
        .bind(&foreign.request_id)
        .execute(f.db.pool())
        .await
        .unwrap();
    assert_eq!(read(&f).await["state"], "incomplete");
    let row = setup::turn_for(&f, SettlementKind::Usage, 0, None, own.key.id, None).await;
    assert_eq!(
        read(&f).await["receipt"]["entryId"],
        row.entry_id.to_string()
    );
    let other = setup::fixture("customer_readiness").await.unwrap();
    assert_eq!(read(&other).await["state"], "missing");
    let (organization, key) = f.db.authenticate(&own.secret).await.unwrap().unwrap();
    assert!(matches!(
        f.db.customer_receipt_readiness(&Principal::Key(KeyPrincipal { organization, key }))
            .await,
        Err(WalletError::Forbidden(_))
    ));
}

#[tokio::test]
async fn bounded_scan_reports_incomplete_without_claiming_no_history() {
    let Some(f) = setup::fixture("customer_readiness").await else {
        return;
    };
    setup::turn(&f, SettlementKind::Usage, 0, None).await;
    for _ in 0..32 {
        setup::turn_for(
            &f,
            SettlementKind::Usage,
            0,
            None,
            f.registration.api_key.key.id,
            Some("request"),
        )
        .await;
    }
    assert_eq!(read(&f).await["state"], "incomplete");
}

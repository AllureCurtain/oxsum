//! Bounded authorized statement history, cursor isolation and forbidden effects.
#[path = "../support/application_statements.rs"]
mod fixture;
use fixture::setup;
use oxsum_core::{IntegrationScope, WalletError};
use uuid::Uuid;

#[tokio::test]
async fn complete_statement_pages_archive_and_foreign_cursor_preserve_history() {
    let Some(w) = fixture::world("application_statements").await else {
        return;
    };
    let first = fixture::statement(&w, "issued", 3, 10).await;
    let second = fixture::statement(&w, "paid", 2, 10).await;
    let open = fixture::statement(&w, "open", 1, 10).await;
    let page =
        w.db.application_customer_statements(&w.actor, w.org, None, 1)
            .await
            .unwrap();
    assert_eq!(page.statements[0].statement_id, second);
    assert_eq!(page.next_cursor, Some(second));
    let page =
        w.db.application_customer_statements(&w.actor, w.org, page.next_cursor, 1)
            .await
            .unwrap();
    assert_eq!(page.statements[0].statement_id, first);
    assert!(page.next_cursor.is_none());
    let detail =
        w.db.application_customer_statement(&w.actor, w.org, first)
            .await
            .unwrap();
    assert_eq!(detail.lines.len(), 3);
    assert_eq!(detail.statement.total_minor, 3);
    assert_eq!(detail.lines.iter().map(|l| l.amount_minor).sum::<i64>(), 3);
    assert!(
        serde_json::to_value(&detail)
            .unwrap()
            .get("businessId")
            .is_none()
    );
    let Some(other) = fixture::world("application_statements").await else {
        return;
    };
    let foreign = fixture::statement(&other, "issued", 1, 10).await;
    for id in [foreign, Uuid::new_v4(), open] {
        assert!(matches!(
            w.db.application_customer_statements(&w.actor, w.org, Some(id), 20)
                .await,
            Err(WalletError::NotFound(_))
        ));
        assert!(matches!(
            w.db.application_customer_statement(&w.actor, w.org, id)
                .await,
            Err(WalletError::NotFound(_))
        ));
    }
    assert!(matches!(
        w.db.application_customer_statements(&w.actor, other.org, None, 20)
            .await,
        Err(WalletError::NotFound(_))
    ));
    w.db.archive_application_customer(
        &w.actor,
        w.org,
        &oxsum_core::ApplicationCustomerArchiveRequest {
            idempotency_key: "archive".into(),
            reason: "retain history".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        w.db.application_customer_statement(&w.actor, w.org, first)
            .await
            .unwrap()
            .lines
            .len(),
        3
    );
    let tenant: String =
        sqlx::query_scalar("SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1")
            .bind(w.org)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let schema: Option<String> = sqlx::query_scalar("SELECT to_regnamespace($1)::text")
        .bind(format!("ledger_{tenant}"))
        .fetch_one(w.db.pool())
        .await
        .unwrap();
    assert!(schema.is_none(), "reads must not open a ledger");
    let effects: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM oxsum.payment_orders WHERE organization_id=$1)+(SELECT count(*) FROM oxsum.evidence_bundles WHERE organization_id=$1)").bind(w.org).fetch_one(w.db.pool()).await.unwrap();
    assert_eq!(effects, 0);
}

#[tokio::test]
async fn statement_reads_require_current_application_billing_authority() {
    let Some(w) = fixture::world("application_statements").await else {
        return;
    };
    let statement = fixture::statement(&w, "issued", 1, 10).await;
    let (_, narrow) =
        fixture::credential(&w.db, w.app, vec![IntegrationScope::EntitlementsRead]).await;
    assert!(matches!(
        w.db.application_customer_statement(&narrow, w.org, statement)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    let legacy =
        w.db.create_integration_credential(
            &w.owner,
            &setup::request(vec![w.org], vec![IntegrationScope::BillingRead]),
        )
        .await
        .unwrap();
    let legacy =
        w.db.authenticate_service(legacy.secret.as_deref().unwrap())
            .await
            .unwrap()
            .unwrap();
    assert!(matches!(
        w.db.application_customer_statement(&legacy, w.org, statement)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    for limit in [0, 51] {
        assert!(matches!(
            w.db.application_customer_statements(&w.actor, w.org, None, limit)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    w.db.revoke_integration_credential(
        &w.owner,
        w.actor.credential_id(),
        &oxsum_core::IntegrationCredentialRevokeRequest {
            idempotency_key: "revoke".into(),
            reason: "rotation".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        w.db.application_customer_statement(&w.actor, w.org, statement)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    let (_, rotated) = fixture::credential(&w.db, w.app, vec![IntegrationScope::BillingRead]).await;
    assert!(
        w.db.application_customer_statement(&rotated, w.org, statement)
            .await
            .is_ok()
    );
    sqlx::query("UPDATE oxsum.integration_credentials SET created_at=clock_timestamp()-interval '2 days',expires_at=clock_timestamp()-interval '1 day' WHERE credential_id=$1").bind(rotated.credential_id()).execute(w.db.pool()).await.unwrap();
    assert!(matches!(
        w.db.application_customer_statements(&rotated, w.org, None, 20)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
}

#[tokio::test]
async fn statement_exports_refuse_oversized_headers_lines_and_counts_without_truncation() {
    let Some(w) = fixture::world("application_statements").await else {
        return;
    };
    let too_many = fixture::statement(&w, "issued", 1001, 1).await;
    let too_large = fixture::statement(&w, "issued", 1, 4 * 1024 * 1024).await;
    let header = fixture::statement(&w, "issued", 1, 1).await;
    sqlx::query("UPDATE oxsum.plan_statements SET void_reason=repeat('x',4*1024*1024) WHERE statement_id=$1").bind(header).execute(w.db.pool()).await.unwrap();
    for id in [too_many, too_large, header] {
        assert!(matches!(
            w.db.application_customer_statement(&w.actor, w.org, id)
                .await,
            Err(WalletError::Conflict(_))
        ));
    }
    assert!(matches!(
        w.db.application_customer_statements(&w.actor, w.org, None, 50)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let complete = fixture::statement(&w, "issued", 1000, 1).await;
    assert_eq!(
        w.db.application_customer_statement(&w.actor, w.org, complete)
            .await
            .unwrap()
            .lines
            .len(),
        1000
    );
}

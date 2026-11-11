//! Scoped monthly document reads preserve original rows and do not initialize ledgers.
#[path = "../support/integration.rs"]
mod setup;
use oxsum_core::{
    BusinessRole, BusinessStaffRequest, Db, NewUser, PaymentStatus, SessionContext,
    SessionContextRequest, WalletError,
};
use uuid::Uuid;

async fn fixture() -> Option<(Db, String, oxsum_core::Registration)> {
    let url = oxsum_testkit::suite_db_url("customer_statements")?;
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "monthly document fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    Some((db, owner, customer))
}
async fn document(db: &Db, org: Uuid, period: &str) -> Uuid {
    let id = Uuid::new_v4();
    // A stored historical fixture, independent of a bootstrapped ledger. Read
    // acceptance must not manufacture ledger accounts or re-finalize history.
    sqlx::query("INSERT INTO oxsum.statements(statement_id,organization_id,period,status,payment_status,total_minor,credit_drawn_minor,paid_minor,payment_terms_days,due_date,finalized_at,entry_count,anchor_entry_id,anchor_log_index,window_hash,rollup_hash) VALUES($1,$2,$3,'finalized','pending',1000,700,200,14,'2026-01-01',now(),2,$4,7,$5,$6)")
        .bind(id).bind(org).bind(period).bind(Uuid::new_v4()).bind("ab".repeat(32)).bind("cd".repeat(32)).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.statement_lines(statement_id,line_no,channel,model,turns,amount_minor) VALUES($1,1,'local','first',1,400),($1,2,'local','second',1,600)").bind(id).execute(db.pool()).await.unwrap();
    id
}
#[tokio::test]
async fn bounded_pages_detail_and_due_display_do_not_write_history_or_open_ledger() {
    let Some((db, owner, customer)) = fixture().await else {
        return;
    };
    let org = customer.organization.id;
    let a = document(&db, org, "2026-08").await;
    let b = document(&db, org, "2026-09").await;
    let original: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM oxsum.statements s WHERE statement_id=$1")
            .bind(a)
            .fetch_one(db.pool())
            .await
            .unwrap();
    let options = db.pool().connect_options();
    let one = Db::from_pool(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect_with((*options).clone())
            .await
            .unwrap(),
    );
    let first = one
        .business_monthly_statements(&owner, org, None, None, 1)
        .await
        .unwrap();
    assert_eq!(first.statements.len(), 1);
    let second = one
        .business_monthly_statements(&owner, org, None, first.next_cursor, 1)
        .await
        .unwrap();
    assert_eq!(second.statements.len(), 1);
    assert!(second.next_cursor.is_none());
    let mut ids = vec![first.statements[0].id, second.statements[0].id];
    ids.sort();
    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(ids, expected);
    let filtered = one
        .business_monthly_statements(&owner, org, Some("2026-09"), None, 20)
        .await
        .unwrap();
    assert_eq!(filtered.statements[0].id, b);
    let detail = one
        .business_monthly_statement(&owner, org, a)
        .await
        .unwrap();
    assert_eq!(detail.statement.payment_status, PaymentStatus::Overdue);
    assert_eq!(
        (
            detail.statement.total_minor,
            detail.statement.credit_drawn_minor,
            detail.statement.paid_minor,
            detail.statement.outstanding_minor
        ),
        (1000, 700, 200, 500)
    );
    assert_eq!(detail.statement.payment_terms_days, 14);
    assert!(detail.statement.overdue_at.is_none());
    assert_eq!(detail.statement.anchor_log_index, Some(7));
    assert_eq!(
        detail.statement.window_hash.as_deref(),
        Some("abababababababababababababababababababababababababababababababab")
    );
    assert_eq!(
        detail
            .lines
            .iter()
            .map(|l| l.model.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM oxsum.statements s WHERE statement_id=$1")
            .bind(a)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(original, after);
    let exists: bool = sqlx::query_scalar("SELECT to_regnamespace($1) IS NOT NULL")
        .bind(format!("ledger_{}", customer.organization.tenant_id))
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!exists);
    for limit in [0, 101] {
        assert!(matches!(
            db.business_monthly_statements(&owner, org, None, None, limit)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
    for period in ["2026-13", "not-month", "2999-01"] {
        assert!(matches!(
            db.business_monthly_statements(&owner, org, Some(period), None, 20)
                .await,
            Err(WalletError::InvalidInput(_))
        ));
    }
}
#[tokio::test]
async fn live_roles_and_foreign_document_pairs_refuse_without_disclosure() {
    let Some((db, owner, customer)) = fixture().await else {
        return;
    };
    let id = document(&db, customer.organization.id, "2026-09").await;
    let foreign = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "monthly document fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    assert!(matches!(
        db.business_monthly_statement(&owner, foreign.organization.id, id)
            .await,
        Err(WalletError::NotFound(_))
    ));
    assert!(matches!(
        db.business_monthly_statements(&owner, Uuid::new_v4(), None, None, 20)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let token = db
        .login(&customer.user.email, "monthly document fixture")
        .await
        .unwrap()
        .into_session()
        .token;
    assert!(matches!(
        db.business_monthly_statement(&token, customer.organization.id, id)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    for (role, allowed) in [
        (BusinessRole::Finance, true),
        (BusinessRole::Viewer, true),
        (BusinessRole::Support, false),
    ] {
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                user_id: customer.user.id,
                role,
                reason: "document role boundary".into(),
            },
        )
        .await
        .unwrap();
        db.switch_session_context(
            &token,
            &SessionContextRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                context: SessionContext::Business,
                organization_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            db.business_monthly_statements(&token, customer.organization.id, None, None, 20)
                .await
                .is_ok(),
            allowed
        );
        assert_eq!(
            db.business_monthly_statement(&token, customer.organization.id, id)
                .await
                .is_ok(),
            allowed
        );
    }
}
#[tokio::test]
async fn oversized_detail_refuses_instead_of_truncating_lines() {
    let Some((db, owner, customer)) = fixture().await else {
        return;
    };
    let id = document(&db, customer.organization.id, "2026-09").await;
    sqlx::query("INSERT INTO oxsum.statement_lines(statement_id,line_no,channel,model,turns,amount_minor) SELECT $1,n,'local','bulk',1,1 FROM generate_series(3,1001) n").bind(id).execute(db.pool()).await.unwrap();
    assert!(matches!(
        db.business_monthly_statement(&owner, customer.organization.id, id)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    sqlx::query("DELETE FROM oxsum.statement_lines WHERE statement_id=$1 AND line_no>2")
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.statement_lines SET model=repeat('x',2200000) WHERE statement_id=$1 AND line_no=1").bind(id).execute(db.pool()).await.unwrap();
    // Measure expanded serialized text so TOAST compression cannot bypass the cap.
    assert!(matches!(
        db.business_monthly_statement(&owner, customer.organization.id, id)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn a_snapshot_waiting_behind_revocation_does_not_return_monthly_history() {
    let Some((db, owner, customer)) = fixture().await else {
        return;
    };
    let id = document(&db, customer.organization.id, "2026-09").await;
    db.set_business_staff(
        &owner,
        &BusinessStaffRequest {
            idempotency_key: "finance".into(),
            user_id: customer.user.id,
            role: BusinessRole::Finance,
            reason: "fixture finance".into(),
        },
    )
    .await
    .unwrap();
    let token = db
        .login(&customer.user.email, "monthly document fixture")
        .await
        .unwrap()
        .into_session()
        .token;
    db.switch_session_context(
        &token,
        &SessionContextRequest {
            idempotency_key: "business".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    let mut revoke = db.pool().begin().await.unwrap();
    sqlx::query("SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE")
        .execute(&mut *revoke)
        .await
        .unwrap();
    let reader_db = db.clone();
    let org = customer.organization.id;
    let reader =
        tokio::spawn(async move { reader_db.business_monthly_statement(&token, org, id).await });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT business_id FROM oxsum.businesses%')").fetch_one(db.pool()).await.unwrap();
            if waiting { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    sqlx::query("DELETE FROM oxsum.business_staff WHERE user_id=$1")
        .bind(customer.user.id)
        .execute(&mut *revoke)
        .await
        .unwrap();
    revoke.commit().await.unwrap();
    assert!(matches!(
        reader.await.unwrap(),
        Err(WalletError::Forbidden(_))
    ));
}

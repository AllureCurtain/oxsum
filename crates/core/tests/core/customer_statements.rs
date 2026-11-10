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

fn generate(period: &str, key: &str) -> oxsum_core::BusinessMonthlyStatementAction {
    oxsum_core::BusinessMonthlyStatementAction::Generate {
        period: period.into(),
        idempotency_key: key.into(),
        reason: "review monthly usage".into(),
    }
}
async fn monthly_usage(
    db: &Db,
    customer: &oxsum_core::Registration,
    wallet: &oxsum_core::Wallet,
    key: &str,
    amount: i64,
) {
    let on = time::macros::date!(2026 - 09 - 15);
    wallet.hold(key, "", amount, on).await.unwrap();
    let receipt = wallet.settle(key, "", amount, on).await.unwrap();
    db.record_usage(&oxsum_core::UsageRow {
        request_id: Uuid::new_v4().to_string(),
        tenant_id: customer.organization.tenant_id.clone(),
        key_id: None,
        mandate_id: None,
        model: "monthly".into(),
        channel: "local".into(),
        price_version: 1,
        kind: oxsum_core::SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        status: "settled".into(),
        failure_reason: None,
        usage: oxsum_core::UsageRecord {
            input_tokens: 7,
            output_tokens: 3,
            ..Default::default()
        },
        counts: oxsum_core::CountsProvenance::Provider {
            input: 7,
            output: 3,
        },
        charged_minor: amount,
        freeze_minor: amount,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 1,
    })
    .await
    .unwrap();
}
async fn review(
    db: &Db,
    owner: &str,
    org: Uuid,
    id: Uuid,
    key: &str,
) -> oxsum_core::BusinessMonthlyStatementAction {
    let detail = db.business_monthly_statement(owner, org, id).await.unwrap();
    oxsum_core::BusinessMonthlyStatementAction::Finalize {
        statement_id: id,
        review_digest: detail.review_digest.unwrap(),
        idempotency_key: key.into(),
        reason: "issue reviewed monthly charges".into(),
    }
}
#[tokio::test]
async fn monthly_actions_converge_preserve_empty_replay_and_use_one_connection() {
    let Some((db, owner, customer)) = fixture().await else {
        return;
    };
    let options = db.pool().connect_options();
    let one = Db::from_pool(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect_with((*options).clone())
            .await
            .unwrap(),
    );
    let tenants = oxsum_core::Tenants::new(one.pool().clone());
    let org = customer.organization.id;
    let empty = one
        .mutate_business_monthly_statement(&owner, &tenants, org, &generate("2026-09", "empty"))
        .await
        .unwrap();
    assert!(empty.statement_id.is_none());
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    wallet
        .set_credit_limit("facility", 5_000_000, time::macros::date!(2026 - 08 - 01))
        .await
        .unwrap();
    monthly_usage(&db, &customer, &wallet, "first", 1_000_001).await;
    let again = one
        .mutate_business_monthly_statement(&owner, &tenants, org, &generate("2026-09", "empty"))
        .await
        .unwrap();
    assert_eq!(again.id, empty.id);
    assert!(again.statement_id.is_none());
    let generated = one
        .mutate_business_monthly_statement(&owner, &tenants, org, &generate("2026-09", "draft"))
        .await
        .unwrap();
    let id = generated.statement_id.unwrap();
    let request = review(&one, &owner, org, id, "finalize").await;
    let size = wallet.log_size().await.unwrap();
    let (a, b) = tokio::join!(
        one.mutate_business_monthly_statement(&owner, &tenants, org, &request),
        one.mutate_business_monthly_statement(&owner, &tenants, org, &request)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(a.statement_id, Some(id));
    assert_eq!(wallet.log_size().await.unwrap(), size + 1);
    let issued = one
        .business_monthly_statement(&owner, org, id)
        .await
        .unwrap();
    assert!(issued.review_digest.is_none());
    assert_eq!(issued.statement.total_minor, 1_000_001);
    assert_eq!(
        issued.statement.status,
        oxsum_core::StatementStatus::Finalized
    );
    assert_eq!(
        issued.statement.due_date,
        Some(time::OffsetDateTime::now_utc().date() + time::Duration::days(30))
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM oxsum.admin_audit WHERE idempotency_key=$1")
            .bind(format!("security:{}", a.id))
            .fetch_one(one.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
    let repeated = one
        .mutate_business_monthly_statement(&owner, &tenants, org, &generate("2026-09", "draft"))
        .await
        .unwrap();
    assert_eq!(repeated.id, generated.id);
    assert!(matches!(
        one.mutate_business_monthly_statement(&owner, &tenants, org, &generate("2026-08", "draft"))
            .await,
        Err(WalletError::Conflict(_))
    ));
    sqlx::query("UPDATE oxsum.customer_profiles SET state='archived',archived_at=now() WHERE organization_id=$1").bind(org).execute(one.pool()).await.unwrap();
    assert_eq!(
        one.mutate_business_monthly_statement(&owner, &tenants, org, &request)
            .await
            .unwrap()
            .id,
        a.id
    );
    assert!(matches!(
        one.mutate_business_monthly_statement(
            &owner,
            &tenants,
            org,
            &generate("2026-08", "archived")
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        sqlx::query("DELETE FROM oxsum.monthly_statement_actions WHERE mutation_id=$1")
            .bind(a.id)
            .execute(one.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn monthly_stale_review_and_audit_failure_leave_draft_and_no_anchor() {
    let Some((db, owner, customer)) = fixture().await else {
        return;
    };
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let org = customer.organization.id;
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    wallet
        .set_credit_limit("facility", 5_000_000, time::macros::date!(2026 - 08 - 01))
        .await
        .unwrap();
    monthly_usage(&db, &customer, &wallet, "first", 1_000_001).await;
    let id = db
        .mutate_business_monthly_statement(&owner, &tenants, org, &generate("2026-09", "draft"))
        .await
        .unwrap()
        .statement_id
        .unwrap();
    let stale = review(&db, &owner, org, id, "stale").await;
    monthly_usage(&db, &customer, &wallet, "later", 10).await;
    let size = wallet.log_size().await.unwrap();
    assert!(matches!(
        db.mutate_business_monthly_statement(&owner, &tenants, org, &stale)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(wallet.log_size().await.unwrap(), size);
    assert_eq!(
        db.business_monthly_statement(&owner, org, id)
            .await
            .unwrap()
            .statement
            .total_minor,
        1_000_001
    );
    db.mutate_business_monthly_statement(&owner, &tenants, org, &generate("2026-09", "refresh"))
        .await
        .unwrap();
    let terms_stale = review(&db, &owner, org, id, "terms").await;
    db.set_payment_terms(org, 45).await.unwrap();
    assert!(matches!(
        db.mutate_business_monthly_statement(&owner, &tenants, org, &terms_stale)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(wallet.log_size().await.unwrap(), size);
    db.mutate_business_monthly_statement(
        &owner,
        &tenants,
        org,
        &generate("2026-09", "refresh-terms"),
    )
    .await
    .unwrap();

    let shown = db
        .business_monthly_statement(&owner, org, id)
        .await
        .unwrap();
    let doc = &shown.statement;
    let yesterday = time::OffsetDateTime::now_utc().date() - time::Duration::days(1);
    let bytes=serde_json::to_vec(&serde_json::json!({"v":1,"statementId":doc.id,"organizationId":doc.organization_id,
        "period":doc.period,"totalMinor":doc.total_minor,"creditDrawnMinor":doc.credit_drawn_minor,"entryCount":doc.entry_count,
        "paymentTermsDays":doc.payment_terms_days,"reviewDay":yesterday.to_string(),"lines":shown.lines})).unwrap();
    let yesterday_request = oxsum_core::BusinessMonthlyStatementAction::Finalize {
        statement_id: id,
        review_digest: doubleentry::Hash::digest(b"oxsum/monthly-review/v1", &bytes).to_hex(),
        idempotency_key: "yesterday".into(),
        reason: "prior UTC day refuses".into(),
    };
    assert!(matches!(
        db.mutate_business_monthly_statement(&owner, &tenants, org, &yesterday_request)
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(wallet.log_size().await.unwrap(), size);
    let current = review(&db, &owner, org, id, "current").await;
    let name = format!("monthly_fail_{}", Uuid::new_v4().simple());
    sqlx::raw_sql(&format!("CREATE FUNCTION oxsum.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='business.customer.monthly_statement' AND NEW.detail->>'organizationId'='{org}' THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {name} BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.{name}();"))
        .execute(db.pool()).await.unwrap();
    let failed = db
        .mutate_business_monthly_statement(&owner, &tenants, org, &current)
        .await;
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {name} ON oxsum.admin_audit; DROP FUNCTION oxsum.{name}();"
    ))
    .execute(db.pool())
    .await
    .unwrap();
    assert!(failed.is_err());
    assert_eq!(wallet.log_size().await.unwrap(), size);
    assert_eq!(
        db.business_monthly_statement(&owner, org, id)
            .await
            .unwrap()
            .statement
            .status,
        oxsum_core::StatementStatus::Draft
    );
    let result = db
        .mutate_business_monthly_statement(&owner, &tenants, org, &current)
        .await
        .unwrap();
    assert_eq!(result.statement_id, Some(id));
    assert_eq!(wallet.log_size().await.unwrap(), size + 1);
    assert_eq!(
        db.business_monthly_statement(&owner, org, id)
            .await
            .unwrap()
            .statement
            .payment_terms_days,
        45
    );
}

#[tokio::test]
async fn monthly_writes_require_live_finance_context_and_refuse_foreign_documents() {
    let Some((db, owner, customer)) = fixture().await else {
        return;
    };
    let org = customer.organization.id;
    let tenants = oxsum_core::Tenants::new(db.pool().clone());
    let token = db
        .login(&customer.user.email, "monthly document fixture")
        .await
        .unwrap()
        .into_session()
        .token;
    assert!(matches!(
        db.mutate_business_monthly_statement(
            &token,
            &tenants,
            org,
            &generate("2026-09", "customer")
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    for (role, allowed) in [
        (BusinessRole::Admin, false),
        (BusinessRole::Viewer, false),
        (BusinessRole::Support, false),
        (BusinessRole::Finance, true),
    ] {
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                user_id: customer.user.id,
                role,
                reason: "monthly action authority".into(),
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
            db.mutate_business_monthly_statement(
                &token,
                &tenants,
                org,
                &generate("2026-09", &Uuid::new_v4().to_string())
            )
            .await
            .is_ok(),
            allowed
        );
    }
    let foreign = document(&db, org, "2026-08").await;
    let stranger = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "monthly foreign fixture".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let request = oxsum_core::BusinessMonthlyStatementAction::Finalize {
        statement_id: foreign,
        review_digest: "ab".repeat(32),
        idempotency_key: "foreign".into(),
        reason: "foreign refuses".into(),
    };
    assert!(matches!(
        db.mutate_business_monthly_statement(&token, &tenants, stranger.organization.id, &request)
            .await,
        Err(WalletError::NotFound(_))
    ));
    let mut revoke = db.pool().begin().await.unwrap();
    sqlx::query("SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE")
        .execute(&mut *revoke)
        .await
        .unwrap();
    let worker_db = db.clone();
    let worker_token = token.clone();
    let worker_tenants = tenants.clone();
    let worker = tokio::spawn(async move {
        worker_db
            .mutate_business_monthly_statement(
                &worker_token,
                &worker_tenants,
                org,
                &generate("2026-09", "revoked-waiter"),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT business_id FROM oxsum.businesses%')").fetch_one(db.pool()).await.unwrap();
            if waiting {break;} tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    sqlx::query("DELETE FROM oxsum.business_staff WHERE user_id=$1")
        .bind(customer.user.id)
        .execute(&mut *revoke)
        .await
        .unwrap();
    revoke.commit().await.unwrap();
    assert!(matches!(
        worker.await.unwrap(),
        Err(WalletError::Forbidden(_))
    ));
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.business_mutations WHERE idempotency_key='revoked-waiter'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

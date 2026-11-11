//! Monthly credit authority, exact replay and financial rollback.
use oxsum_core::{
    BusinessCustomerCreditTermsRequest, BusinessRole, BusinessSetupRegistrationRequest,
    BusinessSetupTokenRequest, BusinessStaffRequest, Db, LoginOutcome, NewUser, Registration,
    SessionContext, SessionContextRequest, SetupAuthority, Tenants, WalletError,
};
use time::OffsetDateTime;
use uuid::Uuid;

const PASSWORD: &str = "monthly credit fixture password";

async fn login(db: &Db, registration: &Registration, business: bool) -> String {
    let LoginOutcome::Session(session) =
        db.login(&registration.user.email, PASSWORD).await.unwrap()
    else {
        panic!("session")
    };
    if business {
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
    }
    session.token
}
async fn fixture() -> Option<(Db, Tenants, String, Registration)> {
    let url = oxsum_testkit::suite_db_url("customer_credit")?;
    let db = Db::connect(&url, 8).await.unwrap();
    db.migrate().await.unwrap();
    let email = format!("owner-{}@example.com", Uuid::new_v4());
    let setup = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &BusinessSetupTokenRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                owner_email: email.clone(),
            },
        )
        .await
        .unwrap();
    let owner = db
        .register_business_owner(&BusinessSetupRegistrationRequest {
            token: setup.token.unwrap(),
            business_name: "Credit fixture".into(),
            email,
            password: PASSWORD.into(),
        })
        .await
        .unwrap()
        .registration
        .unwrap();
    let token = login(&db, &owner, true).await;
    let customer = db
        .register(NewUser {
            email: format!("customer-{}@example.com", Uuid::new_v4()),
            password: PASSWORD.into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let tenants = Tenants::new(db.pool().clone());
    Some((db, tenants, token, customer))
}
fn request(key: &str, limit: i64) -> BusinessCustomerCreditTermsRequest {
    BusinessCustomerCreditTermsRequest {
        idempotency_key: key.into(),
        credit_limit_minor: limit,
        payment_terms_days: 14,
        dunning_grace_days: Some(3),
        reason: "approved monthly facility".into(),
    }
}

#[tokio::test]
async fn read_does_not_create_ledger_and_old_noop_replay_does_not_restore_terms() {
    let Some((db, tenants, owner, customer)) = fixture().await else {
        return;
    };
    let id = customer.organization.id;
    let schema = format!("ledger_{}", customer.organization.tenant_id);
    let before: bool = sqlx::query_scalar("SELECT to_regnamespace($1) IS NOT NULL")
        .bind(&schema)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!before);
    let initial = db.business_customer_credit_terms(&owner, id).await.unwrap();
    assert_eq!(
        (initial.credit_limit_minor, initial.credit_used_minor),
        (0, 0)
    );
    let after: bool = sqlx::query_scalar("SELECT to_regnamespace($1) IS NOT NULL")
        .bind(&schema)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(!after);
    let first = request("noop", 0);
    let receipt = db
        .set_business_customer_credit_terms(&owner, &tenants, id, &first)
        .await
        .unwrap();
    let mut later = request("grant", 1000);
    later.payment_terms_days = 30;
    later.dunning_grace_days = None;
    db.set_business_customer_credit_terms(&owner, &tenants, id, &later)
        .await
        .unwrap();
    assert_eq!(
        db.set_business_customer_credit_terms(&owner, &tenants, id, &first)
            .await
            .unwrap()
            .id,
        receipt.id
    );
    let current = db.business_customer_credit_terms(&owner, id).await.unwrap();
    assert_eq!(
        (
            current.credit_limit_minor,
            current.payment_terms_days,
            current.dunning_grace_days
        ),
        (1000, 30, None)
    );
    assert!(matches!(
        db.set_business_customer_credit_terms(&owner, &tenants, id, &request("noop", 1))
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(
        db.business_customer_credit_terms(&owner, id)
            .await
            .unwrap()
            .credit_limit_minor,
        1000
    );
}

#[tokio::test]
async fn shrink_below_reserved_credit_rolls_back_terms_receipt_and_audit() {
    let Some((db, tenants, owner, customer)) = fixture().await else {
        return;
    };
    let id = customer.organization.id;
    db.set_business_customer_credit_terms(&owner, &tenants, id, &request("grant", 1000))
        .await
        .unwrap();
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    wallet
        .hold(
            "held",
            "reserved credit",
            700,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    let mut shrink = request("shrink", 600);
    shrink.payment_terms_days = 90;
    shrink.dunning_grace_days = None;
    assert!(matches!(
        db.set_business_customer_credit_terms(&owner, &tenants, id, &shrink)
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    let current = db.business_customer_credit_terms(&owner, id).await.unwrap();
    assert_eq!(
        (
            current.credit_limit_minor,
            current.credit_used_minor,
            current.payment_terms_days,
            current.dunning_grace_days
        ),
        (1000, 700, 14, Some(3))
    );
    let count: i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.business_mutations WHERE request->>'organizationId'=$1 AND idempotency_key='shrink'").bind(id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(count, 0);
    let audit: i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.admin_audit WHERE action='business.customer.credit_terms' AND detail->>'organizationId'=$1").bind(id.to_string()).fetch_one(db.pool()).await.unwrap();
    assert_eq!(audit, 1);
    wallet
        .settle("held", "release", 0, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    db.set_business_customer_credit_terms(&owner, &tenants, id, &shrink)
        .await
        .unwrap();
    assert_eq!(wallet.credit_limit().await.unwrap(), 600);
}

#[tokio::test]
async fn rejected_terms_write_rolls_back_successful_ledger_append_and_exact_retry_recovers() {
    let Some((db, tenants, owner, customer)) = fixture().await else {
        return;
    };
    let id = customer.organization.id;
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    let trigger = format!("credit_fault_{}", id.simple());
    sqlx::query(&format!("CREATE FUNCTION oxsum.{trigger}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.organization_id = '{id}'::uuid THEN RAISE EXCEPTION 'credit terms fixture failure'; END IF; RETURN NEW; END $$" )).execute(db.pool()).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {trigger} BEFORE UPDATE OF payment_terms_days ON oxsum.organizations FOR EACH ROW EXECUTE FUNCTION oxsum.{trigger}()" )).execute(db.pool()).await.unwrap();
    let req = request("atomic", 1000);
    assert!(
        db.set_business_customer_credit_terms(&owner, &tenants, id, &req)
            .await
            .is_err()
    );
    assert_eq!(wallet.credit_limit().await.unwrap(), 0);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.business_mutations WHERE request->>'organizationId'=$1",
    )
    .bind(id.to_string())
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {trigger} ON oxsum.organizations; DROP FUNCTION oxsum.{trigger}()"
    ))
    .execute(db.pool())
    .await
    .unwrap();
    let (a, b) = tokio::join!(
        db.set_business_customer_credit_terms(&owner, &tenants, id, &req),
        db.set_business_customer_credit_terms(&owner, &tenants, id, &req)
    );
    assert_eq!(a.unwrap().id, b.unwrap().id);
    assert_eq!(wallet.credit_limit().await.unwrap(), 1000);
}

#[tokio::test]
async fn live_staff_roles_customer_context_foreign_and_archive_boundaries() {
    let Some((db, tenants, owner, customer)) = fixture().await else {
        return;
    };
    let id = customer.organization.id;
    let personal = login(&db, &customer, false).await;
    assert!(matches!(
        db.set_business_customer_credit_terms(&personal, &tenants, id, &request("denied", 1000))
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.business_customer_credit_terms(&owner, Uuid::new_v4())
            .await,
        Err(WalletError::NotFound(_))
    ));
    for (role, read, write) in [
        (BusinessRole::Finance, true, true),
        (BusinessRole::Admin, true, false),
        (BusinessRole::Support, false, false),
        (BusinessRole::Viewer, true, false),
    ] {
        db.set_business_staff(
            &owner,
            &BusinessStaffRequest {
                idempotency_key: Uuid::new_v4().to_string(),
                user_id: customer.user.id,
                role,
                reason: "role boundary".into(),
            },
        )
        .await
        .unwrap();
        let token = login(&db, &customer, true).await;
        assert_eq!(
            db.business_customer_credit_terms(&token, id).await.is_ok(),
            read
        );
        assert_eq!(
            db.set_business_customer_credit_terms(
                &token,
                &tenants,
                id,
                &request(&Uuid::new_v4().to_string(), 1000)
            )
            .await
            .is_ok(),
            write
        );
    }
    db.set_customer_lifecycle(
        &owner,
        id,
        &oxsum_core::BusinessCustomerLifecycle {
            idempotency_key: "archive".into(),
            state: oxsum_core::CustomerState::Archived,
            reason: "archive boundary".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.set_business_customer_credit_terms(
            &owner,
            &tenants,
            id,
            &request("after-archive", 2000)
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(
        db.business_customer_credit_terms(&owner, id)
            .await
            .unwrap()
            .credit_limit_minor,
        1000
    );
}

#[tokio::test]
async fn drawn_credit_and_manual_restriction_survive_terms_changes_with_single_connection() {
    let Some((db, tenants, owner, customer)) = fixture().await else {
        return;
    };
    let id = customer.organization.id;
    // A separate pool of one makes accidental nested checkouts time out.
    let options = db.pool().connect_options();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_with((*options).clone())
        .await
        .unwrap();
    let one = Db::from_pool(pool);
    let single_tenants = Tenants::new(one.pool().clone());
    assert_eq!(
        one.business_customer_credit_terms(&owner, id)
            .await
            .unwrap()
            .credit_limit_minor,
        0
    );
    one.set_business_customer_credit_terms(
        &owner,
        &single_tenants,
        id,
        &request("single-grant", 1000),
    )
    .await
    .unwrap();
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    wallet
        .hold(
            "draw",
            "settled monthly credit",
            700,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    wallet
        .settle("draw", "charge", 500, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    assert_eq!(
        one.business_customer_credit_terms(&owner, id)
            .await
            .unwrap()
            .credit_used_minor,
        500
    );
    assert!(matches!(
        one.set_business_customer_credit_terms(&owner, &single_tenants, id, &request("cut", 499))
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    db.set_customer_suspension(
        &owner,
        id,
        &oxsum_core::BusinessCustomerSuspension {
            idempotency_key: "manual-block".into(),
            suspended: true,
            reason: "manual review".into(),
        },
    )
    .await
    .unwrap();
    one.set_business_customer_credit_terms(&owner, &single_tenants, id, &request("raise", 1200))
        .await
        .unwrap();
    let suspended: bool = sqlx::query_scalar(
        "SELECT suspended_at IS NOT NULL FROM oxsum.organizations WHERE organization_id=$1",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(suspended);
    assert_eq!(
        one.business_customer_credit_terms(&owner, id)
            .await
            .unwrap()
            .credit_used_minor,
        500
    );
}

#[tokio::test]
async fn issued_statement_terms_remain_frozen_and_revoked_staff_cannot_replay() {
    let Some((db, tenants, owner, customer)) = fixture().await else {
        return;
    };
    let id = customer.organization.id;
    let first = request("grant-before-issue", 1000);
    db.set_business_customer_credit_terms(&owner, &tenants, id, &first)
        .await
        .unwrap();
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    let day = time::macros::date!(2026 - 09 - 15);
    wallet
        .hold("issued-hold", "monthly fixture", 500, day)
        .await
        .unwrap();
    let receipt = wallet
        .settle("issued-hold", "monthly usage", 500, day)
        .await
        .unwrap();
    db.record_usage(&oxsum_core::UsageRow {
        request_id: Uuid::new_v4().to_string(),
        tenant_id: customer.organization.tenant_id.clone(),
        key_id: None,
        mandate_id: None,
        model: "credit-fixture".into(),
        channel: "local".into(),
        price_version: 1,
        kind: oxsum_core::SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        status: "settled".into(),
        failure_reason: None,
        usage: oxsum_core::UsageRecord {
            input_tokens: 1,
            ..Default::default()
        },
        counts: oxsum_core::CountsProvenance::Provider {
            input: 1,
            output: 0,
        },
        charged_minor: 500,
        freeze_minor: 500,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 0,
    })
    .await
    .unwrap();
    let organization = db.organization_by_id(id).await.unwrap();
    let period = oxsum_core::statement_period("2026-09").unwrap();
    let draft = db
        .generate_statement(&organization, &wallet, &period)
        .await
        .unwrap()
        .unwrap();
    let issued = db
        .finalize_statement(draft.id, &wallet, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    assert_eq!(issued.payment_terms_days, 14);
    let mut later = request("later-terms", 2000);
    later.payment_terms_days = 90;
    later.dunning_grace_days = None;
    db.set_business_customer_credit_terms(&owner, &tenants, id, &later)
        .await
        .unwrap();
    let retained = db.statement_by_id(issued.id).await.unwrap().unwrap();
    assert_eq!(retained.payment_terms_days, 14);
    assert_eq!(retained.due_date, issued.due_date);
    assert_eq!(retained.anchor_entry_id, issued.anchor_entry_id);
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
    let finance = login(&db, &customer, true).await;
    let decision = request("finance-grant", 3000);
    db.set_business_customer_credit_terms(&finance, &tenants, id, &decision)
        .await
        .unwrap();
    db.revoke_business_staff(
        &owner,
        &oxsum_core::BusinessStaffRevokeRequest {
            idempotency_key: "revoke-finance".into(),
            user_id: customer.user.id,
            reason: "fixture revocation".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        db.set_business_customer_credit_terms(&finance, &tenants, id, &decision)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.business_customer_credit_terms(&finance, id).await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(wallet.credit_limit().await.unwrap(), 3000);
}

#[tokio::test]
async fn read_waiting_behind_revocation_rechecks_authority_outside_old_snapshot() {
    let Some((db, tenants, owner, customer)) = fixture().await else {
        return;
    };
    let id = customer.organization.id;
    db.set_business_customer_credit_terms(&owner, &tenants, id, &request("grant", 1000))
        .await
        .unwrap();
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
    let finance = login(&db, &customer, true).await;
    // Hold the same lock as staff revocation, then let the reader establish its
    // old snapshot and block behind it. Direct fixture SQL isolates that seam.
    let mut revocation = db.pool().begin().await.unwrap();
    sqlx::query("SELECT business_id FROM oxsum.businesses WHERE singleton FOR UPDATE")
        .execute(&mut *revocation)
        .await
        .unwrap();
    let task_db = db.clone();
    let reader =
        tokio::spawn(async move { task_db.business_customer_credit_terms(&finance, id).await });
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT business_id FROM oxsum.businesses%')").fetch_one(db.pool()).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    sqlx::query("DELETE FROM oxsum.business_staff WHERE user_id=$1")
        .bind(customer.user.id)
        .execute(&mut *revocation)
        .await
        .unwrap();
    revocation.commit().await.unwrap();
    assert!(matches!(
        reader.await.unwrap(),
        Err(WalletError::Forbidden(_))
    ));
}

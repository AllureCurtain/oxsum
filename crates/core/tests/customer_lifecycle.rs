//! A04/A05: profiles and admission blockers never move customer identity or money.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use oxsum_core::{
    BusinessCustomerLifecycle, BusinessCustomerSuspension, BusinessCustomerUpdate, BusinessRole,
    BusinessSetupRequest, BusinessSetupTokenRequest, BusinessStaffRequest, CustomerState, Db,
    NewUser, Registration, SessionContext, SessionContextRequest, SetupAuthority, Tenants,
    WalletError, verify_bundle,
};
use uuid::Uuid;
const PASSWORD: &str = "customer lifecycle fixture password";
async fn user(db: &Db, name: &str) -> Registration {
    db.register(NewUser {
        email: format!("{name}@example.com"),
        password: PASSWORD.into(),
        organization_name: None,
    })
    .await
    .unwrap()
}
async fn session(db: &Db, email: &str) -> String {
    db.login(email, PASSWORD)
        .await
        .unwrap()
        .into_session()
        .token
}
fn lifecycle(key: &str, state: CustomerState) -> BusinessCustomerLifecycle {
    BusinessCustomerLifecycle {
        idempotency_key: key.into(),
        state,
        reason: "customer lifecycle test".into(),
    }
}
fn suspension(key: &str, suspended: bool) -> BusinessCustomerSuspension {
    BusinessCustomerSuspension {
        idempotency_key: key.into(),
        suspended,
        reason: "customer suspension test".into(),
    }
}
#[tokio::test]
async fn profiles_archive_and_suspension_preserve_identity_and_in_flight_money() {
    let Some(url) = oxsum_testkit::suite_db_url("customer_lifecycle") else {
        eprintln!("DATABASE_URL absent: skipped");
        return;
    };
    let db = Db::connect(&url, 10).await.unwrap();
    db.migrate().await.unwrap();
    let owner = user(&db, "owner").await;
    let owner_token = session(&db, &owner.user.email).await;
    let invite = db
        .mint_business_setup(
            SetupAuthority::Deployment,
            &BusinessSetupTokenRequest {
                idempotency_key: "setup".into(),
                owner_email: owner.user.email.clone(),
            },
        )
        .await
        .unwrap();
    db.accept_business_setup(
        &owner_token,
        &BusinessSetupRequest {
            token: invite.token.unwrap(),
            business_name: "Customer test".into(),
        },
    )
    .await
    .unwrap();
    db.switch_session_context(
        &owner_token,
        &SessionContextRequest {
            idempotency_key: "business".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    let customer = user(&db, "customer").await;
    let customer_id = customer.organization.id;
    let customer_token = session(&db, &customer.user.email).await;
    let other = user(&db, "other").await;
    assert!(matches!(
        db.business_customer(&customer_token, customer_id).await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.business_customer(&customer.api_key.secret, customer_id)
            .await,
        Err(WalletError::InvalidCredentials)
    ));
    assert!(matches!(
        db.business_customer(&owner_token, Uuid::new_v4()).await,
        Err(WalletError::NotFound(_))
    ));
    let first = db.business_customers(&owner_token, None, 1).await.unwrap();
    assert_eq!(first.customers.len(), 1);
    assert!(first.next_cursor.is_some());
    let second = db
        .business_customers(&owner_token, first.next_cursor, 100)
        .await
        .unwrap();
    assert_eq!(second.customers.len(), 2);
    assert!(
        db.business_customers(&owner_token, None, 101)
            .await
            .is_err()
    );
    let tenants = Tenants::new(db.pool().clone());
    let wallet = tenants.get(&customer.organization.tenant_id).await.unwrap();
    let day = time::OffsetDateTime::now_utc().date();
    wallet.top_up("fund", 1000, day).await.unwrap();
    let (_, key) = db
        .authenticate(&customer.api_key.secret)
        .await
        .unwrap()
        .unwrap();
    let hold = wallet
        .hold_for_key(&key, None, "in-flight", "", 500, day, None)
        .await
        .unwrap();
    let original = db
        .business_customer(&owner_token, customer_id)
        .await
        .unwrap();
    let update = BusinessCustomerUpdate {
        idempotency_key: "rename".into(),
        name: "New customer name".into(),
        contact_email: Some("billing@example.com".into()),
        reason: "contact correction".into(),
    };
    let receipt = db
        .update_business_customer(&owner_token, customer_id, &update)
        .await
        .unwrap();
    assert_eq!(
        db.update_business_customer(&owner_token, customer_id, &update)
            .await
            .unwrap()
            .id,
        receipt.id
    );
    assert!(matches!(
        db.update_business_customer(&owner_token, other.organization.id, &update)
            .await,
        Err(WalletError::Conflict(_))
    ));
    let renamed = db
        .business_customer(&owner_token, customer_id)
        .await
        .unwrap();
    assert_eq!(renamed.external_id, original.external_id);
    assert_eq!(renamed.organization_id, original.organization_id);
    assert_eq!(
        renamed.contact_email.as_deref(),
        Some("billing@example.com")
    );
    let (resolved, _) = db
        .authenticate(&customer.api_key.secret)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resolved.tenant_id, customer.organization.tenant_id);
    assert_eq!(resolved.name, "New customer name");
    assert_eq!(wallet.available().await.unwrap(), 500);
    assert_eq!(wallet.reserved().await.unwrap(), 500);
    // Archive blocks both gateway and trusted service admission, but not settlement or payment.
    db.set_customer_lifecycle(
        &owner_token,
        customer_id,
        &lifecycle("archive", CustomerState::Archived),
    )
    .await
    .unwrap();
    assert!(
        db.business_customer(&owner_token, customer_id)
            .await
            .unwrap()
            .suspended
    );
    assert!(matches!(
        wallet
            .hold_for_key(&key, None, "blocked-key", "", 1, day, None)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        wallet.hold("blocked-service", "", 1, day).await,
        Err(WalletError::Forbidden(_))
    ));
    let settled = wallet.settle("in-flight", "", 120, day).await.unwrap();
    wallet
        .top_up("payment-while-archived", 100, day)
        .await
        .unwrap();
    assert_eq!(wallet.reserved().await.unwrap(), 0);
    assert_eq!(wallet.available().await.unwrap(), 980);
    let proof = wallet
        .receipt_proof(settled.entry_id, None)
        .await
        .unwrap()
        .unwrap();
    assert!(
        verify_bundle(
            &serde_json::to_string(&proof).unwrap(),
            &settled.content_hash
        )
        .unwrap()
    );
    assert!(wallet.receipt_proof(hold.entry_id, None).await.is_ok());
    // Independent reasons: neither restore nor manual clear erases dunning.
    db.set_customer_suspension(&owner_token, customer_id, &suspension("manual", true))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE oxsum.organizations SET dunning_suspended_at=now() WHERE organization_id=$1",
    )
    .bind(customer_id)
    .execute(db.pool())
    .await
    .unwrap();
    db.set_customer_lifecycle(
        &owner_token,
        customer_id,
        &lifecycle("restore", CustomerState::Active),
    )
    .await
    .unwrap();
    db.set_customer_suspension(
        &owner_token,
        customer_id,
        &suspension("manual-clear", false),
    )
    .await
    .unwrap();
    let standing = db
        .business_customer(&owner_token, customer_id)
        .await
        .unwrap();
    assert!(standing.suspended);
    assert!(standing.manual_suspended_at.is_none());
    assert!(standing.dunning_suspended_at.is_some());
    assert!(wallet.hold("still-dunning", "", 1, day).await.is_err());
    // With no overdue statement the sweep can lift only its own marker.
    db.set_customer_suspension(&owner_token, customer_id, &suspension("manual-again", true))
        .await
        .unwrap();
    db.sweep_dunning(day).await.unwrap();
    let standing = db
        .business_customer(&owner_token, customer_id)
        .await
        .unwrap();
    assert!(standing.suspended);
    assert!(standing.manual_suspended_at.is_some());
    assert!(standing.dunning_suspended_at.is_none());
    db.set_customer_suspension(&owner_token, customer_id, &suspension("clear-again", false))
        .await
        .unwrap();
    wallet
        .hold_for_key(&key, None, "restored", "", 10, day, None)
        .await
        .unwrap();
    wallet.settle("restored", "", 0, day).await.unwrap();
    // Exact replay of a historical archive does not reapply it after restore.
    db.set_customer_lifecycle(
        &owner_token,
        customer_id,
        &lifecycle("archive", CustomerState::Archived),
    )
    .await
    .unwrap();
    assert_eq!(
        db.business_customer(&owner_token, customer_id)
            .await
            .unwrap()
            .state,
        "active"
    );
    assert!(
        sqlx::query(
            "UPDATE oxsum.customer_profiles SET external_id='changed' WHERE organization_id=$1"
        )
        .bind(customer_id)
        .execute(db.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE oxsum.customer_profiles SET organization_id=$2 WHERE organization_id=$1"
        )
        .bind(customer_id)
        .bind(other.organization.id)
        .execute(db.pool())
        .await
        .is_err()
    );
    // Archive races hold admission under the shared/exclusive customer lock.
    // A winning hold remains settleable; every later new hold is refused.
    for index in 0..6 {
        db.set_customer_lifecycle(
            &owner_token,
            customer_id,
            &lifecycle(&format!("race-active-{index}"), CustomerState::Active),
        )
        .await
        .unwrap();
        let hold_key = format!("race-hold-{index}");
        let archive = lifecycle(&format!("race-archive-{index}"), CustomerState::Archived);
        let (admitted, archived) = tokio::join!(
            wallet.hold_for_key(&key, None, &hold_key, "", 10, day, None),
            db.set_customer_lifecycle(&owner_token, customer_id, &archive)
        );
        archived.unwrap();
        if admitted.is_ok() {
            wallet.settle(&hold_key, "", 3, day).await.unwrap();
        } else {
            assert!(
                matches!(admitted, Err(WalletError::Forbidden(_))),
                "{admitted:?}"
            );
        }
        assert!(matches!(
            wallet
                .hold_for_key(&key, None, &format!("late-{index}"), "", 1, day, None)
                .await,
            Err(WalletError::Forbidden(_))
        ));
        assert_eq!(wallet.reserved().await.unwrap(), 0);
    }
    db.set_customer_lifecycle(
        &owner_token,
        customer_id,
        &lifecycle("race-cleanup", CustomerState::Active),
    )
    .await
    .unwrap();
    // Roles are enforced by mutating services, not only by transport controls.
    db.set_business_staff(
        &owner_token,
        &BusinessStaffRequest {
            idempotency_key: "support".into(),
            user_id: other.user.id,
            role: BusinessRole::Support,
            reason: "support".into(),
        },
    )
    .await
    .unwrap();
    let support = session(&db, &other.user.email).await;
    db.switch_session_context(
        &support,
        &SessionContextRequest {
            idempotency_key: "business".into(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    assert!(db.business_customer(&support, customer_id).await.is_ok());
    assert!(matches!(
        db.update_business_customer(&support, customer_id, &update)
            .await,
        Err(WalletError::Forbidden(_))
    ));
    assert!(matches!(
        db.set_customer_lifecycle(
            &support,
            customer_id,
            &lifecycle("support-archive", CustomerState::Archived)
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    db.set_customer_suspension(&support, customer_id, &suspension("support-suspend", true))
        .await
        .unwrap();
    // A failed audit rolls back profile and receipt together.
    sqlx::raw_sql("CREATE FUNCTION oxsum.fail_customer_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='business.customer.update' THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_customer_audit BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.fail_customer_audit();").execute(db.pool()).await.unwrap();
    let bad = BusinessCustomerUpdate {
        idempotency_key: "fault".into(),
        name: "Should roll back".into(),
        contact_email: None,
        reason: "fault".into(),
    };
    assert!(
        db.update_business_customer(&owner_token, customer_id, &bad)
            .await
            .is_err()
    );
    assert_eq!(
        db.business_customer(&owner_token, customer_id)
            .await
            .unwrap()
            .name,
        "New customer name"
    );
    sqlx::query("DROP TRIGGER fail_customer_audit ON oxsum.admin_audit")
        .execute(db.pool())
        .await
        .unwrap();
    db.update_business_customer(&owner_token, customer_id, &bad)
        .await
        .unwrap();
}

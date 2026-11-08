//! Controlled concurrent decisions and whole-transaction dispute rollback (#826).
#![allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;

use std::sync::Arc;

use doubleentry::LedgerId;
use doubleentry::storage::postgres::PostgresStore;
use oxsum_core::{
    BusinessRole, BusinessStaffRequest, Db, DisputeResolution, MerchantEventKind, MerchantMode,
    NewPaymentOrder, NewSoldPlan, NewUser, OrderDispute, PaymentOrderPurpose, PeriodAuthorization,
    PeriodKind, SessionContext, SessionContextRequest, Wallet, WalletError,
};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use tokio::time::{Duration, timeout};
use uuid::Uuid;

const FEE: i64 = 2_500_000;
const LEGACY_DRAW: i64 = 1_000_000;

struct World {
    db: Db,
    owner: String,
    org: Uuid,
    wallet: Arc<Wallet>,
    order: Uuid,
    dispute: Uuid,
    plan: Option<Uuid>,
}

async fn world(plan: bool) -> Option<World> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url("dispute_atomicity")?;
    let db = Db::connect(&url, 16).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let customer = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "dispute atomicity fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = customer.organization.id;
    let wallet = Arc::new(
        Wallet::open(db.pool().clone(), &customer.organization.tenant_id)
            .await
            .unwrap(),
    );
    let context = payment::merchant_context(&db, &owner).await;
    let plan = if plan {
        let sold = db
            .publish_sold_plan(
                db.default_business().await.unwrap(),
                &NewSoldPlan {
                    plan_id: None,
                    name: format!("atomic-{}", Uuid::new_v4()),
                    price_minor: FEE,
                    currency: "USD".into(),
                    quota_minor: Some(10_000_000),
                    period_kind: PeriodKind::Anniversary,
                    overage_enabled: true,
                    sort_order: 0,
                },
            )
            .await
            .unwrap();
        Some(
            db.bind_customer_plan(
                org,
                sold.plan_version_id,
                PeriodAuthorization::PaymentOrder(Uuid::nil()),
            )
            .await
            .unwrap()
            .customer_plan_id,
        )
    } else {
        None
    };
    let order = if let Some(plan) = plan {
        payment::created_order(&db, &context, org, plan).await
    } else {
        db.create_payment_order(
            &context,
            org,
            &NewPaymentOrder {
                expected_collection_revision: None,
                expected_plan_quote: None,
                purpose: PaymentOrderPurpose::WalletTopup,
                display_amount_minor: 250,
                display_currency: "USD".into(),
                purpose_ref: None,
                idempotency_key: Uuid::new_v4().to_string(),
                promo_minor: 0,
            },
        )
        .await
        .unwrap()
    };
    payment::mark_paid(&db, &context, &order).await;
    if let Some(plan) = plan {
        db.activate_plan_period(order.order_id, plan).await.unwrap();
        sqlx::query("UPDATE oxsum.plan_periods SET period_start=now()-interval '2 days',period_end=now()-interval '1 day' WHERE customer_plan_id=$1 AND state='active'")
            .bind(plan).execute(db.pool()).await.unwrap();
        sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE organization_id=$1 AND state='active'")
            .bind(org).execute(db.pool()).await.unwrap();
        db.rollover_due_subscriptions().await.unwrap();
    } else {
        payment::fulfill(&db, &order).await;
        let on = OffsetDateTime::now_utc().date();
        wallet
            .set_credit_limit("legacy-line", FEE * 2, on)
            .await
            .unwrap();
        wallet
            .hold("legacy-draw", "", FEE + LEGACY_DRAW, on)
            .await
            .unwrap();
        wallet
            .settle("legacy-draw", "", FEE + LEGACY_DRAW, on)
            .await
            .unwrap();
    }
    let external = Uuid::new_v4().to_string();
    let receipt = db
        .receive_merchant_event(
            &context,
            &oxsum_core::VerifiedMerchantEvent {
                event_id: Uuid::new_v4().to_string(),
                kind: MerchantEventKind::Dispute,
                payment_id: Some(order.order_id),
                provider_payment_id: Some(format!("pay_{}", order.order_id)),
                organization_id: Some(org),
                provider_account_id: Some(context.account_id.clone()),
                mode: Some(MerchantMode::Test),
                amount_minor: Some(order.display_amount_minor),
                currency: Some(order.display_currency.clone()),
                refund_id: Some(external.clone()),
            },
            &[82; 32],
        )
        .await
        .unwrap();
    assert_eq!(receipt.state, "accepted");
    db.apply_refund_event(&wallet, &receipt, OffsetDateTime::now_utc().date())
        .await
        .unwrap();
    let dispute =
        sqlx::query_scalar("SELECT dispute_id FROM oxsum.disputes WHERE provider_dispute_id=$1")
            .bind(external)
            .fetch_one(db.pool())
            .await
            .unwrap();
    Some(World {
        db,
        owner,
        org,
        wallet,
        order: order.order_id,
        dispute,
        plan,
    })
}

async fn resolve(
    w: &World,
    resolution: DisputeResolution,
    key: &str,
) -> Result<OrderDispute, WalletError> {
    w.db.resolve_order_dispute(
        &w.wallet,
        &w.owner,
        w.dispute,
        resolution,
        Some("controlled dispute outcome"),
        key,
        OffsetDateTime::now_utc().date(),
    )
    .await
}

fn spawn(
    w: &World,
    resolution: DisputeResolution,
    key: &str,
) -> tokio::task::JoinHandle<Result<OrderDispute, WalletError>> {
    let db = w.db.clone();
    let wallet = w.wallet.clone();
    let owner = w.owner.clone();
    let dispute = w.dispute;
    let key = key.to_owned();
    tokio::spawn(async move {
        db.resolve_order_dispute(
            &wallet,
            &owner,
            dispute,
            resolution,
            Some("controlled dispute outcome"),
            &key,
            OffsetDateTime::now_utc().date(),
        )
        .await
    })
}

async fn pin_ledger(w: &World) -> (Transaction<'_, Postgres>, i32) {
    let store = PostgresStore::<6>::new(
        w.db.pool().clone(),
        LedgerId::new(format!("tenant-{}", w.wallet.tenant_id())).unwrap(),
    )
    .in_schema(&format!("ledger_{}", w.wallet.tenant_id()));
    let mut tx = w.db.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(store.append_lock())
        .execute(&mut *tx)
        .await
        .unwrap();
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    (tx, pid)
}

async fn blocked(pool: &PgPool, owner: i32) -> i32 {
    timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) LIMIT 1")
                .bind(owner).fetch_optional(pool).await.unwrap();
            if let Some(pid) = pid { return pid; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("a real backend waits on the controlled lock")
}

async fn snapshot(w: &World) -> serde_json::Value {
    let tenant = w.wallet.tenant_id();
    let accounts: serde_json::Value = sqlx::query_scalar(&format!(
        "SELECT COALESCE(jsonb_object_agg(path,net),'{{}}'::jsonb) FROM (SELECT a.path,COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint AS net FROM ledger_{tenant}.postings p JOIN ledger_{tenant}.accounts a USING(account_index) JOIN ledger_{tenant}.entries e USING(entry_id) WHERE p.layer='settled' AND e.log_index IS NOT NULL GROUP BY a.path) n"
    )).fetch_one(w.db.pool()).await.unwrap();
    let journals: serde_json::Value = sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(j) ORDER BY journal_id),'[]'::jsonb) FROM oxsum.invoice_journals j WHERE organization_id=$1")
        .bind(w.org).fetch_one(w.db.pool()).await.unwrap();
    let statements: serde_json::Value = sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(s) ORDER BY statement_id),'[]'::jsonb) FROM oxsum.plan_statements s WHERE organization_id=$1")
        .bind(w.org).fetch_one(w.db.pool()).await.unwrap();
    let dispute: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(d) FROM oxsum.disputes d WHERE dispute_id=$1")
            .bind(w.dispute)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.finance_audit WHERE action='dispute.resolve' AND target=$1",
    )
    .bind(w.dispute.to_string())
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    serde_json::json!({"accounts":accounts,"journals":journals,"statements":statements,"dispute":dispute,"audits":audits,"logSize":w.wallet.log_size().await.unwrap()})
}

#[tokio::test]
async fn a_blocked_loss_holds_the_decision_until_its_effect_and_audit_commit() {
    for plan in [true, false] {
        for (second, same_key) in [
            (DisputeResolution::Won, false),
            (DisputeResolution::Closed, false),
            (DisputeResolution::Lost, false),
            (DisputeResolution::Lost, true),
        ] {
            let Some(w) = world(plan).await else { return };
            let before = snapshot(&w).await;
            let (pin, pid) = pin_ledger(&w).await;
            let lost = spawn(
                &w,
                DisputeResolution::Lost,
                &format!("first-loss-{}", w.dispute),
            );
            let lost_pid = blocked(w.db.pool(), pid).await;
            let other = spawn(
                &w,
                second,
                &format!(
                    "{}-{}",
                    if same_key {
                        "first-loss"
                    } else {
                        "second-decision"
                    },
                    w.dispute
                ),
            );
            // The old three-transaction implementation let the other decision
            // finish here. Actual pg_blocking_pids proves it is now serialized.
            blocked(w.db.pool(), lost_pid).await;
            assert!(!other.is_finished());
            pin.rollback().await.unwrap();
            let lost = timeout(Duration::from_secs(5), lost)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(lost.state, "lost");
            let other = timeout(Duration::from_secs(5), other)
                .await
                .unwrap()
                .unwrap();
            if second == DisputeResolution::Lost {
                assert_eq!(other.unwrap().recharge_entry_id, lost.recharge_entry_id);
            } else {
                assert!(matches!(other, Err(WalletError::Conflict(_))), "{other:?}");
            }
            let after = snapshot(&w).await;
            assert_eq!(after["audits"], 1);
            let cash = before["accounts"]["Assets:Cash"].as_i64().unwrap();
            assert_eq!(after["accounts"]["Assets:Cash"], cash + FEE);
            if plan {
                assert_eq!(after["accounts"]["Assets:Receivables"], -FEE);
                assert_eq!(after["journals"][0]["settled_minor"], 0);
            } else {
                assert_eq!(w.wallet.credit_used().await.unwrap(), LEGACY_DRAW);
            }
            assert_eq!(
                after["logSize"].as_u64().unwrap(),
                before["logSize"].as_u64().unwrap() + 1
            );
        }
    }
}

#[tokio::test]
async fn plan_ownership_is_acquired_before_the_dispute_row() {
    let Some(w) = world(true).await else { return };
    let mut pin = w.db.pool().begin().await.unwrap();
    sqlx::query(
        "SELECT customer_plan_id FROM oxsum.customer_plans WHERE customer_plan_id=$1 FOR UPDATE",
    )
    .bind(w.plan)
    .fetch_one(&mut *pin)
    .await
    .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *pin)
        .await
        .unwrap();
    let operation = spawn(&w, DisputeResolution::Lost, "owner-before-dispute");
    blocked(w.db.pool(), pid).await;
    let lock =
        sqlx::query("SELECT dispute_id FROM oxsum.disputes WHERE dispute_id=$1 FOR UPDATE NOWAIT")
            .bind(w.dispute)
            .fetch_one(&mut *pin)
            .await;
    pin.rollback().await.unwrap();
    assert!(
        lock.is_ok(),
        "dispute was held while waiting for its owner: {lock:?}"
    );
    timeout(Duration::from_secs(5), operation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn failed_audit_rolls_back_cash_documents_terminal_state_and_key() {
    for plan in [true, false] {
        let Some(w) = world(plan).await else { return };
        let before = snapshot(&w).await;
        let key = format!("rollback-and-retry-{}", w.dispute);
        let function = format!("fail_dispute_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dispute.resolve' AND NEW.target='{}' THEN RAISE EXCEPTION 'injected audit failure after ledger append'; END IF; RETURN NEW; END $$", w.dispute))
            .execute(w.db.pool()).await.unwrap();
        sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON oxsum.finance_audit FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()"))
            .execute(w.db.pool()).await.unwrap();
        assert!(resolve(&w, DisputeResolution::Lost, &key).await.is_err());
        assert_eq!(snapshot(&w).await, before);
        sqlx::query(&format!("DROP TRIGGER {function} ON oxsum.finance_audit"))
            .execute(w.db.pool())
            .await
            .unwrap();
        sqlx::query(&format!("DROP FUNCTION oxsum.{function}()"))
            .execute(w.db.pool())
            .await
            .unwrap();
        resolve(&w, DisputeResolution::Lost, &key).await.unwrap();
        let after = snapshot(&w).await;
        resolve(&w, DisputeResolution::Lost, &key).await.unwrap();
        assert_eq!(snapshot(&w).await, after);
        assert_eq!(after["audits"], 1);
    }
}

#[tokio::test]
async fn request_identity_and_terminal_choices_are_not_silently_reinterpreted() {
    let Some(w) = world(true).await else { return };
    resolve(&w, DisputeResolution::EvidenceDue, "evidence-step")
        .await
        .unwrap();
    let evidence = snapshot(&w).await;
    resolve(&w, DisputeResolution::EvidenceDue, "evidence-step")
        .await
        .unwrap();
    assert_eq!(snapshot(&w).await, evidence);
    for (resolution, reason) in [
        (DisputeResolution::Lost, Some("controlled dispute outcome")),
        (DisputeResolution::EvidenceDue, Some("changed reason")),
        (DisputeResolution::EvidenceDue, None),
    ] {
        assert!(matches!(
            w.db.resolve_order_dispute(
                &w.wallet,
                &w.owner,
                w.dispute,
                resolution,
                reason,
                "evidence-step",
                OffsetDateTime::now_utc().date()
            )
            .await,
            Err(WalletError::Conflict(_))
        ));
    }
    let other = world(true).await.unwrap();
    assert!(matches!(
        resolve(&other, DisputeResolution::EvidenceDue, "evidence-step").await,
        Err(WalletError::Conflict(_))
    ));
    w.db.resolve_order_dispute(
        &w.wallet,
        &w.owner,
        w.dispute,
        DisputeResolution::EvidenceDue,
        Some("  controlled dispute outcome \n"),
        "evidence-step",
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    resolve(&w, DisputeResolution::Won, "terminal-won")
        .await
        .unwrap();
    let won = snapshot(&w).await;
    assert!(matches!(
        resolve(&w, DisputeResolution::Lost, "try-lost-after-won").await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(snapshot(&w).await, won);
}

#[tokio::test]
async fn live_authority_and_exact_wallet_are_required_before_every_replay() {
    let Some(w) = world(true).await else { return };
    let other = world(false).await.unwrap();
    let before = snapshot(&w).await;
    assert!(matches!(
        w.db.resolve_order_dispute(
            &other.wallet,
            &w.owner,
            w.dispute,
            DisputeResolution::Lost,
            None,
            "foreign-wallet",
            OffsetDateTime::now_utc().date()
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(
        w.db.resolve_order_dispute(
            &w.wallet,
            "invalid-session",
            w.dispute,
            DisputeResolution::Lost,
            None,
            "wrong-session",
            OffsetDateTime::now_utc().date()
        )
        .await
        .is_err()
    );
    let user =
        w.db.register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "staff dispute authority password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let token =
        w.db.login(&user.user.email, "staff dispute authority password")
            .await
            .unwrap()
            .into_session()
            .token;
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "read-only dispute reviewer".into(),
            user_id: user.user.id,
            role: BusinessRole::Viewer,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        w.db.resolve_order_dispute(
            &w.wallet,
            &token,
            w.dispute,
            DisputeResolution::Lost,
            None,
            "customer-context",
            OffsetDateTime::now_utc().date()
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    w.db.switch_session_context(
        &token,
        &SessionContextRequest {
            context: SessionContext::Business,
            organization_id: None,
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        w.db.resolve_order_dispute(
            &w.wallet,
            &token,
            w.dispute,
            DisputeResolution::Lost,
            None,
            "wrong-role",
            OffsetDateTime::now_utc().date()
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
    assert_eq!(snapshot(&w).await, before);
    resolve(&w, DisputeResolution::Won, "authority-replay")
        .await
        .unwrap();
    assert!(matches!(
        w.db.resolve_order_dispute(
            &other.wallet,
            &w.owner,
            w.dispute,
            DisputeResolution::Won,
            Some("controlled dispute outcome"),
            "authority-replay",
            OffsetDateTime::now_utc().date()
        )
        .await,
        Err(WalletError::Conflict(_))
    ));
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "finance replay".into(),
            user_id: user.user.id,
            role: BusinessRole::Finance,
        },
    )
    .await
    .unwrap();
    w.db.resolve_order_dispute(
        &w.wallet,
        &token,
        w.dispute,
        DisputeResolution::Won,
        Some("controlled dispute outcome"),
        "new-finance-key",
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap();
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            reason: "revoke finance authority".into(),
            user_id: user.user.id,
            role: BusinessRole::Viewer,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        w.db.resolve_order_dispute(
            &w.wallet,
            &token,
            w.dispute,
            DisputeResolution::Won,
            Some("controlled dispute outcome"),
            "new-finance-key",
            OffsetDateTime::now_utc().date()
        )
        .await,
        Err(WalletError::Forbidden(_))
    ));
}

#[tokio::test]
async fn historical_orphaned_chargebacks_refuse_without_repairing_or_reposting() {
    for terminal in [false, true] {
        let Some(w) = world(true).await else { return };
        if terminal {
            resolve(&w, DisputeResolution::Won, "old-won")
                .await
                .unwrap();
        }
        // The public legacy seam reproduces the former middle committed phase,
        // using the real ledger engine and normal document guards.
        w.db.recharge_journal_dispute(
            &w.wallet,
            w.order,
            w.dispute,
            FEE,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap()
        .unwrap();
        let before = snapshot(&w).await;
        for resolution in [
            DisputeResolution::Won,
            DisputeResolution::Lost,
            DisputeResolution::Closed,
        ] {
            let error = resolve(&w, resolution, "review-orphan").await.unwrap_err();
            assert!(matches!(error, WalletError::Conflict(_)), "{error:?}");
            assert!(error.to_string().contains("source review"));
        }
        assert_eq!(snapshot(&w).await, before);
    }
}

#[tokio::test]
async fn terminal_entry_and_audit_links_must_agree_before_replay() {
    for fault in ["entry", "audit"] {
        let Some(w) = world(true).await else { return };
        let key = format!("lost-source-{}", w.dispute);
        resolve(&w, DisputeResolution::Lost, &key).await.unwrap();
        // Existing guards retain terminal state but permit metadata updates.
        if fault == "entry" {
            sqlx::query("UPDATE oxsum.disputes SET recharge_entry_id=$2 WHERE dispute_id=$1")
                .bind(w.dispute)
                .bind(Uuid::new_v4())
                .execute(w.db.pool())
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE oxsum.disputes SET audit_ref=NULL WHERE dispute_id=$1")
                .bind(w.dispute)
                .execute(w.db.pool())
                .await
                .unwrap();
        }
        let before = snapshot(&w).await;
        assert!(matches!(
            resolve(&w, DisputeResolution::Lost, &key).await,
            Err(WalletError::Conflict(_))
        ));
        assert_eq!(snapshot(&w).await, before);
    }
}

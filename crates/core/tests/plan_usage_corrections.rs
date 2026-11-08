//! Historical duplicate usage corrections use real, retained ledger sources (#809).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/plan_payment.rs"]
mod payment;
#[path = "support/integration.rs"]
mod setup;

use doubleentry::storage::postgres::PostgresStore;
use doubleentry::{
    AccountRegistry, Amount, Currency, Draft, Entry, EntryBatch, IdempotencyKey, LedgerId,
    LedgerPolicy, LedgerStore, SealContext,
};
use oxsum_core::{
    BusinessRole, BusinessStaffRequest, Db, NewRefundRequest, NewSoldPlan, NewUser,
    PeriodAuthorization, PeriodKind, PlanUsageCorrection, PlanUsageCorrectionRequest,
    SessionContext, SessionContextRequest, Wallet, WalletError,
};
use time::OffsetDateTime;
use uuid::Uuid;

const FEE: i64 = 2_500_000;
const USAGE: i64 = 1_500_000;
struct World {
    db: Db,
    owner: String,
    org: Uuid,
    tenant: String,
    wallet: Wallet,
    journal: Uuid,
    statement: Uuid,
    order: Uuid,
    last_order: Uuid,
    context: oxsum_core::MerchantSigningContext,
    usage_entry: Uuid,
}

async fn world(collected: i64) -> Option<World> {
    let _ = dotenvy::dotenv();
    let url = oxsum_testkit::suite_db_url("plan_usage_corrections")?;
    let db = Db::connect(&url, 12).await.unwrap();
    db.migrate().await.unwrap();
    let owner = setup::owner(&db).await;
    let user = db
        .register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "usage correction fixture password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let org = user.organization.id;
    let tenant = user.organization.tenant_id;
    let wallet = Wallet::open(db.pool().clone(), &tenant).await.unwrap();
    let business = db.default_business().await.unwrap();
    let sold = db
        .publish_sold_plan(
            business,
            &NewSoldPlan {
                plan_id: None,
                name: format!("legacy-{}", Uuid::new_v4()),
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
    let plan = db
        .bind_customer_plan(
            org,
            sold.plan_version_id,
            PeriodAuthorization::PaymentOrder(Uuid::nil()),
        )
        .await
        .unwrap();
    let context = payment::merchant_context(&db, &owner).await;
    let order = payment::created_order(&db, &context, org, plan.customer_plan_id).await;
    payment::mark_paid(&db, &context, &order).await;
    db.activate_plan_period(order.order_id, plan.customer_plan_id)
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=now()-interval '2 days',period_end=now()-interval '1 day' WHERE customer_plan_id=$1")
        .bind(plan.customer_plan_id).execute(db.pool()).await.unwrap();
    let period: Uuid = sqlx::query_scalar(
        "SELECT plan_period_id FROM oxsum.plan_periods WHERE customer_plan_id=$1",
    )
    .bind(plan.customer_plan_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    // Real usage settlement, then its retained analytics row in the old close's
    // period. No direct writes to ledger entries or postings are used.
    let key = format!("usage-{}", Uuid::new_v4());
    let on = OffsetDateTime::now_utc().date();
    wallet
        .top_up(&format!("fund-{key}"), USAGE, on)
        .await
        .unwrap();
    wallet
        .hold(&key, "historical usage hold", USAGE, on)
        .await
        .unwrap();
    let description=serde_json::json!({"v":3,"request":key,"channel":"fixture","model":"fixture","priceVersion":1,"kind":"usage","usage":{"inputTokens":1,"outputTokens":1},"lines":[["input",1,1500000000000i64],["output",1,0]],"charged":USAGE,"freeze":USAGE}).to_string();
    let usage = wallet.settle(&key, &description, USAGE, on).await.unwrap();
    let usage_entry = *usage.entry_id.as_uuid();
    sqlx::query("INSERT INTO oxsum.usage_records(request_id,tenant_id,model,channel,price_version,kind,entry_id,input_tokens,output_tokens,charged_minor,freeze_minor,settled_at) VALUES($1,$2,'fixture','fixture',1,'usage',$3,1,1,$4,$4,now()-interval '36 hours')")
        .bind(&key).bind(&tenant).bind(usage_entry).bind(USAGE).execute(db.pool()).await.unwrap();
    let journal = Uuid::new_v4();
    let statement = Uuid::new_v4();
    let total = FEE + USAGE;
    // Fixture represents old binaries using the normal journal guards and the
    // official engine append. Pending prepayment intentionally remains deferred
    // for the zero-collected branch; the correction cannot consume that cash.
    sqlx::query("INSERT INTO oxsum.invoice_journals(journal_id,customer_plan_id,organization_id,business_id,plan_period_id,period_index,state,total_minor,currency) VALUES($1,$2,$3,$4,$5,0,'draft',$6,'USD')")
        .bind(journal).bind(plan.customer_plan_id).bind(org).bind(business).bind(period).bind(total).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_statements(statement_id,customer_plan_id,organization_id,business_id,period_index,period_start,period_end,journal_id,state,total_minor,currency) SELECT $1,$2,$3,$4,0,period_start,period_end,$5,'open',$6,'USD' FROM oxsum.plan_periods WHERE plan_period_id=$7")
        .bind(statement).bind(plan.customer_plan_id).bind(org).bind(business).bind(journal).bind(total).bind(period).execute(db.pool()).await.unwrap();
    for (jk, sk, amount) in [
        ("base_fee", "plan_fee", FEE),
        ("usage_overage", "usage", USAGE),
    ] {
        sqlx::query("INSERT INTO oxsum.invoice_journal_lines(line_id,journal_id,kind,amount_minor,description,source_ref) VALUES($1,$2,$3,$4,'Original legacy close',$5)")
            .bind(Uuid::new_v4()).bind(journal).bind(jk).bind(amount).bind(period).execute(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,$3,$4,'Original legacy close',$5)")
            .bind(Uuid::new_v4()).bind(statement).bind(sk).bind(amount).bind(period).execute(db.pool()).await.unwrap();
    }
    append(
        &db,
        &tenant,
        &format!("plan-journal/{journal}/accrual"),
        "Assets:Receivables",
        "Income:PlanFees",
        total,
    )
    .await;
    let prepaid = collected.min(FEE);
    if prepaid > 0 {
        append(
            &db,
            &tenant,
            &format!("plan-journal/{journal}/prepay-settle"),
            "Liabilities:UnearnedPlanFees",
            "Assets:Receivables",
            prepaid,
        )
        .await;
        payment_line(&db, statement, order.order_id, prepaid).await;
    }
    let mut last_order = order.order_id;
    if collected > FEE {
        // Reproduce the old renewal payment's journal allocation without
        // activating another period or changing current production semantics.
        let renewal = payment::created_order(&db, &context, org, plan.customer_plan_id).await;
        payment::mark_paid(&db, &context, &renewal).await;
        append(
            &db,
            &tenant,
            &format!("payment-order/{}/journal/{journal}", renewal.order_id),
            "Assets:Cash",
            "Assets:Receivables",
            collected - FEE,
        )
        .await;
        payment_line(&db, statement, renewal.order_id, collected - FEE).await;
        let remaining = renewal.amount_minor - (collected - FEE);
        if remaining > 0 {
            append(
                &db,
                &tenant,
                &format!("payment-order/{}/prepay", renewal.order_id),
                "Assets:Cash",
                "Liabilities:UnearnedPlanFees",
                remaining,
            )
            .await;
        }
        last_order = renewal.order_id;
    }
    sqlx::query("UPDATE oxsum.invoice_journals SET state='posted',settled_minor=$2,posted_at=now() WHERE journal_id=$1")
        .bind(journal).bind(collected).execute(db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_statements SET state='issued',paid_minor=$2,issued_at=now(),due_at=now()+interval '14 days' WHERE statement_id=$1")
        .bind(statement).bind(collected).execute(db.pool()).await.unwrap();
    if collected == total {
        sqlx::query("UPDATE oxsum.invoice_journals SET state='paid',paid_at=now(),paid_order_id=$2 WHERE journal_id=$1")
            .bind(journal).bind(last_order).execute(db.pool()).await.unwrap();
        sqlx::query("UPDATE oxsum.plan_statements SET state='paid',paid_at=now(),paid_order_id=$2 WHERE statement_id=$1")
            .bind(statement).bind(last_order).execute(db.pool()).await.unwrap();
    }
    Some(World {
        db,
        owner,
        org,
        tenant,
        wallet,
        journal,
        statement,
        order: order.order_id,
        last_order,
        context,
        usage_entry,
    })
}

async fn append(db: &Db, tenant: &str, key: &str, debit: &str, credit: &str, amount: i64) {
    let store = PostgresStore::<6>::new(
        db.pool().clone(),
        LedgerId::new(format!("tenant-{tenant}")).unwrap(),
    )
    .in_schema(&format!("ledger_{tenant}"));
    let registry = AccountRegistry::from_records(store.accounts().await.unwrap()).unwrap();
    let account = |path: &str| {
        registry
            .records()
            .iter()
            .find(|r| r.account.path.to_string() == path)
            .unwrap()
            .id
    };
    let sealed = Entry::<Draft, 6>::new(
        oxsum_core::entry_id_for(key),
        IdempotencyKey::new(key.as_bytes().to_vec()).unwrap(),
        OffsetDateTime::now_utc().date(),
    )
    .debit(account(debit), Amount::from_minor(amount), Currency::USD)
    .credit(account(credit), Amount::from_minor(amount), Currency::USD)
    .seal(&SealContext {
        accounts: &registry,
        calendar: &store.calendar().await.unwrap(),
        policy: &LedgerPolicy::default(),
    })
    .unwrap();
    store.append(&EntryBatch::single(sealed)).await.unwrap();
}
async fn payment_line(db: &Db, statement: Uuid, order: Uuid, amount: i64) {
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'payment_applied',$3,'Historical payment allocation',$4)")
        .bind(Uuid::new_v4()).bind(statement).bind(-amount).bind(order).execute(db.pool()).await.unwrap();
}
fn request(w: &World) -> PlanUsageCorrectionRequest {
    PlanUsageCorrectionRequest {
        journal_id: w.journal,
        reason: "Correct the proven duplicate usage charge".into(),
        idempotency_key: format!("correct-{}", w.journal),
    }
}
async fn run(w: &World, apply: bool) -> PlanUsageCorrection {
    w.db.correct_legacy_plan_usage(
        &w.owner,
        &w.wallet,
        &request(w),
        apply,
        OffsetDateTime::now_utc().date(),
    )
    .await
    .unwrap()
}
async fn net(w: &World, path: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COALESCE(SUM(CASE WHEN p.direction='C' THEN p.amount_minor ELSE -p.amount_minor END),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a USING(account_index) WHERE a.path=$1 AND p.layer='settled'",w.tenant,w.tenant)).bind(path).fetch_one(w.db.pool()).await.unwrap()
}
async fn effects(w: &World) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM oxsum.plan_usage_corrections WHERE journal_id=$1")
        .bind(w.journal)
        .fetch_one(w.db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn unpaid_partly_and_fully_collected_sources_preserve_cash_and_originals() {
    for collected in [0, 1_000_000, FEE, 3_000_000, FEE + USAGE] {
        let Some(w) = world(collected).await else {
            return;
        };
        let cash = net(&w, "Assets:Cash").await;
        let log = w.wallet.log_size().await.unwrap();
        let preview = run(&w, false).await;
        assert_eq!(preview.status, "proposed", "{preview:?}");
        let ar = USAGE.min(FEE + USAGE - collected);
        let payable = USAGE - ar;
        assert_eq!(
            (
                preview.duplicate_minor,
                preview.receivable_minor,
                preview.payable_minor
            ),
            (USAGE, ar, payable)
        );
        assert_eq!(effects(&w).await, 0);
        assert_eq!(w.wallet.log_size().await.unwrap(), log);
        let applied = run(&w, true).await;
        assert_eq!(applied.status, "applied");
        assert_eq!(net(&w, "Assets:Cash").await, cash);
        assert_eq!(net(&w, "Income:Usage").await, USAGE);
        assert_eq!(net(&w, "Income:PlanFees").await, FEE);
        assert_eq!(
            net(&w, "Assets:Receivables").await,
            -(FEE - collected.min(FEE))
        );
        assert_eq!(net(&w, "Liabilities:RefundPayable").await, payable);
        assert_eq!(w.wallet.available().await.unwrap(), 0);
        let j = w.db.plan_journal(w.org, w.journal).await.unwrap().unwrap();
        assert_eq!(
            (j.journal.total_minor, j.journal.settled_minor),
            (FEE, collected.min(FEE))
        );
        assert_eq!(
            j.lines.iter().filter(|l| l.amount_minor == USAGE).count(),
            1
        );
        assert_eq!(
            j.lines
                .iter()
                .filter(|l| l.source_ref == Some(applied.correction_id))
                .count(),
            1
        );
        let s =
            w.db.plan_statement(w.org, w.statement)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            (s.statement.total_minor, s.statement.paid_minor),
            (FEE, collected.min(FEE))
        );
        assert_eq!(
            s.lines.iter().map(|l| l.amount_minor).sum::<i64>(),
            FEE - collected.min(FEE)
        );
        assert_eq!(w.wallet.log_size().await.unwrap(), log + 1);
        let replay = run(&w, true).await;
        assert!(replay.already_applied);
        assert_eq!(w.wallet.log_size().await.unwrap(), log + 1);
        assert_eq!(effects(&w).await, 1);
    }
}

#[tokio::test]
async fn concurrent_apply_and_different_retry_key_post_once() {
    let Some(w) = world(FEE + USAGE).await else {
        return;
    };
    let before = w.wallet.log_size().await.unwrap();
    let (a, b) = tokio::join!(run(&w, true), run(&w, true));
    assert_eq!(a.entry_id, b.entry_id);
    assert_ne!(a.already_applied, b.already_applied);
    let mut retry = request(&w);
    retry.idempotency_key = Uuid::new_v4().to_string();
    let result =
        w.db.correct_legacy_plan_usage(
            &w.owner,
            &w.wallet,
            &retry,
            true,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert!(result.already_applied);
    assert_eq!(w.wallet.log_size().await.unwrap(), before + 1);
    assert_eq!(effects(&w).await, 1);
    assert!(
        sqlx::query(
            "UPDATE oxsum.plan_usage_corrections SET duplicate_minor=1 WHERE journal_id=$1"
        )
        .bind(w.journal)
        .execute(w.db.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM oxsum.invoice_journal_lines WHERE journal_id=$1")
            .bind(w.journal)
            .execute(w.db.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn missing_usage_and_payment_sources_are_retained_refusals_without_money() {
    for fault in ["usage", "payment", "accrual"] {
        let Some(w) = world(FEE).await else { return };
        match fault {
            "usage" => {
                sqlx::query("UPDATE oxsum.usage_records SET entry_id=$2 WHERE entry_id=$1")
                    .bind(w.usage_entry)
                    .bind(Uuid::new_v4())
                    .execute(w.db.pool())
                    .await
                    .unwrap();
            }
            "payment" => {
                sqlx::query("UPDATE oxsum.invoice_journals SET settled_minor=settled_minor-1 WHERE journal_id=$1").bind(w.journal).execute(w.db.pool()).await.unwrap();
            }
            _ => {
                sqlx::query("INSERT INTO oxsum.invoice_journal_lines(line_id,journal_id,kind,amount_minor,description,source_ref) VALUES($1,$2,'tax',1,'unsupported manual source',$3)").bind(Uuid::new_v4()).bind(w.journal).bind(Uuid::new_v4()).execute(w.db.pool()).await.unwrap();
            }
        }
        let before = w.wallet.log_size().await.unwrap();
        let preview = run(&w, false).await;
        assert_eq!(preview.status, "refused");
        assert!(preview.refusal.as_ref().is_some_and(|r| !r.is_empty()));
        let applied = run(&w, true).await;
        assert_eq!(applied.status, "refused");
        assert_eq!(effects(&w).await, 0);
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.finance_audit WHERE action='plan_usage.correct' AND target=$1").bind(w.journal.to_string()).fetch_one(w.db.pool()).await.unwrap();
        assert_eq!(count, 1);
        assert_eq!(run(&w, true).await.refusal, applied.refusal);
        let mut changed = request(&w);
        changed.reason = "different request".into();
        assert!(
            w.db.correct_legacy_plan_usage(
                &w.owner,
                &w.wallet,
                &changed,
                true,
                OffsetDateTime::now_utc().date()
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn a_foreign_settlement_cannot_prove_this_periods_duplicate_charge() {
    let Some(a) = world(FEE).await else { return };
    let b = world(FEE).await.unwrap();
    sqlx::query("UPDATE oxsum.usage_records SET entry_id=$2 WHERE entry_id=$1")
        .bind(a.usage_entry)
        .bind(b.usage_entry)
        .execute(a.db.pool())
        .await
        .unwrap();
    let report = run(&a, true).await;
    assert_eq!(report.status, "refused");
    assert!(report.refusal.unwrap().contains("this customer's ledger"));
    assert_eq!(effects(&a).await, 0);
    assert_eq!(effects(&b).await, 0);
}

#[tokio::test]
async fn usage_window_request_identity_and_statement_sources_must_all_agree() {
    for fault in ["window", "request", "statement"] {
        let Some(w) = world(FEE).await else { return };
        match fault {
            "window" => {
                sqlx::query("UPDATE oxsum.usage_records SET settled_at=now() WHERE entry_id=$1")
                    .bind(w.usage_entry)
                    .execute(w.db.pool())
                    .await
                    .unwrap();
            }
            "request" => {
                sqlx::query("UPDATE oxsum.usage_records SET request_id=$2 WHERE entry_id=$1")
                    .bind(w.usage_entry)
                    .bind(Uuid::new_v4().to_string())
                    .execute(w.db.pool())
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("UPDATE oxsum.plan_statements SET paid_minor=0 WHERE statement_id=$1")
                    .bind(w.statement)
                    .execute(w.db.pool())
                    .await
                    .unwrap();
            }
        }
        let before = w.wallet.log_size().await.unwrap();
        let report = run(&w, true).await;
        assert_eq!(report.status, "refused", "{fault}");
        assert_eq!(effects(&w).await, 0);
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
    }
}

#[tokio::test]
async fn void_refund_and_dispute_histories_refuse_combined_repair() {
    for history in ["void", "refund", "dispute"] {
        let Some(w) = world(FEE).await else { return };
        match history {
            "void" => {
                w.db.void_invoice_journal(
                    &w.owner,
                    &w.wallet,
                    w.journal,
                    "retain void history",
                    &Uuid::new_v4().to_string(),
                    OffsetDateTime::now_utc().date(),
                )
                .await
                .unwrap();
            }
            "refund" => {
                w.db.request_order_refund(
                    w.org,
                    w.order,
                    &NewRefundRequest {
                        amount_minor: Some(100_000),
                        reason: "retain refund history".into(),
                        request_key: Uuid::new_v4().to_string(),
                        requested_by: None,
                    },
                )
                .await
                .unwrap();
            }
            _ => {
                sqlx::query(
                    "UPDATE oxsum.invoice_journals SET disputed_minor=1 WHERE journal_id=$1",
                )
                .bind(w.journal)
                .execute(w.db.pool())
                .await
                .unwrap();
            }
        }
        let before = w.wallet.log_size().await.unwrap();
        let report = run(&w, true).await;
        assert_eq!(report.status, "refused", "{history}: {report:?}");
        assert_eq!(effects(&w).await, 0);
        assert_eq!(w.wallet.log_size().await.unwrap(), before);
    }
}

#[tokio::test]
async fn owner_finance_and_customer_scope_are_live_authority() {
    let Some(w) = world(FEE).await else { return };
    let other = Wallet::open(w.db.pool().clone(), &Uuid::new_v4().simple().to_string())
        .await
        .unwrap();
    let on = OffsetDateTime::now_utc().date();
    assert!(
        w.db.correct_legacy_plan_usage(&w.owner, &other, &request(&w), true, on)
            .await
            .is_err()
    );
    assert!(
        w.db.correct_legacy_plan_usage("invalid-session", &w.wallet, &request(&w), true, on)
            .await
            .is_err()
    );
    let person =
        w.db.register(NewUser {
            email: format!("{}@example.com", Uuid::new_v4()),
            password: "correction finance password".into(),
            organization_name: None,
        })
        .await
        .unwrap();
    let token =
        w.db.login(&person.user.email, "correction finance password")
            .await
            .unwrap()
            .into_session()
            .token;
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            user_id: person.user.id,
            role: BusinessRole::Finance,
            reason: "test finance authority".into(),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    assert!(
        w.db.correct_legacy_plan_usage(&token, &w.wallet, &request(&w), false, on)
            .await
            .is_err()
    );
    w.db.switch_session_context(
        &token,
        &SessionContextRequest {
            idempotency_key: Uuid::new_v4().to_string(),
            context: SessionContext::Business,
            organization_id: None,
        },
    )
    .await
    .unwrap();
    let report =
        w.db.correct_legacy_plan_usage(&token, &w.wallet, &request(&w), true, on)
            .await
            .unwrap();
    assert_eq!(report.status, "applied");
    w.db.set_business_staff(
        &w.owner,
        &BusinessStaffRequest {
            user_id: person.user.id,
            role: BusinessRole::Viewer,
            reason: "revoke funds authority".into(),
            idempotency_key: Uuid::new_v4().to_string(),
        },
    )
    .await
    .unwrap();
    assert!(
        w.db.correct_legacy_plan_usage(&token, &w.wallet, &request(&w), true, on)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_document_write_rolls_back_cashless_entry_and_audit() {
    let Some(w) = world(FEE).await else { return };
    // A fixture trigger fails only this selected statement after evidence was
    // checked. The original immutable guards remain enabled throughout.
    // Use a permanent selected identity in the fixture trigger to reach every
    // pooled connection, with a unique trigger per test's isolated statement.
    let function = format!("fail_usage_{}", w.statement.simple());
    sqlx::query(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.statement_id='{}'::uuid AND NEW.description='Correction of previously settled usage billed twice' THEN RAISE EXCEPTION 'injected document write failure'; END IF; RETURN NEW; END $$",w.statement)).execute(w.db.pool()).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {function} BEFORE INSERT ON oxsum.plan_statement_lines FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()" )).execute(w.db.pool()).await.unwrap();
    let before = w.wallet.log_size().await.unwrap();
    assert!(
        w.db.correct_legacy_plan_usage(
            &w.owner,
            &w.wallet,
            &request(&w),
            true,
            OffsetDateTime::now_utc().date()
        )
        .await
        .is_err()
    );
    assert_eq!(w.wallet.log_size().await.unwrap(), before);
    assert_eq!(effects(&w).await, 0);
    let j = w.db.plan_journal(w.org, w.journal).await.unwrap().unwrap();
    assert_eq!(j.journal.total_minor, FEE + USAGE);
    sqlx::query(&format!(
        "DROP TRIGGER {function} ON oxsum.plan_statement_lines"
    ))
    .execute(w.db.pool())
    .await
    .unwrap();
    assert_eq!(run(&w, true).await.status, "applied");
}

async fn source_event(
    w: &World,
    order: Uuid,
    kind: oxsum_core::MerchantEventKind,
    external: &str,
    display: i64,
) -> oxsum_core::MerchantEventReceipt {
    let event = oxsum_core::VerifiedMerchantEvent {
        event_id: Uuid::new_v4().to_string(),
        kind,
        payment_id: Some(order),
        provider_payment_id: Some(format!("pay_{order}")),
        organization_id: Some(w.org),
        provider_account_id: Some(w.context.account_id.clone()),
        mode: Some(oxsum_core::MerchantMode::Test),
        amount_minor: Some(display),
        currency: Some("USD".into()),
        refund_id: Some(external.into()),
    };
    let receipt =
        w.db.receive_merchant_event(&w.context, &event, &[84; 32])
            .await
            .unwrap();
    assert_eq!(receipt.state, "accepted", "{:?}", receipt.reason);
    receipt
}

#[tokio::test]
async fn every_corrected_funding_order_is_fenced_before_provider_authorization() {
    let Some(w) = world(FEE + USAGE).await else {
        return;
    };
    assert_eq!(run(&w, true).await.status, "applied");
    for order in [w.order, w.last_order] {
        let error =
            w.db.request_order_refund(
                w.org,
                order,
                &NewRefundRequest {
                    amount_minor: Some(500_000),
                    reason: "later payout".into(),
                    request_key: Uuid::new_v4().to_string(),
                    requested_by: None,
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("historical usage correction"));
        // An old writer's request must still be denied at the independent
        // approval gate before any provider authorization is committed.
        let refund = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.refunds(refund_id,order_id,organization_id,business_id,amount_minor,display_amount_minor,display_currency,reason,state,request_key) SELECT $1,order_id,organization_id,business_id,500000,50,'USD','old writer request','requested',$2 FROM oxsum.payment_orders WHERE order_id=$3")
            .bind(refund).bind(refund.to_string()).bind(order).execute(w.db.pool()).await.unwrap();
        let error =
            w.db.resolve_order_refund(
                &w.owner,
                refund,
                oxsum_core::RefundDecision::Approve,
                None,
                &refund.to_string(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("before provider authorization"));
    }
    let sources: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.plan_usage_correction_sources WHERE journal_id=$1",
    )
    .bind(w.journal)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    assert_eq!(sources, 2);
    assert!(
        sqlx::query("DELETE FROM oxsum.plan_usage_correction_sources WHERE journal_id=$1")
            .bind(w.journal)
            .execute(w.db.pool())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_request_racing_correction_cannot_escape_the_funding_fence() {
    let Some(w) = world(FEE).await else { return };
    let request = NewRefundRequest {
        amount_minor: Some(500_000),
        reason: "racing payout".into(),
        request_key: Uuid::new_v4().to_string(),
        requested_by: None,
    };
    let (correction, refund) = tokio::join!(
        run(&w, true),
        w.db.request_order_refund(w.org, w.order, &request)
    );
    if correction.status == "applied" {
        assert!(refund.is_err());
    } else {
        assert_eq!(correction.status, "refused");
        assert!(refund.is_ok());
    }
}

#[tokio::test]
async fn confirmed_late_refunds_keep_full_cash_without_borrowing_a_renewal_or_payable() {
    for use_renewal in [false, true] {
        let Some(w) = world(FEE + USAGE).await else {
            return;
        };
        run(&w, true).await;
        let order = if use_renewal { w.last_order } else { w.order };
        let cash = net(&w, "Assets:Cash").await;
        let deferred = net(&w, "Liabilities:UnearnedPlanFees").await;
        let refund = Uuid::new_v4();
        let note = Uuid::new_v4();
        let provider = format!("late-{refund}");
        // External late fact discovered after the bounded correction, as an
        // imported failed reservation. Ordinary new approval is fenced above.
        sqlx::query("INSERT INTO oxsum.refunds(refund_id,order_id,organization_id,business_id,amount_minor,display_amount_minor,display_currency,reason,state,provider_refund_id,request_key,resolved_by) SELECT $1,order_id,organization_id,business_id,500000,50,'USD','late external cash','failed',$2,$3,(SELECT user_id FROM oxsum.business_staff WHERE business_id=payment_orders.business_id AND role='owner') FROM oxsum.payment_orders WHERE order_id=$4")
            .bind(refund).bind(&provider).bind(refund.to_string()).bind(order).execute(w.db.pool()).await.unwrap();
        sqlx::query("INSERT INTO oxsum.credit_notes(credit_note_id,organization_id,order_id,refund_id,amount_minor,reason,state) VALUES($1,$2,$3,$4,500000,'late external cash','reversed')")
            .bind(note).bind(w.org).bind(order).bind(refund).execute(w.db.pool()).await.unwrap();
        let receipt = source_event(
            &w,
            order,
            oxsum_core::MerchantEventKind::RefundSucceeded,
            &provider,
            50,
        )
        .await;
        for _ in 0..2 {
            w.db.apply_refund_event(&w.wallet, &receipt, OffsetDateTime::now_utc().date())
                .await
                .unwrap();
        }
        assert_eq!(net(&w, "Assets:Cash").await, cash + 500_000);
        assert_eq!(net(&w, "Liabilities:UnearnedPlanFees").await, deferred);
        assert_eq!(net(&w, "Liabilities:RefundPayable").await, USAGE);
        assert_eq!(net(&w, "Assets:Receivables").await, 0);
        assert_eq!(net(&w, "Equity:Adjustments").await, -500_000);
        let effect =
            w.db.recover_plan_refund(
                &w.owner,
                &w.wallet,
                refund,
                false,
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
        assert_eq!(
            (
                effect.unallocated_minor,
                effect.earned_minor,
                effect.deferred_minor,
                effect.payable_minor
            ),
            (500_000, 0, 0, 0)
        );
        assert_eq!(effect.plan_period_id, None);
        let detail: serde_json::Value = sqlx::query_scalar(
            "SELECT detail FROM oxsum.finance_audit WHERE target=$1 AND idempotency_key=$2",
        )
        .bind(refund.to_string())
        .bind(format!("plan-refund-review:{refund}"))
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        assert_eq!(detail["correctedJournals"][0], w.journal.to_string());
        let s =
            w.db.plan_statement(w.org, w.statement)
                .await
                .unwrap()
                .unwrap()
                .statement;
        assert_eq!((s.total_minor, s.paid_minor), (FEE, FEE));
    }
}

#[tokio::test]
async fn later_chargebacks_retain_cash_and_review_without_double_consuming_correction_payable() {
    for use_renewal in [false, true] {
        let Some(w) = world(FEE + USAGE).await else {
            return;
        };
        run(&w, true).await;
        let order = if use_renewal { w.last_order } else { w.order };
        let cash = net(&w, "Assets:Cash").await;
        let deferred = net(&w, "Liabilities:UnearnedPlanFees").await;
        let external = Uuid::new_v4().to_string();
        let receipt = source_event(
            &w,
            order,
            oxsum_core::MerchantEventKind::Dispute,
            &external,
            250,
        )
        .await;
        w.db.apply_refund_event(&w.wallet, &receipt, OffsetDateTime::now_utc().date())
            .await
            .unwrap();
        let dispute: Uuid = sqlx::query_scalar(
            "SELECT dispute_id FROM oxsum.disputes WHERE provider_dispute_id=$1",
        )
        .bind(&external)
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        for _ in 0..2 {
            w.db.resolve_order_dispute(
                &w.wallet,
                &w.owner,
                dispute,
                oxsum_core::DisputeResolution::Lost,
                Some("external loss"),
                &dispute.to_string(),
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
        }
        assert_eq!(net(&w, "Assets:Cash").await, cash + FEE);
        assert_eq!(net(&w, "Liabilities:UnearnedPlanFees").await, deferred);
        assert_eq!(net(&w, "Liabilities:RefundPayable").await, USAGE);
        assert_eq!(net(&w, "Assets:Receivables").await, 0);
        assert_eq!(net(&w, "Equity:Adjustments").await, -FEE);
        let detail: serde_json::Value = sqlx::query_scalar(
            "SELECT detail FROM oxsum.finance_audit WHERE action='dispute.resolve' AND target=$1",
        )
        .bind(dispute.to_string())
        .fetch_one(w.db.pool())
        .await
        .unwrap();
        assert_eq!(detail["unallocatedMinor"], FEE);
        assert_eq!(detail["correctedJournals"][0], w.journal.to_string());
        assert_eq!(detail["requiresReview"], true);
    }
}

#[tokio::test]
async fn a_later_void_preserves_correction_payable_and_adds_only_retained_collection() {
    for statement_review in [false, true] {
        let Some(w) = world(FEE + USAGE).await else {
            return;
        };
        run(&w, true).await;
        let cash = net(&w, "Assets:Cash").await;
        if statement_review {
            w.db.release_statement_review(
                &w.owner,
                w.statement,
                "void",
                "void corrected bill",
                &Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        } else {
            w.db.void_invoice_journal(
                &w.owner,
                &w.wallet,
                w.journal,
                "void corrected bill",
                &Uuid::new_v4().to_string(),
                OffsetDateTime::now_utc().date(),
            )
            .await
            .unwrap();
        }
        assert_eq!(net(&w, "Liabilities:RefundPayable").await, FEE + USAGE);
        assert_eq!(net(&w, "Assets:Cash").await, cash);
        assert_eq!(net(&w, "Income:PlanFees").await, 0);
        assert_eq!(effects(&w).await, 1);
        assert!(run(&w, true).await.already_applied);
    }
}

#[tokio::test]
async fn correction_preserves_other_retained_void_debt_and_its_admission_restriction() {
    let Some(w) = world(FEE).await else { return };
    let plan: Uuid = sqlx::query_scalar(
        "SELECT customer_plan_id FROM oxsum.invoice_journals WHERE journal_id=$1",
    )
    .bind(w.journal)
    .fetch_one(w.db.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE organization_id=$1 AND state='active'").bind(w.org).execute(w.db.pool()).await.unwrap();
    w.db.rollover_due_subscriptions().await.unwrap();
    let order = payment::paid_order(&w.db, &w.owner, w.org, plan).await;
    w.db.activate_plan_period(order.order_id, plan)
        .await
        .unwrap();
    sqlx::query("UPDATE oxsum.plan_periods SET period_start=now()-interval '2 days',period_end=now()-interval '1 day' WHERE customer_plan_id=$1 AND state='active'").bind(plan).execute(w.db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.org_subscriptions SET current_period_start=now()-interval '2 days',current_period_end=now()-interval '1 day' WHERE organization_id=$1 AND state='active'").bind(w.org).execute(w.db.pool()).await.unwrap();
    let other = payment::legacy_close(&w.db, w.org, plan, USAGE).await;
    w.db.rollover_due_subscriptions().await.unwrap();
    let statement: Uuid =
        sqlx::query_scalar("SELECT statement_id FROM oxsum.plan_statements WHERE journal_id=$1")
            .bind(other)
            .fetch_one(w.db.pool())
            .await
            .unwrap();
    sqlx::query("UPDATE oxsum.plan_statements SET state='past_due',due_at=now()-interval '30 days' WHERE statement_id=$1").bind(statement).execute(w.db.pool()).await.unwrap();
    sqlx::query("INSERT INTO oxsum.plan_statement_lines(line_id,statement_id,kind,amount_minor,description,source_ref) SELECT gen_random_uuid(),statement_id,kind,-amount_minor,'retained document-only void',line_id FROM oxsum.plan_statement_lines WHERE statement_id=$1").bind(statement).execute(w.db.pool()).await.unwrap();
    sqlx::query("UPDATE oxsum.plan_statements SET state='void',total_minor=0,paid_minor=0,voided_at=now(),void_reason='historical document-only review' WHERE statement_id=$1").bind(statement).execute(w.db.pool()).await.unwrap();
    sqlx::query(
        "UPDATE oxsum.customer_plans SET dunning_suspended_at=now() WHERE customer_plan_id=$1",
    )
    .bind(plan)
    .execute(w.db.pool())
    .await
    .unwrap();
    assert_eq!(run(&w, true).await.status, "applied");
    let suspended:bool=sqlx::query_scalar("SELECT dunning_suspended_at IS NOT NULL FROM oxsum.customer_plans WHERE customer_plan_id=$1").bind(plan).fetch_one(w.db.pool()).await.unwrap();
    assert!(suspended);
    assert_eq!(
        w.db.plan_credit_standing(w.org)
            .await
            .unwrap()
            .unwrap()
            .outstanding_minor,
        USAGE
    );
    w.wallet
        .top_up(
            "correction-admission",
            100,
            OffsetDateTime::now_utc().date(),
        )
        .await
        .unwrap();
    assert!(matches!(
        w.wallet
            .hold(
                "correction-hold",
                "retained debt",
                1,
                OffsetDateTime::now_utc().date()
            )
            .await,
        Err(WalletError::Forbidden(_))
    ));
    w.db.release_statement_review(
        &w.owner,
        statement,
        "void",
        "reconcile retained debt",
        &Uuid::new_v4().to_string(),
    )
    .await
    .unwrap();
    assert_eq!(
        w.db.plan_credit_standing(w.org)
            .await
            .unwrap()
            .unwrap()
            .outstanding_minor,
        0
    );
    assert!(
        w.wallet
            .hold(
                "correction-restored",
                "last debt cleared",
                1,
                OffsetDateTime::now_utc().date()
            )
            .await
            .is_ok()
    );
}

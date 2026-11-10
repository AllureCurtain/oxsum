//! Statement integration tests, running against real PostgreSQL (issue #124).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! A statement is the monthly billing document: `generate` builds a draft out of
//! the period's usage rows, `finalize` issues it with terms and a proof window,
//! and `reconcile` keeps its payment standing a fact of the ledger — repayments
//! on the credit line settle the owed draws oldest first.

use oxsum_core::{
    AdminOrganization, Db, NewUser, PaymentStatus, SecretKey, SettlementKind, StatementStatus,
    UsageRecord, UsageRow, Wallet, WalletError, statement_period,
};
use serde_json::json;
use sqlx::PgPool;
use time::macros::date;
use time::{Date, Duration, OffsetDateTime};
use uuid::Uuid;

const ONE: i64 = 1_000_000;
/// Two closed months to bill: whatever the real date is, August and September
/// 2026 have fully ended.
const AUG: Date = date!(2026 - 08 - 15);
const SEPT: Date = date!(2026 - 09 - 15);
const SEPT_LATE: Date = date!(2026 - 09 - 25);
const PASSWORD: &str = "correct horse battery staple";

fn url() -> Option<String> {
    let _ = dotenvy::dotenv();
    oxsum_testkit::suite_db_url("statements")
}

macro_rules! db_or_skip {
    () => {
        match url() {
            Some(u) => u,
            None => {
                eprintln!("DATABASE_URL not set, skipping");
                return;
            }
        }
    };
}

fn fresh(name: &str) -> String {
    format!("{name}_{}", &Uuid::new_v4().simple().to_string()[..8])
}

fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

/// A registered organization with its wallet: the smallest world a statement
/// test needs.
struct Fixture {
    db: Db,
    org: AdminOrganization,
    wallet: Wallet,
}

async fn fixture(url: &str, name: &str) -> Fixture {
    let pool: PgPool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(url)
        .await
        .expect("connects to PostgreSQL");
    let db = Db::from_pool(pool);
    db.migrate().await.expect("migrates");
    let registration = db
        .register(NewUser {
            email: format!("{}@example.com", fresh(name)),
            password: PASSWORD.to_owned(),
            organization_name: None,
        })
        .await
        .expect("registers");
    let org = db
        .organization_by_id(registration.organization.id)
        .await
        .expect("the organization reads back");
    let wallet = Wallet::open(db.pool().clone(), &org.tenant_id)
        .await
        .expect("the wallet opens");
    Fixture { db, org, wallet }
}

/// One billed turn: a hold, a settlement, and the usage row the settlement's
/// writer would leave — the inputs a statement aggregates.
async fn turn(
    f: &Fixture,
    request: &str,
    channel: &str,
    model: &str,
    freeze: i64,
    charged: i64,
    on: Date,
) {
    let hold_key = format!("hold-{request}");
    f.wallet
        .hold(&hold_key, "", freeze, on)
        .await
        .expect("the hold reserves");
    let receipt = f
        .wallet
        .settle(&hold_key, "", charged, on)
        .await
        .expect("the turn settles");
    f.db.record_usage(&usage_row(
        f,
        request,
        channel,
        model,
        freeze,
        charged,
        *receipt.entry_id.as_uuid(),
    ))
    .await
    .expect("the usage row is written");
}

fn usage_row(
    f: &Fixture,
    request: &str,
    channel: &str,
    model: &str,
    freeze: i64,
    charged: i64,
    entry_id: Uuid,
) -> UsageRow {
    UsageRow {
        // The usage table deduplicates on request_id globally, so a fixed
        // id a previous run wrote would silently skip this insert.
        request_id: format!("{request}-{}", uuid::Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id: None,
        mandate_id: None,
        model: model.to_owned(),
        channel: channel.to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id,
        status: "settled".to_owned(),
        failure_reason: None,
        usage: UsageRecord {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        counts: oxsum_core::CountsProvenance::Provider {
            input: 100,
            output: 50,
        },
        charged_minor: charged,
        freeze_minor: freeze,
        upstream_cost_minor: None,
        cost_basis: oxsum_core::CostBasis::Missing,
        upstream_attempts: 1,
    }
}

#[tokio::test]
async fn a_statement_aggregates_the_closed_month() {
    let url = db_or_skip!();
    let f = fixture(&url, "agg").await;
    f.wallet
        .top_up("seed", 100 * ONE, AUG)
        .await
        .expect("the seed top-up books");
    turn(&f, "r1", "openai", "gpt-4o", 5 * ONE, 3 * ONE, SEPT).await;
    turn(&f, "r2", "openai", "gpt-4o", 5 * ONE, 2 * ONE, SEPT_LATE).await;
    turn(&f, "r3", "anthropic", "claude", 5 * ONE, 4 * ONE, SEPT).await;
    // August's turn belongs to another period and is not itemized.
    turn(&f, "r4", "openai", "gpt-4o", 10 * ONE, 9 * ONE, AUG).await;

    let period = statement_period("2026-09").expect("the month parses");
    let statement =
        f.db.generate_statement(&f.org, &f.wallet, &period)
            .await
            .expect("the draft generates")
            .expect("a statement exists");
    assert_eq!(statement.status, StatementStatus::Draft);
    assert_eq!(statement.period, "2026-09");
    assert_eq!(statement.organization_id, f.org.id);
    assert_eq!(statement.total_minor, 9 * ONE);
    assert_eq!(statement.credit_drawn_minor, 0);
    assert_eq!(statement.entry_count, 3);
    assert!(statement.log_from_index.is_none());
    assert!(statement.due_date.is_none());

    let lines =
        f.db.statement_lines(statement.id)
            .await
            .expect("the lines read");
    assert_eq!(lines.len(), 2);
    // Ordered by (channel, model): anthropic before openai.
    assert_eq!(lines[0].channel, "anthropic");
    assert_eq!(lines[0].model, "claude");
    assert_eq!(lines[0].turns, 1);
    assert_eq!(lines[0].amount_minor, 4 * ONE);
    assert_eq!(lines[1].channel, "openai");
    assert_eq!(lines[1].turns, 2);
    assert_eq!(lines[1].input_tokens, 200);
    assert_eq!(lines[1].output_tokens, 100);
    assert_eq!(lines[1].amount_minor, 5 * ONE);

    // Regeneration is the same document, rebuilt — the row id stands.
    let regenerated =
        f.db.generate_statement(&f.org, &f.wallet, &period)
            .await
            .expect("regenerates")
            .expect("the statement stands");
    assert_eq!(regenerated.id, statement.id);
    assert_eq!(regenerated.total_minor, 9 * ONE);

    // The discovery list contains this organization among the billable ones.
    let billable = f.db.billable_organizations().await.expect("the list reads");
    assert!(billable.iter().any(|o| o.id == f.org.id));
}

#[tokio::test]
async fn a_running_or_malformed_month_is_refused() {
    assert!(statement_period("not-a-month").is_err());
    assert!(statement_period("2026-13").is_err());
    let running = format!("{:04}-{:02}", today().year(), u8::from(today().month()));
    let err = statement_period(&running).unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "{err:?}");
}

#[tokio::test]
async fn an_organization_with_no_usage_gets_no_statement() {
    let url = db_or_skip!();
    let f = fixture(&url, "empty").await;
    let period = statement_period("2026-09").unwrap();
    assert!(
        f.db.generate_statement(&f.org, &f.wallet, &period)
            .await
            .expect("generates")
            .is_none(),
        "no usage in the period, no document"
    );
    assert!(
        f.db.statement_by_id(Uuid::new_v4())
            .await
            .expect("reads")
            .is_none()
    );
}

#[tokio::test]
async fn finalizing_locks_lines_terms_and_the_proof_window() {
    let url = db_or_skip!();
    let f = fixture(&url, "fin").await;
    // A credit line so the turn draws on it: the statement then owes something.
    f.wallet
        .set_credit_limit("cl-1", 10 * ONE, AUG)
        .await
        .expect("the limit grants");
    turn(&f, "r1", "openai", "gpt-4o", 8 * ONE, 3 * ONE, SEPT).await;

    f.db.set_payment_terms(f.org.id, 45)
        .await
        .expect("the terms write");
    let org = f.db.organization_by_id(f.org.id).await.unwrap();
    let period = statement_period("2026-09").unwrap();
    let draft =
        f.db.generate_statement(&org, &f.wallet, &period)
            .await
            .unwrap()
            .expect("the draft generates");
    assert_eq!(draft.status, StatementStatus::Draft);

    let finalized =
        f.db.finalize_statement(draft.id, &f.wallet, today())
            .await
            .expect("the draft finalizes");
    assert_eq!(finalized.status, StatementStatus::Finalized);
    assert_eq!(finalized.payment_terms_days, 45);
    assert_eq!(finalized.due_date, Some(today() + Duration::days(45)));
    assert_eq!(finalized.credit_drawn_minor, 3 * ONE);
    assert_eq!(finalized.outstanding_minor, 3 * ONE);
    assert_eq!(finalized.payment_status, PaymentStatus::Pending);
    assert!(finalized.finalized_at.is_some());
    assert!(finalized.log_from_index.is_some());
    assert_eq!(finalized.log_from_index, finalized.log_to_index);

    // Issuing is idempotent: a second finalize answers the standing document.
    let again =
        f.db.finalize_statement(draft.id, &f.wallet, today())
            .await
            .expect("the standing statement answers");
    assert_eq!(again.id, finalized.id);
    assert_eq!(again.finalized_at, finalized.finalized_at);

    // Drafts never reach the organization's list; this one is issued now.
    let mine =
        f.db.organization_statements(f.org.id)
            .await
            .expect("the list reads");
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].id, finalized.id);

    // The admin's filter reads by organization and by period.
    let by_period =
        f.db.statements(None, Some("2026-09"))
            .await
            .expect("the admin list reads");
    assert!(by_period.iter().any(|s| s.id == finalized.id));
    let none_for_august =
        f.db.statements(Some(f.org.id), Some("2026-08"))
            .await
            .expect("the admin list reads");
    assert!(none_for_august.is_empty());
}

#[tokio::test]
async fn a_statement_owing_nothing_is_paid_at_issue() {
    let url = db_or_skip!();
    let f = fixture(&url, "zerodue").await;
    f.wallet
        .top_up("seed", 50 * ONE, AUG)
        .await
        .expect("the seed top-up books");
    turn(&f, "r1", "openai", "gpt-4o", 5 * ONE, 3 * ONE, SEPT).await;
    let period = statement_period("2026-09").unwrap();
    let draft =
        f.db.generate_statement(&f.org, &f.wallet, &period)
            .await
            .unwrap()
            .unwrap();
    let finalized =
        f.db.finalize_statement(draft.id, &f.wallet, today())
            .await
            .expect("the draft finalizes");
    // Own-pool usage leaves nothing to collect: issued already paid.
    assert_eq!(finalized.credit_drawn_minor, 0);
    assert_eq!(finalized.payment_status, PaymentStatus::Paid);
    assert!(finalized.paid_at.is_some());
}

#[tokio::test]
async fn repayments_cover_statements_oldest_first() {
    let url = db_or_skip!();
    let f = fixture(&url, "fifo").await;
    f.wallet
        .set_credit_limit("cl-1", 20 * ONE, AUG)
        .await
        .expect("the limit grants");
    // August draws 4 on the line, September draws 3.
    turn(&f, "aug", "openai", "gpt-4o", 5 * ONE, 4 * ONE, AUG).await;
    turn(&f, "sept", "openai", "gpt-4o", 5 * ONE, 3 * ONE, SEPT).await;

    let august =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-08").unwrap())
            .await
            .unwrap()
            .unwrap();
    let september =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-09").unwrap())
            .await
            .unwrap()
            .unwrap();
    let august =
        f.db.finalize_statement(august.id, &f.wallet, today())
            .await
            .unwrap();
    let september =
        f.db.finalize_statement(september.id, &f.wallet, today())
            .await
            .unwrap();
    assert_eq!(august.outstanding_minor, 4 * ONE);
    assert_eq!(september.outstanding_minor, 3 * ONE);

    // A top-up of 5 repays the line: August's 4 is covered first, and the last 1
    // lands on September.
    f.wallet
        .top_up("pay-1", 5 * ONE, today())
        .await
        .expect("the payment books");
    f.db.reconcile_statements(f.org.id, &f.wallet, today())
        .await
        .expect("the book reconciles");
    let august = f.db.statement_by_id(august.id).await.unwrap().unwrap();
    let september = f.db.statement_by_id(september.id).await.unwrap().unwrap();
    assert_eq!(august.payment_status, PaymentStatus::Paid);
    assert_eq!(august.paid_minor, 4 * ONE);
    assert!(august.paid_at.is_some());
    assert_eq!(september.payment_status, PaymentStatus::Pending);
    assert_eq!(september.paid_minor, ONE);
    assert_eq!(september.outstanding_minor, 2 * ONE);

    // The cap a payment on September's statement answers to: the credit-line
    // debt outstanding through its period — August's share already settled.
    let owed =
        f.db.statement_debt_through(&f.wallet, "2026-09")
            .await
            .expect("the debt reads");
    assert_eq!(owed, 2 * ONE);

    // Paying the rest settles September too.
    f.wallet
        .top_up("pay-2", 2 * ONE, today())
        .await
        .expect("the payment books");
    f.db.reconcile_statements(f.org.id, &f.wallet, today())
        .await
        .expect("the book reconciles");
    let september = f.db.statement_by_id(september.id).await.unwrap().unwrap();
    assert_eq!(september.payment_status, PaymentStatus::Paid);
    assert_eq!(september.paid_minor, 3 * ONE);
}

#[tokio::test]
async fn a_past_due_statement_flips_overdue_lazily() {
    let url = db_or_skip!();
    let f = fixture(&url, "overdue").await;
    f.wallet
        .set_credit_limit("cl-1", 10 * ONE, AUG)
        .await
        .expect("the limit grants");
    turn(&f, "r1", "openai", "gpt-4o", 5 * ONE, 3 * ONE, SEPT).await;
    let draft =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-09").unwrap())
            .await
            .unwrap()
            .unwrap();
    let finalized =
        f.db.finalize_statement(draft.id, &f.wallet, today())
            .await
            .unwrap();
    assert_eq!(finalized.payment_status, PaymentStatus::Pending);

    // Reconciling on a date past the due date flips it — the standing is a fact
    // of the calendar, persisted on first read.
    let past_due = today() + Duration::days(31);
    f.db.reconcile_statements(f.org.id, &f.wallet, past_due)
        .await
        .unwrap();
    let statement = f.db.statement_by_id(finalized.id).await.unwrap().unwrap();
    assert_eq!(statement.payment_status, PaymentStatus::Overdue);
    assert!(statement.overdue_at.is_some());
}

#[tokio::test]
async fn a_suspended_statement_still_pays_off() {
    let url = db_or_skip!();
    let f = fixture(&url, "suspend").await;
    f.wallet
        .set_credit_limit("cl-1", 10 * ONE, AUG)
        .await
        .expect("the limit grants");
    turn(&f, "r1", "openai", "gpt-4o", 5 * ONE, 3 * ONE, SEPT).await;
    let draft =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-09").unwrap())
            .await
            .unwrap()
            .unwrap();

    // A draft cannot be suspended — it is not issued yet.
    let err = f.db.suspend_statement(draft.id).await.unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "{err:?}");

    let finalized =
        f.db.finalize_statement(draft.id, &f.wallet, today())
            .await
            .unwrap();
    let suspended =
        f.db.suspend_statement(finalized.id)
            .await
            .expect("the statement suspends");
    assert_eq!(suspended.payment_status, PaymentStatus::Suspended);
    assert!(suspended.suspended_at.is_some());
    // Suspending twice answers the standing document, not an error.
    let again = f.db.suspend_statement(finalized.id).await.unwrap();
    assert_eq!(again.payment_status, PaymentStatus::Suspended);

    // The repayment still lands: enough repaid turns the suspended bill paid.
    f.wallet
        .top_up("pay", 3 * ONE, today())
        .await
        .expect("the payment books");
    f.db.reconcile_statements(f.org.id, &f.wallet, today())
        .await
        .unwrap();
    let paid = f.db.statement_by_id(finalized.id).await.unwrap().unwrap();
    assert_eq!(paid.payment_status, PaymentStatus::Paid);
    assert_eq!(paid.paid_minor, 3 * ONE);

    // A paid statement cannot be suspended.
    let err = f.db.suspend_statement(finalized.id).await.unwrap_err();
    assert!(matches!(err, WalletError::InvalidInput(_)), "{err:?}");
}

#[tokio::test]
async fn debt_through_a_period_counts_earlier_unbilled_draws() {
    let url = db_or_skip!();
    let f = fixture(&url, "debtcap").await;
    f.wallet
        .set_credit_limit("cl-1", 20 * ONE, AUG)
        .await
        .expect("the limit grants");
    // August drew 9 and was never billed; September's statement covers 7.
    turn(&f, "a1", "openai", "gpt-4o", 10 * ONE, 9 * ONE, AUG).await;
    turn(&f, "s1", "openai", "gpt-4o", 10 * ONE, 7 * ONE, SEPT).await;
    let draft =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-09").unwrap())
            .await
            .unwrap()
            .unwrap();
    f.db.finalize_statement(draft.id, &f.wallet, today())
        .await
        .unwrap();

    // The cap is the line's debt through the period, not just what was billed.
    let owed =
        f.db.statement_debt_through(&f.wallet, "2026-09")
            .await
            .unwrap();
    assert_eq!(owed, 16 * ONE);

    // A repayment of the billed slice settles August first: the statement
    // stands pending until the earlier draws are covered too.
    f.wallet
        .top_up("pay-1", 7 * ONE, today())
        .await
        .expect("the payment books");
    f.db.reconcile_statements(f.org.id, &f.wallet, today())
        .await
        .unwrap();
    let statement = f.db.statement_by_id(draft.id).await.unwrap().unwrap();
    assert_eq!(statement.payment_status, PaymentStatus::Pending);
    assert_eq!(statement.paid_minor, 0);
    let owed =
        f.db.statement_debt_through(&f.wallet, "2026-09")
            .await
            .unwrap();
    assert_eq!(owed, 9 * ONE);
}

/// Statement lifecycle events (issue #200): `statement.finalized` lands inside
/// finalization, the due-date flip enqueues `payment_overdue` and the standing
/// `payment_status_updated`, a repayment turning it `paid` announces the same,
/// and `suspend` names its standing.
#[tokio::test]
async fn statement_lifecycle_enqueues_its_events() {
    let url = db_or_skip!();
    let f = fixture(&url, "stmtwh").await;
    let key = SecretKey::from_bytes([9; 32]);
    let endpoint =
        f.db.create_webhook(
            f.org.id,
            "https://receiver.example/hook",
            &[
                "statement.finalized".to_owned(),
                "statement.payment_overdue".to_owned(),
                "statement.payment_status_updated".to_owned(),
            ],
            &key,
        )
        .await
        .unwrap();

    f.wallet
        .set_credit_limit("cl-1", 10 * ONE, AUG)
        .await
        .expect("the limit grants");
    turn(&f, "r1", "openai", "gpt-4o", 5 * ONE, 3 * ONE, SEPT).await;
    let draft =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-09").unwrap())
            .await
            .unwrap()
            .unwrap();
    f.db.finalize_statement(draft.id, &f.wallet, today())
        .await
        .unwrap();

    let read_events = || async {
        f.db.webhook_deliveries(f.org.id, endpoint.endpoint.id)
            .await
            .unwrap()
            .unwrap()
            .iter()
            .map(|d| d.event_type.clone())
            .collect::<Vec<_>>()
    };
    let events = read_events().await;
    assert!(
        events.iter().any(|e| e == "statement.finalized"),
        "finalized enqueued: {events:?}"
    );

    // Past due: the reconcile flips it and announces both the specific and the
    // generic standing event.
    let past_due = today() + Duration::days(31);
    f.db.reconcile_statements(f.org.id, &f.wallet, past_due)
        .await
        .unwrap();
    let events = read_events().await;
    assert!(
        events.iter().any(|e| e == "statement.payment_overdue"),
        "overdue enqueued: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e == "statement.payment_status_updated"),
        "the standing update enqueued: {events:?}"
    );
    let overdue: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM oxsum.webhook_deliveries d \
         JOIN oxsum.webhook_endpoints e ON e.endpoint_id = d.endpoint_id \
         WHERE d.event_type = 'statement.payment_overdue' AND e.endpoint_id = $1 \
         ORDER BY d.created_at DESC LIMIT 1",
    )
    .bind(endpoint.endpoint.id)
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    assert_eq!(overdue["data"]["statementId"], json!(draft.id));
    assert_eq!(overdue["data"]["period"], "2026-09");

    // A second reconcile on the same date repeats no event — nothing changed.
    let before = events.len();
    f.db.reconcile_statements(f.org.id, &f.wallet, past_due)
        .await
        .unwrap();
    assert_eq!(read_events().await.len(), before, "no change, no event");

    // Repaying the line turns it paid — another standing update.
    f.wallet
        .top_up("pay", 3 * ONE, today())
        .await
        .expect("the payment books");
    f.db.reconcile_statements(f.org.id, &f.wallet, past_due)
        .await
        .unwrap();
    let events = read_events().await;
    let paid_updates = events
        .iter()
        .filter(|e| *e == "statement.payment_status_updated")
        .count();
    assert_eq!(paid_updates, 2, "overdue then paid: {events:?}");
}

async fn monthly_draft(f: &Fixture) -> oxsum_core::Statement {
    f.wallet
        .set_credit_limit("credit", 10 * ONE, AUG)
        .await
        .unwrap();
    turn(f, "initial", "openai", "model", 5 * ONE, 3 * ONE, SEPT).await;
    f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-09").unwrap())
        .await
        .unwrap()
        .unwrap()
}

async fn finalized_events(f: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM oxsum.webhook_deliveries WHERE organization_id=$1 AND event_type='statement.finalized'")
        .bind(f.org.id).fetch_one(f.db.pool()).await.unwrap()
}

async fn subscribe(f: &Fixture) {
    f.db.create_webhook(
        f.org.id,
        "https://receiver.example/hook",
        &["statement.finalized".to_owned()],
        &SecretKey::from_bytes([8; 32]),
    )
    .await
    .unwrap();
}

async fn wait_for_blocked(pool: &PgPool, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)))")
                .bind(pid).fetch_one(pool).await.unwrap();
            if blocked {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("writer reaches controlled database lock");
}

#[tokio::test]
async fn issuance_rolls_back_anchor_document_lines_and_event_on_failure() {
    let url = db_or_skip!();
    let f = fixture(&url, "atomic_failure").await;
    subscribe(&f).await;
    let draft = monthly_draft(&f).await;
    // A changed working source makes rollback of both the document and its
    // replacement lines observable, even when enqueue fails last.
    turn(&f, "later", "other", "later", 2 * ONE, ONE, SEPT).await;
    let before = serde_json::to_value(f.db.statement_lines(draft.id).await.unwrap()).unwrap();
    let size = f.wallet.log_size().await.unwrap();
    for (table, condition) in [
        (
            "statements",
            format!("NEW.statement_id='{}' AND NEW.status='finalized'", draft.id),
        ),
        (
            "webhook_deliveries",
            format!(
                "NEW.organization_id='{}' AND NEW.event_type='statement.finalized'",
                f.org.id
            ),
        ),
    ] {
        let name = fresh("fail_monthly");
        sqlx::raw_sql(&format!("CREATE FUNCTION oxsum.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF {condition} THEN RAISE EXCEPTION 'injected monthly issuance failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {name} BEFORE INSERT OR UPDATE ON oxsum.{table} FOR EACH ROW EXECUTE FUNCTION oxsum.{name}();"))
            .execute(f.db.pool()).await.unwrap();
        let result = f.db.finalize_statement(draft.id, &f.wallet, today()).await;
        sqlx::raw_sql(&format!(
            "DROP TRIGGER {name} ON oxsum.{table}; DROP FUNCTION oxsum.{name}();"
        ))
        .execute(f.db.pool())
        .await
        .unwrap();
        assert!(result.is_err(), "injected failure must escape");
        let retained = f.db.statement_by_id(draft.id).await.unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(&retained).unwrap(),
            serde_json::to_value(&draft).unwrap()
        );
        assert_eq!(
            serde_json::to_value(f.db.statement_lines(draft.id).await.unwrap()).unwrap(),
            before
        );
        assert_eq!(f.wallet.log_size().await.unwrap(), size, "no orphan anchor");
        assert_eq!(finalized_events(&f).await, 0);
    }
    let (left, right) = tokio::join!(
        f.db.finalize_statement(draft.id, &f.wallet, today()),
        f.db.finalize_statement(draft.id, &f.wallet, today())
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(
        serde_json::to_value(&left).unwrap(),
        serde_json::to_value(&right).unwrap()
    );
    assert_eq!(left.total_minor, 4 * ONE);
    assert_eq!(f.wallet.log_size().await.unwrap(), size + 1);
    assert_eq!(finalized_events(&f).await, 1);
}

#[tokio::test]
async fn issuance_uses_one_connection_and_preserves_issued_history() {
    let url = db_or_skip!();
    let f = fixture(&url, "monthly_single").await;
    let draft = monthly_draft(&f).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let db = Db::from_pool(pool.clone());
    let wallet = Wallet::open(pool, &f.org.tenant_id).await.unwrap();
    let issued = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let generated = db
            .generate_statement(&f.org, &wallet, &statement_period("2026-09").unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(generated.id, draft.id);
        db.finalize_statement(draft.id, &wallet, today())
            .await
            .unwrap()
    })
    .await
    .expect("no nested pool checkout");
    let lines = serde_json::to_value(db.statement_lines(issued.id).await.unwrap()).unwrap();
    f.db.set_payment_terms(f.org.id, 90).await.unwrap();
    turn(&f, "after_issue", "new", "new", ONE, ONE, SEPT).await;
    let regenerated = db
        .generate_statement(&f.org, &wallet, &statement_period("2026-09").unwrap())
        .await
        .unwrap()
        .unwrap();
    let repeated = db
        .finalize_statement(issued.id, &wallet, today() + Duration::days(5))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&regenerated).unwrap(),
        serde_json::to_value(&issued).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&repeated).unwrap(),
        serde_json::to_value(&issued).unwrap()
    );
    assert_eq!(
        serde_json::to_value(db.statement_lines(issued.id).await.unwrap()).unwrap(),
        lines
    );
    let foreign = fixture(&url, "monthly_foreign").await;
    assert!(matches!(
        db.finalize_statement(issued.id, &foreign.wallet, today())
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    assert!(matches!(
        db.generate_statement(
            &f.org,
            &foreign.wallet,
            &statement_period("2026-09").unwrap()
        )
        .await,
        Err(WalletError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn legacy_orphan_anchor_requires_exact_source_replay() {
    let url = db_or_skip!();
    let f = fixture(&url, "monthly_orphan").await;
    let draft = monthly_draft(&f).await;
    let first =
        f.db.finalize_statement(draft.id, &f.wallet, today())
            .await
            .unwrap();
    let size = f.wallet.log_size().await.unwrap();
    // Recreate the old split-transaction failure using only the application
    // document. The retained ledger entry is never edited or deleted.
    let reset = "UPDATE oxsum.statements SET status='draft',finalized_at=NULL,anchor_entry_id=NULL,anchor_log_index=NULL,window_hash=NULL,rollup_hash=NULL WHERE statement_id=$1";
    sqlx::query(reset)
        .bind(draft.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    let exact =
        f.db.finalize_statement(draft.id, &f.wallet, today())
            .await
            .unwrap();
    assert_eq!(exact.anchor_entry_id, first.anchor_entry_id);
    assert_eq!(f.wallet.log_size().await.unwrap(), size);
    sqlx::query(reset)
        .bind(draft.id)
        .execute(f.db.pool())
        .await
        .unwrap();
    turn(&f, "late", "new", "new", ONE, ONE, SEPT).await;
    let before = f.wallet.log_size().await.unwrap();
    assert!(matches!(
        f.db.finalize_statement(draft.id, &f.wallet, today()).await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(f.wallet.log_size().await.unwrap(), before);
    assert_eq!(
        f.db.statement_by_id(draft.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        StatementStatus::Draft
    );
}

#[tokio::test]
async fn late_usage_cannot_split_the_lines_totals_and_window_snapshot() {
    let url = db_or_skip!();
    let f = fixture(&url, "monthly_source").await;
    let draft = monthly_draft(&f).await;
    f.wallet.hold("late-hold", "", ONE, SEPT).await.unwrap();
    let receipt = f.wallet.settle("late-hold", "", ONE, SEPT).await.unwrap();
    let late = usage_row(
        &f,
        "late",
        "new",
        "new",
        ONE,
        ONE,
        *receipt.entry_id.as_uuid(),
    );
    let hashes =
        f.db.statement_window_hashes(&f.org.tenant_id, &statement_period("2026-09").unwrap())
            .await
            .unwrap();
    let mut pin = f.db.pool().begin().await.unwrap();
    sqlx::query(&format!(
        "LOCK TABLE ledger_{}.postings IN ACCESS EXCLUSIVE MODE",
        f.org.tenant_id
    ))
    .execute(&mut *pin)
    .await
    .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *pin)
        .await
        .unwrap();
    let db = f.db.clone();
    let wallet = std::sync::Arc::new(f.wallet);
    let worker_wallet = std::sync::Arc::clone(&wallet);
    let task = tokio::spawn(async move {
        db.finalize_statement(draft.id, &worker_wallet, today())
            .await
    });
    wait_for_blocked(f.db.pool(), pid).await;
    f.db.record_usage(&late).await.unwrap();
    pin.commit().await.unwrap();
    let issued = task.await.unwrap().unwrap();
    let lines = f.db.statement_lines(issued.id).await.unwrap();
    assert_eq!(issued.entry_count, 1);
    assert_eq!(issued.total_minor, 3 * ONE);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].amount_minor, issued.total_minor);
    let payload: Vec<u8> = hashes.iter().flat_map(|h| h.as_bytes().to_vec()).collect();
    assert_eq!(
        issued.window_hash,
        Some(doubleentry::Hash::digest(b"oxsum/statement-window/v1", &payload).to_hex())
    );
}

#[tokio::test]
async fn issuance_reads_repayment_that_committed_before_its_append_lock() {
    let url = db_or_skip!();
    let f = fixture(&url, "monthly_repayment").await;
    let draft = monthly_draft(&f).await;
    let mut pin = f.db.pool().begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('oxsum/org-admission/v1:' || $1,0))",
    )
    .bind(f.org.id.to_string())
    .execute(&mut *pin)
    .await
    .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *pin)
        .await
        .unwrap();
    let db = f.db.clone();
    let wallet = std::sync::Arc::new(f.wallet);
    let worker_wallet = std::sync::Arc::clone(&wallet);
    let task = tokio::spawn(async move {
        db.finalize_statement(draft.id, &worker_wallet, today())
            .await
    });
    wait_for_blocked(f.db.pool(), pid).await;
    wallet
        .repay_credit("before-issue", ONE, today())
        .await
        .unwrap();
    pin.commit().await.unwrap();
    let issued = task.await.unwrap().unwrap();
    assert_eq!(issued.credit_drawn_minor, 3 * ONE);
    assert_eq!(issued.paid_minor, ONE);
    assert_eq!(issued.outstanding_minor, 2 * ONE);
}

async fn issued_credit_fixture(url: &str, name: &str) -> (Fixture, Uuid) {
    let f = fixture(url, name).await;
    f.wallet
        .set_credit_limit("credit", 1_000, AUG)
        .await
        .unwrap();
    turn(&f, "aug", "mock", "model", 30, 30, AUG).await;
    turn(&f, "sept", "mock", "model", 70, 70, SEPT).await;
    let draft =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-09").unwrap())
            .await
            .unwrap()
            .unwrap();
    f.db.finalize_statement(draft.id, &f.wallet, today())
        .await
        .unwrap();
    (f, draft.id)
}

#[tokio::test]
async fn monthly_payment_paid_replay_binds_source_and_preserves_fifo() {
    let url = db_or_skip!();
    let (f, id) = issued_credit_fixture(&url, "payment_replay").await;
    let key = fresh("repay");
    let (partial, first) =
        f.db.record_monthly_statement_payment(&f.wallet, id, &key, 30, today())
            .await
            .unwrap();
    assert!(first.is_new);
    assert_eq!(
        partial.paid_minor, 0,
        "earlier unbilled draws take their share"
    );
    let final_key = fresh("final");
    let (paid, receipt) =
        f.db.record_monthly_statement_payment(&f.wallet, id, &final_key, 70, today())
            .await
            .unwrap();
    assert_eq!(paid.payment_status, PaymentStatus::Paid);
    let (again, replay) =
        f.db.record_monthly_statement_payment(&f.wallet, id, &final_key, 70, today())
            .await
            .unwrap();
    assert!(!replay.is_new);
    assert_eq!(receipt.entry_id, replay.entry_id);
    assert_eq!(again.paid_minor, 70);
    assert!(matches!(
        f.db.record_monthly_statement_payment(&f.wallet, id, &final_key, 69, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert!(matches!(
        f.db.record_monthly_statement_payment(&f.wallet, id, &fresh("new"), 1, today())
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    let aug =
        f.db.generate_statement(&f.org, &f.wallet, &statement_period("2026-08").unwrap())
            .await
            .unwrap()
            .unwrap();
    f.db.finalize_statement(aug.id, &f.wallet, today())
        .await
        .unwrap();
    assert!(matches!(
        f.db.record_monthly_statement_payment(&f.wallet, aug.id, &final_key, 70, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oxsum.admin_audit WHERE action='statement.payment' AND target=$1",
    )
    .bind(id.to_string())
    .fetch_one(f.db.pool())
    .await
    .unwrap();
    assert_eq!(count, 2);
    assert_eq!(f.wallet.credit_repaid().await.unwrap(), 100);
    assert_eq!(f.wallet.credit_used().await.unwrap(), 0);
}

#[tokio::test]
async fn monthly_payment_concurrent_repayments_and_topup_never_grant_excess() {
    let url = db_or_skip!();
    let (f, id) = issued_credit_fixture(&url, "payment_race").await;
    let one = fresh("one");
    let two = fresh("two");
    let (a, b) = tokio::join!(
        f.db.record_monthly_statement_payment(&f.wallet, id, &one, 100, today()),
        f.db.record_monthly_statement_payment(&f.wallet, id, &two, 100, today()),
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(f.wallet.credit_repaid().await.unwrap(), 100);
    assert_eq!(f.wallet.available().await.unwrap(), 1_000);

    let (f, id) = issued_credit_fixture(&url, "payment_topup_race").await;
    let one = fresh("payment");
    let (a, b) = tokio::join!(
        f.db.record_monthly_statement_payment(&f.wallet, id, &one, 100, today()),
        f.wallet.top_up("topup", 40, today()),
    );
    b.unwrap();
    if a.is_ok() {
        assert_eq!(f.wallet.credit_repaid().await.unwrap(), 100);
        assert_eq!(
            f.wallet.available().await.unwrap(),
            1_040,
            "only the independent topup may fund wallet"
        );
    } else {
        assert!(matches!(a, Err(WalletError::InvalidInput(_))));
        assert_eq!(f.wallet.credit_repaid().await.unwrap(), 40);
        assert_eq!(f.wallet.available().await.unwrap(), 940);
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.admin_audit WHERE action='statement.payment' AND idempotency_key=$1")
            .bind(&one).fetch_one(f.db.pool()).await.unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn monthly_payment_single_connection_and_legacy_audit_replay() {
    let url = db_or_skip!();
    let (f, id) = issued_credit_fixture(&url, "payment_single").await;
    let key = fresh("legacy");
    let original = f.wallet.top_up(&key, 100, today()).await.unwrap();
    assert!(
        matches!(
            f.db.record_monthly_statement_payment(&f.wallet, id, &key, 100, today())
                .await,
            Err(WalletError::Conflict(_))
        ),
        "unaudited legacy cash cannot claim a document"
    );
    f.db.record_audit(
        oxsum_core::audit_action::STATEMENT_PAYMENT,
        Some(&id.to_string()),
        json!({"amountMinor":100,"entryId":original.entry_id}),
        Some(&key),
    )
    .await
    .unwrap();
    let db = Db::connect(&url, 1).await.unwrap();
    let wallet = Wallet::open(db.pool().clone(), &f.org.tenant_id)
        .await
        .unwrap();
    let (statement, receipt) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        db.record_monthly_statement_payment(&wallet, id, &key, 100, today()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(statement.payment_status, PaymentStatus::Paid);
    assert_eq!(receipt.entry_id, original.entry_id);
    assert!(!receipt.is_new);
    // A fresh posting (not merely a replay) also fits one pool connection.
    turn(
        &f,
        "late-credit",
        "mock",
        "model",
        1,
        1,
        date!(2026 - 10 - 15),
    )
    .await;
    let draft = db
        .generate_statement(&f.org, &wallet, &statement_period("2026-10").unwrap())
        .await
        .unwrap()
        .unwrap();
    db.finalize_statement(draft.id, &wallet, today())
        .await
        .unwrap();
    let (_, fresh) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        db.record_monthly_statement_payment(&wallet, draft.id, &fresh("single-new"), 1, today()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(fresh.is_new);
    db.pool().close().await;
}

#[tokio::test]
async fn monthly_payment_audit_failure_rolls_back_cash_standing_and_key() {
    let url = db_or_skip!();
    let (f, id) = issued_credit_fixture(&url, "payment_rollback").await;
    let key = fresh("fail_audit");
    let function = fresh("fail_monthly_audit");
    sqlx::raw_sql(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.idempotency_key = '{key}' THEN RAISE EXCEPTION 'injected monthly audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {function} BEFORE INSERT ON oxsum.admin_audit FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()"))
        .execute(f.db.pool()).await.unwrap();
    assert!(
        f.db.record_monthly_statement_payment(&f.wallet, id, &key, 100, today())
            .await
            .is_err()
    );
    assert_eq!(f.wallet.credit_repaid().await.unwrap(), 0);
    assert_eq!(
        f.db.statement_by_id(id).await.unwrap().unwrap().paid_minor,
        0
    );
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {function} ON oxsum.admin_audit; DROP FUNCTION oxsum.{function}()"
    ))
    .execute(f.db.pool())
    .await
    .unwrap();
    let (statement, receipt) =
        f.db.record_monthly_statement_payment(&f.wallet, id, &key, 100, today())
            .await
            .unwrap();
    assert!(receipt.is_new);
    assert_eq!(statement.payment_status, PaymentStatus::Paid);
}

#[tokio::test]
async fn monthly_payment_caps_selected_period_and_refuses_foreign_identity() {
    let url = db_or_skip!();
    let (f, id) = issued_credit_fixture(&url, "payment_caps").await;
    // Later debt cannot justify exceeding this statement's closed-period cap.
    turn(&f, "oct", "mock", "model", 50, 50, date!(2026 - 10 - 15)).await;
    let key = fresh("bounded");
    assert!(matches!(
        f.db.record_monthly_statement_payment(&f.wallet, id, &key, 101, today())
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    let (a, b) = tokio::join!(
        f.db.record_monthly_statement_payment(&f.wallet, id, &key, 100, today()),
        f.db.record_monthly_statement_payment(&f.wallet, id, &key, 100, today()),
    );
    let (_, a) = a.unwrap();
    let (_, b) = b.unwrap();
    assert_eq!(a.entry_id, b.entry_id);
    assert_ne!(a.is_new, b.is_new);
    assert_eq!(f.wallet.credit_used().await.unwrap(), 50);
    let (other, other_id) = issued_credit_fixture(&url, "payment_other").await;
    assert!(matches!(
        f.db.record_monthly_statement_payment(&other.wallet, id, &fresh("foreign"), 1, today())
            .await,
        Err(WalletError::InvalidInput(_))
    ));
    assert!(matches!(
        other
            .db
            .record_monthly_statement_payment(&other.wallet, other_id, &key, 100, today())
            .await,
        Err(WalletError::Conflict(_))
    ));
    assert_eq!(other.wallet.credit_used().await.unwrap(), 100);
}

#[tokio::test]
async fn monthly_payment_delivery_failure_rolls_back_audit_and_cash() {
    let url = db_or_skip!();
    let (f, id) = issued_credit_fixture(&url, "payment_event_failure").await;
    f.db.create_webhook(
        f.org.id,
        "https://receiver.example/hook",
        &["statement.payment_status_updated".into()],
        &SecretKey::from_bytes([8; 32]),
    )
    .await
    .unwrap();
    let key = fresh("event_payment");
    let function = fresh("fail_payment_event");
    sqlx::raw_sql(&format!("CREATE FUNCTION oxsum.{function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.organization_id = '{}' THEN RAISE EXCEPTION 'injected monthly event failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {function} BEFORE INSERT ON oxsum.webhook_deliveries FOR EACH ROW EXECUTE FUNCTION oxsum.{function}()", f.org.id))
        .execute(f.db.pool()).await.unwrap();
    assert!(
        f.db.record_monthly_statement_payment(&f.wallet, id, &key, 100, today())
            .await
            .is_err()
    );
    sqlx::raw_sql(&format!(
        "DROP TRIGGER {function} ON oxsum.webhook_deliveries; DROP FUNCTION oxsum.{function}()"
    ))
    .execute(f.db.pool())
    .await
    .unwrap();
    assert_eq!(f.wallet.credit_repaid().await.unwrap(), 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.admin_audit WHERE action='statement.payment' AND idempotency_key=$1")
        .bind(&key).fetch_one(f.db.pool()).await.unwrap();
    assert_eq!(
        count, 0,
        "audit inserted before the failed enqueue rolls back too"
    );
    let (paid, receipt) =
        f.db.record_monthly_statement_payment(&f.wallet, id, &key, 100, today())
            .await
            .unwrap();
    assert_eq!(paid.payment_status, PaymentStatus::Paid);
    assert!(receipt.is_new);
    f.db.record_monthly_statement_payment(&f.wallet, id, &key, 100, today())
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM oxsum.webhook_deliveries WHERE organization_id=$1 AND event_type='statement.payment_status_updated'")
        .bind(f.org.id).fetch_one(f.db.pool()).await.unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
async fn monthly_payment_rechecks_cap_after_waiting_for_a_queued_topup() {
    use sha2::{Digest, Sha256};

    let url = db_or_skip!();
    let (f, id) = issued_credit_fixture(&url, "payment_queued_topup").await;
    let f = std::sync::Arc::new(f);
    let mut hasher = Sha256::new();
    hasher.update(b"oxsum/credit-limit/v1\0");
    hasher.update(f.org.tenant_id.as_bytes());
    let bytes = hasher.finalize();
    let lock = i64::from_le_bytes(bytes[..8].try_into().unwrap());
    let mut blocker = f.db.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let topup_fixture = f.clone();
    let topup = tokio::spawn(async move {
        topup_fixture
            .wallet
            .top_up("queued-topup", 40, today())
            .await
    });
    wait_for_blocked(f.db.pool(), pid).await;
    let payment_fixture = f.clone();
    let key = fresh("queued-payment");
    let payment = tokio::spawn(async move {
        payment_fixture
            .db
            .record_monthly_statement_payment(&payment_fixture.wallet, id, &key, 100, today())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiters: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid))")
                .bind(pid).fetch_one(f.db.pool()).await.unwrap();
            if waiters >= 2 { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("both repayment paths are queued behind the held credit lock");
    blocker.commit().await.unwrap();
    topup.await.unwrap().unwrap();
    assert!(matches!(
        payment.await.unwrap(),
        Err(WalletError::InvalidInput(_))
    ));
    assert_eq!(f.wallet.credit_repaid().await.unwrap(), 40);
    assert_eq!(f.wallet.credit_used().await.unwrap(), 60);
    assert_eq!(f.wallet.available().await.unwrap(), 940);
}

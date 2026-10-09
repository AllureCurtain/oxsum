//! Statement integration tests, running against real PostgreSQL (issue #124).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it the tests skip
//! instead of failing, so a bare `cargo test` still passes.
//!
//! A statement is the monthly billing document: `generate` builds a draft out of
//! the period's usage rows, `finalize` issues it with terms and a proof window,
//! and `reconcile` keeps its payment standing a fact of the ledger — repayments
//! on the credit line settle the owed draws oldest first.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use oxsum_core::{
    AdminOrganization, Db, NewUser, PaymentStatus, SettlementKind, StatementStatus, UsageRecord,
    UsageRow, Wallet, WalletError, statement_period,
};
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
    std::env::var("DATABASE_URL").ok()
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
    f.db.record_usage(&UsageRow {
        // The usage table deduplicates on request_id globally, so a fixed
        // id a previous run wrote would silently skip this insert.
        request_id: format!("{request}-{}", uuid::Uuid::new_v4().simple()),
        tenant_id: f.org.tenant_id.clone(),
        key_id: None,
        model: model.to_owned(),
        channel: channel.to_owned(),
        price_version: 1,
        kind: SettlementKind::Usage,
        entry_id: *receipt.entry_id.as_uuid(),
        usage: UsageRecord {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        charged_minor: charged,
        freeze_minor: freeze,
        upstream_cost_minor: None,
        upstream_attempts: 1,
    })
    .await
    .expect("the usage row is written");
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
    assert!(matches!(err, WalletError::InvalidInput(_)));
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
    assert!(matches!(err, WalletError::InvalidInput(_)));

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
    assert!(matches!(err, WalletError::InvalidInput(_)));
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

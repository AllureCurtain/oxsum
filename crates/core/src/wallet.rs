//! The wallet facade: holds, settlements, receipts and ledger reads.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use doubleentry::account::AccountRegistry;
use doubleentry::storage::postgres::{PostgresError, PostgresStore};
use doubleentry::{
    AccountId, Amount, BalanceKey, BalanceLimit, BalanceQuery, Balanced, Currency, Cursor,
    Description, Direction, DocumentRef, Draft, Entry, EntryBatch, EntryId, Hash, IdempotencyKey,
    Layer, LedgerId, LedgerPolicy, LedgerStore, LogIndex, Period, PeriodId, PeriodState, Posting,
    Provenance, Seal, SealContext, StoredEntry,
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::macros::date;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use crate::billing::{Settlement, SettlementKind, SettlementRecord};
use crate::entitlement::{EntitlementMandate, EntitlementRead};
use crate::error::{WalletError, invalid};
use crate::evidence::{EVIDENCE_BUNDLE_DOCUMENT, EvidenceEntryBundle};
use crate::heads::{
    Consistency, HeadSigningKey, STATEMENT_ANCHOR_DOCUMENT, SignedHead, USAGE_ANCHOR_DOCUMENT,
    USAGE_REPORT_DOCUMENT, canonical_price_row, origin_for, sign_head, sign_price_head,
    sign_sale_checkpoint,
};
use crate::keys::{ActingKey, BudgetDuration};
use crate::mandates::PresentedMandate;
use crate::proof::ProofBundle;
use crate::usage::UsageRecord;
use crate::usage_reports::{ReportEntryBundle, UsageReportRow};

/// Money precision: 6 decimal places; 1 credit = 1_000_000 minor, fine enough for per-token pricing.
///
/// Defined in `oxsum-verify`, the crate the browser page shares: the entry encoding —
/// and therefore the content hash — depends on it, so writer and verifier use the one
/// definition.
pub use oxsum_verify::SCALE;
/// A credit amount at the ledger's six-decimal scale; 1 credit = 1_000_000
/// minor units.
pub type Credits = Amount<SCALE>;

/// Account opening date. Every posting date must not be earlier than this.
const OPENED: Date = date!(2026 - 01 - 01);

pub(crate) const WALLET: &str = "Liabilities:Wallet";
/// Granted credit — the signup bonus and admin grants — kept in its own pool so
/// settlement can draw it before the purchased balance (decision D1, see
/// docs/decisions.md). Equity, because the platform contributes it and no cash
/// ever stood behind it.
pub(crate) const BONUS: &str = "Equity:Bonus";
/// Promotional credit — the grant pool the `oxsum.promo_lots` rows project
/// (issue #609). A promo lot is a grant with a provenance and an optional
/// expiry, so its money spends before every other pool: it is the balance that
/// can be lost. Equity like the bonus pool, and never refundable — expiry and
/// correction burn it to [`PROMO_EXPIRY`], never back to cash.
pub(crate) const PROMO: &str = "Equity:Promo";
/// Where expired or cancelled promo credit goes: `debit Promo / credit
/// PromoExpiry` is the net-zero marker that records the lapse beside the grant
/// — the amount leaves the spendable pool and can never return to one.
pub(crate) const PROMO_EXPIRY: &str = "Equity:PromoExpiry";
const CASH: &str = "Assets:Cash";
const REVENUE: &str = "Income:Usage";
/// Operator-side money that is neither cash received nor usage revenue: admin
/// adjustments and the signup bonus draw on it (a grant debits it, a deduction
/// credits it back). Equity, so the books keep it out of both income and assets.
const ADJUSTMENTS: &str = "Equity:Adjustments";
/// The drawable part of the organization's credit line — a liability like the
/// wallet, funded by the facility the operator grants and drawn after the bonus
/// and purchased pools exhaust (decision D4, see docs/decisions.md). It carries
/// the same `FundedReservations` rule the other pools do, so a hold on credit
/// reserves against what the line really carries and a release cannot invent
/// room the line never gave.
pub(crate) const CREDIT_LINE: &str = "Liabilities:CreditLine";
/// The credit facility committed to the organization, kept gross: the account's
/// debit balance is the granted limit, and a shrink debits `CreditLine` back
/// against it, so the ledger itself refuses a limit cut below the outstanding
/// draw. A draw never touches it — it moves between `CreditLine` and
/// `Revenue` — so what the organization owes is the limit minus the line's
/// balance, not a separate counter that could drift.
pub(crate) const FACILITY: &str = "Assets:CreditFacility";
/// What customers owe on billed plan fees — the asset a posted
/// `oxsum.invoice_journals` row creates (issue #611, T20). Charge lines debit
/// it; discounts, promo coverage and order payments credit it back to zero. A
/// journal's open receivable is this account's balance on the journal's legs.
pub(crate) const RECEIVABLES: &str = "Assets:Receivables";
/// Plan-fee revenue — base fees and usage overage billed at period close,
/// kept off `Income:Usage` so metering income and subscription income report
/// separately.
pub(crate) const PLAN_FEES: &str = "Income:PlanFees";
/// Contra plan-fee revenue: discounts and refund corrections book here rather
/// than netting inside `Income:PlanFees`, so the gross fee a journal billed
/// and the corrections it took stay separately provable.
pub(crate) const DISCOUNTS: &str = "Income:Discounts";
/// Plan-fee money collected ahead of the period it buys — the deferred-revenue
/// liability a paid `planPeriod` order books at activation and the period
/// close reclassifies into the receivable.
pub(crate) const UNEARNED_PLAN_FEES: &str = "Liabilities:UnearnedPlanFees";
/// Money the business owes a customer — a voided paid journal parks the
/// settlement here until the refund rail carries it out.
pub(crate) const REFUND_PAYABLE: &str = "Liabilities:RefundPayable";

/// The idempotency key under which the one-time pool migration posts its
/// `debit Wallet, credit Bonus` entry. Deterministic, so a second
/// reclassification run sees the entry and does nothing, and the key doubles as
/// the marker spend reads exclude: the move is a rebalancing between pools, not
/// a charge, so it must not count as settled wallet spend.
const RECLASS_KEY: &str = "pool-reclassification";

/// The idempotency key under which the promo-lots migration (issue #609) posts
/// its `debit Bonus, credit Promo` entry — the aggregate legacy bonus moving
/// into a `legacy_migration` lot. Fixed like `RECLASS_KEY`: a re-open finds
/// the entry and does nothing, and spend reads exclude the rebalancing the
/// same way.
const LEGACY_PROMO_KEY: &str = "legacy-promo-migration";

/// How many times a pool-splitting write recomputes its split before giving up.
/// Each retry re-reads pool balances, so a loser only loops while racing appends
/// keep moving the pools under it; a write the combined balance genuinely cannot
/// cover fails every attempt anyway and surfaces the same refusal as before.
const POOL_SPLIT_ATTEMPTS: usize = 3;

/// How many of one key's holds may be inside the locked transaction at once —
/// the width of the in-process queue [`Wallet::key_queue`] admits by (issue
/// #486). One is the honest width: the per-key advisory lock serializes the
/// holds anyway, so a second admitted hold can only wait inside the transaction
/// — holding its pooled connection — for exactly the connection-pinning the
/// queue exists to prevent. The width lives in the semaphore's permit count,
/// never as a second copy.
const KEY_QUEUE_WIDTH: usize = 1;

/// How long a hold may wait in the in-process queue before refusing fast rather
/// than queueing longer than the pool's own acquire timeout would have let it.
/// The queued hold costs nothing — no connection — so the budget is the
/// caller's patience, not the pool's; it matches `ACQUIRE_TIMEOUT` so a refused
/// queue-wait surfaces on the same timescale a refused checkout did.
const KEY_QUEUE_WAIT: Duration = Duration::from_secs(5);

/// How many times `open` re-reads the account table after a racing opener
/// committed it mid-bootstrap. The winner's write is one statement batch, so a
/// loser only loops while racers keep landing in between.
const OPEN_RACE_ATTEMPTS: usize = 3;

/// How many times a `hold` may lose the ledger's append lock to a racing writer
/// before refusing `PoolBusy`: each miss drops the transaction's connection and
/// sleeps `LOCK_WAIT_BACKOFF` off the pool, so the count times the patience
/// window, not the contention depth. Sized so a queued burst drains — a busy
/// ledger appends in milliseconds — rather than on the pool's acquire timeout.
const LOCK_WAIT_ATTEMPTS: usize = 250;

/// How long a hold that lost the append-lock claim sleeps before re-checking —
/// async time, so the wait costs no connection.
const LOCK_WAIT_BACKOFF: Duration = Duration::from_millis(20);

/// How one amount divides across the funded pools — promo first (it is the
/// money that can expire), then the bonus pool, the purchased wallet, and the
/// credit line last. Any side may be zero; the parts always sum to the amount
/// the split was computed for.
#[derive(Clone, Copy)]
struct PoolSplit {
    promo: i64,
    bonus: i64,
    wallet: i64,
    credit: i64,
}

impl PoolSplit {
    fn total(&self) -> i64 {
        self.promo + self.bonus + self.wallet + self.credit
    }
}

/// What the wallet account may do: be drawn on only up to what it holds, and never
/// release a reservation it did not take.
///
/// `NoDebitBalance` alone is not enough, and the difference is the whole point: a
/// settlement *credits* the pending layer, and a pending credit neither consumes
/// nor grants room under that rule, so settling an amount that was never held
/// raised the available balance by exactly that amount. `FundedReservations` adds
/// the missing half — the pending layer may not carry a credit of its own — so a
/// release can only give back room a hold first took. It is enforced inside the
/// append, against the balance the entry would leave behind, which is why it holds
/// under concurrency: two settlements cannot both release a reservation only one
/// of them can cover.
///
/// oxsum change (not upstream) on the engine side too: the variant exists for this
/// account. See docs/decisions.md.
const WALLET_LIMIT: BalanceLimit = BalanceLimit::FundedReservations;

/// What the credit-facility account may do: carry a debit balance — the committed
/// limit — and never a credit one, so a shrink entry cannot reduce the facility
/// past zero or take back a grant the line already drew.
const FACILITY_LIMIT: BalanceLimit = BalanceLimit::NoCreditBalance;

/// One tenant's wallet ledger.
///
/// Nine fixed accounts:
/// - wallet: the purchased balance owed to the user, a liability. Carries
///   `FundedReservations`: no overdraft, and no release beyond what was reserved.
/// - bonus: granted credit, equity. Carries the same limit — a pool is what a
///   hold reserves against and a settlement draws down, so the same invariants
///   apply on both sides of the split.
/// - promo: promotional credit, equity under the same limit again, and the
///   pool every funded draw takes first because it is the money that can
///   expire. The `oxsum.promo_lots` rows say which grant still funds how much
///   of it; `promo_draws` pins which lots each hold reserved.
/// - promo expiry: the equity sink an expired or cancelled lot's unspent
///   remainder posts to — a net-zero marker, never a return to refundable
///   money.
/// - credit line: the drawable part of the operator-granted credit facility, a
///   liability under the same limit again. Holds and settlements draw it only
///   after promo, bonus and purchased funds exhaust; the organization owes
///   limit minus its balance.
/// - credit facility: the committed limit, an asset kept gross of draws.
/// - cash: money received from top-ups.
/// - revenue: income recognized on settlement.
/// - adjustments: operator grants and deductions — the signup bonus and platform-admin
///   adjustments — booked against the bonus pool with a reason.
pub struct Wallet {
    store: PostgresStore<SCALE>,
    registry: AccountRegistry,
    wallet: AccountId,
    bonus: AccountId,
    promo: AccountId,
    promo_expiry: AccountId,
    credit_line: AccountId,
    facility: AccountId,
    cash: AccountId,
    revenue: AccountId,
    /// Operator grants and deductions: [`Self::adjust`] posts the other side of the
    /// pools here.
    adjustments: AccountId,
    /// Plan-fee journal legs (issue #611): the billed receivable, the revenue
    /// it recognized, the contra corrections, the prepayment liability and the
    /// payable a voided settlement leaves.
    receivables: AccountId,
    plan_fees: AccountId,
    discounts: AccountId,
    unearned_plan_fees: AccountId,
    refund_payable: AccountId,
    policy: LedgerPolicy,
    /// The validated tenant id this ledger belongs to: the identity its signed tree heads
    /// are published under (`oxsum/ledgers/<tenant_id>`).
    tenant_id: String,
    /// The in-process queue one key's holds wait in *before* a pooled connection is
    /// checked out (issue #486): a hold waiting on the per-key advisory lock while
    /// already holding a connection starves the shared pool — `N` queued holds pin
    /// `N` connections and every tenant's unrelated reads time out behind them.
    /// Keyed by the same [`limit_lock_key`] hash the advisory lock takes, so the
    /// queue and the lock serialize the same set of callers; the semaphore's job is
    /// only to keep the waiting off the pool — the lock remains the real arbiter.
    key_queue: Arc<Mutex<HashMap<i64, Arc<tokio::sync::Semaphore>>>>,
}

/// Receipt of one posting. `content_hash` is what the user keeps to verify the bill later.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    /// The posted entry's id.
    pub entry_id: EntryId,
    /// The entry's position in the Merkle log, when it is sequenced.
    pub log_index: Option<u64>,
    /// The content hash the Merkle log commits, which the entry's proof
    /// verifies against.
    pub content_hash: Hash,
    /// false means an idempotent replay: nothing new was written.
    pub is_new: bool,
}

/// One ledger entry as the dashboard's transaction log shows it.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Position in the log.
    pub index: u64,
    /// The entry's id, hex-encoded.
    pub id: String,
    /// What the writer recorded: holds and settlements describe the request.
    pub description: String,
    /// The content hash the Merkle log commits to.
    pub content_hash: String,
}

/// One settled entry as the bills page lists it and its exports carry it.
#[derive(Debug, Clone)]
pub struct SettledEntry {
    /// The entry's id, hex-encoded.
    pub id: String,
    /// The booking date: the server's UTC date when the entry was written. The ledger
    /// records no time of day.
    pub booked_on: Date,
    /// What the entry charged, in minor units, read from the ledger's own postings; 0 for
    /// the settlements that charge nothing.
    pub charged_minor: i64,
    /// The content hash the Merkle log commits to, and that the entry's proof verifies
    /// against.
    pub content_hash: String,
}

/// Which of the bills page's kinds a ledger entry is (issue #90).
///
/// A hold is deliberately not a kind: a freeze in flight is the overview's in-flight
/// list, and an entry that only reserved money bills nothing yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionKind {
    /// Money in: `Assets:Cash` posted against the wallet.
    TopUp,
    /// An operator grant or deduction, or the signup bonus: `Equity:Adjustments`.
    Adjustment,
    /// A hold released — a settled gateway turn, or a settlement written through the
    /// wallet API by hand. The charge is what the wallet's settled layer lost.
    Settlement,
}

/// One ledger entry as the bills page's full transaction view lists it (issue #90):
/// top-ups, bonuses and adjustments, and settled requests — every movement the page
/// names, newest first.
#[derive(Debug, Clone)]
pub struct TransactionEntry {
    /// The entry's id in the ledger: what the proof endpoint takes.
    pub id: String,
    /// Position in the log.
    pub index: u64,
    /// The booking date: the server's UTC date when the entry was written. The ledger
    /// records no time of day.
    pub booked_on: Date,
    /// What the writer recorded: a settlement record for a gateway turn, a reason for
    /// a top-up or an adjustment.
    pub description: String,
    /// The content hash the Merkle log commits to, and that the entry's proof verifies
    /// against.
    pub content_hash: String,
    /// The entry's effect on the wallet's settled balance, signed: a top-up or a grant
    /// reads positive, a deduction or a settled charge negative, a zero-charge
    /// settlement zero.
    pub amount_minor: i64,
    /// Which of the page's kinds this entry is.
    pub kind: TransactionKind,
    /// The key that paid, where the entry attributes one: the provenance actor, as its
    /// id in uuid simple form. Top-ups and adjustments carry none — they are
    /// organization history any member may read.
    pub key_id: Option<String>,
    /// The settled turn's own record, when the entry is a gateway settlement; `None`
    /// for a settlement written by hand and for every other kind.
    pub record: Option<SettlementRecord>,
}

/// Entries one bills read scans for settlements, per page. A gateway turn writes two
/// entries — the hold and the settlement that releases it — so a page this size carries
/// hundreds of bills, and a read stops as soon as it has enough.
const BILLS_SCAN: usize = 512;

/// Entries one requests read scans for settlement records, per page.
///
/// A gateway turn writes two entries — the hold and the settlement that releases it — and
/// top-ups and hand-driven holds write more, so a scan this size carries a full page of
/// requests even when other writes interleave. A read stops as soon as it has enough.
const REQUESTS_SCAN: usize = 512;

// The API calendar-date representation for a gateway request's UTC booking date.
time::serde::format_description!(request_date, Date, "[year]-[month]-[day]");

/// One gateway request as the dashboard's requests page lists it, read from the ledger's
/// settlement entries: what the turn was, how it was priced, what it used, what it charged
/// and which key paid it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestEntry {
    /// The booking date: the server's UTC date when the settlement was written. The ledger
    /// records no time of day.
    #[serde(with = "request_date")]
    pub booked_on: Date,
    /// The request id from `x-oxsum-request-id`; the ledger keys are derived from it.
    pub request_id: String,
    /// The settlement entry's own id — what `GET /api/v1/entries/{entryId}/proof` names to
    /// reach this turn's inclusion proof (issue #236). Deriving it from the request id is
    /// no longer the caller's job.
    pub entry_id: EntryId,
    /// The model the caller asked for.
    pub model: String,
    /// How the turn was priced, read back as the settlement record's kind.
    pub kind: SettlementKind,
    /// Tokens billed as input, as the settlement recorded them.
    pub input_tokens: i64,
    /// Tokens billed as output, as the settlement recorded them.
    pub output_tokens: i64,
    /// What the settlement charged, in minor units.
    pub charged_minor: i64,
    /// The discount the turn's organization qualified for (issue #158) — the
    /// percent the settlement description snapshots. `None` when none applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount_percent: Option<i64>,
    /// How many upstream calls the turn made under the one hold (issue #168):
    /// above 1 means it failed over between channels. Records written before
    /// failover existed read back as 1 — the one attempt they made.
    pub upstream_attempts: i64,
    /// The API key that paid the turn, as its id in uuid simple form: the hold's provenance
    /// actor, copied onto the settlement. `None` for a turn held without key attribution.
    pub key_id: Option<String>,
    /// How the turn ended — [`SettlementKind::status`]'s spelling (issue #233):
    /// `settled` for the outcomes that charged, `errored` for the turns that
    /// served nothing after their hold, `refused` for the requests admission
    /// turned away before a hold.
    pub status: &'static str,
    /// Why the turn refused or errored, when the settlement recorded a reason —
    /// `None` for a clean settle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

/// One slice of a list that walks the log backward, newest first (issue #93).
///
/// The cursor is the log's own index: `before` bounds a page to entries older than it,
/// and `next_cursor` names where the next page resumes — `None` when the read reached
/// the log's start. New entries always land above every cursor, so a walk sees every
/// entry exactly once, whatever was appended while it walked.
pub struct ListPage<T, C = u64> {
    /// The page's rows, newest first.
    pub rows: Vec<T>,
    /// Where the next page resumes — `None` when the list's start was reached and the
    /// walk is done.
    pub next_cursor: Option<C>,
}

impl<T, C> ListPage<T, C> {
    /// The same page with each row mapped: the cursor is position, not content.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> ListPage<U, C> {
        ListPage {
            rows: self.rows.into_iter().map(f).collect(),
            next_cursor: self.next_cursor,
        }
    }
}

impl Wallet {
    /// Opens a tenant's ledger on the shared pool, creating the ledger on first use.
    ///
    /// The pool is the process's one pool, shared by every tenant: a wallet is a facade
    /// over a schema, not an owner of connections. Which schema each statement resolves
    /// is pinned per transaction inside `PostgresStore`, so the same connection can serve
    /// one tenant and then the next without carrying anything over — see
    /// docs/decisions.md, "all tenants share one connection pool".
    ///
    /// The schema name is derived server-side from the validated tenant id and is never taken from external input.
    pub async fn open(pool: PgPool, tenant_id: &str) -> Result<Self, WalletError> {
        validate_tenant_id(tenant_id)?;
        let ledger = LedgerId::new(format!("tenant-{tenant_id}")).map_err(invalid)?;
        let schema = format!("ledger_{tenant_id}");
        let store = PostgresStore::<SCALE>::new(pool.clone(), ledger).in_schema(&schema);
        // Two opens of one fresh tenant race `migrate`'s `CREATE SCHEMA IF NOT
        // EXISTS` — Postgres' IF NOT EXISTS is not atomic: both pass the existence
        // check and the loser fails pg_namespace's unique index (23505 — plain
        // CREATE SCHEMA reports 42P06 only when the row is already committed, so
        // the in-flight race surfaces as a unique violation), and `Tenants::get`
        // only serializes opens inside one process. Create the schema here and
        // treat either "already exists" answer as the other opener having won; the
        // rest of `migrate` already runs under the store's own locks, and its
        // IF NOT EXISTS sees the committed schema. Holding an advisory lock across
        // migrate would pin a pooled connection while migrate waits for more of
        // the same pool — a starvation deadlock on a small pool.
        match sqlx::query(&format!(
            "CREATE SCHEMA \"{}\"",
            schema.replace('"', "\"\"")
        ))
        .execute(&pool)
        .await
        {
            Err(sqlx::Error::Database(e))
                if e.code().as_deref() == Some("42P06")
                    || (e.code().as_deref() == Some("23505")
                        && e.constraint() == Some("pg_namespace_nspname_index")) => {}
            r => r.map(|_| ())?,
        }
        // An open that raced another opener through `migrate` can deadlock with
        // the loser's schema DDL: the winner's `persist_accounts` or lazy-
        // migration appends write into tables the loser's `IF NOT EXISTS`
        // re-walk still touches with DDL-grade locks, and a deadlock picks one
        // to kill. `40P01` is that race, not corruption — every stage is
        // idempotent (the migrate lock, the upserts, the marker-keyed lazy
        // migrations), so the whole open simply restarts.
        let mut open_attempts = OPEN_RACE_ATTEMPTS;
        let registry = loop {
            match Self::bootstrap(&store, tenant_id).await {
                Err(e) if open_attempts > 0 && is_deadlock(&e) => {
                    open_attempts -= 1;
                }
                r => break r?,
            }
        };

        let wallet = Self {
            wallet: find_account(&registry, WALLET)?,
            bonus: find_account(&registry, BONUS)?,
            promo: find_account(&registry, PROMO)?,
            promo_expiry: find_account(&registry, PROMO_EXPIRY)?,
            credit_line: find_account(&registry, CREDIT_LINE)?,
            facility: find_account(&registry, FACILITY)?,
            cash: find_account(&registry, CASH)?,
            revenue: find_account(&registry, REVENUE)?,
            adjustments: find_account(&registry, ADJUSTMENTS)?,
            receivables: find_account(&registry, RECEIVABLES)?,
            plan_fees: find_account(&registry, PLAN_FEES)?,
            discounts: find_account(&registry, DISCOUNTS)?,
            unearned_plan_fees: find_account(&registry, UNEARNED_PLAN_FEES)?,
            refund_payable: find_account(&registry, REFUND_PAYABLE)?,
            store,
            registry,

            policy: LedgerPolicy::default(),
            tenant_id: tenant_id.to_owned(),
            key_queue: Arc::default(),
        };
        Ok(wallet)
    }

    /// Opens an already initialized ledger without migrations or bootstrap
    /// writes. Operational previews must not run lazy reclassification merely
    /// by reading historical source entries.
    ///
    /// # Errors
    /// Missing schemas/accounts and storage failures require normal operator
    /// initialization; this reader never creates a replacement ledger.
    pub async fn open_existing(pool: PgPool, tenant_id: &str) -> Result<Self, WalletError> {
        validate_tenant_id(tenant_id)?;
        let ledger = LedgerId::new(format!("tenant-{tenant_id}")).map_err(invalid)?;
        let store =
            PostgresStore::<SCALE>::new(pool, ledger).in_schema(&format!("ledger_{tenant_id}"));
        let registry = AccountRegistry::from_records(store.accounts().await?).map_err(invalid)?;
        Ok(Self {
            wallet: find_account(&registry, WALLET)?,
            bonus: find_account(&registry, BONUS)?,
            promo: find_account(&registry, PROMO)?,
            promo_expiry: find_account(&registry, PROMO_EXPIRY)?,
            credit_line: find_account(&registry, CREDIT_LINE)?,
            facility: find_account(&registry, FACILITY)?,
            cash: find_account(&registry, CASH)?,
            revenue: find_account(&registry, REVENUE)?,
            adjustments: find_account(&registry, ADJUSTMENTS)?,
            receivables: find_account(&registry, RECEIVABLES)?,
            plan_fees: find_account(&registry, PLAN_FEES)?,
            discounts: find_account(&registry, DISCOUNTS)?,
            unearned_plan_fees: find_account(&registry, UNEARNED_PLAN_FEES)?,
            refund_payable: find_account(&registry, REFUND_PAYABLE)?,
            store,
            registry,
            policy: LedgerPolicy::default(),
            tenant_id: tenant_id.to_owned(),
            key_queue: Arc::default(),
        })
    }

    /// The restartable body of [`open`](Self::open): schema, account registry
    /// and the lazy migrations, for the deadlock retry to re-run. Returns the
    /// converged registry; the caller's `store` is the same one throughout.
    async fn bootstrap(
        store: &PostgresStore<SCALE>,
        tenant_id: &str,
    ) -> Result<AccountRegistry, WalletError> {
        store.migrate().await?;

        // Account handles must be restored from storage on restart. Re-registering by
        // path could mint different handle numbers and mispoint historical entries.
        // The nine the wallet needs are registered wherever the store lacks them —
        // a second opener can read while the first is still mid-bootstrap, and a read
        // that finds some of them has to converge, not fail on the half it can see.
        let mut attempts = OPEN_RACE_ATTEMPTS;
        let registry = loop {
            let stored = store.accounts().await?;
            let mut registry = AccountRegistry::from_records(stored).map_err(invalid)?;
            for path in [
                WALLET,
                BONUS,
                PROMO,
                PROMO_EXPIRY,
                CREDIT_LINE,
                FACILITY,
                CASH,
                REVENUE,
                ADJUSTMENTS,
                RECEIVABLES,
                PLAN_FEES,
                DISCOUNTS,
                UNEARNED_PLAN_FEES,
                REFUND_PAYABLE,
            ] {
                if find_account(&registry, path).is_err() {
                    registry.register_path(path, OPENED).map_err(invalid)?;
                }
            }

            // The pools' limit is oxsum's rule about its own accounts, so it is applied on
            // every open rather than only where a ledger is created. `register_account`
            // upserts master data, which is what lets a ledger written under a weaker rule be
            // tightened here instead of keeping that rule for the rest of its life.
            let wallet = find_account(&registry, WALLET)?;
            let bonus = find_account(&registry, BONUS)?;
            let promo = find_account(&registry, PROMO)?;
            let credit_line = find_account(&registry, CREDIT_LINE)?;
            let facility = find_account(&registry, FACILITY)?;
            let weakened = registry.records().iter().any(|r| {
                ((r.id == wallet || r.id == bonus || r.id == promo || r.id == credit_line)
                    && r.account.limit != WALLET_LIMIT)
                    || (r.id == facility && r.account.limit != FACILITY_LIMIT)
            });
            if weakened {
                for account in [wallet, bonus, promo, credit_line] {
                    registry.set_limit(account, WALLET_LIMIT).map_err(invalid)?;
                }
                registry
                    .set_limit(facility, FACILITY_LIMIT)
                    .map_err(invalid)?;
            }
            // The full account set is persisted on every open. `register_account` upserts —
            // a record the store already holds is a no-op — and two opens racing a fresh
            // ledger mint the same handles for the same missing paths, so they write the
            // same rows. The upsert's arbiter is the handle, though: a racer that commits
            // between our read and our write leaves the loser's insert to violate the *path*
            // unique constraint instead — read the winner's rows again and converge on them.
            match persist_accounts(store, &registry).await {
                Ok(()) => break registry,
                Err(error) if attempts > 0 && is_unique_violation(&error) => {
                    attempts -= 1;
                }
                Err(error) => return Err(error),
            }
        };

        // Ledgers opened for the first time under the two-pool model move their
        // unspent historical grants into Bonus here — once per ledger, marked by
        // the entry's fixed idempotency key. Doing it at open keeps the migration
        // lazy: a tenant pays for its own reclassification the first time it is
        // used under the new code rather than every `Db::migrate` paying for all
        // of them under the global lock. The promo-lots model (issue #609)
        // migrates the aggregate bonus pool's unreserved balance into a
        // `legacy_migration` lot the same lazy way — the money never leaves the
        // pools, so net balances cannot move. Both run through a bootstrap
        // `Wallet` built on the freshly-read registry: the handles are the
        // store's own account ids, so the migrations' appends land exactly where
        // the returned `Wallet` will read them.
        let wallet = Self {
            wallet: find_account(&registry, WALLET)?,
            bonus: find_account(&registry, BONUS)?,
            promo: find_account(&registry, PROMO)?,
            promo_expiry: find_account(&registry, PROMO_EXPIRY)?,
            credit_line: find_account(&registry, CREDIT_LINE)?,
            facility: find_account(&registry, FACILITY)?,
            cash: find_account(&registry, CASH)?,
            revenue: find_account(&registry, REVENUE)?,
            adjustments: find_account(&registry, ADJUSTMENTS)?,
            receivables: find_account(&registry, RECEIVABLES)?,
            plan_fees: find_account(&registry, PLAN_FEES)?,
            discounts: find_account(&registry, DISCOUNTS)?,
            unearned_plan_fees: find_account(&registry, UNEARNED_PLAN_FEES)?,
            refund_payable: find_account(&registry, REFUND_PAYABLE)?,
            store: store.clone(),
            registry: registry.clone(),

            policy: LedgerPolicy::default(),
            tenant_id: tenant_id.to_owned(),
            key_queue: Arc::default(),
        };
        wallet.reclassify_grants().await?;
        wallet.migrate_legacy_bonus().await?;
        Ok(registry)
    }

    /// The tenant id this ledger belongs to: the identity its signed tree heads carry.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    /// Top-up: cash and wallet balance increase together.
    ///
    /// When the organization owes on its credit line, the deposit repays the line
    /// first — `debit Cash, credit CreditLine` for the drawn part, the rest into
    /// the wallet — so paying in restores borrowed headroom before it adds own
    /// funds: "borrow first, settle monthly" needs the receipt to shrink the debt,
    /// not to stack on top of it. The repay split is computed under the credit
    /// lock, which serializes it with `set_credit_limit` and other repaying
    /// top-ups; a draw racing it only ever makes the drawn figure *larger*, so a
    /// stale read can under-repay, never over-credit the line past its limit.
    ///
    /// The entry's shape depends on what is drawn, so a retry could compute a
    /// different split than the one already committed under the same key. The
    /// derived entry id is looked up before anything is sealed: a committed
    /// entry under it answers with its own receipt, whatever shape it took.
    pub async fn top_up(&self, key: &str, minor: i64, on: Date) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        let entry_id = entry_id_for(key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return self.topup_replay(&stored, minor, key);
        }
        if self.credit_drawn().await? <= 0 {
            let receipt = self
                .append(
                    Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on)
                        .debit(self.cash, amt, currency())
                        .credit(self.wallet, amt, currency()),
                )
                .await;
            return match receipt {
                // The key was taken between our read and the append — by a
                // racing copy of this same request, whose repay split may
                // differ from ours, or by a different request under it.
                Err(WalletError::Conflict(_)) => match self.store.get(entry_id).await? {
                    Some(stored) => self.topup_replay(&stored, minor, key),
                    None => Err(WalletError::Conflict(format!(
                        "idempotency key {key:?} is already taken"
                    ))),
                },
                other => other,
            };
        }
        let mut tx = self.store.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(credit_lock_key(&self.tenant_id))
            .execute(&mut *tx)
            .await?;
        // Re-read under the lock: the figure the split is computed against is the
        // one serialized with every other write that restores the line.
        let drawn = self.credit_drawn_in(&mut tx).await?;
        let repay = minor.min(drawn.max(0));
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on).debit(self.cash, amt, currency());
        if repay > 0 {
            draft = draft.credit(self.credit_line, Credits::from_minor(repay), currency());
        }
        if minor - repay > 0 {
            draft = draft.credit(self.wallet, Credits::from_minor(minor - repay), currency());
        }
        let receipt = self.append_sealed(self.seal(draft).await?).await;
        match receipt {
            Ok(receipt) => {
                tx.commit().await?;
                Ok(receipt)
            }
            Err(WalletError::Conflict(_)) => {
                tx.rollback().await?;
                // A racing copy committed while this one waited on the lock:
                // its repay split need not be the shape this draft computed.
                match self.store.get(entry_id).await? {
                    Some(stored) => self.topup_replay(&stored, minor, key),
                    None => Err(WalletError::Conflict(format!(
                        "idempotency key {key:?} is already taken"
                    ))),
                }
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// Answers for a key that already has a committed top-up entry: the
    /// stored entry's receipt when it is the top-up being asked for — a cash
    /// debit of the full amount, pool credits adding to it, whatever the
    /// repay split (what is drawn moves after a top-up lands, so the
    /// committed shape is the answer, never the split a retry would compute
    /// now) — and the key-reuse conflict when the entry is anything else. A
    /// credit-limit grant also credits a pool; requiring the cash leg is what
    /// tells the two apart.
    fn topup_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let cash: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| p.account == self.cash && p.direction == Direction::Debit)
            .map(|p| p.amount.to_minor())
            .sum();
        let credited: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| self.is_pool(p.account) && p.direction == Direction::Credit)
            .map(|p| p.amount.to_minor())
            .sum();
        if cash == minor && credited == minor {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }

    /// Repays the credit line and nothing else: cash in, credit-line balance
    /// restored, the wallet and bonus pools untouched.
    ///
    /// This is the posting a purpose-scoped invoice repayment makes — an order
    /// exists to settle a drawn receivable, so the whole amount books against
    /// `CreditLine` and none of it can become spendable wallet balance or a
    /// granted pool. A `top_up` cannot stand in for it: the top-up's own rule
    /// credits whatever exceeds the draw to the wallet, which is exactly the
    /// cash a repayment purpose must never grant.
    ///
    /// The amount is capped at what is drawn: a repayment larger than the
    /// outstanding debt has nothing to settle, so it is refused rather than
    /// booking the excess into a pool the purpose forbids. The repay figure is
    /// read under the credit lock, serialized with `set_credit_limit` and every
    /// other repaying write, so a stale read can under-pay, never over-credit
    /// the line past its limit.
    ///
    /// The entry replays under `key`: a crash between the append and the order's
    /// fulfillment write leaves a committed repayment the retry finds through
    /// `Self::repay_replay`, and a raced copy of the same repayment reads the
    /// drawn figure under the same lock.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount or one over the
    /// drawn debt; [`WalletError::Conflict`] when `key` is taken by a different
    /// entry; storage failures surface as [`WalletError`].
    pub async fn repay_credit(
        &self,
        key: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        let entry_id = entry_id_for(key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return self.repay_replay(&stored, minor, key);
        }
        let mut tx = self.store.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(credit_lock_key(&self.tenant_id))
            .execute(&mut *tx)
            .await?;
        // Re-read under the lock, and re-check the entry id: a same-key
        // repayment may have committed while this one waited on it.
        if let Some(stored) = self.store.get(entry_id).await? {
            tx.rollback().await?;
            return self.repay_replay(&stored, minor, key);
        }
        let drawn = self.credit_drawn_in(&mut tx).await?;
        if minor > drawn {
            tx.rollback().await?;
            return Err(WalletError::InvalidInput(
                "a repayment cannot exceed the outstanding credit debt".into(),
            ));
        }
        let draft = Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on)
            .debit(self.cash, amt, currency())
            .credit(self.credit_line, amt, currency());
        let receipt = self.append_sealed(self.seal(draft).await?).await;
        match receipt {
            Ok(receipt) => {
                tx.commit().await?;
                Ok(receipt)
            }
            Err(WalletError::Conflict(_)) => {
                tx.rollback().await?;
                match self.store.get(entry_id).await? {
                    Some(stored) => self.repay_replay(&stored, minor, key),
                    None => Err(WalletError::Conflict(format!(
                        "idempotency key {key:?} is already taken"
                    ))),
                }
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// Answers for a key that already has a committed repayment entry: the
    /// stored receipt when it is the repayment being asked for — a cash debit
    /// and a credit-line credit of the same amount, nothing reaching the
    /// spendable pools — and the key-reuse conflict when the entry is anything
    /// else, including a top-up that split part of the same amount to wallet.
    fn repay_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let cash: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| p.account == self.cash && p.direction == Direction::Debit)
            .map(|p| p.amount.to_minor())
            .sum();
        let repaid: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| p.account == self.credit_line && p.direction == Direction::Credit)
            .map(|p| p.amount.to_minor())
            .sum();
        let pooled: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| {
                (p.account == self.wallet || p.account == self.bonus)
                    && p.direction == Direction::Credit
            })
            .map(|p| p.amount.to_minor())
            .sum();
        if cash == minor && repaid == minor && pooled == 0 {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }

    /// Adjustment: a signed amount the operator books against the pools, with a
    /// `reason` that becomes the entry's description — covered by its content hash,
    /// so the reason is part of the proof.
    ///
    /// A positive `minor` grants credits into the bonus pool — a grant is the
    /// platform's contribution, never cash-backed. A negative one deducts, drawing
    /// the bonus pool first: what the platform granted is what it takes back before
    /// touching money the user paid for. Borrowed headroom is never a pool a
    /// deduction draws — shrinking what the organization may borrow is the credit
    /// limit's job, so a deduction deeper than granted plus purchased funds is
    /// refused even while undrawn credit remains. The pools' `FundedReservations` limit is
    /// what refuses a deduction the balance cannot carry —
    /// [`WalletError::InsufficientFunds`], the same refusal a hold gets — so the
    /// rule holds under concurrency rather than being a read-then-write check; the
    /// retry only re-reads the split after a racing append moved it.
    /// Zero is [`WalletError::InvalidInput`]: a no-op adjustment would only be a
    /// reason-looking entry that moved nothing.
    pub async fn adjust(
        &self,
        key: &str,
        reason: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if reason.trim().is_empty() {
            return Err(WalletError::InvalidInput(
                "an adjustment needs a reason".into(),
            ));
        }
        let amt = Credits::from_minor(
            minor
                .checked_abs()
                .ok_or(WalletError::InvalidInput("amount is out of range".into()))?,
        );
        if minor == 0 {
            return Err(WalletError::InvalidInput("amount must not be zero".into()));
        }
        if minor > 0 {
            return self
                .append(
                    Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
                        .with_description(description_of(reason)?)
                        .debit(self.adjustments, amt, currency())
                        .credit(self.bonus, amt, currency()),
                )
                .await;
        }
        let description = description_of(reason)?;
        let mut last_err = None;
        for _ in 0..POOL_SPLIT_ATTEMPTS {
            // The deduction's split is a read-then-write choice: a racing append can
            // shrink a pool between the read and the write, which the append refuses
            // — re-read and try the split that fits now.
            let bonus_take = amt.to_minor().min(self.bonus_settled().await?.max(0));
            let wallet_take = amt.to_minor() - bonus_take;
            let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
                .with_description(description.clone());
            if bonus_take > 0 {
                draft = draft.debit(self.bonus, Credits::from_minor(bonus_take), currency());
            }
            if wallet_take > 0 {
                draft = draft.debit(self.wallet, Credits::from_minor(wallet_take), currency());
            }
            match self
                .append(draft.credit(self.adjustments, amt, currency()))
                .await
            {
                Err(error @ WalletError::InsufficientFunds) => last_err = Some(error),
                // The key was taken between our read and the append — by a
                // racing copy of this same deduction, whose pool split may
                // differ from ours, or by a different write under it.
                Err(WalletError::Conflict(_)) => {
                    return match self.store.get(entry_id_for(key)).await? {
                        Some(stored) => self.deduction_replay(&stored, &description, minor, key),
                        None => Err(WalletError::Conflict(format!(
                            "idempotency key {key:?} is already taken"
                        ))),
                    };
                }
                other => return other,
            }
        }
        Err(last_err.unwrap_or(WalletError::InsufficientFunds))
    }

    /// Issues a credit note: one ledger entry that gives back credit the
    /// customer already paid for, without moving money (issue #352, decision
    /// D12). The posting is the mirror of a settlement's charge — debit
    /// Revenue, credit Wallet — and the reason is the entry's description, so
    /// the note is a new fact beside the charge, never a rewrite of it.
    ///
    /// `pub(crate)`: only [`crate::Db::issue_credit_note`] calls it. A retried
    /// key replays the standing entry rather than crediting twice.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount, an empty
    /// reason, or a malformed key.
    pub(crate) async fn credit_note(
        &self,
        idempotency_key: &str,
        reason: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if reason.trim().is_empty() {
            return Err(WalletError::InvalidInput(
                "a credit note needs a reason".into(),
            ));
        }
        if minor <= 0 {
            return Err(WalletError::InvalidInput(
                "a credit note is a positive amount".into(),
            ));
        }
        let key = format!("credit-note/{idempotency_key}");
        let amt = Credits::from_minor(minor);
        self.append(
            Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                .with_description(description_of(reason)?)
                .debit(self.revenue, amt, currency())
                .credit(self.wallet, amt, currency()),
        )
        .await
    }

    /// Reverses a credited deposit: one ledger entry that takes back purchased
    /// credit the deposit put in — the exact mirror of a top-up, Wallet debited
    /// and Cash credited, the operator's reason as the entry's description
    /// (issue #356, decision D12). The deposit's own credit stays in history
    /// beside the reversal; nothing is rewritten.
    ///
    /// The draw is on the purchased pool alone — granted credit never stood
    /// behind a rail payment, so the bonus and credit-line pools are never
    /// touched. The pool's `FundedReservations` limit is what refuses a
    /// reversal the wallet cannot carry (the customer already spent the
    /// credit): [`WalletError::InsufficientFunds`], enforced inside the append
    /// against the balance the entry would leave, so it holds under
    /// concurrency rather than being a read-then-write check.
    ///
    /// `pub(crate)`: only [`crate::Db::reverse_deposit`] calls it, under a key
    /// derived from the deposit id — a retried reversal replays the standing
    /// entry rather than debiting twice.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount, an empty
    /// reason, or a malformed key; [`WalletError::InsufficientFunds`] when the
    /// purchased pool cannot cover the debit.
    pub(crate) async fn reverse_deposit(
        &self,
        idempotency_key: &str,
        reason: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if reason.trim().is_empty() {
            return Err(WalletError::InvalidInput(
                "a deposit reversal needs a reason".into(),
            ));
        }
        let amt = positive(minor)?;
        let key = format!("deposit-reversal/{idempotency_key}");
        self.append(
            Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                .with_description(description_of(reason)?)
                .debit(self.wallet, amt, currency())
                .credit(self.cash, amt, currency()),
        )
        .await
    }

    /// Takes back wallet credit a payment-order refund returned in cash
    /// (issue #608): one ledger entry that mirrors the funding leg —
    /// `debit Wallet, credit Cash` — so the purchased pool shrinks by exactly
    /// what the PSP gave back. The refund's own credit note records the
    /// correction beside it; nothing is rewritten.
    ///
    /// The draw is on the purchased pool alone: granted credit never stood
    /// behind a rail payment, and drawn credit-line debt is the dispute
    /// re-charge's to restore, not a refund's. `FundedReservations` is what
    /// refuses a take-back the pool cannot carry — the customer already spent
    /// the refunded credit — surfacing [`WalletError::InsufficientFunds`]
    /// enforced inside the append, so the bound holds under a consume racing
    /// the refund, never as a read-then-write check.
    ///
    /// `pub(crate)`: only [`crate::Db::settle_order_refund`] calls it, under a
    /// key derived from the refund id — a retried or recovered settlement
    /// replays the standing entry rather than debiting twice.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount, an empty
    /// reason, or a malformed key; [`WalletError::InsufficientFunds`] when
    /// the purchased pool cannot cover the debit.
    pub(crate) async fn refund_debit(
        &self,
        idempotency_key: &str,
        reason: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if reason.trim().is_empty() {
            return Err(WalletError::InvalidInput(
                "a refund take-back needs a reason".into(),
            ));
        }
        let amt = positive(minor)?;
        let key = format!("order-refund/{idempotency_key}");
        self.append(
            Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                .with_description(description_of(reason)?)
                .debit(self.wallet, amt, currency())
                .credit(self.cash, amt, currency()),
        )
        .await
    }

    /// Re-charges a drawn receivable a lost dispute restored (issue #608):
    /// the same `debit Cash, credit CreditLine` shape
    /// [`Self::repay_credit`] posts — the platform's cash goes back out to the
    /// card network and the line the customer drew is owed again — but under
    /// the dispute's own ledger key, `dispute-recharge/<dispute_id>`, so the
    /// customer-visible ledger reads a contested-charge restoration, never a
    /// voluntary repayment, and a retried settle replays the standing entry.
    ///
    /// Like `repay_credit` the amount is capped at what is drawn: a dispute
    /// lost over money the line no longer carries has nothing to restore, and
    /// the refusal surfaces rather than booking into a pool the outcome
    /// forbids.
    ///
    /// `pub(crate)`: only [`crate::Db::resolve_order_dispute`] calls it.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a non-positive amount or one over
    /// the drawn debt; [`WalletError::Conflict`] when the key is taken by a
    /// different entry; storage failures surface as [`WalletError`].
    pub(crate) async fn dispute_recharge(
        &self,
        dispute_id: &uuid::Uuid,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        self.repay_credit(&format!("dispute-recharge/{dispute_id}"), minor, on)
            .await
    }

    // ── plan-fee journal legs (issue #611, T20) ──────────────────────────────

    /// The accrual a closing period's journal books: charge lines
    /// `debit Receivables / credit PlanFees`, discounts `debit Discounts /
    /// credit Receivables`, promo coverage `debit Promo / credit Receivables`
    /// — one entry per journal, keyed `plan-journal/<id>/accrual`, inside the
    /// caller's transaction so the journal row, its lines and the postings
    /// land or roll back together.
    ///
    /// `charges_minor` is the sum of the charge kinds (base fee, overage, tax);
    /// `discount_minor`/`promo_minor` are positive magnitudes of the credit
    /// legs. `promo_minor` must not exceed what the promo pool settles — the
    /// caller clamps it, `FundedReservations` is the backstop.
    ///
    /// A zero-leg journal (a free plan's 0-total close) posts nothing and
    /// returns `None` — an entry cannot carry an empty amount set.
    pub(crate) async fn plan_journal_accrual_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: &uuid::Uuid,
        charges_minor: i64,
        discount_minor: i64,
        promo_minor: i64,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        if charges_minor < 0 || discount_minor < 0 || promo_minor < 0 {
            return Err(WalletError::InvalidInput(
                "journal leg amounts must not be negative".into(),
            ));
        }
        if charges_minor + discount_minor + promo_minor == 0 {
            return Ok(None);
        }
        let key = format!("plan-journal/{journal_id}/accrual");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return Ok(Some(stored_receipt(&stored)));
        }
        let mut draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on).with_description(
            description_of(&format!("plan fee journal {journal_id}: accrual"))?,
        );
        if charges_minor > 0 {
            let amt = Credits::from_minor(charges_minor);
            draft = draft.debit(self.receivables, amt, currency()).credit(
                self.plan_fees,
                amt,
                currency(),
            );
        }
        if discount_minor > 0 {
            let amt = Credits::from_minor(discount_minor);
            draft = draft.debit(self.discounts, amt, currency()).credit(
                self.receivables,
                amt,
                currency(),
            );
        }
        if promo_minor > 0 {
            let amt = Credits::from_minor(promo_minor);
            draft =
                draft
                    .debit(self.promo, amt, currency())
                    .credit(self.receivables, amt, currency());
        }
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => match self.store.get(entry_id).await? {
                Some(stored) => Ok(Some(stored_receipt(&stored))),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other.map(Some),
        }
    }

    /// The prepayment a fulfilled `planPeriod` order books for the period it
    /// authorizes: `debit Cash / credit UnearnedPlanFees` — deferred revenue,
    /// not yet the receivable the period close will post. Keyed
    /// `payment-order/<order>/prepay`; replay-idempotent like every order leg.
    pub(crate) async fn plan_prepay_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        order_id: &uuid::Uuid,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        let key = format!("payment-order/{order_id}/prepay");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return Ok(stored_receipt(&stored));
        }
        let draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on)
            .with_description(description_of(&format!(
                "plan period prepayment for order {order_id}"
            ))?)
            .debit(self.cash, amt, currency())
            .credit(self.unearned_plan_fees, amt, currency());
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => match self.store.get(entry_id).await? {
                Some(stored) => Ok(stored_receipt(&stored)),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other,
        }
    }

    /// Reclassifies prepayment into the receivable a closing journal opened:
    /// `debit UnearnedPlanFees / credit Receivables` up to what the journal
    /// still owes. Keyed `plan-journal/<id>/prepay-settle` — one journal
    /// consumes prepayment exactly once.
    pub(crate) async fn plan_prepay_settle_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: &uuid::Uuid,
        minor: i64,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        if minor <= 0 {
            return Ok(None);
        }
        let amt = Credits::from_minor(minor);
        let key = format!("plan-journal/{journal_id}/prepay-settle");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return Ok(Some(stored_receipt(&stored)));
        }
        let draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on)
            .with_description(description_of(&format!(
                "plan fee journal {journal_id}: prepayment applied"
            ))?)
            .debit(self.unearned_plan_fees, amt, currency())
            .credit(self.receivables, amt, currency());
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => match self.store.get(entry_id).await? {
                Some(stored) => Ok(Some(stored_receipt(&stored))),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other.map(Some),
        }
    }

    /// Reads the actual legacy refund legs, never inferring cash from a mutable
    /// journal state. Only the old Discounts/Cash-or-Receivables shape qualifies.
    pub(crate) async fn legacy_plan_refund_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        entry_id: Option<Uuid>,
    ) -> Result<Option<crate::plan_refunds::LegacyPlanRefund>, WalletError> {
        let Some(entry_id) = entry_id else {
            return Ok(None);
        };
        let sql = format!(
            "SELECT a.path,p.direction,p.amount_minor FROM ledger_{}.postings p JOIN ledger_{}.accounts a ON a.account_index=p.account_index JOIN ledger_{}.entries e ON e.entry_id=p.entry_id WHERE p.entry_id=$1 AND p.layer='settled' AND e.log_index IS NOT NULL",
            self.tenant_id, self.tenant_id, self.tenant_id
        );
        let rows = sqlx::query(&sql)
            .bind(entry_id)
            .fetch_all(&mut **tx)
            .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        use sqlx::Row;
        let mut old = crate::plan_refunds::LegacyPlanRefund {
            entry_id,
            cash: 0,
            receivable: 0,
            discount: 0,
        };
        for row in rows {
            let path: String = row.try_get("path")?;
            let direction: String = row.try_get("direction")?;
            let amount: i64 = row.try_get("amount_minor")?;
            let target = match (path.as_str(), direction.as_str()) {
                (CASH, "C") => &mut old.cash,
                (RECEIVABLES, "C") => &mut old.receivable,
                (DISCOUNTS, "D") => &mut old.discount,
                _ => {
                    return Err(WalletError::Conflict(
                        "legacy plan-refund entry has unsupported legs".into(),
                    ));
                }
            };
            *target = target
                .checked_add(amount)
                .ok_or_else(|| invalid("refund postings overflow"))?;
        }
        if old.cash.checked_add(old.receivable) != Some(old.discount) {
            return Err(WalletError::Conflict(
                "legacy plan-refund entry is not balanced".into(),
            ));
        }
        Ok(Some(old))
    }

    /// Original source credits, read through this transaction's snapshot.
    pub(crate) async fn plan_source_credit_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        path: &str,
    ) -> Result<i64, WalletError> {
        self.plan_source_minor_in(tx, key, path, "C").await
    }

    /// Actual debit allocation in a source-linked entry.
    pub(crate) async fn plan_source_debit_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        path: &str,
    ) -> Result<i64, WalletError> {
        self.plan_source_minor_in(tx, key, path, "D").await
    }

    async fn plan_source_minor_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        key: &str,
        path: &str,
        direction: &str,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT COALESCE(SUM(p.amount_minor),0)::bigint FROM ledger_{}.postings p JOIN ledger_{}.accounts a ON a.account_index=p.account_index JOIN ledger_{}.entries e ON e.entry_id=p.entry_id WHERE p.entry_id=$1 AND a.path=$2 AND p.layer='settled' AND p.direction=$3 AND e.log_index IS NOT NULL",
            self.tenant_id, self.tenant_id, self.tenant_id
        );
        Ok(sqlx::query_scalar(&sql)
            .bind(entry_id_for(key).as_uuid())
            .bind(path)
            .bind(direction)
            .fetch_one(&mut **tx)
            .await?)
    }

    /// The original prepayment allocation, retained even after a buggy refund
    /// lowered a journal's mutable settled counter.
    pub(crate) async fn plan_journal_prepaid_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: Uuid,
    ) -> Result<i64, WalletError> {
        self.plan_source_credit_in(
            tx,
            &format!("plan-journal/{journal_id}/prepay-settle"),
            RECEIVABLES,
        )
        .await
    }

    /// Actual chargeback cash entries tied to this order, including a crash
    /// after posting and before its dispute row became terminal.
    pub(crate) async fn plan_order_chargebacks_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        order_id: Uuid,
    ) -> Result<i64, WalletError> {
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT dispute_id FROM oxsum.disputes WHERE order_id=$1")
                .bind(order_id)
                .fetch_all(&mut **tx)
                .await?;
        let mut total = 0i64;
        for id in ids {
            let amount = self
                .plan_source_credit_in(tx, &format!("dispute-recharge/{id}"), CASH)
                .await?;
            total = total
                .checked_add(amount)
                .ok_or_else(|| invalid("chargeback amount overflows"))?;
        }
        Ok(total)
    }

    /// Post the desired cash allocation minus its retained legacy legs. The
    /// exception debit is explicit reconciliation suspense, never customer debt.
    pub(crate) async fn plan_refund_cash_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        allocation: &crate::PlanRefundRecovery,
        old: Option<&crate::plan_refunds::LegacyPlanRefund>,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let key = format!("plan-refund-cash/{}", allocation.refund_id);
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return Ok(stored_receipt(&stored));
        }
        let old_cash = old.map_or(0, |v| v.cash);
        let old_ar = old.map_or(0, |v| v.receivable);
        let old_discount = old.map_or(0, |v| v.discount);
        let legs = [
            (self.unearned_plan_fees, allocation.deferred_minor),
            (self.discounts, allocation.earned_minor - old_discount),
            (self.refund_payable, allocation.payable_minor),
            (self.adjustments, allocation.unallocated_minor),
            (self.receivables, old_ar),
            (self.cash, old_cash - allocation.cash_minor),
        ];
        if legs.iter().all(|(_, amount)| *amount == 0) {
            let previous = old.ok_or_else(|| invalid("refund allocation has no source legs"))?;
            let stored = self
                .store
                .get(EntryId::from_uuid(previous.entry_id))
                .await?
                .ok_or_else(|| invalid("legacy refund entry disappeared"))?;
            return Ok(stored_receipt(&stored));
        }
        let mut draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on).with_description(
            description_of(&format!(
                "confirmed plan refund {}: original order {}; unallocated {}",
                allocation.refund_id, allocation.order_id, allocation.unallocated_minor
            ))?,
        );
        for (account, signed) in legs {
            if signed > 0 {
                draft = draft.debit(account, Credits::from_minor(signed), currency());
            } else if signed < 0 {
                draft = draft.credit(account, Credits::from_minor(-signed), currency());
            }
        }
        self.append_in(tx, draft).await
    }

    /// Remove proven duplicated plan revenue, reducing only original open debt
    /// and retaining already collected excess as payable. Cash never moves.
    pub(crate) async fn plan_usage_correction_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        correction: &crate::PlanUsageCorrection,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amount = positive(correction.duplicate_minor)?;
        if correction.receivable_minor < 0
            || correction.payable_minor < 0
            || correction
                .receivable_minor
                .checked_add(correction.payable_minor)
                != Some(correction.duplicate_minor)
        {
            return Err(invalid("invalid duplicate-usage allocation"));
        }
        let key = format!("plan-usage-correction/{}", correction.journal_id);
        let mut draft = Entry::<Draft,SCALE>::new(entry_id_for(&key),idem(&key)?,on)
            .with_description(description_of(&format!("historical duplicate usage correction {}; original accrual {}; receivable {}; refund payable {}; evidence sha256 {}; cash unchanged",correction.journal_id,correction.original_entry_id,correction.receivable_minor,correction.payable_minor,correction.evidence_sha256))?)
            .debit(self.plan_fees,amount,currency());
        for (account, minor) in [
            (self.receivables, correction.receivable_minor),
            (self.refund_payable, correction.payable_minor),
        ] {
            if minor > 0 {
                draft = draft.credit(account, Credits::from_minor(minor), currency());
            }
        }
        self.append_in(tx, draft).await
    }

    /// Record the full observed chargeback cash. Only original collected funds
    /// restore debt; a voided bill draws its payable and excess is review suspense.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn plan_journal_dispute_recharge_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: &Uuid,
        dispute_id: &Uuid,
        cash_minor: i64,
        receivable_minor: i64,
        payable_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(cash_minor)?;
        let suspense = cash_minor
            .checked_sub(receivable_minor)
            .and_then(|v| v.checked_sub(payable_minor))
            .filter(|v| *v >= 0)
            .ok_or_else(|| invalid("invalid chargeback source allocation"))?;
        let key = format!("dispute-recharge/{dispute_id}");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return Ok(stored_receipt(&stored));
        }
        let mut draft=Entry::<Draft,SCALE>::new(entry_id,idem(&key)?,on)
            .with_description(description_of(&format!("plan fee journal {journal_id}: chargeback cash {cash_minor}, receivable {receivable_minor}, payable {payable_minor}, review suspense {suspense}"))?)
            .credit(self.cash,amt,currency());
        for (account, minor) in [
            (self.receivables, receivable_minor),
            (self.refund_payable, payable_minor),
            (self.adjustments, suspense),
        ] {
            if minor < 0 {
                return Err(invalid("negative chargeback allocation"));
            }
            if minor > 0 {
                draft = draft.debit(account, Credits::from_minor(minor), currency());
            }
        }
        self.append_in(tx, draft).await
    }

    /// The reversal a staff void posts: every line's leg reversed in one entry
    /// — `debit PlanFees / credit Receivables` for the charge sums,
    /// `debit Receivables / credit Discounts` and `debit Receivables / credit
    /// Promo` for the credit legs, so the journal nets to zero and no history
    /// is rewritten. On a `paid` journal the receivable is already zero, so the
    /// settlement unwinds to `RefundPayable` instead — what the customer is
    /// owed back. `paid` selects between the two shapes.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn plan_journal_void_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        journal_id: &uuid::Uuid,
        charges_minor: i64,
        discount_minor: i64,
        promo_minor: i64,
        settled_minor: i64,
        journal_paid: bool,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        let key = format!("plan-journal/{journal_id}/void");
        let entry_id = entry_id_for(&key);
        if let Some(stored) = self.store.get(entry_id).await? {
            return Ok(Some(stored_receipt(&stored)));
        }
        let mut draft = Entry::<Draft, SCALE>::new(entry_id, idem(&key)?, on).with_description(
            description_of(&format!("plan fee journal {journal_id}: void reversal"))?,
        );
        let mut legs = 0i64;
        if journal_paid {
            // The receivable is zero; the collected money unwinds to a payable.
            if charges_minor > 0 {
                let amt = Credits::from_minor(charges_minor);
                draft = draft.debit(self.plan_fees, amt, currency()).credit(
                    self.refund_payable,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            // Credit legs the void backs out re-charge the payable — a
            // discounted bill voided owes back only what was paid, so the
            // payable nets to the journal's current total (prior cash refunds
            // already left through the refund corrections).
            if discount_minor > 0 {
                let amt = Credits::from_minor(discount_minor);
                draft = draft.debit(self.refund_payable, amt, currency()).credit(
                    self.discounts,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            if promo_minor > 0 {
                let amt = Credits::from_minor(promo_minor);
                draft = draft.debit(self.refund_payable, amt, currency()).credit(
                    self.promo,
                    amt,
                    currency(),
                );
                legs += 1;
            }
        } else {
            if charges_minor > 0 {
                let amt = Credits::from_minor(charges_minor);
                draft = draft.debit(self.plan_fees, amt, currency()).credit(
                    self.receivables,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            if discount_minor > 0 {
                let amt = Credits::from_minor(discount_minor);
                draft = draft.debit(self.receivables, amt, currency()).credit(
                    self.discounts,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            if promo_minor > 0 {
                let amt = Credits::from_minor(promo_minor);
                draft = draft.debit(self.receivables, amt, currency()).credit(
                    self.promo,
                    amt,
                    currency(),
                );
                legs += 1;
            }
            // Any part the receivable already collected (partial settlement)
            // is owed back rather than reversed against the open receivable.
            if settled_minor > 0 {
                let amt = Credits::from_minor(settled_minor);
                draft = draft.debit(self.receivables, amt, currency()).credit(
                    self.refund_payable,
                    amt,
                    currency(),
                );
                legs += 1;
            }
        }
        if legs == 0 {
            return Ok(None);
        }
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => match self.store.get(entry_id).await? {
                Some(stored) => Ok(Some(stored_receipt(&stored))),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other.map(Some),
        }
    }

    /// The promo pool's settled balance — the bound a `promo_credit` journal
    /// line is clamped to before the accrual posts.
    pub(crate) async fn promo_settled_in(
        &self,
        conn: &mut sqlx::PgConnection,
    ) -> Result<i64, WalletError> {
        self.settled_net_in(conn, self.promo).await
    }

    /// Answers for a key that already has a committed deduction entry: the
    /// stored entry's receipt when it drew the amount asked for, under the
    /// same reason — whatever its pool split (balances move after a
    /// deduction lands, so the committed shape is the answer, never the
    /// split a retry would compute now) — and the key-reuse conflict when
    /// the entry is anything else.
    fn deduction_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let drawn: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| {
                self.is_pool(p.account)
                    && p.layer == Layer::Settled
                    && p.direction == Direction::Debit
            })
            .map(|p| p.amount.to_minor())
            .sum();
        // A settlement draws the same settled debits a deduction does; the
        // adjustments credit is the leg only a deduction carries.
        let booked: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| {
                p.account == self.adjustments
                    && p.layer == Layer::Settled
                    && p.direction == Direction::Credit
            })
            .map(|p| p.amount.to_minor())
            .sum();
        if drawn == minor.abs()
            && booked == minor.abs()
            && stored.entry.description().as_str() == description.as_str()
        {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }

    /// Hold: reserves part of the balance in the pending layer; the settled balance is untouched.
    ///
    /// The reservation splits across the pools — the bonus pool funds what it
    /// can, the wallet carries what it can, and the credit line picks up the
    /// rest — so the pending debit each pool takes stays inside what that pool's
    /// own balance covers, which is what `FundedReservations` enforces at
    /// append. A racing hold can take bonus room between the split's read and
    /// its write; the retry re-reads and splits again rather than refusing a
    /// request the combined balance could have served.
    /// Refused with [`WalletError::InsufficientFunds`] when the balance cannot
    /// cover it, and with [`WalletError::Forbidden`] when the organization is
    /// suspended (issue #162) — suspension gates every hold, attributed or not.
    ///
    /// `description` is what the entry says it is, in the caller's words — for a gateway request, the
    /// record built by [`crate::hold_description`]. An empty description records none; either way it
    /// is part of the entry and covered by its content hash.
    pub async fn hold(
        &self,
        key: &str,
        description: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        self.hold_inner(key, description, minor, None, on).await
    }

    /// A hold attributed to a service credential: the metering API's hold
    /// (issue #172). The credential is the entry's provenance actor —
    /// `svc-<credential id>` — so the hold and its settlement name the
    /// reporting service the way a key hold names the key. Everything else is
    /// [`hold`](Self::hold): the same pool split, the same suspension gate.
    ///
    /// # Errors
    ///
    /// As [`hold`](Self::hold).
    pub async fn hold_for_service(
        &self,
        service: &Uuid,
        key: &str,
        description: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let actor = Provenance::none()
            .with_actor(&format!("svc-{}", service.as_simple()))
            .map_err(invalid)?;
        self.hold_inner(key, description, minor, Some(actor), on)
            .await
    }

    /// The body [`hold`](Self::hold) and [`hold_for_service`](Self::hold_for_service)
    /// share: `actor` is the provenance the hold entry carries, `None` for an
    /// unattributed one.
    async fn hold_inner(
        &self,
        key: &str,
        description: &str,
        minor: i64,
        actor: Option<Provenance>,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        positive(minor)?;
        let description = description_of(description)?;
        let entry_id = entry_id_for(key);
        // A retry recomputes its pool split against balances that have moved
        // since the original hold landed, so the draft it builds need not be
        // the committed entry's shape: the committed one is the answer, like
        // `top_up`'s repaying shape. A stored entry that is not this hold —
        // another request under the same key — is the engine's key-reuse
        // conflict.
        if let Some(stored) = self.store.get(entry_id).await? {
            return self.hold_replay(&stored, &description, actor.as_ref(), minor, key);
        }
        // The suspension check is a short transaction of its own (issue #162): the
        // shared org-admission lock makes it wait for a suspension mid-commit, so a
        // check that runs observes the settled flag — a hold decided before the
        // suspension lands is an in-flight hold, which settles like every other.
        // Unlike `hold_for_key` the lock is not held across the append: an
        // unattributed hold has no per-key serialization to preserve anyway, and
        // holding a connection through it would starve the pool the append draws on.
        if let Ok(organization) = Uuid::parse_str(&self.tenant_id) {
            let mut tx = self.store.pool().begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended($1 || $2, 0))")
                .bind(ORG_ADMISSION_LOCK)
                .bind(organization.to_string())
                .execute(&mut *tx)
                .await?;
            let mut suspended: bool = sqlx::query_scalar(
                "SELECT (suspended_at IS NOT NULL OR dunning_suspended_at IS NOT NULL OR EXISTS (SELECT 1 FROM oxsum.customer_profiles c WHERE c.organization_id=oxsum.organizations.organization_id AND c.state='archived')) FROM oxsum.organizations \
                 WHERE organization_id = $1",
            )
            .bind(organization)
            .fetch_one(&mut *tx)
            .await?;
            if !suspended {
                // A past-due plan statement past its grace also suspends
                // admission (issue #612, T21): the binding's
                // `dunning_suspended_at` is the gate the overdue sweep stamps
                // and the payment reconcile lifts — read under the same
                // org-admission lock so a settled suspension is observed. The
                // table-presence check runs first so a pre-`sold_plans`
                // schema (a legacy baseline fixture) parses no dangling
                // reference: no `customer_plans` means no plan suspension.
                let has_plans: bool =
                    sqlx::query_scalar("SELECT to_regclass('oxsum.customer_plans') IS NOT NULL")
                        .fetch_one(&mut *tx)
                        .await?;
                if has_plans {
                    suspended = sqlx::query_scalar(
                        "SELECT COALESCE((SELECT dunning_suspended_at IS NOT NULL FROM oxsum.customer_plans \
                           WHERE organization_id=$1 \
                             AND state IN ('pending_payment','active','renewal_pending','past_due') \
                           LIMIT 1), false)",
                    )
                    .bind(organization)
                    .fetch_one(&mut *tx)
                    .await?;
                }
            }
            drop(tx);
            if suspended {
                return Err(WalletError::Forbidden(
                    "the organization is suspended".into(),
                ));
            }
        }
        // The lock-wait itself is bounded off the pool: `pg_try_advisory_xact_lock`
        // answers immediately, so a hold that loses the race drops its
        // connection and sleeps on async time rather than pinning it through a
        // lock wait — enough queued holds on one tenant would otherwise exhaust
        // the shared pool behind the append lock (issue #486's doctrine, moved
        // one level down to the ledger lock the tx ends on). Waiters past the
        // patience window get the same `PoolBusy` a saturated checkout answers.
        let mut lock_waits = LOCK_WAIT_ATTEMPTS;
        let mut split_attempts = POOL_SPLIT_ATTEMPTS;
        loop {
            // The split, the pinned promo draws and the hold append share one
            // transaction: the `promo_draws` rows name the lots whose
            // `remaining_minor` this hold decremented, so settlement debits the
            // rows admission picked rather than re-scanning, and the pending
            // debit the entry posts commits beside them or not at all (issue
            // #609). Holding the transaction's connection through the append
            // costs nothing the split's borrowed read did not already — the
            // append is the same connection the draw-pinning rides.
            let mut tx = self.store.pool().begin().await?;
            // Claim the ledger's append lock up front, without waiting on the
            // connection: the engine takes it re-entrantly inside `append_in`
            // (`pg_advisory_xact_lock` counts per session), so a held lock
            // here means the append below never waits — and a lost claim means
            // this hold frees the connection it would have waited on. A
            // lock-wait miss is not a split attempt — the balance read has not
            // happened yet, so there is nothing a racing append invalidated.
            let claimed: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
                .bind(self.store.append_lock())
                .fetch_one(&mut *tx)
                .await?;
            if !claimed {
                tx.rollback().await?;
                lock_waits -= 1;
                if lock_waits == 0 {
                    return Err(WalletError::PoolBusy);
                }
                tokio::time::sleep(LOCK_WAIT_BACKOFF).await;
                continue;
            }
            let split = self.pool_split(&mut tx, minor).await?;
            if split.promo > 0 {
                self.pin_promo_draws_in(&mut tx, key, split.promo).await?;
            }
            let receipt = self
                .append_in(
                    &mut tx,
                    self.hold_entry(key, &description, split, actor.clone(), on)?,
                )
                .await;
            match receipt {
                Err(error @ WalletError::InsufficientFunds) => {
                    tx.rollback().await?;
                    split_attempts -= 1;
                    if split_attempts == 0 {
                        return Err(error);
                    }
                }
                // A concurrent `Wallet::open` on this same tenant can still be
                // walking `apply_schema`'s DDL when this hold writes: AccessExclusive
                // on the ledger tables against the append's RowExclusive is a
                // textbook cycle — a `40P01`, not corruption. The append is
                // deterministic against committed state, so the retry re-reads
                // and lands.
                Err(error) if is_deadlock(&error) => {
                    tx.rollback().await?;
                    lock_waits -= 1;
                    if lock_waits == 0 {
                        return Err(error);
                    }
                    tokio::time::sleep(LOCK_WAIT_BACKOFF).await;
                }
                // The key was taken between our read and the append — by a
                // racing copy of this same request or by a different one.
                Err(WalletError::Conflict(_)) => {
                    tx.rollback().await?;
                    return match self.store.get(entry_id).await? {
                        Some(stored) => {
                            self.hold_replay(&stored, &description, actor.as_ref(), minor, key)
                        }
                        None => Err(WalletError::Conflict(format!(
                            "idempotency key {key:?} is already taken"
                        ))),
                    };
                }
                Ok(receipt) => {
                    tx.commit().await?;
                    return Ok(receipt);
                }
                Err(error) => {
                    tx.rollback().await?;
                    return Err(error);
                }
            }
        }
    }

    /// Answers for a key that already has a committed entry: the stored
    /// entry's receipt when it is the hold being asked for — same amount,
    /// same description, same actor — whatever its pool split (balances move
    /// after a hold lands, so the committed shape is the answer, never the
    /// split a retry would compute now), and the key-reuse conflict when the
    /// entry is anything else.
    fn hold_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        actor: Option<&Provenance>,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let held: i64 = stored
            .entry
            .postings()
            .iter()
            .filter(|p| {
                self.is_pool(p.account)
                    && p.layer == Layer::Pending
                    && p.direction == Direction::Debit
            })
            .map(|p| p.amount.to_minor())
            .sum();
        let stored_actor = stored
            .entry
            .provenance()
            .actor
            .as_ref()
            .map(|label| label.as_str());
        let asked_actor = actor.and_then(|p| p.actor.as_ref().map(|label| label.as_str()));
        if held == minor
            && stored.entry.description().as_str() == description.as_str()
            && stored_actor == asked_actor
        {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }

    /// The split of `minor` across the pools, most-restricted money first: the
    /// promo pool funds what it can — it is the balance that can expire — then
    /// the bonus pool, the purchased wallet carries what it can, and the
    /// credit line picks up the rest. `split.total() == minor` always; any side
    /// may be zero, and `credit` above what the line still carries is what the
    /// append's `FundedReservations` check refuses.
    async fn pool_split(
        &self,
        conn: &mut sqlx::PgConnection,
        minor: i64,
    ) -> Result<PoolSplit, WalletError> {
        let promo = minor.min(self.pool_available_in(conn, self.promo).await?.max(0));
        let bonus = (minor - promo).min(self.pool_available_in(conn, self.bonus).await?.max(0));
        let wallet =
            (minor - promo - bonus).min(self.pool_available_in(conn, self.wallet).await?.max(0));
        Ok(PoolSplit {
            promo,
            bonus,
            wallet,
            credit: minor - promo - bonus - wallet,
        })
    }

    /// Builds the pending-layer hold entry for a computed split: a pending debit
    /// on each pool it draws and the pending credit on revenue for the whole
    /// amount, with the optional provenance actor the hold attributes to.
    fn hold_entry(
        &self,
        key: &str,
        description: &Description,
        split: PoolSplit,
        actor: Option<Provenance>,
        on: Date,
    ) -> Result<Entry<Draft, SCALE>, WalletError> {
        let minor = split.total();
        let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
            .with_description(description.clone());
        if let Some(actor) = actor {
            draft = draft.with_provenance(actor);
        }
        for (account, part) in [
            (self.promo, split.promo),
            (self.bonus, split.bonus),
            (self.wallet, split.wallet),
            (self.credit_line, split.credit),
        ] {
            if part > 0 {
                draft = draft.post(
                    Posting::debit(account, Credits::from_minor(part), currency())
                        .in_layer(Layer::Pending),
                );
            }
        }
        Ok(draft.post(
            Posting::credit(self.revenue, Credits::from_minor(minor), currency())
                .in_layer(Layer::Pending),
        ))
    }

    /// Hold attributed to an API key, enforcing the key's spend limit when it has one.
    ///
    /// The hold entry carries the key id as its provenance actor, so the key's committed
    /// spend — settled charges plus outstanding holds attributed to it — is readable from
    /// the ledger afterwards, even for keys that have no limit today.
    ///
    /// The limit check is serialized per key and atomic with the append: the transaction
    /// takes a per-key advisory lock, re-reads the limit from the key row (locked, so a
    /// concurrent limit change cannot slip between the read and the append), reads the
    /// key's committed spend from the ledger, and only then appends through the engine's
    /// normal path. The lock is held until commit — after the engine's append committed —
    /// so a racing second hold for the same key can only read usage that already includes
    /// the first hold. Two racing holds cannot both pass the check.
    ///
    /// Refused with [`WalletError::KeyLimitExceeded`] when the hold would push the key's
    /// committed spend past its limit, and with [`WalletError::InsufficientFunds`] when the
    /// balance cannot cover it — the key limit never overrides the balance.
    ///
    /// `model` is the gateway model the request wants, when the caller names one — the
    /// `/holds` endpoint passes `None` and is not governed by a model allowlist. A hold
    /// for a model the key may not use is [`WalletError::Forbidden`].
    ///
    /// The reservation splits across the pools like [`hold`](Self::hold)'s does; the
    /// split is computed on the transaction's connection so the lock, the limit read
    /// and the balance read agree on what is committed, and the whole flow retries
    /// when a racing append invalidates the split it chose.
    /// `mandate` is a verified `X-Oxsum-Mandate` the caller parsed and checked
    /// against the key's public half ([`crate::mandates::parse_presented`]).
    /// `hold_for_key` applies its bounds on top of the key's own — window, cap
    /// and model — under the same per-key lock, so a delegate's spend is
    /// checked against the ledger-derived committed figure atomically with the
    /// append, exactly like the key's own limit.
    #[allow(clippy::too_many_arguments)]
    pub async fn hold_for_key(
        &self,
        key: &ActingKey,
        model: Option<&str>,
        idem_key: &str,
        description: &str,
        minor: i64,
        on: Date,
        mandate: Option<&PresentedMandate>,
    ) -> Result<Receipt, WalletError> {
        positive(minor)?;
        let description = description_of(description)?;
        let mut actor = Provenance::none()
            .with_actor(&key.key_id.as_simple().to_string())
            .map_err(invalid)?;
        if let Some(mandate) = mandate {
            // The mandate id rides the hold entry's provenance correlation, so
            // the mandate's committed spend is readable from the ledger itself —
            // settled charges and outstanding holds alike — the same way the
            // key's own limit reads it, and a settle copies it onto the charge
            // the way `outstanding_actor` is copied (issue #262).
            actor = actor
                .with_correlation(&mandate.verified.mandate_id.as_simple().to_string())
                .map_err(invalid)?;
        }
        let entry_id = entry_id_for(idem_key);
        // A committed entry under the key is this request's answer before any
        // constraint is consulted: a replayed hold gets its own receipt even
        // when a budget or an allowlist has since moved — its split need not
        // match either, because the balances it was computed against moved too.
        if let Some(stored) = self.store.get(entry_id).await? {
            return self.hold_replay(&stored, &description, Some(&actor), minor, idem_key);
        }
        // Queue the key's own holds before a connection is checked out (issue
        // #486): the advisory lock below serializes them, and a hold that waits
        // for it inside a transaction pins a pooled connection for the whole
        // wait — enough queued holds on one key exhaust the shared pool and
        // every tenant's reads fail behind them. Waiting here costs no
        // connection: the semaphore admits only `KEY_QUEUE_WIDTH` into the
        // transaction, and the rest of the queue sits on async permits until a
        // slot frees or `KEY_QUEUE_WAIT` refuses the wait fast.
        let _queue = self.key_queue_wait(&key.key_id).await?;
        let mut last_err = None;
        for _ in 0..POOL_SPLIT_ATTEMPTS {
            let mut tx = self.store.pool().begin().await?;
            // The check and the append serialize on this lock, per key. The engine's own
            // append takes the per-tenant lock inside, so the order is always key lock first,
            // tenant lock second, and the engine never takes the key lock: no deadlock, and no
            // tenant-level contention beyond the append lock that already exists.
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(limit_lock_key(&self.tenant_id, &key.key_id))
                .execute(&mut *tx)
                .await?;
            // The org-admission lock, shared (issue #162): holds of one organization
            // run in parallel, while `set_suspended`'s exclusive lock waits for every
            // admitted hold and makes the next one observe the flag. It is taken
            // before any row lock, so every transaction's order is advisory locks
            // first, rows second — no cycle with the membership writers that take
            // the organization and membership rows only.
            sqlx::query(
                "SELECT pg_advisory_xact_lock_shared(hashtextextended($2 || organization_id::text, 0)) \
                 FROM oxsum.api_keys WHERE key_id = $1",
            )
            .bind(key.key_id)
            .bind(ORG_ADMISSION_LOCK)
            .execute(&mut *tx)
            .await?;
            // The constraints in force now, not the ones the request authenticated with:
            // locked, so a PATCH landing between authentication and this hold cannot be missed.
            // The join reads the organization's suspension and names the minting member
            // (`created_by` names the member; a removed membership leaves the key
            // unbudgeted); the membership row itself locks in a second read below,
            // because FOR UPDATE cannot reach the nullable side of an outer join
            // (issue #162).
            let constraints = sqlx::query(
                "SELECT k.spend_limit_minor, k.budget_duration, k.model_allowlist, \
                        k.max_concurrent_holds, k.organization_id, \
                        (o.suspended_at IS NOT NULL OR o.dunning_suspended_at IS NOT NULL OR EXISTS (SELECT 1 FROM oxsum.customer_profiles c WHERE c.organization_id=o.organization_id AND c.state='archived')) AS suspended, \
                        m.user_id AS budget_member, m.team_id AS budget_team \
                 FROM oxsum.api_keys k \
                 JOIN oxsum.organizations o ON o.organization_id = k.organization_id \
                 LEFT JOIN oxsum.memberships m \
                        ON m.organization_id = k.organization_id \
                       AND m.user_id = k.created_by \
                 WHERE k.key_id = $1 FOR UPDATE OF k",
            )
            .bind(key.key_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(constraints) = constraints else {
                // Keys are never deleted; a missing row means the credential died mid-request.
                return Err(WalletError::Unauthenticated);
            };
            // The minting member's row, locked for update so the budget read and the
            // holds that check against it serialize: two racing holds of one member's
            // keys cannot both fit under the cap.
            let member_budget = {
                use sqlx::Row;
                let member: Option<Uuid> = constraints.try_get("budget_member")?;
                let organization: Uuid = constraints.try_get("organization_id")?;
                let team: Option<Uuid> = constraints.try_get("budget_team")?;
                let mut member_budget: Option<i64> = None;
                let mut team_budget: Option<i64> = None;
                if let Some(member) = member {
                    member_budget = sqlx::query_scalar(
                        "SELECT budget_limit_minor FROM oxsum.memberships \
                         WHERE organization_id = $1 AND user_id = $2 FOR UPDATE",
                    )
                    .bind(organization)
                    .bind(member)
                    .fetch_one(&mut *tx)
                    .await?;
                }
                if let Some(team) = team {
                    // The team's row locks beside the membership's: the cap
                    // read and the holds that check against it serialize, so
                    // two racing holds of the team's members cannot both fit
                    // (issue #440). LOCK the team row only when the member
                    // sits in one — an unassigned member reads no cap here.
                    team_budget = sqlx::query_scalar(
                        "SELECT budget_limit_minor FROM oxsum.teams \
                         WHERE organization_id = $1 AND team_id = $2 FOR UPDATE",
                    )
                    .bind(organization)
                    .bind(team)
                    .fetch_one(&mut *tx)
                    .await?;
                }
                (member, organization, member_budget, team, team_budget)
            };
            {
                use sqlx::Row;
                // A suspended organization reserves nothing new; its in-flight
                // holds still settle, and money-in still lands.
                if constraints.try_get::<bool, _>("suspended")? {
                    return Err(WalletError::Forbidden(
                        "the organization is suspended".into(),
                    ));
                }
                // A past-due plan statement past its grace suspends the
                // wallet-side overage the same way (issue #612, T21): the
                // binding's `dunning_suspended_at` is the gate the overdue
                // sweep stamps and the payment reconcile lifts, read under
                // this same key+admission lock. The table-presence check runs
                // first so a pre-`sold_plans` schema parses no dangling
                // reference.
                let organization: Uuid = constraints.try_get("organization_id")?;
                let has_plans: bool =
                    sqlx::query_scalar("SELECT to_regclass('oxsum.customer_plans') IS NOT NULL")
                        .fetch_one(&mut *tx)
                        .await?;
                if has_plans {
                    let plan_suspended: bool = sqlx::query_scalar(
                        "SELECT COALESCE((SELECT dunning_suspended_at IS NOT NULL FROM oxsum.customer_plans \
                           WHERE organization_id=$1 \
                             AND state IN ('pending_payment','active','renewal_pending','past_due') \
                           LIMIT 1), false)",
                    )
                    .bind(organization)
                    .fetch_one(&mut *tx)
                    .await?;
                    if plan_suspended {
                        return Err(WalletError::Forbidden(
                            "the organization is suspended".into(),
                        ));
                    }
                }
            }
            let split = self.pool_split(&mut tx, minor).await?;
            let draft = self.hold_entry(idem_key, &description, split, Some(actor.clone()), on)?;
            {
                use sqlx::Row;
                // Like the limit, the allowlist guards new spend: a replayed hold answers
                // with its own receipt even if the list has since dropped the model.
                let allowlist: Option<Vec<String>> = constraints.try_get("model_allowlist")?;
                if let (Some(model), Some(list)) = (model, &allowlist)
                    && !list.iter().any(|allowed| allowed == model)
                {
                    return Err(WalletError::Forbidden(format!(
                        "this key may not call model {model:?}"
                    )));
                }
                let limit: Option<i64> = constraints.try_get("spend_limit_minor")?;
                if let Some(limit) = limit {
                    // A budget window makes the limit periodic: committed counts only the
                    // current period's settled charges; outstanding holds count whatever
                    // their age, because they still reserve money now.
                    let since = constraints
                        .try_get::<Option<String>, _>("budget_duration")?
                        .as_deref()
                        .map(BudgetDuration::parse)
                        .transpose()?
                        .map(|d| d.period_start(on));
                    let committed = self.key_committed_in(&mut tx, &key.key_id, since).await?;
                    if committed + minor > limit {
                        return Err(WalletError::KeyLimitExceeded {
                            limit_minor: limit,
                            committed_minor: committed,
                        });
                    }
                }
                // The member budget caps what the key's creator may commit across
                // every key they minted — the membership row is locked above, so
                // the sum and the append agree (issue #162).
                let (member, organization, member_budget, team, team_budget) = member_budget;
                if let (Some(member), Some(limit)) = (member, member_budget) {
                    let committed = self
                        .member_committed_in(&mut tx, &organization, &member)
                        .await?;
                    if committed + minor > limit {
                        return Err(WalletError::KeyLimitExceeded {
                            limit_minor: limit,
                            committed_minor: committed,
                        });
                    }
                }
                // The team budget caps what the subgroup's members may commit
                // across every key they minted — the team row is locked beside
                // the membership's, so the sum and the append agree (issue
                // #440). The layer sits between member and organization: a
                // member outside any team, or a team without a cap, reads no
                // bound here.
                if let (Some(team), Some(limit)) = (team, team_budget) {
                    let committed = self
                        .team_committed_in(&mut tx, &organization, &team)
                        .await?;
                    if committed + minor > limit {
                        return Err(WalletError::KeyLimitExceeded {
                            limit_minor: limit,
                            committed_minor: committed,
                        });
                    }
                }
                // The concurrent-holds cap bounds hold-spam: how many reservations
                // the key may have open at once, counted from the ledger's pending
                // entries under the same lock so racing holds cannot both fit.
                let hold_cap: Option<i32> = constraints.try_get("max_concurrent_holds")?;
                if let Some(cap) = hold_cap {
                    let open = self.key_open_holds_in(&mut tx, &key.key_id).await?;
                    if open >= i64::from(cap) {
                        return Err(WalletError::TooManyHolds {
                            limit: i64::from(cap),
                            open,
                        });
                    }
                }
                // The mandate's own bounds, on top of the key's (issue #262). Its
                // signature was already verified against the key's public half;
                // what runs here under the per-key lock is what needs the
                // ledger-derived figure: the window, the model subset and the
                // committed-under-mandate cap.
                if let Some(mandate) = mandate {
                    let now = OffsetDateTime::now_utc();
                    // The mandate delegates this key only — a blob minted for one
                    // key never spends under another, and the `orgId` binding
                    // keeps a mandate out of a different organization's ledger.
                    if mandate.verified.key_id != key.key_id {
                        return Err(WalletError::InvalidMandate);
                    }
                    // The window bound is the mandate's own, refused as its own
                    // code so a delegate learns the delegation expired rather
                    // than the organization's budget biting.
                    if now.unix_timestamp() < mandate.not_before_ts() {
                        return Err(WalletError::MandateOutOfWindow { not_yet: true });
                    }
                    if now.unix_timestamp() > mandate.not_after_ts() {
                        return Err(WalletError::MandateOutOfWindow { not_yet: false });
                    }
                    // The mandate's allowlist narrows the key's, never widens:
                    // the request's model must sit inside it.
                    if let (Some(model), Some(models)) = (model, &mandate.verified.models)
                        && !models.iter().any(|allowed| allowed == model)
                    {
                        return Err(WalletError::Forbidden(format!(
                            "the mandate does not delegate model {model:?}"
                        )));
                    }
                    // committed-under-mandate + this freeze ≤ cap, read from the
                    // ledger under the per-key lock so a racing hold cannot both
                    // fit — the same shape `key_committed_in` takes per key.
                    let committed = self
                        .mandate_committed_in(&mut tx, &mandate.verified.mandate_id)
                        .await?;
                    if committed + minor > mandate.verified.cap_minor {
                        return Err(WalletError::MandateExceeded {
                            cap_minor: mandate.verified.cap_minor,
                            committed_minor: committed,
                        });
                    }
                }
            }
            // The promo part of the split pins its lots here, inside the hold's
            // transaction, just before the entry appends into the same one:
            // `promo_draws` names which lots fund `split.promo`, so the
            // settlement debits the rows admission picked and a release returns
            // to exactly them — never a re-scan. The draw decrement and the
            // pending debit commit or roll back together (issue #609).
            if split.promo > 0 {
                // The pin writes `promo_lots`, so the append lock precedes it
                // — the global order is append advisory lock first, promo row
                // locks second (issue #723). No promo share means no promo
                // row, so the engine's append takes the lock on its own.
                self.lock_append_in(&mut tx).await?;
                self.pin_promo_draws_in(&mut tx, idem_key, split.promo)
                    .await?;
            }
            match self.append_in(&mut tx, draft).await {
                Err(error @ WalletError::InsufficientFunds) => {
                    tx.rollback().await?;
                    last_err = Some(error);
                }
                // The key was taken between our read and the append — by a
                // racing copy of this same request or by a different one.
                Err(WalletError::Conflict(_)) => {
                    tx.rollback().await?;
                    return match self.store.get(entry_id).await? {
                        Some(stored) => {
                            self.hold_replay(&stored, &description, Some(&actor), minor, idem_key)
                        }
                        None => Err(WalletError::Conflict(format!(
                            "idempotency key {idem_key:?} is already taken"
                        ))),
                    };
                }
                receipt => {
                    let receipt = receipt?;
                    tx.commit().await?;
                    return Ok(receipt);
                }
            }
        }
        Err(last_err.unwrap_or(WalletError::InsufficientFunds))
    }

    /// The in-process queue [`hold_for_key`](Self::hold_for_key) waits in before
    /// checking out a connection (issue #486).
    ///
    /// The advisory lock inside the transaction serializes one key's holds
    /// against each other; what it cannot do is keep a waiting hold from pinning
    /// the pooled connection it already checked out. Under enough concurrency on
    /// one key every pooled connection is checked-out-and-waiting on the lock,
    /// and the shared pool times out behind them — a key with no constraints
    /// could starve every tenant. The semaphore admits `KEY_QUEUE_WIDTH` into
    /// the locked transaction and parks the rest of the queue on async permits:
    /// a queued hold holds no connection, so the wait is free until it is
    /// admitted or `KEY_QUEUE_WAIT` refuses it as [`WalletError::PoolBusy`], the
    /// same fast-fail a saturated checkout answers.
    ///
    /// The guard lives until the caller's hold resolves, so the permit covers
    /// every retry the pool-split loop runs — the lock's own serialization stays
    /// exactly one waiter's depth inside the transaction, and a semaphore keyed
    /// on the same [`limit_lock_key`] hash can never widen it. Multi-instance
    /// deployments still serialize on the database lock; the queue only removes
    /// the in-process part of the pile-up.
    ///
    /// # Errors
    ///
    /// [`WalletError::PoolBusy`] when the queue stays full past `KEY_QUEUE_WAIT`.
    async fn key_queue_wait(
        &self,
        key_id: &Uuid,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, WalletError> {
        let lock_key = limit_lock_key(&self.tenant_id, key_id);
        let semaphore = {
            // A poisoned lock means another thread panicked mid-update; handing
            // out permits from half-updated queue state would be worse.
            #[expect(clippy::expect_used, reason = "a poisoned lock must panic")]
            let mut queues = self.key_queue.lock().expect("key queue mutex poisoned");
            queues
                .entry(lock_key)
                .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(KEY_QUEUE_WIDTH)))
                .clone()
        };
        let permit = tokio::time::timeout(KEY_QUEUE_WAIT, semaphore.acquire_owned())
            .await
            .map_err(|_| {
                // The queue drained nothing in the caller's patience window —
                // refuse like a saturated checkout rather than queue deeper.
                WalletError::PoolBusy
            })?
            .map_err(|_| WalletError::PoolBusy)?;
        // A semaphore nobody queues behind is dead weight in the map; it stays
        // (an idle entry costs a few bytes) because removing it races a waiter
        // that already took the `Arc` — the map bounds itself by the keys that
        // were ever busy, which `max_concurrent_holds`-less churn does not grow.
        Ok(permit)
    }

    /// What one API key has committed: settled charges plus outstanding holds attributed
    /// to it, in minor units. Read from the ledger's own postings — the pending release
    /// and the settled charge of a settlement both carry the hold's actor — so it cannot
    /// drift from the books.
    pub async fn key_committed(&self, key_id: &Uuid) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.key_committed_in(&mut conn, key_id, None).await
    }

    /// The [`key_committed`](Self::key_committed) read for a window — settled
    /// charges plus outstanding holds, the settled half scoped to entries
    /// booked on or after `since` (issue #232: the consumption a
    /// `budgetDuration` budget is measured against).
    pub async fn key_committed_window(
        &self,
        key_id: &Uuid,
        since: Option<Date>,
    ) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.key_committed_in(&mut conn, key_id, since).await
    }

    /// The `member_committed_in` read on its own
    /// connection: the member's committed spend across every key they minted
    /// (issue #232).
    pub async fn member_committed(
        &self,
        organization_id: &Uuid,
        member: &Uuid,
    ) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.member_committed_in(&mut conn, organization_id, member)
            .await
    }

    /// The [`key_committed`](Self::key_committed) read, on the caller's connection: the
    /// limit check runs it inside the transaction that holds the per-key advisory lock.
    ///
    /// `since` scopes the settled half to entries booked on or after that date — the
    /// period start a `budget_duration` window gives. Pending postings always count in
    /// full: an outstanding hold reserves money now whatever day it was taken.
    async fn key_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        key_id: &Uuid,
        since: Option<Date>,
    ) -> Result<i64, WalletError> {
        // The schema name is assembled from the validated tenant id, like `Wallet::open`
        // does; the actor is the key id in uuid simple form, bound as a parameter.
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_actor = $2 \
               AND (p.layer = 'pending' OR $3::date IS NULL OR e.booking_date >= $3)",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(key_id.as_simple().to_string())
            .bind(since)
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }

    /// What one member's keys have committed in total: the sum of
    /// [`key_committed_in`](Self::key_committed_in) over every key they minted —
    /// the member budget's denominator. The ledger attributes each posting to
    /// the key's actor id, and `api_keys.created_by` maps keys to the member, so
    /// the sum is the ledger's own figure for the member's spend (issue #162).
    ///
    /// Run inside the transaction holding the member's locked membership row, so
    /// the sum and the hold append that follows it agree on what is committed.
    /// Revoked keys still count: the money they spent stayed spent.
    async fn member_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        organization_id: &Uuid,
        member: &Uuid,
    ) -> Result<i64, WalletError> {
        // The actor is the key id in uuid simple form, like `key_committed_in`.
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT replace(key_id::text, '-', '') FROM oxsum.api_keys \
             WHERE organization_id = $1 AND created_by = $2",
        )
        .bind(organization_id)
        .bind(member)
        .fetch_all(&mut *conn)
        .await?;
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_actor = ANY($2)",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(keys)
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }

    /// The `team_committed_in` read on its own
    /// connection: the team's committed spend across every key its members
    /// minted (issue #440).
    pub async fn team_committed(
        &self,
        organization_id: &Uuid,
        team_id: &Uuid,
    ) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.team_committed_in(&mut conn, organization_id, team_id)
            .await
    }

    /// What one team's members have committed in total: the sum of
    /// [`key_committed_in`](Self::key_committed_in) over every key minted by
    /// the team's members — the team budget's denominator (issue #440). The
    /// ledger attributes each posting to the key's actor id, and
    /// `memberships.team_id` + `api_keys.created_by` map keys to the team, so
    /// the sum is the ledger's own figure for the subgroup's spend — the same
    /// attribution `member_committed_in` gives
    /// per member, aggregated.
    ///
    /// Run inside the transaction holding the team's locked row, so the sum
    /// and the hold append that follows it agree on what is committed.
    /// Committed spend follows the roster, not the team's history: a member
    /// moved between teams carries their keys' spend into the new team's sum,
    /// and keys minted by a since-removed member still count.
    async fn team_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        organization_id: &Uuid,
        team_id: &Uuid,
    ) -> Result<i64, WalletError> {
        // The actor is the key id in uuid simple form, like
        // `key_committed_in`; the roster is the members the team groups.
        let keys: Vec<String> = sqlx::query_scalar(
            "SELECT replace(k.key_id::text, '-', '') FROM oxsum.api_keys k \
             JOIN oxsum.memberships m \
               ON m.organization_id = k.organization_id AND m.user_id = k.created_by \
             WHERE k.organization_id = $1 AND m.team_id = $2",
        )
        .bind(organization_id)
        .bind(team_id)
        .fetch_all(&mut *conn)
        .await?;
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_actor = ANY($2)",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(keys)
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }

    /// How many holds the key has outstanding, counted from the ledger's own
    /// entries: an entry that debits a pool account in the pending layer is a
    /// live reservation, and an entry that credits one is the release leg of a
    /// settlement — the release rides the settlement entry, so a hold stays open
    /// until another entry frees it. The difference is the open count.
    ///
    /// Run inside the transaction holding the per-key advisory lock, so the count
    /// and the hold append that follows it agree on what is committed.
    async fn key_open_holds_in(
        &self,
        conn: &mut sqlx::PgConnection,
        key_id: &Uuid,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT count(*) FILTER (WHERE pending_debit > 0) \
                  - count(*) FILTER (WHERE pending_credit > 0) \
             FROM ( \
                 SELECT e.entry_id, \
                        SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor ELSE 0 END) \
                            AS pending_debit, \
                        SUM(CASE WHEN p.direction = 'C' THEN p.amount_minor ELSE 0 END) \
                            AS pending_credit \
                 FROM ledger_{schema}.postings p \
                 JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = ANY($1) AND e.provenance_actor = $2 \
                   AND p.layer = 'pending' \
                 GROUP BY e.entry_id \
             ) per_entry",
            schema = self.tenant_id,
        );
        let open: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(key_id.as_simple().to_string())
            .fetch_one(&mut *conn)
            .await?;
        Ok(open)
    }

    /// What one mandate has committed: the settled charges under its
    /// What one mandate has committed in total: the sum of settled charges and
    /// outstanding holds its entries carry, read from the ledger by the mandate
    /// id each writes into `provenance_correlation` — the same figure
    /// [`key_committed_in`](Self::key_committed_in) answers per key, sliced by
    /// the delegation instead of the key (issue #262).
    ///
    /// Run inside the transaction holding the per-key advisory lock, so the sum
    /// and the hold append that follows it agree on what is committed. A hold's
    /// pending debits count toward the cap; the settlement's release leg credits
    /// the same accounts back, so a settled charge nets to what it billed.
    async fn mandate_committed_in(
        &self,
        conn: &mut sqlx::PgConnection,
        mandate_id: &Uuid,
    ) -> Result<i64, WalletError> {
        // The schema name is assembled from the validated tenant id, like
        // `Wallet::open` does; the mandate id is the entry's provenance
        // correlation in uuid simple form, bound as a parameter.
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN p.direction = 'D' THEN p.amount_minor \
                              ELSE -p.amount_minor END), 0)::bigint AS committed \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND e.provenance_correlation = $2",
            schema = self.tenant_id,
        );
        let committed: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(mandate_id.as_simple().to_string())
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
    }

    /// The ledger side of the entitlement read (issue #501): every check the
    /// ledger itself can answer for a request — suspension, the allowlist, the
    /// spend caps layered key → member → team, the open-holds count, and a
    /// presented mandate's window, model subset and committed cap — gathered
    /// on one connection under no lock.
    ///
    /// Every constraint is read even when an earlier one already refuses: the
    /// `checks` breakdown the endpoint exists for needs the whole picture, and
    /// a read that short-circuits could not say what else was wrong. The
    /// request window (the server's in-process limiter) and the settled-token
    /// window (`Db::tpm_window*` on the `oxsum` tables) are not in here — the
    /// limits they are judged against ride the returned [`EntitlementRead`] so
    /// the route can run those two and compose the answer itself.
    ///
    /// `model` is the gateway model the request would call (`None` skips the
    /// allowlist question, like `POST /api/v1/holds`); `estimate` is a
    /// hypothetical charge in minor units — the freeze a real request would
    /// take — judged against the spend caps' headroom (`None` asks whether the
    /// caps have room at all, against one minor unit — the smallest hold the
    /// ledger takes). `mandate` is the already-verified delegation the caller
    /// presented — the route ran [`crate::mandates::parse_presented`]; what is
    /// judged here are its verified fields, and a mandate minted for another
    /// key refuses `mandate_invalid` rather than leak which key it belongs to.
    /// `tags` are the request's `metadata` tag values: the key's
    /// `tokensPerMinuteScopes` buckets resolve to their scope names only where
    /// the request would actually land in them, so a scope the turn does not
    /// tag into is no more a constraint than it is at admission.
    ///
    /// The figures are a snapshot a racing request may already have spent —
    /// the surface is "can I", not "reserve me", and the atomic admission
    /// checks stay authoritative. `scope` is the caller's [`crate::KeyScope`],
    /// applied like the key list's: a member asks only about the keys they
    /// created — any other id answers `None`, indistinguishable from a foreign
    /// or nonexistent one.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    #[allow(clippy::too_many_arguments)]
    pub async fn entitlement_read(
        &self,
        organization_id: &Uuid,
        key_id: &Uuid,
        scope: crate::KeyScope,
        model: Option<&str>,
        estimate: Option<i64>,
        tags: &BTreeMap<String, String>,
        mandate: Option<&EntitlementMandate>,
    ) -> Result<Option<EntitlementRead>, WalletError> {
        use crate::entitlement::{EntitlementRead, MandateVerdict};
        let mut conn = self.store.pool().acquire().await?;
        // The scope clause is the key list's: `Own` narrows to keys the member
        // created; a scoped-out id reads as missing — 404 upstream, never 403.
        let row = {
            let created_by = match scope {
                crate::KeyScope::Own(user_id) => Some(user_id),
                crate::KeyScope::Key(id) => {
                    if id != *key_id {
                        return Ok(None);
                    }
                    None
                }
                crate::KeyScope::Organization | crate::KeyScope::All => None,
            };
            sqlx::query(
                "SELECT k.key_id, k.spend_limit_minor, k.budget_duration, \
                        k.model_allowlist, k.max_concurrent_holds, \
                        k.requests_per_minute, k.tokens_per_minute, \
                        k.tokens_per_minute_scopes, \
                        k.revoked_at IS NOT NULL OR \
                            (k.expires_at IS NOT NULL AND k.expires_at <= now()) AS inactive, \
                        (o.suspended_at IS NOT NULL OR o.dunning_suspended_at IS NOT NULL OR EXISTS (SELECT 1 FROM oxsum.customer_profiles c WHERE c.organization_id=o.organization_id AND c.state='archived')) AS suspended, \
                        m.user_id AS budget_member, m.team_id AS budget_team \
                 FROM oxsum.api_keys k \
                 JOIN oxsum.organizations o ON o.organization_id = k.organization_id \
                 LEFT JOIN oxsum.memberships m \
                        ON m.organization_id = k.organization_id \
                       AND m.user_id = k.created_by \
                 WHERE k.key_id = $1 AND k.organization_id = $2 \
                   AND ($3::uuid IS NULL OR k.created_by = $3)",
            )
            .bind(key_id)
            .bind(organization_id)
            .bind(created_by)
            .fetch_optional(&mut *conn)
            .await?
        };
        let Some(row) = row else {
            return Ok(None);
        };
        use sqlx::Row;
        let key_active = !row.try_get::<bool, _>("inactive")?;
        let org_suspended = row.try_get::<bool, _>("suspended")?;
        // The allowlist question only runs when a model was asked: `None` skips
        // it the way `POST /api/v1/holds` does, and `model_allowed` stays
        // `None` to say so.
        let allowlist: Option<Vec<String>> = row.try_get("model_allowlist")?;
        let model_allowed = model.map(|model| {
            allowlist
                .as_ref()
                .is_none_or(|list| list.iter().any(|allowed| allowed == model))
        });
        // The spend limit's headroom — committed reads the same figure
        // `hold_for_key` judges, budget-windowed when the key carries one.
        // `estimate` is what the request would reserve; absent, the check asks
        // whether the caps have room at all — one minor unit, the smallest
        // hold the ledger takes, is the bar.
        let bar = estimate.unwrap_or(1);
        let limit: Option<i64> = row.try_get("spend_limit_minor")?;
        let on = OffsetDateTime::now_utc().date();
        let mut remaining_budget: Option<i64> = None;
        let mut budget_ok: Option<bool> = None;
        if let Some(limit) = limit {
            let since = row
                .try_get::<Option<String>, _>("budget_duration")?
                .as_deref()
                .map(BudgetDuration::parse)
                .transpose()?
                .map(|d| d.period_start(on));
            let committed = self.key_committed_in(&mut conn, key_id, since).await?;
            remaining_budget = Some(limit - committed);
            budget_ok = Some(committed + bar <= limit);
        }
        // The member and team caps layer onto the key's own, exactly like
        // `hold_for_key` reads them — folded into the same `budget_ok` verdict
        // and the same `remaining_budget_minor` floor, so the answer names the
        // tightest cap in force rather than the key's alone.
        let member: Option<Uuid> = row.try_get("budget_member")?;
        if let Some(member) = member {
            let member_cap: Option<i64> = sqlx::query_scalar(
                "SELECT budget_limit_minor FROM oxsum.memberships \
                 WHERE organization_id = $1 AND user_id = $2",
            )
            .bind(organization_id)
            .bind(member)
            .fetch_one(&mut *conn)
            .await?;
            if let Some(cap) = member_cap {
                let committed_member = self
                    .member_committed_in(&mut conn, organization_id, &member)
                    .await?;
                if committed_member + bar > cap {
                    budget_ok = Some(false);
                }
                // The tighter of the caps is the headroom worth reporting: the
                // member cap bounds the sum across every key they minted, so
                // `cap - committed_member` is already that scope's own
                // remaining figure.
                remaining_budget = Some(
                    remaining_budget
                        .map_or(cap - committed_member, |r| r.min(cap - committed_member)),
                );
            }
            let team: Option<Uuid> = row.try_get("budget_team")?;
            if let Some(team) = team {
                let team_cap: Option<i64> = sqlx::query_scalar(
                    "SELECT budget_limit_minor FROM oxsum.teams \
                     WHERE organization_id = $1 AND team_id = $2",
                )
                .bind(organization_id)
                .bind(team)
                .fetch_one(&mut *conn)
                .await?;
                if let Some(cap) = team_cap {
                    let committed_team = self
                        .team_committed_in(&mut conn, organization_id, &team)
                        .await?;
                    if committed_team + bar > cap {
                        budget_ok = Some(false);
                    }
                    remaining_budget = Some(
                        remaining_budget
                            .map_or(cap - committed_team, |r| r.min(cap - committed_team)),
                    );
                }
            }
        }
        // The outstanding-holds cap reads the same ledger count the hold path
        // enforces — `concurrentHolds` reports the figure either way, so a
        // surface can draw the occupancy without a second query.
        let open = self.key_open_holds_in(&mut conn, key_id).await?;
        // The tag-scoped token buckets, resolved the way admission resolves
        // them: the scope's `scope_name` against the request's tags names the
        // bucket the turn would accrue into — a scope the request does not tag
        // into is skipped, exactly like `hold_for_key`'s callers do.
        let scopes_json: Option<serde_json::Value> = row.try_get("tokens_per_minute_scopes")?;
        let mut tpm_scopes: Vec<(String, i64)> = Vec::new();
        if let Some(scopes) = crate::keys::scopes_from_json(scopes_json) {
            for (tag, scope) in scopes.iter() {
                let tag_scope = crate::TagScope {
                    tag: tag.clone(),
                    scope: scope.clone(),
                };
                if let Some(name) = tag_scope.scope_name(tags) {
                    tpm_scopes.push((name, scope.cap()));
                }
            }
        }
        // The mandate's own bounds, on top of the key's — the same three the
        // hold path enforces inside its lock, judged against the same
        // ledger-derived committed figure.
        let mandate_verdict = match mandate {
            None => None,
            Some(mandate) if mandate.key_id != *key_id => Some(MandateVerdict::Invalid),
            Some(mandate) => {
                let now = OffsetDateTime::now_utc().unix_timestamp();
                if now < mandate.not_before_ts {
                    Some(MandateVerdict::NotYetOrExpired(
                        (mandate.not_before_ts - now) as u64,
                    ))
                } else if now > mandate.not_after_ts {
                    Some(MandateVerdict::NotYetOrExpired(0))
                } else if let (Some(model), Some(models)) = (model, &mandate.models)
                    && !models.iter().any(|allowed| allowed == model)
                {
                    Some(MandateVerdict::ModelRefused)
                } else {
                    let committed_mandate = self
                        .mandate_committed_in(&mut conn, &mandate.mandate_id)
                        .await?;
                    if committed_mandate + bar > mandate.cap_minor {
                        Some(MandateVerdict::CapExceeded(
                            mandate.cap_minor - committed_mandate,
                        ))
                    } else {
                        Some(MandateVerdict::Ok)
                    }
                }
            }
        };
        Ok(Some(EntitlementRead {
            key_active,
            org_suspended,
            model_allowed,
            remaining_budget_minor: remaining_budget,
            budget_ok,
            open_holds: open,
            max_concurrent_holds: row.try_get("max_concurrent_holds")?,
            requests_per_minute: row.try_get("requests_per_minute")?,
            tokens_per_minute: row.try_get("tokens_per_minute")?,
            tpm_scopes,
            mandate: mandate_verdict,
        }))
    }

    /// Whether the daily usage anchor has sealed `day` for this ledger's
    /// organization (issue #340).
    ///
    /// A wallet opened over a bare ledger schema — the generative tests'
    /// model-only world — has no `oxsum` tables at all: the seal cannot exist
    /// there, so the check answers "not sealed" instead of failing the write.
    async fn day_is_anchored(&self, day: Date) -> Result<bool, WalletError> {
        let table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('oxsum.usage_anchors')::text")
                .fetch_one(self.store.pool())
                .await?;
        if table.is_none() {
            return Ok(false);
        }
        let anchored: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM oxsum.usage_anchors a \
             JOIN oxsum.organizations o ON o.organization_id = a.organization_id \
             WHERE o.tenant_id = $1 AND a.day = $2)",
        )
        .bind(&self.tenant_id)
        .bind(day)
        .fetch_one(self.store.pool())
        .await?;
        Ok(anchored)
    }

    /// Settle: one entry does two things — releases the hold (a reversal in the pending layer)
    /// and charges the actual usage (recorded in the settled layer).
    ///
    /// The settlement names the hold it releases: `hold_key` is the idempotency key the hold was
    /// taken under. The held amount is read from the hold entry in the ledger — the entry's
    /// pending-layer debit on the wallet account — so there is nothing for the caller to assert
    /// and nothing for a caller to get wrong. The hold's provenance actor is copied onto the
    /// settlement entry, so both the release and the charge attribute to the key that held.
    ///
    /// `actual` may be less than the held amount; the difference returns to the available
    /// balance. `actual` of zero amounts to a full release.
    ///
    /// One hold settles at most once. The settlement entry's idempotency key is
    /// [`settlement_key_for`] of the hold's key, so the ledger's idempotency gate — inside the
    /// append — refuses a second settlement of the same hold with [`WalletError::Conflict`]:
    /// concurrent settlements naming one hold cannot both release it, and retrying the
    /// identical settlement replays it. The wallet's limit still refuses a release the
    /// outstanding reservations cannot cover, as the backstop behind the pairing.
    ///
    /// Naming a hold that is not outstanding — no entry under the key, or an entry that is not
    /// a hold — is [`WalletError::HoldNotFound`], refused even when other holds would cover the
    /// amount.
    ///
    /// `description` is what the entry says it is, in the caller's words — for a gateway turn, the
    /// record built by [`crate::Settlement::description`], which carries the token counts and the
    /// prices so a bill proves the arithmetic and not only the total. It must be derived from the
    /// request alone: two settlements of one hold with different descriptions are different
    /// requests, and the ledger refuses the second.
    pub async fn settle(
        &self,
        hold_key: &str,
        description: &str,
        actual_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        // A call's own argument is bounded before the ledger is consulted.
        idem(hold_key)?;
        // A day the daily anchor sealed is closed to settlements (issue #340):
        // booking into it would rewrite a day leaf the anchor already fixed.
        if self.day_is_anchored(on).await? {
            return Err(WalletError::DaySealed);
        }
        if actual_minor < 0 {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let (
            promo_held,
            bonus_held,
            wallet_held,
            credit_held,
            outstanding_actor,
            outstanding_correlation,
        ) = self.outstanding_hold(hold_key).await?;
        if actual_minor > promo_held + bonus_held + wallet_held + credit_held {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let held = Credits::from_minor(promo_held + bonus_held + wallet_held + credit_held);
        let settle_key = settlement_key_for(hold_key);
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id_for(&settle_key), idem(&settle_key)?, on)
                .with_description(description_of(description)?);
        // The settlement attributes to the key the hold attributed to: the release side and
        // the settled charge side both count toward that key's committed spend, with no
        // caller input to get wrong. A hold taken before key attribution existed settles
        // unattributed, like it was held. The mandate id the hold carried as its provenance
        // correlation is copied the same way, so the settled charge lands inside the
        // mandate's committed sum once the hold's reservation releases (issue #262).
        if outstanding_actor.is_some() || outstanding_correlation.is_some() {
            let mut provenance = Provenance::none();
            if let Some(actor) = outstanding_actor {
                provenance = provenance.with_actor(&actor).map_err(invalid)?;
            }
            if let Some(correlation) = outstanding_correlation {
                provenance = provenance.with_correlation(&correlation).map_err(invalid)?;
            }
            draft = draft.with_provenance(provenance);
        }
        // The release returns exactly what the hold reserved in each pool.
        for (account, held_part) in [
            (self.promo, promo_held),
            (self.bonus, bonus_held),
            (self.wallet, wallet_held),
            (self.credit_line, credit_held),
        ] {
            if held_part > 0 {
                draft = draft.post(
                    Posting::credit(account, Credits::from_minor(held_part), currency())
                        .in_layer(Layer::Pending),
                );
            }
        }
        draft = draft.post(Posting::debit(self.revenue, held, currency()).in_layer(Layer::Pending));
        if actual_minor > 0 {
            // The charge draws the promo pool first — it is the money that can
            // expire — then the bonus pool, the purchased wallet, then the
            // credit line; never more than this hold reserved in each, because
            // taking another hold's reservation is exactly what the per-pool
            // split exists to prevent.
            let promo_take = actual_minor.min(promo_held);
            let bonus_take = (actual_minor - promo_take).min(bonus_held);
            let wallet_take = (actual_minor - promo_take - bonus_take).min(wallet_held);
            let credit_take = actual_minor - promo_take - bonus_take - wallet_take;
            for (account, take) in [
                (self.promo, promo_take),
                (self.bonus, bonus_take),
                (self.wallet, wallet_take),
                (self.credit_line, credit_take),
            ] {
                if take > 0 {
                    draft = draft.debit(account, Credits::from_minor(take), currency());
                }
            }
            draft = draft.credit(self.revenue, Credits::from_minor(actual_minor), currency());
        }
        // The pinned promo draws consume the same transaction the settlement
        // entry lands in: the uncharged share of a hold's draw returns to the
        // lot it was pinned on — or burns to `Equity:PromoExpiry` when the lot
        // already expired or was cancelled — never to refundable money. The
        // row updates run before the entry; the burn posts after it, because
        // the release's pending credits are what let the burn's settled debit
        // keep the pool's `FundedReservations` invariant.
        let mut tx = self.store.pool().begin().await?;
        // `consume_promo_draws_in` locks promo_draws and promo_lots rows; the
        // append lock goes first so the settlement cannot invert against a
        // hold pinning those rows under the same lock (issue #723). A hold
        // with no promo share has no promo rows to lock.
        if promo_held > 0 {
            self.lock_append_in(&mut tx).await?;
        }
        let burn = self
            .consume_promo_draws_in(&mut tx, hold_key, promo_held, actual_minor)
            .await?;
        match self.append_in(&mut tx, draft).await {
            Ok(receipt) => {
                if burn > 0 {
                    // Released into a closed lot: the burn posts after the
                    // release's pending credits, so the promo pool's settled
                    // balance still covers the debit it takes.
                    if let Err(error) = self
                        .burn_promo_in(&mut tx, &format!("{hold_key}:promo-burn"), burn, on)
                        .await
                    {
                        tx.rollback().await?;
                        return Err(error);
                    }
                }
                tx.commit().await?;
                Ok(receipt)
            }
            // The settlement's idempotency key is derived from the hold's key, so a key
            // conflict here means exactly one thing: the hold was already settled.
            Err(WalletError::Conflict(_)) => {
                tx.rollback().await?;
                Err(WalletError::Conflict("hold already settled".into()))
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// [`settle`](Self::settle) inside the caller's transaction: the same
    /// release-and-charge draft, appended through the engine's borrowed
    /// transaction so the settlement entry, the operation's terminal row, the
    /// subscription draw's consumption and the usage row commit as one unit
    /// (issue #602, D05). The caller owns the commit — a failure leaves nothing
    /// half-written: no release, no charge, no operation close.
    ///
    /// The outstanding-hold read still runs on the pool first: the hold entry it
    /// reads is immutable once committed, and reading it inside the caller's
    /// transaction would only lengthen the lock hold. The settle draft the read
    /// feeds cannot drift either — the settlement's idempotency key is derived
    /// from the hold's key, so a raced second settle conflicts at the append
    /// inside the transaction rather than writing a different release.
    ///
    /// # Errors
    ///
    /// As [`settle`](Self::settle); the transaction is the caller's to commit
    /// or roll back.
    pub async fn settle_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        hold_key: &str,
        description: &str,
        actual_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        idem(hold_key)?;
        if self.day_is_anchored(on).await? {
            return Err(WalletError::DaySealed);
        }
        if actual_minor < 0 {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let (
            promo_held,
            bonus_held,
            wallet_held,
            credit_held,
            outstanding_actor,
            outstanding_correlation,
        ) = self.outstanding_hold(hold_key).await?;
        if actual_minor > promo_held + bonus_held + wallet_held + credit_held {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let held = Credits::from_minor(promo_held + bonus_held + wallet_held + credit_held);
        let settle_key = settlement_key_for(hold_key);
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id_for(&settle_key), idem(&settle_key)?, on)
                .with_description(description_of(description)?);
        if outstanding_actor.is_some() || outstanding_correlation.is_some() {
            let mut provenance = Provenance::none();
            if let Some(actor) = outstanding_actor {
                provenance = provenance.with_actor(&actor).map_err(invalid)?;
            }
            if let Some(correlation) = outstanding_correlation {
                provenance = provenance.with_correlation(&correlation).map_err(invalid)?;
            }
            draft = draft.with_provenance(provenance);
        }
        for (account, held_part) in [
            (self.promo, promo_held),
            (self.bonus, bonus_held),
            (self.wallet, wallet_held),
            (self.credit_line, credit_held),
        ] {
            if held_part > 0 {
                draft = draft.post(
                    Posting::credit(account, Credits::from_minor(held_part), currency())
                        .in_layer(Layer::Pending),
                );
            }
        }
        draft = draft.post(Posting::debit(self.revenue, held, currency()).in_layer(Layer::Pending));
        if actual_minor > 0 {
            let promo_take = actual_minor.min(promo_held);
            let bonus_take = (actual_minor - promo_take).min(bonus_held);
            let wallet_take = (actual_minor - promo_take - bonus_take).min(wallet_held);
            let credit_take = actual_minor - promo_take - bonus_take - wallet_take;
            for (account, take) in [
                (self.promo, promo_take),
                (self.bonus, bonus_take),
                (self.wallet, wallet_take),
                (self.credit_line, credit_take),
            ] {
                if take > 0 {
                    draft = draft.debit(account, Credits::from_minor(take), currency());
                }
            }
            draft = draft.credit(self.revenue, Credits::from_minor(actual_minor), currency());
        }
        // The pinned promo draws consume inside the caller's transaction: the
        // charged part debits the lots the hold named, the released part
        // returns to them — or burns to `Equity:PromoExpiry` for a closed lot.
        // The burn posts after the release's pending credits, like `settle`'s.
        // The append lock precedes the draw and lot row locks (issue #723);
        // it is re-entrant, so the append below takes it again.
        if promo_held > 0 {
            self.lock_append_in(tx).await?;
        }
        let burn = self
            .consume_promo_draws_in(tx, hold_key, promo_held, actual_minor)
            .await?;
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => {
                Err(WalletError::Conflict("hold already settled".into()))
            }
            other => match other {
                Ok(receipt) => {
                    if burn > 0 {
                        self.burn_promo_in(tx, &format!("{hold_key}:promo-burn"), burn, on)
                            .await?;
                    }
                    Ok(receipt)
                }
                Err(error) => Err(error),
            },
        }
    }

    /// The typed zero-cost usage receipt (issue #602, D05): the settlement
    /// record a turn with no wallet hold still needs — a fully entitlement- or
    /// grant-covered call that served with zero wallet funds.
    ///
    /// Shaped like the statement- and usage-day anchors: a 1-minor balanced
    /// `Equity:Adjustments` debit/credit in the *settled* layer, so the marker
    /// is metadata, not money — no pool moves, no pending reservation, no
    /// revenue. The idempotency key is `settlement_key_for(hold_key)`, the same
    /// figure a wallet-hold settlement derives, so a replayed receipt and a
    /// raced settlement of the same turn cannot both land. The description is
    /// the ordinary [`crate::Settlement::description`] record — `charged: 0`,
    /// the subscription block naming what the entitlement paid — so a bill
    /// proves the turn ran, what it used, and that the wallet was never
    /// asked.
    ///
    /// `actor` and `correlation` are the turn's attribution — the key id and
    /// the mandate id it ran under — carried onto the receipt the way a hold's
    /// settlement carries them, so the key's request history and the mandate's
    /// record stay complete even though no money moved.
    ///
    /// # Errors
    ///
    /// [`WalletError::Conflict`] when the key is already taken — the turn
    /// settled, or a different entry claimed it. Storage failures surface as
    /// [`WalletError`].
    pub async fn usage_receipt(
        &self,
        hold_key: &str,
        description: &str,
        actor: Option<&str>,
        correlation: Option<&str>,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if self.day_is_anchored(on).await? {
            return Err(WalletError::DaySealed);
        }
        let draft = self.usage_receipt_draft(hold_key, description, actor, correlation, on)?;
        match self.append(draft).await {
            Err(WalletError::Conflict(_)) => {
                Err(WalletError::Conflict("hold already settled".into()))
            }
            other => other,
        }
    }

    /// [`usage_receipt`](Self::usage_receipt) inside the caller's transaction —
    /// the borrowed-append half of the zero-cost settle (issue #602). The
    /// receipt commits with whatever else the caller's transaction carries:
    /// the operation's terminal row, the entitlement's consumption, the usage
    /// row. `tx` is the caller's to commit or roll back.
    ///
    /// # Errors
    ///
    /// As [`usage_receipt`](Self::usage_receipt).
    pub async fn usage_receipt_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        hold_key: &str,
        description: &str,
        actor: Option<&str>,
        correlation: Option<&str>,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        if self.day_is_anchored(on).await? {
            return Err(WalletError::DaySealed);
        }
        let draft = self.usage_receipt_draft(hold_key, description, actor, correlation, on)?;
        match self.append_in(tx, draft).await {
            Err(WalletError::Conflict(_)) => {
                Err(WalletError::Conflict("hold already settled".into()))
            }
            other => other,
        }
    }

    /// The receipt's draft: the 1-minor balanced adjustments marker carrying
    /// the settlement description and the turn's provenance. Shared by
    /// [`usage_receipt`](Self::usage_receipt) and
    /// [`usage_receipt_in`](Self::usage_receipt_in).
    fn usage_receipt_draft(
        &self,
        hold_key: &str,
        description: &str,
        actor: Option<&str>,
        correlation: Option<&str>,
        on: Date,
    ) -> Result<Entry<Draft, SCALE>, WalletError> {
        idem(hold_key)?;
        let settle_key = settlement_key_for(hold_key);
        // The marker amount is immaterial — 1 minor like the other anchors —
        // and balanced on one account: the entry is the receipt, not a charge.
        let minor = Credits::from_minor(1);
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id_for(&settle_key), idem(&settle_key)?, on)
                .with_description(description_of(description)?)
                .debit(self.adjustments, minor, currency())
                .credit(self.adjustments, minor, currency());
        if actor.is_some() || correlation.is_some() {
            let mut provenance = Provenance::none();
            if let Some(actor) = actor {
                provenance = provenance.with_actor(actor).map_err(invalid)?;
            }
            if let Some(correlation) = correlation {
                provenance = provenance.with_correlation(correlation).map_err(invalid)?;
            }
            draft = draft.with_provenance(provenance);
        }
        Ok(draft)
    }

    /// The settlement record a hold's settlement committed, when it did: how a
    /// replayed settlement answers the original outcome — the settlement entry
    /// id derives from the hold's key, so the read is one probe (issue #172).
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settled_record(
        &self,
        hold_key: &str,
    ) -> Result<Option<crate::SettlementRecord>, WalletError> {
        let Some(stored) = self
            .store
            .get(entry_id_for(&settlement_key_for(hold_key)))
            .await?
        else {
            return Ok(None);
        };
        Ok(crate::SettlementRecord::parse(
            stored.entry.description().as_str(),
        ))
    }

    /// Available balance in minor units = settled balance - unsettled holds.
    pub async fn available(&self) -> Result<i64, WalletError> {
        Ok(self.settled().await? - self.reserved().await?)
    }

    /// Credits that have settled across the pools: what top-ups and grants put there,
    /// less what settlements and deductions have charged.
    ///
    /// Separate from [`reserved`](Self::reserved) because a hold and a settlement are answered
    /// from different layers: a hold draws on the pools together, a settlement gives back part of
    /// the reserved one.
    pub async fn settled(&self) -> Result<i64, WalletError> {
        self.pool_net(Layer::Settled).await
    }

    /// Credits currently reserved by holds that have not been settled yet, across the pools.
    ///
    /// Never negative: each pool's limit keeps its pending layer on the hold side, which is what
    /// makes this the ceiling on what a settlement may release.
    pub async fn reserved(&self) -> Result<i64, WalletError> {
        Ok(-self.pool_net(Layer::Pending).await?)
    }

    /// [`available`](Self::available) on the caller's connection (issue #602):
    /// the atomic settle's `balance.low` read, taken on the settle's own
    /// transaction so the figure already counts the uncommitted settlement —
    /// a pool read would still see the pre-settle balance and judge the
    /// threshold against money the turn had already spent.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn available_in(&self, conn: &mut sqlx::PgConnection) -> Result<i64, WalletError> {
        let mut total = 0i64;
        for account in [self.promo, self.bonus, self.wallet, self.credit_line] {
            total += self.pool_available_in(conn, account).await?;
        }
        Ok(total)
    }

    /// The credit limit the operator granted, in minor units: the credit-facility
    /// account's settled debit balance, gross of draws. 0 for a wallet that has
    /// never been given a line.
    pub async fn credit_limit(&self) -> Result<i64, WalletError> {
        Ok(-self.account_net(self.facility, Layer::Settled).await?)
    }

    /// The part of the credit line in use, in minor units: the committed limit
    /// minus what the line still carries, so it counts settled draws and the
    /// credit reservations in flight alike. 0 for a wallet with no line at all.
    pub async fn credit_used(&self) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        let committed = self.committed_limit_in(&mut conn).await?;
        let headroom = self.pool_available_in(&mut conn, self.credit_line).await?;
        Ok((committed - headroom).max(0))
    }

    /// What the organization owes on its line right now, in minor units: limit
    /// minus the line's settled balance — settled draws only, not the holds
    /// still reserving it. 0 when nothing is drawn.
    async fn credit_drawn(&self) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.credit_drawn_in(&mut conn).await
    }

    /// The [`credit_drawn`](Self::credit_drawn) read on the caller's connection:
    /// `top_up` runs it inside the transaction that holds the credit lock.
    async fn credit_drawn_in(&self, conn: &mut sqlx::PgConnection) -> Result<i64, WalletError> {
        let committed = self.committed_limit_in(conn).await?;
        let line = self.settled_net_in(conn, self.credit_line).await?;
        Ok((committed - line).max(0))
    }

    /// The facility committed, on the caller's connection: the account is
    /// debit-normal, so its settled net — credits minus debits — reads negated.
    async fn committed_limit_in(&self, conn: &mut sqlx::PgConnection) -> Result<i64, WalletError> {
        Ok(-self.settled_net_in(conn, self.facility).await?)
    }

    /// What the credit line carried for settlements whose booking dates fall in
    /// `[from, to]` inclusive — a statement period's credit-drawn figure: the part
    /// of the month's charges the organization borrowed rather than paid from its
    /// own pools (issue #124).
    ///
    /// Settled debits on the credit line, entries carrying a facility leg excluded:
    /// a limit *shrink* debits the line too, and granting the organization less
    /// credit is not it spending.
    pub async fn credit_drawn_between(&self, from: Date, to: Date) -> Result<i64, WalletError> {
        self.credit_flow("D", Some(from), Some(to)).await
    }

    /// The same read cumulative through `to`: where the FIFO match of repayments
    /// against draws positions a statement's debt. Repayments cover the line's
    /// draws oldest first, so a statement is paid in the measure that all-time
    /// repaid exceeds the draws booked before its period (issue #124).
    pub async fn credit_drawn_through(&self, to: Date) -> Result<i64, WalletError> {
        self.credit_flow("D", None, Some(to)).await
    }

    /// What top-ups and payments have repaid to the credit line over all time —
    /// settled credits on it that carry no facility leg, which excludes the limit
    /// *grants* that credit the line without a payment behind them (issue #124).
    pub async fn credit_repaid(&self) -> Result<i64, WalletError> {
        self.credit_flow("C", None, None).await
    }

    /// Settled postings on the credit line of one `direction` (`D` or `C`),
    /// bounded by booking date, facility-leg entries excluded — those are the
    /// credit-limit changes, and a limit moving is neither a draw nor a repayment.
    async fn credit_flow(
        &self,
        direction: &str,
        from: Option<Date>,
        to: Option<Date>,
    ) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        let sql = format!(
            "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint AS flow \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = $1 AND p.direction = $2 AND p.layer = 'settled' \
               AND e.log_index IS NOT NULL \
               AND ($3::date IS NULL OR e.booking_date >= $3) \
               AND ($4::date IS NULL OR e.booking_date <= $4) \
               AND NOT EXISTS (SELECT 1 FROM ledger_{schema}.postings f \
                               WHERE f.entry_id = p.entry_id AND f.account_index = $5)",
            schema = self.tenant_id,
        );
        let flow: i64 = sqlx::query_scalar(&sql)
            .bind(self.credit_line.index() as i32)
            .bind(direction)
            .bind(from)
            .bind(to)
            .bind(self.facility.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
        Ok(flow)
    }

    /// The log-index window covering the named entries: `(min, max)` of the
    /// log indices the ids sit at, `None`s when none of them is in the log.
    /// A finalized statement stores the window over its settlement entries so
    /// the lines' proof window is a range the caller can walk (issue #124).
    pub async fn log_window_of(
        &self,
        entry_ids: &[Uuid],
    ) -> Result<(Option<i64>, Option<i64>), WalletError> {
        if entry_ids.is_empty() {
            return Ok((None, None));
        }
        use sqlx::Row;
        let mut conn = self.store.pool().acquire().await?;
        let sql = format!(
            "SELECT min(log_index)::bigint, max(log_index)::bigint \
             FROM ledger_{schema}.entries \
             WHERE entry_id = ANY($1) AND log_index IS NOT NULL",
            schema = self.tenant_id,
        );
        let row = sqlx::query(&sql)
            .bind(entry_ids)
            .fetch_one(&mut *conn)
            .await?;
        Ok((row.try_get(0)?, row.try_get(1)?))
    }

    /// Grants or resizes the organization's credit line, booking the delta as a
    /// ledger entry: `debit CreditFacility, credit CreditLine` to grow the line,
    /// the reverse to shrink it.
    ///
    /// `limit_minor` is the whole limit, not a delta — the entry's amount is the
    /// difference from what the facility already commits, and an unchanged limit
    /// writes nothing and answers `None`. The computation runs under the credit
    /// lock so two calls cannot both read the same committed figure and double
    /// the line.
    ///
    /// Shrinking is bounded by the line's own `FundedReservations` limit: the
    /// shrink debits `CreditLine`, which refuses a debit beyond what it carries
    /// — the undrawn part — so a limit below the outstanding draw is
    /// [`WalletError::InvalidInput`], not a partial write. A draw racing the
    /// shrink can only make the refusal stricter, never permit a cut that should
    /// have failed.
    ///
    /// The idempotency key derives the entry's id like every other write: a
    /// retried call under one key replays, a second call under a fresh key is a
    /// second change.
    pub async fn set_credit_limit(
        &self,
        key: &str,
        limit_minor: i64,
        on: Date,
    ) -> Result<Option<Receipt>, WalletError> {
        if limit_minor < 0 {
            return Err(WalletError::InvalidInput(
                "a credit limit cannot be negative".into(),
            ));
        }
        idem(key)?;
        // The target rides in the description: the entry then says what it set,
        // not just what it moved, and the replay check can tell the same request
        // — same key, same limit — from a different one under a reused key.
        let description = description_of(&format!("credit limit set to {limit_minor} minor"))?;
        let entry_id = entry_id_for(key);
        if let Some(stored) = self.store.get(entry_id).await? {
            if stored.entry.description().as_str() == description.as_str() {
                return Ok(Some(stored_receipt(&stored)));
            }
            return Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )));
        }
        let mut tx = self.store.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(credit_lock_key(&self.tenant_id))
            .execute(&mut *tx)
            .await?;
        // A same-key call may have committed while this one waited on the
        // lock: answer it from the stored entry exactly as the pre-lock check
        // does — a matching change replays its receipt, anything else is the
        // key-reuse conflict — rather than reporting a no-op for a change
        // that did write.
        if let Some(stored) = self.store.get(entry_id).await? {
            tx.rollback().await?;
            if stored.entry.description().as_str() == description.as_str() {
                return Ok(Some(stored_receipt(&stored)));
            }
            return Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )));
        }
        let committed = self.committed_limit_in(&mut tx).await?;
        let delta = limit_minor - committed;
        if delta == 0 {
            tx.rollback().await?;
            return Ok(None);
        }
        let amt = Credits::from_minor(delta.abs());
        let draft = if delta > 0 {
            Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on)
                .with_description(description)
                .debit(self.facility, amt, currency())
                .credit(self.credit_line, amt, currency())
        } else {
            Entry::<Draft, SCALE>::new(entry_id, idem(key)?, on)
                .with_description(description)
                .debit(self.credit_line, amt, currency())
                .credit(self.facility, amt, currency())
        };
        let receipt = self.append_sealed(self.seal(draft).await?).await;
        match receipt {
            // A shrink that would cut into the outstanding draw surfaces as the
            // credit line's `InsufficientFunds`; name what it actually is.
            Err(WalletError::InsufficientFunds) if delta < 0 => {
                tx.rollback().await?;
                Err(WalletError::InvalidInput(
                    "the credit limit cannot go below what is still drawn".into(),
                ))
            }
            Ok(receipt) => {
                // The re-set line re-arms the organization's threshold latches
                // (issue #232), like the key PATCH clears its forecast date.
                sqlx::query(
                    "DELETE FROM oxsum.budget_threshold_fires \
                     WHERE scope = 'organization' AND scope_id = $1",
                )
                .bind(Uuid::parse_str(&self.tenant_id).unwrap_or_default())
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(Some(receipt))
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// Credits the pools have been charged on or after `from`, in minor units: the
    /// settled-layer debits on the pool accounts, which is what a settlement or a
    /// deduction books when it draws the balance down.
    ///
    /// The gross debits, not the layer's net: a top-up credits the same layer, and
    /// subtracting the credits would read a month of top-ups as negative spend. Holds
    /// live in the pending layer, so an outstanding hold is not spend either, the
    /// pool reclassifications are excluded by their idempotency keys — moving money
    /// between pools is not spend — and a credit-limit change is excluded by the
    /// facility leg it always carries: shrinking the line debits the credit pool,
    /// but giving the organization less credit is not charging it. A window in
    /// which nothing settled is an empty sum: zero, not an error.
    pub async fn settled_spend_since(&self, from: Date) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        let sql = format!(
            "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint AS spend \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND p.direction = 'D' AND p.layer = 'settled' \
               AND e.log_index IS NOT NULL AND e.booking_date >= $2 \
               AND e.idempotency_key <> ALL($3) \
               AND NOT EXISTS (SELECT 1 FROM ledger_{schema}.postings f \
                               WHERE f.entry_id = p.entry_id AND f.account_index = $4)",
            schema = self.tenant_id,
        );
        let spend: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.promo.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(from)
            .bind(vec![
                RECLASS_KEY.as_bytes().to_vec(),
                LEGACY_PROMO_KEY.as_bytes().to_vec(),
            ])
            .bind(self.facility.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
        Ok(spend)
    }

    /// What the current UTC month has charged the wallet, in minor units — the dashboard's
    /// "spent this month".
    ///
    /// The month is the ledger's own. Entries carry whole days and every write is dated with
    /// the server's current UTC date (`today` in `crates/server/src/lib.rs`), so the first
    /// instant of the month is its first day as a booking date, and the window is inclusive
    /// of that day.
    pub async fn month_spend(&self) -> Result<i64, WalletError> {
        // The first of any month exists, so this cannot fail; it is mapped rather than
        // unwrapped because a panic on the way out of the domain layer is not an option.
        let first = OffsetDateTime::now_utc()
            .date()
            .replace_day(1)
            .map_err(invalid)?;
        self.settled_spend_since(first).await
    }

    /// Closes a month: defines the `YYYY-MM` period covering `first`'s month if the
    /// ledger does not know it, stops new postings into it and seals it — the seal
    /// the store appends is the month's closing record, chained onto the seals
    /// before it. From then on every write whose booking date falls in the month is
    /// refused at seal time (see `Wallet::seal`), which is product.md's "after
    /// closing, that month accepts no new entries" made structural rather than
    /// conventional.
    ///
    /// `first` must be the month's first day — the period id, `YYYY-MM`, is what
    /// callers and the admin page both say. Only a month that has fully ended can
    /// be closed: one that still contains the server's current UTC date would
    /// refuse the very next request the gateway writes, so it is rejected here.
    ///
    /// Closing an already-sealed month answers its existing record: the call is
    /// idempotent, because the admin page's button and a retried request must
    /// agree with what the seals table already holds.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] for a `first` that is not a month's first day
    /// or a month that has not ended; storage failures surface as
    /// [`WalletError::Storage`].
    pub async fn close_month(&self, first: Date) -> Result<Seal, WalletError> {
        if first.day() != 1 {
            return Err(invalid("the month's first day"));
        }
        let last = first
            .replace_day(first.month().length(first.year()))
            .map_err(invalid)?;
        if last >= OffsetDateTime::now_utc().date() {
            return Err(invalid("only a month that has fully ended can be closed"));
        }
        let period = PeriodId::new(format!(
            "{:04}-{:02}",
            first.year(),
            u8::from(first.month())
        ))
        .map_err(invalid)?;

        match self.store.calendar().await?.get(&period).map(|p| p.state) {
            // Already closed: answer the record rather than a conflict — a repeat
            // click and a retry must say the same thing.
            Some(PeriodState::Sealed) => {
                return self
                    .store
                    .seals()
                    .await?
                    .into_iter()
                    .find(|seal| seal.period == period)
                    .ok_or_else(|| invalid("a sealed period with no seal"));
            }
            Some(PeriodState::Closing) => {}
            Some(PeriodState::Open) => {
                self.store
                    .transition_period(&period, PeriodState::Closing)
                    .await?;
            }
            None => {
                self.store
                    .define_period(&Period::new(period.clone(), first, last).map_err(invalid)?)
                    .await?;
                self.store
                    .transition_period(&period, PeriodState::Closing)
                    .await?;
            }
        }
        self.store.seal_period(&period).await.map_err(Into::into)
    }

    /// The seals this ledger holds, in chain order — the closing records the admin's
    /// closing page lists.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError::Storage`].
    pub async fn seals(&self) -> Result<Vec<Seal>, WalletError> {
        self.store.seals().await.map_err(Into::into)
    }

    /// What the hold taken under `hold_key` reserved in each pool — the promo
    /// part first, then the bonus, the wallet and the credit-line parts — plus
    /// the provenance actor the hold attributed to: the API key whose spend it
    /// counts toward, if the hold named one.
    ///
    /// The hold entry is found by the id it was given when the hold was taken
    /// ([`entry_id_for`] of the key), and the amounts are the entry's pending-layer
    /// debits on the pool accounts — the ledger's own record of what was reserved,
    /// not a caller assertion. [`WalletError::HoldNotFound`] when no entry is stored
    /// under the key, or when the entry is not a hold.
    async fn outstanding_hold(
        &self,
        hold_key: &str,
    ) -> Result<(i64, i64, i64, i64, Option<String>, Option<String>), WalletError> {
        let Some(stored) = self.store.get(entry_id_for(hold_key)).await? else {
            return Err(WalletError::HoldNotFound(format!(
                "no hold under key {hold_key:?}"
            )));
        };
        let mut promo_held = 0;
        let mut bonus_held = 0;
        let mut wallet_held = 0;
        let mut credit_held = 0;
        for p in stored.entry.postings() {
            if p.layer != Layer::Pending {
                continue;
            }
            let signed = match p.direction {
                Direction::Debit => p.amount.to_minor(),
                Direction::Credit => -p.amount.to_minor(),
            };
            if p.account == self.promo {
                promo_held += signed;
            } else if p.account == self.bonus {
                bonus_held += signed;
            } else if p.account == self.wallet {
                wallet_held += signed;
            } else if p.account == self.credit_line {
                credit_held += signed;
            }
        }
        if promo_held + bonus_held + wallet_held + credit_held <= 0 {
            return Err(WalletError::HoldNotFound(format!(
                "key {hold_key:?} does not name a hold"
            )));
        }
        let provenance = stored.entry.provenance();
        let actor = provenance
            .actor
            .as_ref()
            .map(|actor| actor.as_str().to_owned());
        let correlation = provenance
            .correlation
            .as_ref()
            .map(|correlation| correlation.as_str().to_owned());
        Ok((
            promo_held,
            bonus_held,
            wallet_held,
            credit_held,
            actor,
            correlation,
        ))
    }

    /// The one-time pool migration (decision D1): moves the unspent remainder of
    /// historical grants out of `Liabilities:Wallet` into `Equity:Bonus` with a
    /// single `debit Wallet, credit Bonus` entry under `RECLASS_KEY`.
    ///
    /// `grants` counts the settled credits on Wallet that pair with an Adjustments
    /// debit — top-ups pair with Cash instead, so money the user paid in is never
    /// misread as granted. The amount moved is
    /// `clamp(grants - settled wallet outflow, 0, wallet balance)`: the
    /// draw-bonus-first convention applied retroactively, so granted money counts
    /// as spent before purchased money and what survives of it is grants minus
    /// everything the wallet ever paid out, bounded by what it still holds.
    ///
    /// Idempotent and self-marking: the entry's fixed key makes a second run a
    /// no-op, which is what makes a failed first pass safe to retry on the next
    /// `open`. Runs from `open` — the migration is lazy, each ledger reclassifying
    /// itself the first time it is opened under the two-pool model. Returns
    /// whether an entry was written. The move is a
    /// rebalancing between pools — the settled_spend_since read excludes the key,
    /// and the bills page never lists it (it has no pending, cash or adjustments
    /// posting, so `classify` has no kind for it).
    pub async fn reclassify_grants(&self) -> Result<bool, WalletError> {
        let schema = &self.tenant_id;
        let amount = {
            let mut conn = self.store.pool().acquire().await?;
            let done: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM ledger_{schema}.entries WHERE idempotency_key = $1)"
            ))
            .bind(RECLASS_KEY.as_bytes())
            .fetch_one(&mut *conn)
            .await?;
            if done {
                return Ok(false);
            }
            let grants: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint \
                 FROM ledger_{schema}.postings p \
                 JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = $1 AND p.direction = 'C' AND p.layer = 'settled' \
                   AND e.log_index IS NOT NULL \
                   AND EXISTS (SELECT 1 FROM ledger_{schema}.postings q \
                       WHERE q.entry_id = p.entry_id AND q.account_index = $2 \
                         AND q.direction = 'D' AND q.layer = 'settled')"
            ))
            .bind(self.wallet.index() as i32)
            .bind(self.adjustments.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
            let outflow: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint \
                 FROM ledger_{schema}.postings p \
                 JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = $1 AND p.direction = 'D' AND p.layer = 'settled' \
                   AND e.log_index IS NOT NULL AND e.idempotency_key <> $2"
            ))
            .bind(self.wallet.index() as i32)
            .bind(RECLASS_KEY.as_bytes())
            .fetch_one(&mut *conn)
            .await?;
            let balance: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(SUM(CASE WHEN p.direction = 'C' THEN p.amount_minor \
                                          ELSE -p.amount_minor END), 0)::bigint \
                 FROM ledger_{schema}.postings p \
                 JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
                 WHERE p.account_index = $1 AND p.layer = 'settled' AND e.log_index IS NOT NULL"
            ))
            .bind(self.wallet.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
            (grants - outflow).clamp(0, balance)
        };
        if amount == 0 {
            return Ok(false);
        }
        let amt = Credits::from_minor(amount);
        let appended = self
            .append(
                Entry::<Draft, SCALE>::new(
                    entry_id_for(RECLASS_KEY),
                    idem(RECLASS_KEY)?,
                    OffsetDateTime::now_utc().date(),
                )
                .with_description(description_of("bonus pool reclassification")?)
                .debit(self.wallet, amt, currency())
                .credit(self.bonus, amt, currency()),
            )
            .await;
        match appended {
            Ok(_) => Ok(true),
            // Two opens of one tenant can both pass the marker check and write at
            // once; the loser of the idempotent append sees the marker on re-read
            // and has nothing to do.
            Err(e) => {
                let mut conn = self.store.pool().acquire().await?;
                let done: bool = sqlx::query_scalar(&format!(
                    "SELECT EXISTS (SELECT 1 FROM ledger_{schema}.entries WHERE idempotency_key = $1)"
                ))
                .bind(RECLASS_KEY.as_bytes())
                .fetch_one(&mut *conn)
                .await?;
                if done { Ok(false) } else { Err(e) }
            }
        }
    }

    /// The one-time promo migration (issue #609): moves the aggregate bonus
    /// pool's *unreserved* balance into a `legacy_migration` promo lot, so the
    /// historical grants the two-pool reclassification parked in `Equity:Bonus`
    /// become a lot like every other — the ledger balance moves `debit Bonus /
    /// credit Promo`, a net-zero rebalancing between two spend-first pools.
    ///
    /// Only the *available* part of the bonus pool migrates: a hold taken
    /// before this build keeps its pending debit on Bonus, and its settlement
    /// still releases and charges through that pool — so the migration moves
    /// `settled + pending-net`, leaving the exact amount the in-flight
    /// reservations may still draw. The lot the remainder lands in never
    /// expires (`expires_at NULL`), and its `source_ref` is the tenant id, so
    /// the unique lot anchor and the fixed entry key both make a rerun a
    /// no-op. Returns whether a lot was written.
    async fn migrate_legacy_bonus(&self) -> Result<bool, WalletError> {
        // The marker is the migration lot's own row plus the ledger entry's
        // fixed key: either alone proves the move ran, but checking the ledger
        // key first keeps the path identical to `reclassify_grants`.
        let schema = &self.tenant_id;
        let amount = {
            let mut conn = self.store.pool().acquire().await?;
            let done: bool = sqlx::query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM ledger_{schema}.entries WHERE idempotency_key = $1)"
            ))
            .bind(LEGACY_PROMO_KEY.as_bytes())
            .fetch_one(&mut *conn)
            .await?;
            if done {
                return Ok(false);
            }
            // What the bonus pool can still fund: its settled balance minus the
            // reservations pending holds already carry. Migrating that figure
            // leaves every in-flight bonus hold the settled balance it may
            // settle against.
            self.pool_available_in(&mut conn, self.bonus).await?.max(0)
        };
        if amount == 0 {
            // Nothing to migrate — but the marker still lands, so a wallet that
            // earned bonus credit later (impossible under this build: `adjust`
            // credits Bonus only) or that reopens cannot re-run the move.
            return Ok(false);
        }
        let on = OffsetDateTime::now_utc().date();
        let mut tx = self.store.pool().begin().await?;
        // The lot row and the ledger move are one transaction: the grant
        // cannot exist without the projection that names it. The append lock
        // precedes the lot insert like every other promo writer (issue #723).
        self.lock_append_in(&mut tx).await?;
        let lot_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.promo_lots\
             (lot_id, organization_id, source, source_ref, amount_minor, remaining_minor, expires_at) \
             VALUES ($1, $2, 'legacy_migration', $3, $4, $4, NULL) \
             ON CONFLICT (organization_id, source, source_ref) DO NOTHING",
        )
        .bind(lot_id)
        .bind(self.organization_id_hint().await?)
        .bind(&self.tenant_id)
        .bind(amount)
        .execute(&mut *tx)
        .await?;
        let amt = Credits::from_minor(amount);
        let appended = self
            .append_in(
                &mut tx,
                Entry::<Draft, SCALE>::new(
                    entry_id_for(LEGACY_PROMO_KEY),
                    idem(LEGACY_PROMO_KEY)?,
                    on,
                )
                .with_description(description_of("legacy bonus migrates to a promo lot")?)
                .debit(self.bonus, amt, currency())
                .credit(self.promo, amt, currency()),
            )
            .await;
        match appended {
            Ok(_) => {
                tx.commit().await?;
                Ok(true)
            }
            Err(e) => {
                tx.rollback().await?;
                // A racing open may have committed while this one read; the
                // fixed key is the arbiter, like `reclassify_grants`.
                let mut conn = self.store.pool().acquire().await?;
                let done: bool = sqlx::query_scalar(&format!(
                    "SELECT EXISTS (SELECT 1 FROM ledger_{schema}.entries WHERE idempotency_key = $1)"
                ))
                .bind(LEGACY_PROMO_KEY.as_bytes())
                .fetch_one(&mut *conn)
                .await?;
                if done { Ok(false) } else { Err(e) }
            }
        }
    }

    /// The organization id this wallet's tenant belongs to, looked up for the
    /// migration lot's `organization_id` — `promo_lots` keys on the
    /// `oxsum.organizations` identity, not the tenant string. `None` for a bare
    /// ledger schema with no oxsum row (the generative model world), where the
    /// migration has nothing to write.
    async fn organization_id_hint(&self) -> Result<Option<Uuid>, WalletError> {
        let table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('oxsum.organizations')::text")
                .fetch_one(self.store.pool())
                .await?;
        if table.is_none() {
            return Ok(None);
        }
        let organization = sqlx::query_scalar(
            "SELECT organization_id FROM oxsum.organizations WHERE tenant_id = $1",
        )
        .bind(&self.tenant_id)
        .fetch_optional(self.store.pool())
        .await?;
        Ok(organization)
    }

    /// The promo draw-pinning [`hold`](Self::hold) and
    /// [`hold_for_key`](Self::hold_for_key) run inside the hold's transaction:
    /// walks this organization's open lots in deterministic draw order —
    /// soonest expiry first, never-expiring last — decrements each lot's
    /// `remaining_minor` by what the hold's promo share takes from it, and
    /// records each pick in `promo_draws` so the settlement debits the rows
    /// admission named (issue #609).
    ///
    /// The caller already computed `promo_minor` from the ledger's own pool
    /// availability inside this same transaction, so the lots' rows and the
    /// pool agree by construction: `FOR UPDATE` serializes the decrement
    /// against a racing hold's allocation and against the expiry pass, and the
    /// lot's `CHECK (remaining_minor >= 0)` is the last-line guard a stale read
    /// could never over-draw.
    async fn pin_promo_draws_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        hold_key: &str,
        promo_minor: i64,
    ) -> Result<(), WalletError> {
        let Some(organization) = self.organization_id_hint().await? else {
            // A bare ledger schema has no oxsum rows at all: the promo pool
            // stays empty there and `pool_split` would never take from it, so
            // reaching here means the model world asked for promo money it
            // cannot name — an internal contradiction, not user error.
            return Err(WalletError::InvalidInput(
                "promo draws need an organization".into(),
            ));
        };
        let mut left = promo_minor;
        // Lock in the allocation's own order so two racing holds cannot
        // deadlock: both walk the same list in the same order. An open lot is
        // pinnable even past its `expires_at`: the ledger pool counts its
        // remainder until the expiry pass burns it, so the pin and the split
        // must agree — and pinning a due lot first is exactly the
        // most-restricted-money-first order the pools draw in. The pass closes
        // the row under the same `FOR UPDATE`; a settlement that releases into
        // the closed lot burns rather than refunds.
        let lots = sqlx::query(
            "SELECT lot_id, remaining_minor FROM oxsum.promo_lots \
             WHERE organization_id = $1 AND closed_at IS NULL AND remaining_minor > 0 \
             ORDER BY expires_at ASC NULLS LAST, lot_id FOR UPDATE",
        )
        .bind(organization)
        .fetch_all(&mut **tx)
        .await?;
        for lot in &lots {
            use sqlx::Row;
            if left <= 0 {
                break;
            }
            let lot_id: Uuid = lot.try_get("lot_id")?;
            let remaining: i64 = lot.try_get("remaining_minor")?;
            let take = left.min(remaining);
            sqlx::query(
                "UPDATE oxsum.promo_lots SET remaining_minor = remaining_minor - $2 \
                 WHERE lot_id = $1",
            )
            .bind(lot_id)
            .bind(take)
            .execute(&mut **tx)
            .await?;
            sqlx::query(
                "INSERT INTO oxsum.promo_draws (hold_key, tenant_id, lot_id, amount_minor) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(hold_key)
            .bind(&self.tenant_id)
            .bind(lot_id)
            .bind(take)
            .execute(&mut **tx)
            .await?;
            left -= take;
        }
        if left > 0 {
            // The ledger's pool availability and the lots' rows drifted apart —
            // the invariant tests check — so the hold cannot name the lots its
            // promo share funds. Refusing is honest; drawing unattributed
            // promo would let settlement pick a different lot than admission.
            return Err(WalletError::InsufficientFunds);
        }
        Ok(())
    }

    /// The draw-consumption [`settle`](Self::settle) and
    /// [`settle_in`](Self::settle_in) run inside the settlement's transaction
    /// *before* the entry appends: walks the hold's pinned `promo_draws` in
    /// the order admission took them, marks each row's
    /// `charged_minor`/`released_minor` (summing to its `amount_minor`
    /// exactly), and returns each released share to its lot while the lot is
    /// still open — or totals it for the caller to burn after the entry lands,
    /// because only the release's pending credits let the burn's settled debit
    /// keep `FundedReservations`. Returns the amount to burn via
    /// [`burn_promo_in`](Self::burn_promo_in); 0 when every released share
    /// went home.
    ///
    /// `promo_charged` is `min(actual, promo_held)`: the promo part of the
    /// charge the entry writes. A drawn-from closed lot burns rather than
    /// refunds — expiry and correction never turn promo into cash (issue
    /// #609).
    async fn consume_promo_draws_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        hold_key: &str,
        promo_held: i64,
        actual_minor: i64,
    ) -> Result<i64, WalletError> {
        use sqlx::Row;
        if promo_held <= 0 {
            return Ok(0);
        }
        let promo_charged = actual_minor.min(promo_held);
        let mut charge_left = promo_charged;
        let draws = sqlx::query(
            "SELECT d.lot_id, d.amount_minor, l.closed_at \
             FROM oxsum.promo_draws d JOIN oxsum.promo_lots l ON l.lot_id = d.lot_id \
             WHERE d.hold_key = $1 AND d.charged_minor IS NULL \
             ORDER BY l.expires_at ASC NULLS LAST, d.lot_id \
             FOR UPDATE OF d, l",
        )
        .bind(hold_key)
        .fetch_all(&mut **tx)
        .await?;
        let mut burn = 0i64;
        for draw in &draws {
            let lot_id: Uuid = draw.try_get("lot_id")?;
            let reserved: i64 = draw.try_get("amount_minor")?;
            let charged = charge_left.min(reserved);
            charge_left -= charged;
            let released = reserved - charged;
            sqlx::query(
                "UPDATE oxsum.promo_draws SET charged_minor = $2, released_minor = $3 \
                 WHERE hold_key = $1 AND lot_id = $4",
            )
            .bind(hold_key)
            .bind(charged)
            .bind(released)
            .bind(lot_id)
            .execute(&mut **tx)
            .await?;
            if released > 0 {
                let closed: Option<OffsetDateTime> = draw.try_get("closed_at")?;
                if closed.is_none() {
                    // Open lot: the released share returns to the row it was
                    // pinned on — the deterministic un-draw.
                    sqlx::query(
                        "UPDATE oxsum.promo_lots SET remaining_minor = remaining_minor + $2 \
                         WHERE lot_id = $1",
                    )
                    .bind(lot_id)
                    .bind(released)
                    .execute(&mut **tx)
                    .await?;
                } else {
                    // Expired or cancelled: the released share can never be
                    // spent or refunded — the caller burns it to
                    // `Equity:PromoExpiry` once the release has landed.
                    burn += released;
                }
            }
        }
        Ok(burn)
    }

    /// The promo-leg grant a bundle fulfillment or an admin grant posts:
    /// `debit Adjustments / credit Promo`, booked inside the caller's
    /// transaction beside the `promo_lots` row it belongs to. `source` and
    /// `source_ref` are the lot's provenance and idempotency anchor — the
    /// unique pair makes a replayed grant a no-op; `expires_at` of `NULL`
    /// never expires.
    ///
    /// Returns `(lot_id, receipt)`; a replay finds the standing lot and its
    /// standing entry through the anchors rather than granting twice.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn grant_promo_lot_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        organization: Uuid,
        source: &str,
        source_ref: &str,
        ledger_key: &str,
        minor: i64,
        expires_at: Option<OffsetDateTime>,
        on: Date,
    ) -> Result<(Uuid, Receipt), WalletError> {
        let amt = positive(minor)?;
        // The grant inserts a promo_lots row; the append lock goes first so a
        // hold scanning the organization's lots cannot hold the append lock
        // while waiting on this grant's uncommitted row (issue #723).
        self.lock_append_in(tx).await?;
        // Replay first: the standing lot answers with its own id and the entry
        // id its grant landed under — the anchors make the pair a fact.
        if let Some(row) = sqlx::query(
            "SELECT lot_id FROM oxsum.promo_lots \
             WHERE organization_id = $1 AND source = $2 AND source_ref = $3",
        )
        .bind(organization)
        .bind(source)
        .bind(source_ref)
        .fetch_optional(&mut **tx)
        .await?
        {
            use sqlx::Row;
            let lot_id: Uuid = row.try_get("lot_id")?;
            let receipt = match self.store.get(entry_id_for(ledger_key)).await? {
                Some(stored) => stored_receipt(&stored),
                None => {
                    return Err(WalletError::InvalidInput(
                        "the promo grant's ledger entry is missing".into(),
                    ));
                }
            };
            return Ok((lot_id, receipt));
        }
        let lot_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO oxsum.promo_lots\
             (lot_id, organization_id, source, source_ref, amount_minor, remaining_minor, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $5, $6)",
        )
        .bind(lot_id)
        .bind(organization)
        .bind(source)
        .bind(source_ref)
        .bind(amt.to_minor())
        .bind(expires_at)
        .execute(&mut **tx)
        .await?;
        let receipt = self
            .append_in(
                tx,
                Entry::<Draft, SCALE>::new(entry_id_for(ledger_key), idem(ledger_key)?, on)
                    .with_description(description_of("promo lot grant")?)
                    .debit(self.adjustments, amt, currency())
                    .credit(self.promo, amt, currency()),
            )
            .await?;
        Ok((lot_id, receipt))
    }

    /// The burn posting an expiry or a cancellation writes: `debit Promo /
    /// credit PromoExpiry`, a net-zero move out of the spendable pool that can
    /// never return to refundable money (issue #609). `burn_key` names the
    /// event — `promo-expire:<lot>` or `promo-cancel:<lot>` — so a replayed
    /// pass posts the same entry once. Runs inside the caller's transaction
    /// beside the lot's own close, so the burn and the row update commit or
    /// roll back together.
    pub(crate) async fn burn_promo_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        burn_key: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        self.append_in(
            tx,
            Entry::<Draft, SCALE>::new(entry_id_for(burn_key), idem(burn_key)?, on)
                .with_description(description_of("promo lot expired")?)
                .debit(self.promo, amt, currency())
                .credit(self.promo_expiry, amt, currency()),
        )
        .await
    }
    ///
    /// When `key` is the operator's signing key and the entry is a settlement that
    /// names a price, the bundle also carries a [`PriceLink`](oxsum_verify::PriceLink)
    /// — the signed checkpoint binding the `(channel, model, priceVersion)` the bill
    /// records to the catalog row it priced under (issue #204). `None` key — a
    /// deployment that signs no heads — leaves `price` absent, as does a
    /// non-settlement entry or a settlement whose named row is gone; the verifier
    /// reports the binding absent rather than failed.
    pub async fn receipt_proof(
        &self,
        entry_id: EntryId,
        key: Option<&HeadSigningKey>,
    ) -> Result<Option<ProofBundle>, WalletError> {
        let Some(stored) = self.store.get(entry_id).await? else {
            return Ok(None);
        };
        let index = stored.require_index().map_err(invalid)?;
        let head = self.store.head().await?;
        let proof = self.store.prove_inclusion(index).await?;
        let (price, sale) = match key {
            Some(key) => (
                self.price_link(&stored, key).await?,
                self.sale_link(&stored, key).await?,
            ),
            None => (None, None),
        };
        Ok(Some(ProofBundle::new(
            stored.entry,
            head,
            proof,
            price,
            sale,
        )))
    }

    /// Customer evidence must apply the same attribution scope as record lists.
    pub async fn scoped_receipt_proof(
        &self,
        entry_id: EntryId,
        key: Option<&HeadSigningKey>,
        scope: &crate::CustomerRecordScope,
    ) -> Result<Option<ProofBundle>, WalletError> {
        let Some(stored) = self.store.get(entry_id).await? else {
            return Ok(None);
        };
        if !scope.allows_entry(&stored.entry) {
            return Ok(None);
        }
        self.receipt_proof(entry_id, key).await
    }

    /// The signed price binding for one settlement entry, or `None` when it is not a
    /// settlement or the price it names is gone.
    ///
    /// Reads the `(channel, model, priceVersion)` the settlement description records —
    /// the same fields the entry's content hash already covers — fetches that row, and
    /// signs it as a catalog checkpoint. Signing is stateless and deterministic over
    /// (key, origin, version, row), so the note a v1 bundle carries is identical
    /// whether it was made at bill time or after the catalog moved on.
    async fn price_link(
        &self,
        stored: &StoredEntry<SCALE>,
        key: &HeadSigningKey,
    ) -> Result<Option<oxsum_verify::PriceLink>, WalletError> {
        let Some(record) = SettlementRecord::parse(stored.entry.description().as_str()) else {
            return Ok(None);
        };
        // The catalog lives in the shared `oxsum` schema on the same pool the
        // tenant ledger runs on — no second handle to thread through.
        let Some(row) = crate::channels::price_row(
            self.store.pool(),
            &record.channel,
            &record.model,
            Some(record.price_version),
        )
        .await?
        else {
            return Ok(None);
        };
        let canonical = canonical_price_row(&record.channel, &row)?;
        let signed = sign_price_head(key, &record.channel, &row, &canonical).map_err(invalid)?;
        Ok(Some(
            oxsum_verify::PriceLink::builder()
                .channel(signed.channel)
                .model(signed.model)
                .version(signed.version)
                .row(canonical)
                .note(signed.note)
                .key_name(signed.key.name)
                .public_key(hex(&signed.key.public_key))
                .key_hash(hex(&signed.key.key_hash))
                .build(),
        ))
    }

    /// The signed sale binding for one settlement entry, or `None` when the
    /// entry is not a settlement, the settlement priced under the wholesale
    /// channel catalog (its description commits no `sale` digest), or the
    /// snapshot the digest names was never recorded.
    ///
    /// Reads the `sale.digest` the settlement description commits — the same
    /// field the entry's content hash already covers — fetches the canonical
    /// snapshot row the digest names, and signs it as a sale-catalog checkpoint
    /// (issue #600). Signing is stateless and deterministic over (key, origin,
    /// version, row), so the note a bundle carries is identical whether it was
    /// made at bill time or after the sale book moved on. The canonical row
    /// binds only the customer-visible terms, so the evidence cannot attest a
    /// supplier rate.
    async fn sale_link(
        &self,
        stored: &StoredEntry<SCALE>,
        key: &HeadSigningKey,
    ) -> Result<Option<oxsum_verify::SaleLink>, WalletError> {
        let Some(record) = SettlementRecord::parse(stored.entry.description().as_str()) else {
            return Ok(None);
        };
        let Some(sale) = record.sale else {
            return Ok(None);
        };
        // The digest names the stored canonical row verbatim — the bytes the
        // checkpoint's root commits to, so re-serving them keeps the proof
        // honest however the sale book has since moved.
        let Some((business, canonical)) = crate::Db::from_pool(self.store.pool().clone())
            .sale_snapshot_by_digest(&sale.digest)
            .await?
        else {
            return Ok(None);
        };
        let snapshot: serde_json::Value = serde_json::from_str(&canonical)
            .map_err(|e| WalletError::Misconfigured(format!("stored sale row: {e}")))?;
        let str_field = |field: &str| {
            snapshot
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let organization: uuid::Uuid = str_field("organizationId")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| invalid("stored sale row has no organizationId"))?;
        let model = str_field("model").ok_or_else(|| invalid("stored sale row has no model"))?;
        let version = snapshot
            .get("version")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let signed = sign_sale_checkpoint(key, business, organization, &model, version, &canonical)
            .map_err(invalid)?;
        Ok(Some(
            oxsum_verify::SaleLink::builder()
                .organization_id(organization.to_string())
                .model(model)
                .digest(sale.digest.clone())
                .row(canonical)
                .note(signed.note)
                .key_name(signed.key.name)
                .public_key(hex(&signed.key.public_key))
                .key_hash(hex(&signed.key.key_hash))
                .build(),
        ))
    }

    /// Number of entries in the log. With isolated ledgers, each tenant counts from zero.
    pub async fn log_size(&self) -> Result<u64, WalletError> {
        Ok(self.store.head().await?.size)
    }

    /// Appends the statement anchor — the marker entry committing a finalized
    /// statement's digests into the tenant ledger (issue #221). `pub(crate)`:
    /// only [`crate::Db::finalize_statement`] calls it.
    ///
    /// One ledger entry per statement: a 1-minor balanced `Equity:Adjustments`
    /// debit/credit (the posting amount is immaterial — the marker is metadata,
    /// not money), idempotency-keyed `statement-anchor/<statement_id>` so a
    /// retried finalize replays rather than duplicates. The entry's
    /// [`DocumentRef`] binds the marker to the window digest, and its
    /// description carries the rollup digest plus the figures it summarizes.
    /// Returns the marker's id and its position in the log — the head that
    /// follows that position is what proves the anchor was committed.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub(crate) async fn anchor_statement(
        &self,
        statement_id: Uuid,
        description_json: &str,
        window_hash: Hash,
    ) -> Result<(EntryId, u64), WalletError> {
        let key = format!("statement-anchor/{statement_id}");
        let minor = Credits::from_minor(1);
        let entry = Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_document(DocumentRef::new(STATEMENT_ANCHOR_DOCUMENT, window_hash).map_err(invalid)?)
        .with_description(description_of(description_json)?)
        .debit(self.adjustments, minor, currency())
        .credit(self.adjustments, minor, currency());
        let receipt = match self.append(entry).await {
            Ok(receipt) => receipt,
            Err(error) => {
                // A raced finalize can land on the same idempotency key; the
                // standing entry is the same anchor either way, so read it back
                // instead of failing the race's loser.
                let Some(stored) = self.store.get(entry_id_for(&key)).await? else {
                    return Err(error);
                };
                stored_receipt(&stored)
            }
        };
        let index = receipt.log_index.ok_or_else(|| {
            WalletError::InvalidInput("the statement anchor did not sequence".into())
        })?;
        Ok((receipt.entry_id, index))
    }

    /// Appends a usage-report marker — the entry that commits an end-user's
    /// subset into the tenant ledger (issue #261). `pub(crate)`: only
    /// [`crate::Db::mint_usage_report`] calls it.
    ///
    /// One ledger entry per `(end_user, from, to)`: a 1-minor balanced
    /// `Equity:Adjustments` debit/credit (the posting amount is immaterial —
    /// the marker is metadata, not money), idempotency-keyed
    /// `usage-report/<endUser>/<from>/<to>` so a re-request for the same span
    /// replays the standing marker rather than committing a second subset —
    /// the once-per-span rule the completeness claim rests on. The entry's
    /// [`DocumentRef`] binds the marker to the subset digest, and its
    /// description carries the canonical commitment.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub(crate) async fn anchor_usage_report(
        &self,
        end_user: &str,
        from: &str,
        to: &str,
        description_json: &str,
        subset_hash: Hash,
    ) -> Result<(EntryId, u64), WalletError> {
        let key = format!("usage-report/{end_user}/{from}/{to}");
        let minor = Credits::from_minor(1);
        let entry = Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_document(DocumentRef::new(USAGE_REPORT_DOCUMENT, subset_hash).map_err(invalid)?)
        .with_description(description_of(description_json)?)
        .debit(self.adjustments, minor, currency())
        .credit(self.adjustments, minor, currency());
        let receipt = match self.append(entry).await {
            Ok(receipt) => receipt,
            Err(error) => {
                // A raced mint lands on the same idempotency key; the standing
                // entry is the same report either way, so read it back.
                let Some(stored) = self.store.get(entry_id_for(&key)).await? else {
                    return Err(error);
                };
                stored_receipt(&stored)
            }
        };
        let index = receipt.log_index.ok_or_else(|| {
            WalletError::InvalidInput("the usage-report marker did not sequence".into())
        })?;
        Ok((receipt.entry_id, index))
    }

    /// Appends a usage day anchor — the marker entry committing `day`'s
    /// sorted-map root into the tenant ledger (issue #338). `pub(crate)`: only
    /// [`crate::Db::anchor_usage_days`] calls it.
    ///
    /// One entry per `(organization, day)`: a 1-minor balanced
    /// `Equity:Adjustments` debit/credit (the marker is metadata, not money),
    /// idempotency-keyed `usage-anchor/<day>` so a retried pass replays the
    /// standing entry instead of committing a second root for the same day. The
    /// [`DocumentRef`] binds the anchor to the map root, and the description
    /// carries the day, root and customer count.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub(crate) async fn anchor_usage_day(
        &self,
        day: time::Date,
        description_json: &str,
        map_root: Hash,
    ) -> Result<(EntryId, u64), WalletError> {
        let key = format!("usage-anchor/{day}");
        let minor = Credits::from_minor(1);
        let entry = Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_document(DocumentRef::new(USAGE_ANCHOR_DOCUMENT, map_root).map_err(invalid)?)
        .with_description(description_of(description_json)?)
        .debit(self.adjustments, minor, currency())
        .credit(self.adjustments, minor, currency());
        let receipt = match self.append(entry).await {
            Ok(receipt) => receipt,
            Err(error) => {
                // A raced pass lands on the same idempotency key; the standing
                // entry is the same anchor either way, so read it back.
                let Some(stored) = self.store.get(entry_id_for(&key)).await? else {
                    return Err(error);
                };
                stored_receipt(&stored)
            }
        };
        let index = receipt
            .log_index
            .ok_or_else(|| WalletError::InvalidInput("the usage anchor did not sequence".into()))?;
        Ok((receipt.entry_id, index))
    }

    /// Assembles a usage report's self-contained bundle under one head (issue
    /// #261): the marker entry, every shown settlement entry, each with its
    /// inclusion proof, all proven against the *same* head — then that head
    /// signed when the deployment signs heads.
    ///
    /// The single head is the load-bearing detail. Proving each row against
    /// the current head as it is read would let a concurrent append hand the
    /// bundle two different heads, and a row's proof would fail against the
    /// head the report actually attests. Here the head is read once and every
    /// `prove_inclusion_at` pins to its size, so the bundle is internally
    /// consistent even while the ledger grows.
    ///
    /// `rows` are the frozen `usage_report_rows` (or a fresh mint's subset): each
    /// carries `entry_id`, `log_index` and the recorded `content_hash`. The
    /// `entry` object is read back from the ledger by id — the bundle serves the
    /// entry's full serde encoding so the verifier recomputes the hash rather
    /// than trusting a served one. A row whose id no longer resolves, or whose
    /// recomputed hash disagrees with what was frozen, is a data-integrity
    /// failure and surfaces as [`WalletError::InvalidInput`].
    ///
    /// Returns `(marker_bundle, entry_bundles, head, signed_head)`; the route
    /// serializes them into the `UsageReport` body. `signed_head` is `None` on a
    /// deployment that does not sign heads — the bundle is then verified to the
    /// digests alone, the same posture `receipt_proof` takes with `price`.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub async fn usage_report_bundle(
        &self,
        marker_id: EntryId,
        rows: &[UsageReportRow],
        key: Option<&HeadSigningKey>,
    ) -> Result<
        (
            ReportEntryBundle,
            Vec<ReportEntryBundle>,
            doubleentry::TreeHead,
            Option<SignedHead>,
        ),
        WalletError,
    > {
        let head = self.store.head().await?;
        // The marker is identified by id and reports its own hash; the shown
        // entries additionally pin the hash frozen at mint, so a ledger row
        // altered underneath is reported rather than served.
        let Some(stored) = self.store.get(marker_id).await? else {
            return Err(WalletError::InvalidInput(
                "the usage-report marker is gone from the ledger".into(),
            ));
        };
        let marker = ReportEntryBundle {
            proof: self
                .store
                .prove_inclusion_at(stored.require_index().map_err(invalid)?, head.size)
                .await
                .map_err(invalid)?,
            content_hash: stored.content_hash.to_hex(),
            entry: stored.entry,
        };
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(stored) = self.store.get(EntryId::from_uuid(row.entry_id)).await? else {
                return Err(WalletError::InvalidInput(
                    "a usage-report row's entry is gone from the ledger".into(),
                ));
            };
            if stored.content_hash.to_hex() != row.content_hash {
                return Err(WalletError::InvalidInput(
                    "a usage-report row's hash disagrees with the ledger".into(),
                ));
            }
            entries.push(ReportEntryBundle {
                proof: self
                    .store
                    .prove_inclusion_at(stored.require_index().map_err(invalid)?, head.size)
                    .await
                    .map_err(invalid)?,
                content_hash: stored.content_hash.to_hex(),
                entry: stored.entry,
            });
        }
        let signed = match key {
            Some(key) => {
                let origin = origin_for(&self.tenant_id).map_err(invalid)?;
                Some(sign_head(key, &origin, head).map_err(invalid)?)
            }
            None => None,
        };
        Ok((marker, entries, head, signed))
    }

    /// One entry's bundle part — the entry, its recorded hash and its inclusion
    /// proof against `head` — for callers that need a marker-like entry outside
    /// a report's row list (the daily usage anchor, issue #339).
    pub(crate) async fn entry_bundle(
        &self,
        entry_id: EntryId,
        head: doubleentry::TreeHead,
    ) -> Result<ReportEntryBundle, WalletError> {
        let Some(stored) = self.store.get(entry_id).await? else {
            return Err(WalletError::InvalidInput(
                "the anchored entry is gone from the ledger".into(),
            ));
        };
        Ok(ReportEntryBundle {
            proof: self
                .store
                .prove_inclusion_at(stored.require_index().map_err(invalid)?, head.size)
                .await
                .map_err(invalid)?,
            content_hash: stored.content_hash.to_hex(),
            entry: stored.entry,
        })
    }

    /// Appends an evidence-bundle marker — the entry that commits a customer
    /// evidence bundle's manifest digest into the tenant ledger (issue #616).
    /// `pub(crate)`: only [`crate::Db::mint_period_evidence`] and
    /// [`crate::Db::mint_statement_evidence`] call it.
    ///
    /// One ledger entry per `(kind, scope)`: a 1-minor balanced
    /// `Equity:Adjustments` debit/credit (the posting amount is immaterial —
    /// the marker is metadata, not money), idempotency-keyed
    /// `evidence-bundle/<scope>` so a re-request for the same span or
    /// statement replays the standing marker rather than committing a second
    /// manifest — the once-per-scope rule the manifest's completeness claim
    /// rests on. The entry's [`DocumentRef`] binds the marker to the manifest
    /// digest, and its description carries the scope and figures.
    ///
    /// # Errors
    ///
    /// Storage and ledger failures surface as [`WalletError`].
    pub(crate) async fn anchor_evidence_bundle(
        &self,
        scope_key: &str,
        description_json: &str,
        manifest_digest: Hash,
    ) -> Result<(EntryId, u64), WalletError> {
        let key = format!("evidence-bundle/{scope_key}");
        let minor = Credits::from_minor(1);
        let entry = Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_document(
            DocumentRef::new(EVIDENCE_BUNDLE_DOCUMENT, manifest_digest).map_err(invalid)?,
        )
        .with_description(description_of(description_json)?)
        .debit(self.adjustments, minor, currency())
        .credit(self.adjustments, minor, currency());
        let receipt = match self.append(entry).await {
            Ok(receipt) => receipt,
            Err(error) => {
                // A raced mint lands on the same idempotency key; the standing
                // entry is the same bundle either way, so read it back.
                let Some(stored) = self.store.get(entry_id_for(&key)).await? else {
                    return Err(error);
                };
                stored_receipt(&stored)
            }
        };
        let index = receipt.log_index.ok_or_else(|| {
            WalletError::InvalidInput("the evidence marker did not sequence".into())
        })?;
        Ok((receipt.entry_id, index))
    }

    /// The marker plus every entry `ids` names, each proven against the one
    /// head — the `entries` array an evidence bundle serializes (issue #616).
    /// Proving every row against a single read head keeps the bundle
    /// internally consistent while the ledger grows, the same discipline
    /// `usage_report_bundle` keeps.
    ///
    /// `ids` pairs each entry id with the `(kind, id)` of the artifact that
    /// wants its proof; the marker comes first, tagged `("bundle", its id)`.
    /// `signed_head` — `signed_head_at(head.size)` under the caller's key —
    /// is left to the caller, which already holds the optional key.
    ///
    /// Returns `(head, marker, entries)` in the bundle's own order.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when an id no longer resolves, or a stored
    /// entry's hash disagrees with what the artifact committed; storage and
    /// ledger failures surface as [`WalletError`].
    pub(crate) async fn evidence_entry_bundles(
        &self,
        marker_id: EntryId,
        ids: &[(EntryId, String, String)],
    ) -> Result<
        (
            doubleentry::TreeHead,
            EvidenceEntryBundle,
            Vec<EvidenceEntryBundle>,
        ),
        WalletError,
    > {
        let head = self.store.head().await?;
        let marker = self.entry_bundle(marker_id, head).await?;
        let marker = EvidenceEntryBundle {
            entry: marker.entry,
            content_hash: marker.content_hash,
            proof: marker.proof,
            for_kind: "bundle".to_owned(),
            for_id: marker_id.as_uuid().to_string(),
        };
        let mut entries = Vec::with_capacity(ids.len());
        for (entry_id, for_kind, for_id) in ids {
            let bundle = self.entry_bundle(*entry_id, head).await?;
            entries.push(EvidenceEntryBundle {
                entry: bundle.entry,
                content_hash: bundle.content_hash,
                proof: bundle.proof,
                for_kind: for_kind.clone(),
                for_id: for_id.clone(),
            });
        }
        Ok((head, marker, entries))
    }

    /// A refused gateway request's billing record: one marker entry appended
    /// under `req-<request>:refused`, so a request admission turned away still
    /// lands on the billing page with its terminal status and reason (issue
    /// #233). The marker is a self-balancing 1-minor round-trip on the
    /// adjustments account — like the statement anchor — because a refusal
    /// moves no money but is still a billing fact the ledger proves: the
    /// description is a settlement record of kind `refused`, so the requests
    /// page reads it the same way it reads every other outcome.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`]. A request id already
    /// marked replays the first marker: the append and the read answer the
    /// same id.
    pub async fn record_refusal(
        &self,
        request_id: &str,
        model: &str,
        channel: &str,
        reason: &str,
        actor: Option<&str>,
    ) -> Result<EntryId, WalletError> {
        let key = format!("req-{request_id}:refused");
        let usage = UsageRecord::default();
        // The reason truncates to keep the record inside the description's
        // 512-character cap — the full message is already in the answer the
        // caller got.
        let mut end = reason.len().min(150);
        while end > 0 && !reason.is_char_boundary(end) {
            end -= 1;
        }
        let reason = &reason[..end];
        let description = Settlement {
            request: request_id,
            channel,
            model,
            // A refused request served nothing: the asked-for name is what the
            // record reports on both sides.
            requested_model: model,
            served_model: model,
            // A refused request never resolved its route — nothing upstream to name.
            upstream_model: None,
            price_version: 0,
            kind: SettlementKind::Refused,
            usage: &usage,
            lines: &[],
            matched_rule: None,
            discount_percent: None,
            charged: 0,
            freeze: 0,
            upstream_attempts: 0,
            mandate: None,
            subscription: None,
            cache: None,
            guardrail: Vec::new(),
            reason: Some(reason),
            experiment: None,
            // A refused request never priced: no sale terms to commit.
            sale: None,
        }
        .description()?;
        let mut draft = Entry::<Draft, SCALE>::new(
            entry_id_for(&key),
            idem(&key)?,
            OffsetDateTime::now_utc().date(),
        )
        .with_description(description_of(&description)?);
        if let Some(actor) = actor {
            draft = draft.with_provenance(Provenance::none().with_actor(actor).map_err(invalid)?);
        }
        let minor = Credits::from_minor(1);
        let entry = draft.debit(self.adjustments, minor, currency()).credit(
            self.adjustments,
            minor,
            currency(),
        );
        let receipt = match self.append(entry).await {
            Ok(receipt) => receipt,
            Err(error) => {
                // A raced or repeated marker lands on the same key; the
                // standing entry is the same refusal either way.
                let Some(stored) = self.store.get(entry_id_for(&key)).await? else {
                    return Err(error);
                };
                stored_receipt(&stored)
            }
        };
        Ok(receipt.entry_id)
    }

    /// The current tree head, signed by the operator: what `GET /api/v1/log/head` serves.
    ///
    /// The note attests the head at signing time; the ledger keeps growing underneath, and a
    /// later call signs a later head. Nothing is stored: the signature is recomputed from the
    /// key, so rotating the key rotates every head without a migration.
    pub async fn signed_head(&self, key: &HeadSigningKey) -> Result<SignedHead, WalletError> {
        let origin = origin_for(&self.tenant_id).map_err(invalid)?;
        let head = self.store.head().await?;
        sign_head(key, &origin, head).map_err(invalid)
    }

    /// The head at an earlier log size, signed by the operator: what the statement
    /// anchor serves (issue #221) — the head right after the marker is what proves
    /// the anchor's inclusion.
    ///
    /// # Errors
    ///
    /// [`WalletError::InvalidInput`] when `size` exceeds the log's current size.
    pub async fn signed_head_at(
        &self,
        key: &HeadSigningKey,
        size: u64,
    ) -> Result<SignedHead, WalletError> {
        let origin = origin_for(&self.tenant_id).map_err(invalid)?;
        let head = self.store.head_at(size).await.map_err(invalid)?;
        sign_head(key, &origin, head).map_err(invalid)
    }

    /// A consistency proof from an earlier size to the current head, with the new head signed:
    /// what `GET /api/v1/log/consistency` serves.
    ///
    /// `from` must name a real head: at least 1 — every log extends the empty tree, so a
    /// proof from size 0 would verify against any history at all and is refused — and at most
    /// the current size. The old head is recomputed from the log, not taken from the caller,
    /// so the two heads and the proof always agree with each other.
    pub async fn consistency(
        &self,
        from: u64,
        key: &HeadSigningKey,
    ) -> Result<Consistency, WalletError> {
        if from == 0 {
            return Err(WalletError::InvalidInput(
                "consistency proofs start at size 1: a proof from the empty tree would verify against any history".into(),
            ));
        }
        let head = self.store.head().await?;
        if from > head.size {
            return Err(WalletError::InvalidInput(format!(
                "cannot prove consistency from size {from} against a log of size {}",
                head.size
            )));
        }
        // The proof is built against the head just read, and the response signs that same
        // head: a write landing between the two leaves the answer one entry behind the
        // absolute latest, still a true statement about the head it attests.
        let proof = self
            .store
            .prove_consistency_between(from, head.size)
            .await
            .map_err(invalid)?;
        let old_head = self.store.head_at(from).await.map_err(invalid)?;
        let origin = origin_for(&self.tenant_id).map_err(invalid)?;
        Ok(Consistency {
            signed: sign_head(key, &origin, head).map_err(invalid)?,
            old_head,
            proof,
        })
    }

    /// The newest entries of the log, newest first, at most `limit`.
    ///
    /// What the dashboard's transaction log lists: the position, the id, the description
    /// the writer recorded, and the content hash the Merkle log commits to. The proof
    /// page (a later item) is what verifies one; this only lists them.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_entries(&self, limit: usize) -> Result<Vec<LogEntry>, WalletError> {
        Ok(self.entries_page(None, limit).await?.rows)
    }

    /// The transaction log, one page at a time (issue #93): `before` bounds the page
    /// to entries with a smaller log index — the ledger's own cursor — and the
    /// answer's `next_cursor` resumes there, `None` at the log's start.
    ///
    /// Same rows as [`recent_entries`](Self::recent_entries), which reads the first
    /// page.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn entries_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<LogEntry>, WalletError> {
        self.scoped_entries_page(
            before,
            limit,
            &crate::CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            },
        )
        .await
    }
    /// Raw entries filtered by their immutable provenance before they leave core.
    pub async fn scoped_entries_page(
        &self,
        before: Option<u64>,
        limit: usize,
        scope: &crate::CustomerRecordScope,
    ) -> Result<ListPage<LogEntry>, WalletError> {
        self.scan_back(before, limit.clamp(1, 100), 100, &mut |stored| {
            if !scope.allows_entry(&stored.entry) {
                return None;
            }
            Some(LogEntry {
                index: stored
                    .require_index()
                    .map(LogIndex::get)
                    .unwrap_or(u64::MAX),
                id: stored.entry.id().to_string(),
                description: stored.entry.description().as_str().to_owned(),
                content_hash: stored.content_hash.to_string(),
            })
        })
        .await
    }

    /// The newest settled entries, newest first, at most `limit`.
    ///
    /// What the dashboard's bills page lists and its CSV and JSON exports carry: the id,
    /// the booking date, the charge in minor units and the content hash.
    ///
    /// A settled entry is one that *released a hold*, and the ledger says which those are:
    /// [`Wallet::settle`] always credits the wallet account in the pending layer — the
    /// mirror of the pending debit a hold takes — and no other write does. The entry's
    /// description cannot answer this: the wallet API's settlements record none
    /// (`POST /api/v1/settlements`, whose whole input is the hold key and the actual), so
    /// identifying settlements by their description would silently omit them.
    ///
    /// The charge is the entry's net debit on the wallet account in the *settled* layer,
    /// i.e. what the books hold; it is 0 for the settlement kinds that charge nothing (an
    /// upstream error, an unreachable upstream, a swept hold).
    ///
    /// The read walks back one page at a time and stops as soon as it has `limit` bills, so
    /// a busy log costs no more than the entries above the last one it returns.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settled_entries(&self, limit: usize) -> Result<Vec<SettledEntry>, WalletError> {
        Ok(self.settled_entries_page(None, limit).await?.rows)
    }

    /// The settled entries, one page at a time (issue #93): same rows as
    /// [`settled_entries`](Self::settled_entries), which reads the first page.
    /// `before` bounds the page to entries with a smaller log index and the answer's
    /// `next_cursor` resumes there.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settled_entries_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<SettledEntry>, WalletError> {
        self.scan_back(before, limit.clamp(1, 100), BILLS_SCAN, &mut |stored| {
            self.is_settlement(stored).then(|| SettledEntry {
                id: stored.entry.id().to_string(),
                booked_on: stored.entry.booking_date(),
                charged_minor: self.settled_charge(stored),
                content_hash: stored.content_hash.to_string(),
            })
        })
        .await
    }

    /// The organization's transactions as the bills page lists them, newest first,
    /// at most `limit`: top-ups, adjustments — the signup bonus among them — and
    /// settled requests (issue #90).
    ///
    /// Each entry carries its kind, its signed effect on the wallet's settled
    /// balance, the key that paid where the entry attributes one, and — for a
    /// settled gateway turn — the settlement record the entry's own description
    /// holds, so a row shows exactly what the bill's content hash covers. A hold
    /// bills nothing yet and is skipped; the overview's in-flight list is where
    /// it shows.
    ///
    /// The read walks back one page at a time and stops as soon as it has `limit`
    /// transactions, so a long log costs no more than the entries above the oldest
    /// one it returns.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_transactions(
        &self,
        limit: usize,
    ) -> Result<Vec<TransactionEntry>, WalletError> {
        Ok(self.transactions_page(None, limit).await?.rows)
    }

    /// The organization's transactions, one page at a time (issue #93): same rows as
    /// [`recent_transactions`](Self::recent_transactions), which reads the first page.
    /// `before` bounds the page to entries with a smaller log index and the answer's
    /// `next_cursor` resumes there — the bills page's older rows and the CSV and JSON
    /// exports, which now list *every* transaction, walk these pages.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn transactions_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<TransactionEntry>, WalletError> {
        self.scoped_transactions_page(
            before,
            limit,
            &crate::CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            },
        )
        .await
    }

    /// Apply immutable record attribution before selecting a page of rows.
    pub async fn scoped_transactions_page(
        &self,
        before: Option<u64>,
        limit: usize,
        scope: &crate::CustomerRecordScope,
    ) -> Result<ListPage<TransactionEntry>, WalletError> {
        self.scan_back(before, limit.clamp(1, 100), BILLS_SCAN, &mut |stored| {
            if !scope.allows_entry(&stored.entry) {
                return None;
            }
            let kind = self.classify(stored)?;
            Some(TransactionEntry {
                id: stored.entry.id().to_string(),
                index: stored
                    .require_index()
                    .map(LogIndex::get)
                    .unwrap_or(u64::MAX),
                booked_on: stored.entry.booking_date(),
                description: stored.entry.description().as_str().to_owned(),
                content_hash: stored.content_hash.to_string(),
                amount_minor: self.settled_effect(stored),
                kind,
                key_id: stored
                    .entry
                    .provenance()
                    .actor
                    .as_ref()
                    .map(|actor| actor.as_str().to_owned()),
                record: (kind == TransactionKind::Settlement)
                    .then(|| SettlementRecord::parse(stored.entry.description().as_str()))
                    .flatten(),
            })
        })
        .await
    }

    /// Which of the bills page's kinds an entry is, or `None` for an entry the page does
    /// not list: a hold, which bills nothing yet.
    ///
    /// The postings decide, never the description: a settlement is the pending-layer
    /// credit that releases the reserve, a top-up posts `Assets:Cash`, an adjustment
    /// posts `Equity:Adjustments` — each exactly once, because those are the only
    /// writers these accounts have.
    fn classify(&self, stored: &StoredEntry<SCALE>) -> Option<TransactionKind> {
        let mut pending_credit = false;
        let mut pending_debit = false;
        let mut cash = false;
        let mut adjustments = false;
        let mut facility = false;
        for p in stored.entry.postings() {
            if self.is_pool(p.account) {
                match (p.layer, p.direction) {
                    (Layer::Pending, Direction::Credit) => pending_credit = true,
                    (Layer::Pending, Direction::Debit) => pending_debit = true,
                    _ => {}
                }
            } else if p.account == self.cash {
                cash = true;
            } else if p.account == self.adjustments {
                adjustments = true;
            } else if p.account == self.facility {
                facility = true;
            }
        }
        if pending_credit {
            return Some(TransactionKind::Settlement);
        }
        if pending_debit {
            return None;
        }
        if cash {
            return Some(TransactionKind::TopUp);
        }
        // A credit-limit change touches only the facility and the credit line:
        // an operator action that moves what the organization may draw, shown
        // like the other operator movements rather than hidden from the history.
        if adjustments || facility {
            return Some(TransactionKind::Adjustment);
        }
        None
    }

    /// The entry's effect on the pools' combined settled balance, signed: credits
    /// add, debits take. A top-up or a grant reads positive, a deduction or a
    /// settled charge negative, and a settlement that charged nothing reads zero —
    /// the pending-layer release it also writes is not a movement of settled money.
    /// The pool reclassification nets to zero across the two accounts.
    fn settled_effect(&self, stored: &StoredEntry<SCALE>) -> i64 {
        stored
            .entry
            .postings()
            .iter()
            .filter(|p| self.is_pool(p.account) && p.layer == Layer::Settled)
            .map(|p| match p.direction {
                Direction::Credit => p.amount.to_minor(),
                Direction::Debit => -p.amount.to_minor(),
            })
            .sum()
    }

    /// True when the entry settled a hold: the pending-layer credit on a pool account
    /// that [`Wallet::settle`] writes for every settlement, whatever it charges.
    fn is_settlement(&self, stored: &StoredEntry<SCALE>) -> bool {
        stored.entry.postings().iter().any(|p| {
            self.is_pool(p.account) && p.layer == Layer::Pending && p.direction == Direction::Credit
        })
    }

    /// What the entry charged: the pool accounts' net debit in the settled layer, in
    /// minor units. Nothing at all for a settlement that charged nothing.
    fn settled_charge(&self, stored: &StoredEntry<SCALE>) -> i64 {
        stored
            .entry
            .postings()
            .iter()
            .filter(|p| self.is_pool(p.account) && p.layer == Layer::Settled)
            .map(|p| match p.direction {
                Direction::Debit => p.amount.to_minor(),
                Direction::Credit => -p.amount.to_minor(),
            })
            .sum()
    }

    /// The four pools' combined balance in one layer. All four are
    /// credit-normal — the wallet and the credit line are liabilities, the
    /// bonus and promo pools equity — so a credit balance is the positive side
    /// on each.
    async fn pool_net(&self, layer: Layer) -> Result<i64, WalletError> {
        Ok(self.account_net(self.wallet, layer).await?
            + self.account_net(self.bonus, layer).await?
            + self.account_net(self.promo, layer).await?
            + self.account_net(self.credit_line, layer).await?)
    }

    /// Whether the account is one a hold reserves against and a settlement draws:
    /// the promo pool, the bonus pool, the purchased wallet, or the credit line.
    /// The facility and promo-expiry accounts are not pools — nothing ever
    /// spends out of them.
    fn is_pool(&self, account: AccountId) -> bool {
        account == self.wallet
            || account == self.bonus
            || account == self.promo
            || account == self.credit_line
    }

    /// The bonus pool's settled balance: what a deduction may draw before it
    /// touches purchased credit.
    async fn bonus_settled(&self) -> Result<i64, WalletError> {
        self.account_net(self.bonus, Layer::Settled).await
    }

    async fn account_net(&self, account: AccountId, layer: Layer) -> Result<i64, WalletError> {
        let b = self
            .store
            .balance(
                BalanceKey {
                    account,
                    currency: currency(),
                    layer,
                },
                BalanceQuery::all(),
            )
            .await?;
        Ok(b.credits.to_minor() - b.debits.to_minor())
    }

    /// An account's settled balance on the caller's connection — credits minus
    /// debits — for reads that must agree with what a locked transaction sees.
    /// [`account_net`](Self::account_net) answers the same figure on its own
    /// connection; this one exists for the credit paths, whose read has to be
    /// covered by the advisory lock the caller already holds.
    async fn settled_net_in(
        &self,
        conn: &mut sqlx::PgConnection,
        account: AccountId,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT COALESCE(SUM(CASE \
                 WHEN p.direction = 'C' THEN p.amount_minor \
                 ELSE -p.amount_minor END), 0)::bigint AS net \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = $1 AND p.layer = 'settled' AND e.log_index IS NOT NULL",
            schema = self.tenant_id,
        );
        let net: i64 = sqlx::query_scalar(&sql)
            .bind(account.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
        Ok(net)
    }

    /// What a pool account can still fund: its settled balance minus the
    /// reservations pending holds still carry. Pending debits subtract what is
    /// held, pending credits add back what a settlement released — and
    /// `FundedReservations` keeps the pending layer's net on the hold side, so
    /// the figure can never exceed the settled balance.
    async fn pool_available_in(
        &self,
        conn: &mut sqlx::PgConnection,
        account: AccountId,
    ) -> Result<i64, WalletError> {
        let sql = format!(
            "SELECT COALESCE(SUM(CASE \
                 WHEN p.layer = 'settled' AND p.direction = 'C' THEN p.amount_minor \
                 WHEN p.layer = 'settled' AND p.direction = 'D' THEN -p.amount_minor \
                 WHEN p.layer = 'pending' AND p.direction = 'D' THEN -p.amount_minor \
                 WHEN p.layer = 'pending' AND p.direction = 'C' THEN p.amount_minor \
                 ELSE 0 END), 0)::bigint AS available \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = $1 AND e.log_index IS NOT NULL",
            schema = self.tenant_id,
        );
        let available: i64 = sqlx::query_scalar(&sql)
            .bind(account.index() as i32)
            .fetch_one(&mut *conn)
            .await?;
        Ok(available)
    }

    /// Seals a draft against the ledger's *current* calendar — read back from
    /// storage on every write, not held from `open`, so a month the operator closes
    /// stops new postings dated into it on the very next call rather than after a
    /// restart. This is where "a sealed month accepts no new entries" is enforced:
    /// `Entry::seal` refuses a booking date the calendar's sealed watermark covers.
    async fn seal(
        &self,
        draft: Entry<Draft, SCALE>,
    ) -> Result<Entry<Balanced, SCALE>, WalletError> {
        let calendar = self.store.calendar().await?;
        draft
            .seal(&SealContext {
                accounts: &self.registry,
                calendar: &calendar,
                policy: &self.policy,
            })
            .map_err(invalid)
    }

    async fn append(&self, draft: Entry<Draft, SCALE>) -> Result<Receipt, WalletError> {
        let entry = self.seal(draft).await?;
        self.append_sealed(entry).await
    }

    async fn append_sealed(&self, entry: Entry<Balanced, SCALE>) -> Result<Receipt, WalletError> {
        let recorded = self.store.append(&EntryBatch::single(entry)).await?;
        let r = recorded
            .into_iter()
            .next()
            .ok_or_else(|| WalletError::InvalidInput("empty append result".into()))?;
        Ok(Receipt {
            entry_id: r.id,
            log_index: r.index.map(|i| i.get()),
            content_hash: r.content_hash,
            is_new: r.is_new,
        })
    }

    /// Takes the ledger's append advisory lock inside `tx`.
    ///
    /// Every transaction that writes `oxsum.promo_lots` or `oxsum.promo_draws`
    /// takes this lock before its first promo row lock (issue #723). The hold
    /// path already does — `hold_inner` pre-claims it, `hold_for_key` calls
    /// this helper before pinning — and expiry, cancellation, settlement,
    /// granting and the legacy migration call it for the same reason: the
    /// global order is append advisory lock first, promo row locks second, so
    /// a hold waiting on a lot row can never cycle with a writer waiting on
    /// the append lock.
    ///
    /// The lock is transaction-scoped and re-entrant: the engine's own
    /// `append_in_tx` takes it again once the row work is done.
    pub(crate) async fn lock_append_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), WalletError> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(self.store.append_lock())
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// [`append`](Self::append) inside the caller's transaction (issue #602):
    /// the draft seals against the calendar read off the pool — same as
    /// `append` does — then rides the borrowed-transaction append, so the
    /// entry commits with whatever else the caller's transaction carries.
    /// The caller owns the commit; a rolled-back transaction leaves no entry,
    /// no node and no idempotency claim.
    async fn append_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        draft: Entry<Draft, SCALE>,
    ) -> Result<Receipt, WalletError> {
        let entry = self.seal(draft).await?;
        let recorded = self
            .store
            .append_in_tx(tx, &EntryBatch::single(entry))
            .await?;
        let r = recorded
            .into_iter()
            .next()
            .ok_or_else(|| WalletError::InvalidInput("empty append result".into()))?;
        Ok(Receipt {
            entry_id: r.id,
            log_index: r.index.map(|i| i.get()),
            content_hash: r.content_hash,
            is_new: r.is_new,
        })
    }

    /// The newest gateway requests, newest first, at most `limit`.
    ///
    /// What the dashboard's requests page lists (issue #55): each settled turn's request
    /// id, model, pricing kind, token counts, charge in minor units, booking date and the
    /// key that paid it. The facts come out of the settlement entry's own description —
    /// the record [`Settlement::description`] writes, covered by the entry's content hash
    /// — so the page cannot show a number that the bill does not prove, and the kind is
    /// read back as a [`SettlementKind`] rather than re-derived from the charge.
    ///
    /// A request appears once it has settled: a turn still in flight has no entry yet, and
    /// the overview is where its hold is shown live. A settlement the wallet API wrote by
    /// hand carries no record, and a hold or a top-up is not a request, so those entries
    /// are skipped.
    ///
    /// The read walks back one page at a time and stops as soon as it has `limit`
    /// requests, so a log full of top-ups costs no more than the entries above the last
    /// request it returns.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_requests(&self, limit: usize) -> Result<Vec<RequestEntry>, WalletError> {
        Ok(self.requests_page(None, limit).await?.rows)
    }

    /// The settled gateway requests, one page at a time (issue #93): same rows as
    /// [`recent_requests`](Self::recent_requests), which reads the first page.
    /// `before` bounds the page to entries with a smaller log index and the answer's
    /// `next_cursor` resumes there.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn requests_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<RequestEntry>, WalletError> {
        self.scoped_requests_page(
            before,
            limit,
            &crate::CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            },
        )
        .await
    }

    /// Apply immutable record attribution before selecting a page of rows.
    pub async fn scoped_requests_page(
        &self,
        before: Option<u64>,
        limit: usize,
        scope: &crate::CustomerRecordScope,
    ) -> Result<ListPage<RequestEntry>, WalletError> {
        Ok(self
            .scoped_settlements_page(before, limit, scope)
            .await?
            .map(|turn| RequestEntry {
                booked_on: turn.booked_on,
                request_id: turn.record.request,
                entry_id: turn.entry_id,
                model: turn.record.model,
                kind: turn.record.kind,
                input_tokens: turn.record.input_tokens,
                output_tokens: turn.record.output_tokens,
                charged_minor: turn.record.charged,
                discount_percent: turn.record.discount_percent,
                upstream_attempts: turn.record.upstream_attempts,
                key_id: turn.key_id,
                status: turn.record.kind.status(),
                failure_reason: turn.record.reason,
            }))
    }

    /// Each settled gateway turn's own [`SettlementRecord`], newest first, with its
    /// booking date and the key that paid it: the platform admin's anomalies list and
    /// the dashboard's requests page both read this (issue #57).
    ///
    /// Same contract as [`recent_requests`](Self::recent_requests): only entries whose
    /// description is a settlement record count, and the walk stops once `limit` turns
    /// are found.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn recent_settlements(&self, limit: usize) -> Result<Vec<SettledTurn>, WalletError> {
        Ok(self.settlements_page(None, limit).await?.rows)
    }

    /// The settled gateway turns' own records, one page at a time (issue #93): same
    /// rows as [`recent_settlements`](Self::recent_settlements), which reads the first
    /// page.
    ///
    /// # Errors
    ///
    /// Storage failures surface as [`WalletError`].
    pub async fn settlements_page(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<ListPage<SettledTurn>, WalletError> {
        self.scoped_settlements_page(
            before,
            limit,
            &crate::CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            },
        )
        .await
    }

    /// Apply immutable record attribution before selecting a page of rows.
    pub async fn scoped_settlements_page(
        &self,
        before: Option<u64>,
        limit: usize,
        scope: &crate::CustomerRecordScope,
    ) -> Result<ListPage<SettledTurn>, WalletError> {
        self.scan_back(before, limit.clamp(1, 100), REQUESTS_SCAN, &mut |stored| {
            if !scope.allows_entry(&stored.entry) {
                return None;
            }
            let record = SettlementRecord::parse(stored.entry.description().as_str())?;
            Some(SettledTurn {
                booked_on: stored.entry.booking_date(),
                entry_id: stored.entry.id(),
                record,
                key_id: stored
                    .entry
                    .provenance()
                    .actor
                    .as_ref()
                    .map(|actor| actor.as_str().to_owned()),
            })
        })
        .await
    }

    /// The walk every backward list shares: one store page at a time, the store's
    /// own log order reversed for the answer, at most `wanted` rows `keep`
    /// recognizes. `before` bounds the walk to indices below it — `None` reads the
    /// head; the resume point is the smallest index the walk left unvisited,
    /// `None` when index 0 was scanned (issue #93).
    async fn scan_back<T>(
        &self,
        before: Option<u64>,
        wanted: usize,
        window: usize,
        keep: &mut impl FnMut(&StoredEntry<SCALE>) -> Option<T>,
    ) -> Result<ListPage<T>, WalletError> {
        let size = self.store.head().await?.size;
        let mut end = before.unwrap_or(size).min(size);
        let mut rows = Vec::new();
        while end > 0 && rows.len() < wanted {
            let start = end.saturating_sub(window as u64);
            let after = start.checked_sub(1).map(LogIndex::new);
            let page = self
                .store
                .page(Cursor {
                    after,
                    limit: (end - start) as usize,
                })
                .await?;
            let mut visited = start;
            // The store pages in log order; the page wants newest first.
            for stored in page.records.iter().rev() {
                if let Some(row) = keep(stored) {
                    rows.push(row);
                    if rows.len() == wanted {
                        // Unvisited indices are now [0, stored.index): resume below
                        // the row that filled the page, so no entry is skipped.
                        visited = stored.require_index().map_err(invalid)?.get();
                        break;
                    }
                }
            }
            end = visited;
        }
        Ok(ListPage {
            rows,
            next_cursor: (end > 0).then_some(end),
        })
    }
}

/// A settled gateway turn as the ledger recorded it, newest first: the settlement's
/// own record, its booking date and the key that paid it.
#[derive(Debug, Clone)]
pub struct SettledTurn {
    /// The booking date: the server's UTC date when the settlement was written.
    pub booked_on: Date,
    /// The settlement entry's own id (issue #236): the id `RequestEntry` and any
    /// proof lookup name, straight off the stored record rather than derived.
    pub entry_id: EntryId,
    /// What the turn charged, in the settlement record's own fields — channel, price
    /// version and freeze included, so the anomalies page can say where the platform
    /// lost money upstream.
    pub record: SettlementRecord,
    /// The API key that paid the turn, as its id in uuid simple form: the hold's
    /// provenance actor, copied onto the settlement.
    pub key_id: Option<String>,
}

/// Lowercase hex of a byte slice, the spelling the signed-link key fields use.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The handle of the account registered at `path`, or an error naming it.
///
/// A ledger whose accounts are not the three this module registers is not a wallet;
/// saying which one is missing beats a panic or a silent default.
fn find_account(registry: &AccountRegistry, path: &str) -> Result<AccountId, WalletError> {
    registry
        .records()
        .into_iter()
        .find(|r| r.account.path.to_string() == path)
        .map(|r| r.id)
        .ok_or_else(|| WalletError::InvalidInput(format!("missing account {path}")))
}

/// Tenant ids end up inside schema names, so only lowercase letters, digits and underscores are allowed, 1 to 40 chars.
fn validate_tenant_id(tenant_id: &str) -> Result<(), WalletError> {
    let ok = !tenant_id.is_empty()
        && tenant_id.len() <= 40
        && tenant_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(WalletError::InvalidInput(format!(
            "bad tenant id {tenant_id:?}"
        )))
    }
}

fn currency() -> Currency {
    Currency::USD
}

fn positive(minor: i64) -> Result<Credits, WalletError> {
    if minor <= 0 {
        return Err(WalletError::InvalidInput("amount must be positive".into()));
    }
    Ok(Credits::from_minor(minor))
}

fn idem(key: &str) -> Result<IdempotencyKey, WalletError> {
    IdempotencyKey::new(key.as_bytes().to_vec()).map_err(invalid)
}

/// The entry's description, validated by the ledger: it caps the length and refuses control
/// characters, so a caller cannot smuggle either into a stored entry.
fn description_of(text: &str) -> Result<Description, WalletError> {
    Description::new(text.to_owned()).map_err(invalid)
}

/// The receipt of an entry already committed — what a replayed write answers.
fn stored_receipt(stored: &StoredEntry<SCALE>) -> Receipt {
    Receipt {
        entry_id: stored.entry.id(),
        log_index: stored.require_index().map(|index| index.get()).ok(),
        content_hash: stored.content_hash,
        is_new: false,
    }
}

/// Persists every account record the registry knows — `register_account`
/// upserts master data, so a record the store already holds is a no-op.
async fn persist_accounts(
    store: &PostgresStore<SCALE>,
    registry: &AccountRegistry,
) -> Result<(), WalletError> {
    for record in registry.records() {
        store.register_account(&record).await?;
    }
    Ok(())
}

/// Whether the storage error is a unique-violation — the losing write of a
/// raced account bootstrap.
fn is_unique_violation(error: &WalletError) -> bool {
    match error {
        WalletError::Storage(PostgresError::Database(sqlx::Error::Database(e))) => {
            e.code().as_deref() == Some("23505")
        }
        _ => false,
    }
}

/// Whether the storage error is a deadlock (`40P01`) — the open-path race
/// between this open's lazy-migration appends and a racing open's schema DDL.
/// Retrying is safe: both migrations are marker-keyed, so a second pass is a
/// no-op where the winner's work already landed.
fn is_deadlock(error: &WalletError) -> bool {
    match error {
        WalletError::Storage(PostgresError::Database(sqlx::Error::Database(e))) => {
            e.code().as_deref() == Some("40P01")
        }
        _ => false,
    }
}

/// Derives the EntryId deterministically from the idempotency key, so a retry always lands on the
/// same entry.
///
/// Public because the derivation is part of the interface: a gateway hold is taken under
/// `req-<id>:hold`, and a caller holding a request id can name that entry without asking the
/// server (docs/api.md).
#[must_use]
pub fn entry_id_for(key: &str) -> EntryId {
    EntryId::from_uuid(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        key.as_bytes(),
    ))
}

/// The idempotency key of the settlement that releases the hold taken under `hold_key`.
///
/// One hold settles at most once, so the hold's key is the settlement's idempotency key in
/// derived form: retrying the same settlement replays it, and settling the same hold twice is
/// refused by the ledger's idempotency gate, inside the append — which is what makes the
/// pairing hold under concurrency. The derivation hashes the hold key because a hold key may
/// already sit at the engine's 128-byte idempotency limit, where a readable suffix would
/// overflow it.
///
/// Public because the derivation is part of the interface: the settlement entry's id is
/// [`entry_id_for`] of this key, so a caller holding a hold key can name the settlement entry
/// without asking the server (docs/api.md).
#[must_use]
pub fn settlement_key_for(hold_key: &str) -> String {
    format!("settle:{}", entry_id_for(hold_key).as_uuid().as_simple())
}

/// The advisory-lock key serializing one key's limit check with its appends.
///
/// A stable hash of the tenant id and the key id: every process holding the same key
/// computes the same lock, and a key of one tenant can never collide with a key of
/// another. The domain prefix keeps it out of the engine's lock namespace.
/// The advisory-lock namespace serializing hold admission against a suspension
/// flip (issue #162): [`Wallet::hold_for_key`] takes it shared and
/// `Db::set_suspended` takes it exclusive, both keyed by organization id, so a
/// suspended flag that commits is the flag the next hold reads.
pub(crate) const ORG_ADMISSION_LOCK: &str = "oxsum/org-admission/v1:";

/// The advisory-lock key `hold_for_key` serializes one key's holds on. Public so
/// a test can hold the same lock the wallet waits on — issue #486's pool-fairness
/// check pins a queued hold's connection use by blocking this lock first.
#[must_use]
pub fn limit_lock_key(tenant_id: &str, key_id: &Uuid) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(b"oxsum/key-spend-limit/v1\0");
    hasher.update(tenant_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(key_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    i64::from_le_bytes(bytes)
}

/// The advisory-lock key serializing one tenant's credit-line writes: a limit
/// change reads the committed figure, and a repaying top-up reads the drawn one,
/// so both serialize on this lock — a stale read under it can under-count, never
/// grant past what was committed. Draws and releases stay off the lock: they
/// only ever move the line's balance down, the safe direction for both readers.
fn credit_lock_key(tenant_id: &str) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(b"oxsum/credit-limit/v1\0");
    hasher.update(tenant_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    i64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::validate_tenant_id;

    #[test]
    fn tenant_id_rules() {
        assert!(validate_tenant_id("acme_01").is_ok());
        assert!(validate_tenant_id("").is_err());
        assert!(validate_tenant_id("Acme").is_err());
        assert!(validate_tenant_id("a-b").is_err());
        assert!(validate_tenant_id("x\"; DROP SCHEMA").is_err());
        assert!(validate_tenant_id(&"a".repeat(41)).is_err());
    }
}

//! The wallet facade: holds, settlements, receipts and ledger reads.
//!
//! The [`Wallet`] struct, its account handles, `open`/`bootstrap` and the shared
//! read/write helpers stay in this file; the method families live in the `wallet/`
//! leaf modules beside it, split by domain (admission, funding_preview,
//! transaction_reads, holds, keys, settle, balances, credit, promo, plan, anchors,
//! reads). Every leaf is an `impl Wallet` block in a child module, so it sees the
//! struct's private fields; items only a leaf calls stay `pub(super)`.

#[path = "wallet/admission.rs"]
mod admission;
#[path = "wallet/anchors.rs"]
mod anchors;
#[path = "wallet/balances.rs"]
mod balances;
#[path = "wallet/credit.rs"]
mod credit;
#[path = "wallet/funding_preview.rs"]
mod funding_preview;
#[path = "wallet/holds.rs"]
mod holds;
#[path = "wallet/keys.rs"]
mod keys;
#[path = "wallet/plan.rs"]
mod plan;
#[path = "wallet/promo.rs"]
mod promo;
#[path = "wallet/reads.rs"]
mod reads;
#[path = "wallet/settle.rs"]
mod settle;
#[path = "wallet/transaction_reads.rs"]
pub(super) mod transaction_reads;

pub use admission::GatewayAdmission;
pub use funding_preview::GatewayFundingPreview;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use doubleentry::account::AccountRegistry;
use doubleentry::storage::postgres::{PostgresError, PostgresStore};
use doubleentry::{
    AccountId, Amount, BalanceLimit, Balanced, Currency, Description, Direction, Draft, Entry,
    EntryBatch, EntryId, Hash, IdempotencyKey, Layer, LedgerId, LedgerPolicy, SealContext,
    StoredEntry,
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::Date;
use time::macros::date;
use uuid::Uuid;

use crate::billing::{SettlementKind, SettlementRecord};
use doubleentry::LedgerStore;

use crate::error::{WalletError, invalid};

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
    /// Long v15 failure reason identity, exclusive with inline `failure_reason`.
    /// This projection does not verify separately retained full text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason_commitment: Option<oxsum_verify::ReasonCommitment>,
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
        // Every bootstrap stage remains idempotent, so an opener aborted with
        // `40P01` may restart. Current engine revisions skip DDL entirely: an
        // opener-only retry cannot rescue an active settlement chosen as the
        // other deadlock victim. A real revision upgrade still requires drained
        // writers; the account upserts and marker-keyed lazy migrations remain.
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
        let receipt = self.append_in(&mut tx, draft).await;
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
    pub(super) fn topup_replay(
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
            let key = key.to_owned();
            let description = description_of(reason)?;
            match self
                .append(
                    Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                        .with_description(description.clone())
                        .debit(self.adjustments, amt, currency())
                        .credit(self.bonus, amt, currency()),
                )
                .await
            {
                // The key was taken between the read and the append — most often by
                // a racing copy of this same grant, whose stored entry answers the
                // retry rather than a bare conflict.
                Err(WalletError::Conflict(_)) => {
                    return match self.store.get(entry_id_for(&key)).await? {
                        Some(stored) => self.adjust_replay(&stored, &description, minor, &key),
                        None => Err(WalletError::Conflict(format!(
                            "idempotency key {key:?} is already taken"
                        ))),
                    };
                }
                other => return other,
            }
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
        let description = description_of(reason)?;
        match self
            .append(
                Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                    .with_description(description.clone())
                    .debit(self.revenue, amt, currency())
                    .credit(self.wallet, amt, currency()),
            )
            .await
        {
            Err(WalletError::Conflict(_)) => match self.store.get(entry_id_for(&key)).await? {
                Some(stored) => self.credit_note_replay(&stored, &description, minor, &key),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other,
        }
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
        let description = description_of(reason)?;
        match self
            .append(
                Entry::<Draft, SCALE>::new(entry_id_for(&key), idem(&key)?, on)
                    .with_description(description.clone())
                    .debit(self.wallet, amt, currency())
                    .credit(self.cash, amt, currency()),
            )
            .await
        {
            Err(WalletError::Conflict(_)) => match self.store.get(entry_id_for(&key)).await? {
                Some(stored) => self.reverse_deposit_replay(&stored, &description, minor, &key),
                None => Err(WalletError::Conflict(format!(
                    "idempotency key {key:?} is already taken"
                ))),
            },
            other => other,
        }
    }

    // ── plan-fee journal legs (issue #611, T20) ──────────────────────────────

    /// Answers for a key that already has a committed deduction entry: the
    /// stored entry's receipt when it drew the amount asked for, under the
    /// same reason — whatever its pool split (balances move after a
    /// deduction lands, so the committed shape is the answer, never the
    /// split a retry would compute now) — and the key-reuse conflict when
    /// the entry is anything else.
    pub(super) fn deduction_replay(
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

    /// The shared replay check for a two-leg entry: `debit` of `minor` on
    /// `debit_account`, `credit` of `minor` on `credit_account`, the reason as
    /// the description. Answers the stored receipt when the entry is exactly
    /// that, else the key-reuse conflict — the same shape `deduction_replay`
    /// uses, generalized so every crediting/debiting write replays its own key.
    pub(super) fn two_leg_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        debit_account: AccountId,
        credit_account: AccountId,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        let leg = |account: AccountId, direction: Direction| -> i64 {
            stored
                .entry
                .postings()
                .iter()
                .filter(|p| {
                    p.account == account && p.layer == Layer::Settled && p.direction == direction
                })
                .map(|p| p.amount.to_minor())
                .sum()
        };
        if leg(debit_account, Direction::Debit) == minor
            && leg(credit_account, Direction::Credit) == minor
            && stored.entry.description().as_str() == description.as_str()
        {
            Ok(stored_receipt(stored))
        } else {
            Err(WalletError::Conflict(format!(
                "idempotency key {key:?} is already taken"
            )))
        }
    }

    /// Replay for a positive `adjust` (a grant): `debit Adjustments / credit
    /// Bonus`. The negative `adjust` arm goes through `deduction_replay`.
    pub(super) fn adjust_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        self.two_leg_replay(
            stored,
            self.adjustments,
            self.bonus,
            description,
            minor,
            key,
        )
    }

    /// Replay for a `credit_note`: `debit Revenue / credit Wallet`.
    pub(super) fn credit_note_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        self.two_leg_replay(stored, self.revenue, self.wallet, description, minor, key)
    }

    /// Replay for a `reverse_deposit`: `debit Wallet / credit Cash`.
    pub(super) fn reverse_deposit_replay(
        &self,
        stored: &StoredEntry<SCALE>,
        description: &Description,
        minor: i64,
        key: &str,
    ) -> Result<Receipt, WalletError> {
        self.two_leg_replay(stored, self.wallet, self.cash, description, minor, key)
    }

    /// Seals a draft against the ledger's *current* calendar — read back from
    /// storage on every write, not held from `open`, so a month the operator closes
    /// stops new postings dated into it on the very next call rather than after a
    /// restart. This is where "a sealed month accepts no new entries" is enforced:
    /// `Entry::seal` refuses a booking date the calendar's sealed watermark covers.
    pub(super) async fn seal(
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

    pub(super) async fn append(&self, draft: Entry<Draft, SCALE>) -> Result<Receipt, WalletError> {
        let entry = self.seal(draft).await?;
        self.append_sealed(entry).await
    }

    pub(super) async fn append_sealed(
        &self,
        entry: Entry<Balanced, SCALE>,
    ) -> Result<Receipt, WalletError> {
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
    /// the draft seals against the calendar read on the same connection,
    /// then rides the borrowed-transaction append, so the
    /// entry commits with whatever else the caller's transaction carries.
    /// The caller owns the commit; a rolled-back transaction leaves no entry,
    /// no node and no idempotency claim.
    pub(super) async fn append_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        draft: Entry<Draft, SCALE>,
    ) -> Result<Receipt, WalletError> {
        let entry = self.seal_in(tx, draft).await?;
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
pub(super) fn validate_tenant_id(tenant_id: &str) -> Result<(), WalletError> {
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

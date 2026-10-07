use doubleentry::account::AccountRegistry;
use doubleentry::storage::postgres::{PostgresError, PostgresStore};
use doubleentry::{
    AccountId, Amount, BalanceKey, BalanceLimit, BalanceQuery, Balanced, Currency, Cursor,
    Description, Direction, Draft, Entry, EntryBatch, EntryId, Hash, IdempotencyKey, Layer,
    LedgerId, LedgerPolicy, LedgerStore, LogIndex, Period, PeriodId, PeriodState, Posting,
    Provenance, Seal, SealContext, StoredEntry,
};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::Date;
use time::OffsetDateTime;
use time::macros::date;
use uuid::Uuid;

use crate::billing::{SettlementKind, SettlementRecord};
use crate::error::{WalletError, invalid};
use crate::heads::{Consistency, HeadSigningKey, SignedHead, origin_for, sign_head};
use crate::keys::{ActingKey, BudgetDuration};
use crate::proof::ProofBundle;

/// Money precision: 6 decimal places; 1 credit = 1_000_000 minor, fine enough for per-token pricing.
///
/// Defined in `oxsum-verify`, the crate the browser page shares: the entry encoding —
/// and therefore the content hash — depends on it, so writer and verifier use the one
/// definition.
pub use oxsum_verify::SCALE;
pub type Credits = Amount<SCALE>;

/// Account opening date. Every posting date must not be earlier than this.
const OPENED: Date = date!(2026 - 01 - 01);

const WALLET: &str = "Liabilities:Wallet";
/// Granted credit — the signup bonus and admin grants — kept in its own pool so
/// settlement can draw it before the purchased balance (decision D1, see
/// docs/decisions.md). Equity, because the platform contributes it and no cash
/// ever stood behind it.
const BONUS: &str = "Equity:Bonus";
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
const CREDIT_LINE: &str = "Liabilities:CreditLine";
/// The credit facility committed to the organization, kept gross: the account's
/// debit balance is the granted limit, and a shrink debits `CreditLine` back
/// against it, so the ledger itself refuses a limit cut below the outstanding
/// draw. A draw never touches it — it moves between `CreditLine` and
/// `Revenue` — so what the organization owes is the limit minus the line's
/// balance, not a separate counter that could drift.
const FACILITY: &str = "Assets:CreditFacility";

/// The idempotency key under which the one-time pool migration posts its
/// `debit Wallet, credit Bonus` entry. Deterministic, so a second
/// reclassification run sees the entry and does nothing, and the key doubles as
/// the marker spend reads exclude: the move is a rebalancing between pools, not
/// a charge, so it must not count as settled wallet spend.
const RECLASS_KEY: &str = "pool-reclassification";

/// How many times a pool-splitting write recomputes its split before giving up.
/// Each retry re-reads pool balances, so a loser only loops while racing appends
/// keep moving the pools under it; a write the combined balance genuinely cannot
/// cover fails every attempt anyway and surfaces the same refusal as before.
const POOL_SPLIT_ATTEMPTS: usize = 3;

/// How many times `open` re-reads the account table after a racing opener
/// committed it mid-bootstrap. The winner's write is one statement batch, so a
/// loser only loops while racers keep landing in between.
const OPEN_RACE_ATTEMPTS: usize = 3;

/// How one amount divides across the funded pools — bonus, then purchased
/// wallet, then the credit line. Any side may be zero; the parts always sum to
/// the amount the split was computed for.
#[derive(Clone, Copy)]
struct PoolSplit {
    bonus: i64,
    wallet: i64,
    credit: i64,
}

impl PoolSplit {
    fn total(&self) -> i64 {
        self.bonus + self.wallet + self.credit
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
/// Seven fixed accounts:
/// - wallet: the purchased balance owed to the user, a liability. Carries
///   `FundedReservations`: no overdraft, and no release beyond what was reserved.
/// - bonus: granted credit, equity. Carries the same limit — a pool is what a
///   hold reserves against and a settlement draws down, so the same invariants
///   apply on both sides of the split.
/// - credit line: the drawable part of the operator-granted credit facility, a
///   liability under the same limit again. Holds and settlements draw it only
///   after bonus and purchased funds exhaust; the organization owes limit minus
///   its balance.
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
    credit_line: AccountId,
    facility: AccountId,
    cash: AccountId,
    revenue: AccountId,
    /// Operator grants and deductions: [`Self::adjust`] posts the other side of the
    /// pools here.
    adjustments: AccountId,
    policy: LedgerPolicy,
    /// The validated tenant id this ledger belongs to: the identity its signed tree heads
    /// are published under (`oxsum/ledgers/<tenant_id>`).
    tenant_id: String,
}

/// Receipt of one posting. `content_hash` is what the user keeps to verify the bill later.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub entry_id: EntryId,
    pub log_index: Option<u64>,
    pub content_hash: Hash,
    /// false means an idempotent replay: nothing new was written.
    pub is_new: bool,
}

/// One ledger entry as the dashboard's transaction log shows it.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Position in the log.
    pub index: u64,
    pub id: String,
    /// What the writer recorded: holds and settlements describe the request.
    pub description: String,
    /// The content hash the Merkle log commits to.
    pub content_hash: String,
}

/// One settled entry as the bills page lists it and its exports carry it.
#[derive(Debug, Clone)]
pub struct SettledEntry {
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

/// One gateway request as the dashboard's requests page lists it, read from the ledger's
/// settlement entries: what the turn was, how it was priced, what it used, what it charged
/// and which key paid it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestEntry {
    /// The booking date: the server's UTC date when the settlement was written. The ledger
    /// records no time of day.
    pub booked_on: Date,
    /// The request id from `x-oxsum-request-id`; the ledger keys are derived from it.
    pub request_id: String,
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
    /// The API key that paid the turn, as its id in uuid simple form: the hold's provenance
    /// actor, copied onto the settlement. `None` for a turn held without key attribution.
    pub key_id: Option<String>,
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
        store.migrate().await?;

        // Account handles must be restored from storage on restart. Re-registering by
        // path could mint different handle numbers and mispoint historical entries.
        // The seven the wallet needs are registered wherever the store lacks them —
        // a second opener can read while the first is still mid-bootstrap, and a read
        // that finds some of them has to converge, not fail on the half it can see.
        let mut attempts = OPEN_RACE_ATTEMPTS;
        let registry = loop {
            let stored = store.accounts().await?;
            let mut registry = AccountRegistry::from_records(stored).map_err(invalid)?;
            for path in [
                WALLET,
                BONUS,
                CREDIT_LINE,
                FACILITY,
                CASH,
                REVENUE,
                ADJUSTMENTS,
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
            let credit_line = find_account(&registry, CREDIT_LINE)?;
            let facility = find_account(&registry, FACILITY)?;
            let weakened = registry.records().iter().any(|r| {
                ((r.id == wallet || r.id == bonus || r.id == credit_line)
                    && r.account.limit != WALLET_LIMIT)
                    || (r.id == facility && r.account.limit != FACILITY_LIMIT)
            });
            if weakened {
                for account in [wallet, bonus, credit_line] {
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
            match persist_accounts(&store, &registry).await {
                Ok(()) => break registry,
                Err(error) if attempts > 0 && is_unique_violation(&error) => {
                    attempts -= 1;
                }
                Err(error) => return Err(error),
            }
        };

        let wallet = Self {
            wallet: find_account(&registry, WALLET)?,
            bonus: find_account(&registry, BONUS)?,
            credit_line: find_account(&registry, CREDIT_LINE)?,
            facility: find_account(&registry, FACILITY)?,
            cash: find_account(&registry, CASH)?,
            revenue: find_account(&registry, REVENUE)?,
            adjustments: find_account(&registry, ADJUSTMENTS)?,
            store,
            registry,

            policy: LedgerPolicy::default(),
            tenant_id: tenant_id.to_owned(),
        };
        // Ledgers opened for the first time under the two-pool model move their
        // unspent historical grants into Bonus here — once per ledger, marked by
        // the entry's fixed idempotency key. Doing it at open keeps the migration
        // lazy: a tenant pays for its own reclassification the first time it is
        // used under the new code rather than every `Db::migrate` paying for all
        // of them under the global lock.
        wallet.reclassify_grants().await?;
        Ok(wallet)
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
    /// cover it.
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
            return self.hold_replay(&stored, &description, None, minor, key);
        }
        let mut last_err = None;
        for _ in 0..POOL_SPLIT_ATTEMPTS {
            // The split's read borrows a connection only for itself: holding one
            // while the append waits for another would starve the pool under
            // concurrent holds.
            let split = {
                let mut conn = self.store.pool().acquire().await?;
                self.pool_split(&mut conn, minor).await?
            };
            let receipt = self
                .append(self.hold_entry(key, &description, split, None, on)?)
                .await;
            match receipt {
                Err(error @ WalletError::InsufficientFunds) => last_err = Some(error),
                // The key was taken between our read and the append — by a
                // racing copy of this same request or by a different one.
                Err(WalletError::Conflict(_)) => {
                    return match self.store.get(entry_id).await? {
                        Some(stored) => self.hold_replay(&stored, &description, None, minor, key),
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

    /// The split of `minor` across the pools, own funds first: the bonus pool
    /// funds what it can, the purchased wallet carries what it can, and the
    /// credit line picks up the rest. `split.total() == minor` always; any side
    /// may be zero, and `credit` above what the line still carries is what the
    /// append's `FundedReservations` check refuses.
    async fn pool_split(
        &self,
        conn: &mut sqlx::PgConnection,
        minor: i64,
    ) -> Result<PoolSplit, WalletError> {
        let bonus = minor.min(self.pool_available_in(conn, self.bonus).await?.max(0));
        let wallet = (minor - bonus).min(self.pool_available_in(conn, self.wallet).await?.max(0));
        Ok(PoolSplit {
            bonus,
            wallet,
            credit: minor - bonus - wallet,
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
    pub async fn hold_for_key(
        &self,
        key: &ActingKey,
        model: Option<&str>,
        idem_key: &str,
        description: &str,
        minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        positive(minor)?;
        let description = description_of(description)?;
        let actor = Provenance::none()
            .with_actor(&key.key_id.as_simple().to_string())
            .map_err(invalid)?;
        let entry_id = entry_id_for(idem_key);
        // A committed entry under the key is this request's answer before any
        // constraint is consulted: a replayed hold gets its own receipt even
        // when a budget or an allowlist has since moved — its split need not
        // match either, because the balances it was computed against moved too.
        if let Some(stored) = self.store.get(entry_id).await? {
            return self.hold_replay(&stored, &description, Some(&actor), minor, idem_key);
        }
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
            // The constraints in force now, not the ones the request authenticated with:
            // locked, so a PATCH landing between authentication and this hold cannot be missed.
            let constraints = sqlx::query(
                "SELECT spend_limit_minor, budget_duration, model_allowlist, \
                        max_concurrent_holds \
                 FROM oxsum.api_keys WHERE key_id = $1 FOR UPDATE",
            )
            .bind(key.key_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(constraints) = constraints else {
                // Keys are never deleted; a missing row means the credential died mid-request.
                return Err(WalletError::Unauthenticated);
            };
            let split = self.pool_split(&mut tx, minor).await?;
            let entry = self
                .seal(self.hold_entry(idem_key, &description, split, Some(actor.clone()), on)?)
                .await?;
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
            }
            match self.append_sealed(entry).await {
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

    /// What one API key has committed: settled charges plus outstanding holds attributed
    /// to it, in minor units. Read from the ledger's own postings — the pending release
    /// and the settled charge of a settlement both carry the hold's actor — so it cannot
    /// drift from the books.
    pub async fn key_committed(&self, key_id: &Uuid) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        self.key_committed_in(&mut conn, key_id, None).await
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
        if actual_minor < 0 {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let (bonus_held, wallet_held, credit_held, outstanding_actor) =
            self.outstanding_hold(hold_key).await?;
        if actual_minor > bonus_held + wallet_held + credit_held {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let held = Credits::from_minor(bonus_held + wallet_held + credit_held);
        let settle_key = settlement_key_for(hold_key);
        let mut draft =
            Entry::<Draft, SCALE>::new(entry_id_for(&settle_key), idem(&settle_key)?, on)
                .with_description(description_of(description)?);
        // The settlement attributes to the key the hold attributed to: the release side and
        // the settled charge side both count toward that key's committed spend, with no
        // caller input to get wrong. A hold taken before key attribution existed settles
        // unattributed, like it was held.
        if let Some(actor) = outstanding_actor {
            draft = draft.with_provenance(Provenance::none().with_actor(&actor).map_err(invalid)?);
        }
        // The release returns exactly what the hold reserved in each pool.
        for (account, held_part) in [
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
            // The charge draws the bonus pool first, then the purchased wallet, then
            // the credit line — never more than this hold reserved in each, because
            // taking another hold's reservation is exactly what the per-pool split
            // exists to prevent.
            let bonus_take = actual_minor.min(bonus_held);
            let wallet_take = (actual_minor - bonus_take).min(wallet_held);
            let credit_take = actual_minor - bonus_take - wallet_take;
            for (account, take) in [
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
        match self.append(draft).await {
            // The settlement's idempotency key is derived from the hold's key, so a key
            // conflict here means exactly one thing: the hold was already settled.
            Err(WalletError::Conflict(_)) => {
                Err(WalletError::Conflict("hold already settled".into()))
            }
            other => other,
        }
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
    /// pool reclassification is excluded by its idempotency key — moving money
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
               AND e.idempotency_key <> $3 \
               AND NOT EXISTS (SELECT 1 FROM ledger_{schema}.postings f \
                               WHERE f.entry_id = p.entry_id AND f.account_index = $4)",
            schema = self.tenant_id,
        );
        let spend: i64 = sqlx::query_scalar(&sql)
            .bind(vec![
                self.wallet.index() as i32,
                self.bonus.index() as i32,
                self.credit_line.index() as i32,
            ])
            .bind(from)
            .bind(RECLASS_KEY.as_bytes())
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
    /// refused at seal time (see [`Wallet::seal`]), which is product.md's "after
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

    /// What the hold taken under `hold_key` reserved in each pool — the bonus
    /// part, the wallet part, then the credit-line part — plus the provenance
    /// actor the hold attributed to: the API key whose spend it counts toward,
    /// if the hold named one.
    ///
    /// The hold entry is found by the id it was given when the hold was taken
    /// ([`entry_id_for`] of the key), and the amounts are the entry's pending-layer
    /// debits on the pool accounts — the ledger's own record of what was reserved,
    /// not a caller assertion. [`WalletError::HoldNotFound`] when no entry is stored
    /// under the key, or when the entry is not a hold.
    async fn outstanding_hold(
        &self,
        hold_key: &str,
    ) -> Result<(i64, i64, i64, Option<String>), WalletError> {
        let Some(stored) = self.store.get(entry_id_for(hold_key)).await? else {
            return Err(WalletError::HoldNotFound(format!(
                "no hold under key {hold_key:?}"
            )));
        };
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
            if p.account == self.bonus {
                bonus_held += signed;
            } else if p.account == self.wallet {
                wallet_held += signed;
            } else if p.account == self.credit_line {
                credit_held += signed;
            }
        }
        if bonus_held + wallet_held + credit_held <= 0 {
            return Err(WalletError::HoldNotFound(format!(
                "key {hold_key:?} does not name a hold"
            )));
        }
        let actor = stored
            .entry
            .provenance()
            .actor
            .as_ref()
            .map(|actor| actor.as_str().to_owned());
        Ok((bonus_held, wallet_held, credit_held, actor))
    }

    /// The one-time pool migration (decision D1): moves the unspent remainder of
    /// historical grants out of `Liabilities:Wallet` into `Equity:Bonus` with a
    /// single `debit Wallet, credit Bonus` entry under [`RECLASS_KEY`].
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
    /// posting, so [`classify`](Self::classify) has no kind for it).
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

    /// Builds the proof bundle for one entry. Returns None when the entry does not exist.
    pub async fn receipt_proof(
        &self,
        entry_id: EntryId,
    ) -> Result<Option<ProofBundle>, WalletError> {
        let Some(stored) = self.store.get(entry_id).await? else {
            return Ok(None);
        };
        let index = stored.require_index().map_err(invalid)?;
        let head = self.store.head().await?;
        let proof = self.store.prove_inclusion(index).await?;
        Ok(Some(ProofBundle {
            entry: stored.entry,
            head,
            proof,
        }))
    }

    /// Number of entries in the log. With isolated ledgers, each tenant counts from zero.
    pub async fn log_size(&self) -> Result<u64, WalletError> {
        Ok(self.store.head().await?.size)
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
        self.scan_back(before, limit.clamp(1, 100), 100, &mut |stored| {
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
        self.scan_back(before, limit.clamp(1, 100), BILLS_SCAN, &mut |stored| {
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

    /// The three pools' combined balance in one layer. All three are
    /// credit-normal — the wallet and the credit line are liabilities, the bonus
    /// pool equity — so a credit balance is the positive side on each.
    async fn pool_net(&self, layer: Layer) -> Result<i64, WalletError> {
        Ok(self.account_net(self.wallet, layer).await?
            + self.account_net(self.bonus, layer).await?
            + self.account_net(self.credit_line, layer).await?)
    }

    /// Whether the account is one a hold reserves against and a settlement draws:
    /// the bonus pool, the purchased wallet, or the credit line. The facility
    /// account is not a pool — nothing ever spends out of it.
    fn is_pool(&self, account: AccountId) -> bool {
        account == self.wallet || account == self.bonus || account == self.credit_line
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
        Ok(self
            .settlements_page(before, limit)
            .await?
            .map(|turn| RequestEntry {
                booked_on: turn.booked_on,
                request_id: turn.record.request,
                model: turn.record.model,
                kind: turn.record.kind,
                input_tokens: turn.record.input_tokens,
                output_tokens: turn.record.output_tokens,
                charged_minor: turn.record.charged,
                key_id: turn.key_id,
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
        self.scan_back(before, limit.clamp(1, 100), REQUESTS_SCAN, &mut |stored| {
            let record = SettlementRecord::parse(stored.entry.description().as_str())?;
            Some(SettledTurn {
                booked_on: stored.entry.booking_date(),
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
    /// What the turn charged, in the settlement record's own fields — channel, price
    /// version and freeze included, so the anomalies page can say where the platform
    /// lost money upstream.
    pub record: SettlementRecord,
    /// The API key that paid the turn, as its id in uuid simple form: the hold's
    /// provenance actor, copied onto the settlement.
    pub key_id: Option<String>,
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
fn limit_lock_key(tenant_id: &str, key_id: &Uuid) -> i64 {
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

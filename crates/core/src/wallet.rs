use doubleentry::account::AccountRegistry;
use doubleentry::storage::postgres::PostgresStore;
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

/// One tenant's wallet ledger.
///
/// Five fixed accounts:
/// - wallet: the purchased balance owed to the user, a liability. Carries
///   `FundedReservations`: no overdraft, and no release beyond what was reserved.
/// - bonus: granted credit, equity. Carries the same limit — a pool is what a
///   hold reserves against and a settlement draws down, so the same invariants
///   apply on both sides of the split.
/// - cash: money received from top-ups.
/// - revenue: income recognized on settlement.
/// - adjustments: operator grants and deductions — the signup bonus and platform-admin
///   adjustments — booked against the bonus pool with a reason.
pub struct Wallet {
    store: PostgresStore<SCALE>,
    registry: AccountRegistry,
    wallet: AccountId,
    bonus: AccountId,
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
#[derive(Debug, Clone)]
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
        // The three the wallet needs are registered wherever the store lacks them —
        // a second opener can read while the first is still mid-bootstrap, and a read
        // that finds some of them has to converge, not fail on the half it can see.
        let stored = store.accounts().await?;
        let mut registry = AccountRegistry::from_records(stored).map_err(invalid)?;
        for path in [WALLET, BONUS, CASH, REVENUE, ADJUSTMENTS] {
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
        let weakened = registry
            .records()
            .iter()
            .any(|r| (r.id == wallet || r.id == bonus) && r.account.limit != WALLET_LIMIT);
        if weakened {
            for account in [wallet, bonus] {
                registry.set_limit(account, WALLET_LIMIT).map_err(invalid)?;
            }
        }
        // The full account set is persisted on every open. `register_account` upserts —
        // a record the store already holds is a no-op — and two opens racing a fresh
        // ledger mint the same handles for the same missing paths, so they write the
        // same rows and converge instead of one failing on a half-written bootstrap.
        for record in registry.records() {
            store.register_account(&record).await?;
        }

        let wallet = Self {
            wallet,
            bonus,
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
    pub async fn top_up(&self, key: &str, minor: i64, on: Date) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        self.append(
            Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
                .debit(self.cash, amt, currency())
                .credit(self.wallet, amt, currency()),
        )
        .await
    }

    /// Adjustment: a signed amount the operator books against the pools, with a
    /// `reason` that becomes the entry's description — covered by its content hash,
    /// so the reason is part of the proof.
    ///
    /// A positive `minor` grants credits into the bonus pool — a grant is the
    /// platform's contribution, never cash-backed. A negative one deducts, drawing
    /// the bonus pool first: what the platform granted is what it takes back before
    /// touching money the user paid for. The pools' `FundedReservations` limit is
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
        let mut last_err = None;
        for _ in 0..POOL_SPLIT_ATTEMPTS {
            // The deduction's split is a read-then-write choice: a racing append can
            // shrink a pool between the read and the write, which the append refuses
            // — re-read and try the split that fits now.
            let bonus_take = amt.to_minor().min(self.bonus_settled().await?.max(0));
            let wallet_take = amt.to_minor() - bonus_take;
            let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
                .with_description(description_of(reason)?);
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
                other => return other,
            }
        }
        Err(last_err.unwrap_or(WalletError::InsufficientFunds))
    }

    /// Hold: reserves part of the balance in the pending layer; the settled balance is untouched.
    ///
    /// The reservation splits across the pools — the bonus pool funds what it can
    /// and the wallet carries the rest — so the pending debit each pool takes stays
    /// inside what that pool's own balance covers, which is what
    /// `FundedReservations` enforces at append. A racing hold can take bonus room
    /// between the split's read and its write; the retry re-reads and splits again
    /// rather than refusing a request the combined balance could have served.
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
        let mut last_err = None;
        for _ in 0..POOL_SPLIT_ATTEMPTS {
            // The split's read borrows a connection only for itself: holding one
            // while the append waits for another would starve the pool under
            // concurrent holds.
            let (bonus_part, wallet_part) = {
                let mut conn = self.store.pool().acquire().await?;
                self.pool_split(&mut conn, minor).await?
            };
            let receipt = self
                .append(self.hold_entry(key, description, bonus_part, wallet_part, None, on)?)
                .await;
            match receipt {
                Err(error @ WalletError::InsufficientFunds) => last_err = Some(error),
                other => return other,
            }
        }
        Err(last_err.unwrap_or(WalletError::InsufficientFunds))
    }

    /// The split of `minor` across the pools, bonus first: how much of the
    /// reservation the bonus pool funds and how much falls to the wallet.
    /// `bonus_part + wallet_part == minor` always; either side may be zero.
    async fn pool_split(
        &self,
        conn: &mut sqlx::PgConnection,
        minor: i64,
    ) -> Result<(i64, i64), WalletError> {
        let bonus_part = minor.min(self.pool_available_in(conn, self.bonus).await?.max(0));
        Ok((bonus_part, minor - bonus_part))
    }

    /// Builds the pending-layer hold entry for a computed split: a pending debit
    /// on each pool it draws and the pending credit on revenue for the whole
    /// amount, with the optional provenance actor the hold attributes to.
    fn hold_entry(
        &self,
        key: &str,
        description: &str,
        bonus_part: i64,
        wallet_part: i64,
        actor: Option<Provenance>,
        on: Date,
    ) -> Result<Entry<Draft, SCALE>, WalletError> {
        let minor = bonus_part + wallet_part;
        let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
            .with_description(description_of(description)?);
        if let Some(actor) = actor {
            draft = draft.with_provenance(actor);
        }
        if bonus_part > 0 {
            draft = draft.post(
                Posting::debit(self.bonus, Credits::from_minor(bonus_part), currency())
                    .in_layer(Layer::Pending),
            );
        }
        if wallet_part > 0 {
            draft = draft.post(
                Posting::debit(self.wallet, Credits::from_minor(wallet_part), currency())
                    .in_layer(Layer::Pending),
            );
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
        let actor = Provenance::none()
            .with_actor(&key.key_id.as_simple().to_string())
            .map_err(invalid)?;
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
                "SELECT spend_limit_minor, budget_duration, model_allowlist \
                 FROM oxsum.api_keys WHERE key_id = $1 FOR UPDATE",
            )
            .bind(key.key_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(constraints) = constraints else {
                // Keys are never deleted; a missing row means the credential died mid-request.
                return Err(WalletError::Unauthenticated);
            };
            let (bonus_part, wallet_part) = self.pool_split(&mut tx, minor).await?;
            let entry = self
                .seal(self.hold_entry(
                    idem_key,
                    description,
                    bonus_part,
                    wallet_part,
                    Some(actor.clone()),
                    on,
                )?)
                .await?;
            // An identical retry replays instead of spending again: the limit guards new spend,
            // so a replay answers before the check. A different request under a reused key falls
            // through to the engine's idempotency gate, which names the conflict — also past the
            // check, because that write never lands either.
            let existing = self.store.get(entry.id()).await?;
            if let Some(stored) = &existing
                && stored.content_hash == entry.content_hash()
            {
                tx.rollback().await?;
                return Ok(Receipt {
                    entry_id: stored.entry.id(),
                    log_index: stored.require_index().map(|index| index.get()).ok(),
                    content_hash: stored.content_hash,
                    is_new: false,
                });
            }
            if existing.is_none() {
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
            }
            match self.append_sealed(entry).await {
                Err(error @ WalletError::InsufficientFunds) => {
                    tx.rollback().await?;
                    last_err = Some(error);
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
            .bind(vec![self.wallet.index() as i32, self.bonus.index() as i32])
            .bind(key_id.as_simple().to_string())
            .bind(since)
            .fetch_one(&mut *conn)
            .await?;
        Ok(committed)
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
        let (bonus_held, wallet_held, outstanding_actor) = self.outstanding_hold(hold_key).await?;
        if actual_minor > bonus_held + wallet_held {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let held = Credits::from_minor(bonus_held + wallet_held);
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
        if bonus_held > 0 {
            draft = draft.post(
                Posting::credit(self.bonus, Credits::from_minor(bonus_held), currency())
                    .in_layer(Layer::Pending),
            );
        }
        if wallet_held > 0 {
            draft = draft.post(
                Posting::credit(self.wallet, Credits::from_minor(wallet_held), currency())
                    .in_layer(Layer::Pending),
            );
        }
        draft = draft.post(Posting::debit(self.revenue, held, currency()).in_layer(Layer::Pending));
        if actual_minor > 0 {
            // The charge draws the bonus pool first, never more than this hold
            // reserved there — taking another hold's reservation is exactly what
            // the per-pool split exists to prevent.
            let bonus_take = actual_minor.min(bonus_held);
            let wallet_take = actual_minor - bonus_take;
            if bonus_take > 0 {
                draft = draft.debit(self.bonus, Credits::from_minor(bonus_take), currency());
            }
            if wallet_take > 0 {
                draft = draft.debit(self.wallet, Credits::from_minor(wallet_take), currency());
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
    /// from different layers: a hold draws on the two together, a settlement gives back part of
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

    /// Credits the pools have been charged on or after `from`, in minor units: the
    /// settled-layer debits on the pool accounts, which is what a settlement or a
    /// deduction books when it draws the balance down.
    ///
    /// The gross debits, not the layer's net: a top-up credits the same layer, and
    /// subtracting the credits would read a month of top-ups as negative spend. Holds
    /// live in the pending layer, so an outstanding hold is not spend either, and the
    /// pool reclassification is excluded by its idempotency key — moving money between
    /// pools is not spend. A window in which nothing settled is an empty sum: zero,
    /// not an error.
    pub async fn settled_spend_since(&self, from: Date) -> Result<i64, WalletError> {
        let mut conn = self.store.pool().acquire().await?;
        let sql = format!(
            "SELECT COALESCE(SUM(p.amount_minor), 0)::bigint AS spend \
             FROM ledger_{schema}.postings p \
             JOIN ledger_{schema}.entries e ON e.entry_id = p.entry_id \
             WHERE p.account_index = ANY($1) AND p.direction = 'D' AND p.layer = 'settled' \
               AND e.log_index IS NOT NULL AND e.booking_date >= $2 \
               AND e.idempotency_key <> $3",
            schema = self.tenant_id,
        );
        let spend: i64 = sqlx::query_scalar(&sql)
            .bind(vec![self.wallet.index() as i32, self.bonus.index() as i32])
            .bind(from)
            .bind(RECLASS_KEY.as_bytes())
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

    /// What the hold taken under `hold_key` reserved in each pool — the bonus part
    /// first, then the wallet part — plus the provenance actor the hold attributed
    /// to: the API key whose spend it counts toward, if the hold named one.
    ///
    /// The hold entry is found by the id it was given when the hold was taken
    /// ([`entry_id_for`] of the key), and the amounts are the entry's pending-layer
    /// debits on the pool accounts — the ledger's own record of what was reserved,
    /// not a caller assertion. [`WalletError::HoldNotFound`] when no entry is stored
    /// under the key, or when the entry is not a hold.
    async fn outstanding_hold(
        &self,
        hold_key: &str,
    ) -> Result<(i64, i64, Option<String>), WalletError> {
        let Some(stored) = self.store.get(entry_id_for(hold_key)).await? else {
            return Err(WalletError::HoldNotFound(format!(
                "no hold under key {hold_key:?}"
            )));
        };
        let mut bonus_held = 0;
        let mut wallet_held = 0;
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
            }
        }
        if bonus_held + wallet_held <= 0 {
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
        Ok((bonus_held, wallet_held, actor))
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
        for p in stored.entry.postings() {
            if p.account == self.wallet || p.account == self.bonus {
                match (p.layer, p.direction) {
                    (Layer::Pending, Direction::Credit) => pending_credit = true,
                    (Layer::Pending, Direction::Debit) => pending_debit = true,
                    _ => {}
                }
            } else if p.account == self.cash {
                cash = true;
            } else if p.account == self.adjustments {
                adjustments = true;
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
        if adjustments {
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
            .filter(|p| {
                (p.account == self.wallet || p.account == self.bonus) && p.layer == Layer::Settled
            })
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
            (p.account == self.wallet || p.account == self.bonus)
                && p.layer == Layer::Pending
                && p.direction == Direction::Credit
        })
    }

    /// What the entry charged: the pool accounts' net debit in the settled layer, in
    /// minor units. Nothing at all for a settlement that charged nothing.
    fn settled_charge(&self, stored: &StoredEntry<SCALE>) -> i64 {
        stored
            .entry
            .postings()
            .iter()
            .filter(|p| {
                (p.account == self.wallet || p.account == self.bonus) && p.layer == Layer::Settled
            })
            .map(|p| match p.direction {
                Direction::Debit => p.amount.to_minor(),
                Direction::Credit => -p.amount.to_minor(),
            })
            .sum()
    }

    /// The two pools' combined balance in one layer. Both accounts are
    /// credit-normal — the wallet is a liability, the bonus pool equity — so a
    /// credit balance is the positive side on each.
    async fn pool_net(&self, layer: Layer) -> Result<i64, WalletError> {
        Ok(self.account_net(self.wallet, layer).await?
            + self.account_net(self.bonus, layer).await?)
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

    /// What a pool account can still fund: its settled balance minus the
    /// reservations pending holds already took. The `FundedReservations` limit
    /// keeps pending credits off a pool account, so the pending layer only ever
    /// subtracts.
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

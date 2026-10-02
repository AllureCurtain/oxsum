use doubleentry::account::AccountRegistry;
use doubleentry::storage::postgres::PostgresStore;
use doubleentry::{
    AccountId, Amount, BalanceKey, BalanceLimit, BalanceQuery, Balanced, Currency, Draft, Entry,
    EntryBatch, EntryId, Hash, IdempotencyKey, Layer, LedgerId, LedgerPolicy, LedgerStore,
    PeriodCalendar, Posting, SealContext,
};
use sqlx::PgPool;
use time::Date;
use time::macros::date;

use crate::error::{WalletError, invalid};
use crate::proof::ProofBundle;

/// Money precision: 6 decimal places; 1 credit = 1_000_000 minor, fine enough for per-token pricing.
pub const SCALE: u8 = 6;
pub type Credits = Amount<SCALE>;

/// Account opening date. Every posting date must not be earlier than this.
const OPENED: Date = date!(2026 - 01 - 01);

const WALLET: &str = "Liabilities:Wallet";
const CASH: &str = "Assets:Cash";
const REVENUE: &str = "Income:Usage";

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
/// Three fixed accounts:
/// - wallet: the balance owed to the user, a liability. Carries `FundedReservations`:
///   no overdraft, and no release beyond what was reserved.
/// - cash: money received from top-ups.
/// - revenue: income recognized on settlement.
pub struct Wallet {
    store: PostgresStore<SCALE>,
    registry: AccountRegistry,
    wallet: AccountId,
    cash: AccountId,
    revenue: AccountId,
    calendar: PeriodCalendar,
    policy: LedgerPolicy,
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
        let store = PostgresStore::<SCALE>::new(pool, ledger).in_schema(&schema);
        store.migrate().await?;

        // Account handles must be restored from storage on restart. Re-registering by
        // path could mint different handle numbers and mispoint historical entries.
        let stored = store.accounts().await?;
        let creating = stored.is_empty();
        let mut registry = if creating {
            let mut r = AccountRegistry::new();
            r.register_path(WALLET, OPENED).map_err(invalid)?;
            r.register_path(CASH, OPENED).map_err(invalid)?;
            r.register_path(REVENUE, OPENED).map_err(invalid)?;
            r
        } else {
            AccountRegistry::from_records(stored).map_err(invalid)?
        };

        // The wallet's limit is oxsum's rule about its own account, so it is applied on
        // every open rather than only where a ledger is created. `register_account`
        // upserts master data, which is what lets a ledger written under a weaker rule be
        // tightened here instead of keeping that rule for the rest of its life.
        let wallet = find_account(&registry, WALLET)?;
        let weakened = registry
            .records()
            .into_iter()
            .find(|r| r.id == wallet)
            .is_none_or(|r| r.account.limit != WALLET_LIMIT);
        if weakened {
            registry.set_limit(wallet, WALLET_LIMIT).map_err(invalid)?;
            let record = registry
                .records()
                .into_iter()
                .find(|r| r.id == wallet)
                .ok_or_else(|| WalletError::InvalidInput(format!("missing account {WALLET}")))?;
            store.register_account(&record).await?;
        }
        // A ledger that did not exist a moment ago has no rows at all, so the other two
        // accounts are written once, with it.
        if creating {
            for record in registry.records() {
                if record.id != wallet {
                    store.register_account(&record).await?;
                }
            }
        }

        Ok(Self {
            wallet,
            cash: find_account(&registry, CASH)?,
            revenue: find_account(&registry, REVENUE)?,
            store,
            registry,
            calendar: PeriodCalendar::new(),
            policy: LedgerPolicy::default(),
        })
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

    /// Hold: reserves part of the wallet balance in the pending layer; the settled balance is untouched.
    ///
    /// The wallet's limit counts the pending layer, so concurrent holds cannot
    /// together exceed the balance. Refused with [`WalletError::InsufficientFunds`]
    /// when the balance cannot cover it.
    pub async fn hold(&self, key: &str, minor: i64, on: Date) -> Result<Receipt, WalletError> {
        let amt = positive(minor)?;
        self.append(
            Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
                .post(Posting::debit(self.wallet, amt, currency()).in_layer(Layer::Pending))
                .post(Posting::credit(self.revenue, amt, currency()).in_layer(Layer::Pending)),
        )
        .await
    }

    /// Settle: one entry does two things — releases the hold (a reversal in the pending layer)
    /// and charges the actual usage (recorded in the settled layer).
    ///
    /// `actual` may be less than the held amount; the difference returns to the available
    /// balance. `actual` of zero amounts to a full release.
    ///
    /// `held_minor` is a claim about a hold this wallet took, and it is checked as one: the
    /// release credits the pending layer, and the wallet's limit refuses a pending credit that
    /// the outstanding reservations cannot cover, with
    /// [`WalletError::InsufficientFunds`]. So a settlement releases at most what is reserved
    /// in total, and two settlements cannot both release the same reservation — the check runs
    /// inside the append, against the balance the entry would leave behind.
    ///
    /// What it does not do is pair one settlement with one hold: the reservation layer is
    /// checked in aggregate, so releasing more than the hold it names is permitted while other
    /// reservations cover the amount. No value can be fabricated that way — the total released
    /// still cannot exceed the total reserved — but the pairing is loose, see docs/decisions.md.
    pub async fn settle(
        &self,
        key: &str,
        held_minor: i64,
        actual_minor: i64,
        on: Date,
    ) -> Result<Receipt, WalletError> {
        let held = positive(held_minor)?;
        if !(0..=held_minor).contains(&actual_minor) {
            return Err(WalletError::InvalidInput(
                "actual must be within 0..=held".into(),
            ));
        }
        let mut draft = Entry::<Draft, SCALE>::new(entry_id_for(key), idem(key)?, on)
            .post(Posting::credit(self.wallet, held, currency()).in_layer(Layer::Pending))
            .post(Posting::debit(self.revenue, held, currency()).in_layer(Layer::Pending));
        if actual_minor > 0 {
            let actual = Credits::from_minor(actual_minor);
            draft = draft.debit(self.wallet, actual, currency()).credit(
                self.revenue,
                actual,
                currency(),
            );
        }
        self.append(draft).await
    }

    /// Available balance in minor units = settled balance - unsettled holds.
    pub async fn available(&self) -> Result<i64, WalletError> {
        Ok(self.settled().await? - self.reserved().await?)
    }

    /// Credits that have settled into the wallet: what top-ups put there, less what settlements
    /// have charged.
    ///
    /// Separate from [`reserved`](Self::reserved) because a hold and a settlement are answered
    /// from different layers: a hold draws on the two together, a settlement gives back part of
    /// the reserved one.
    pub async fn settled(&self) -> Result<i64, WalletError> {
        self.wallet_net(Layer::Settled).await
    }

    /// Credits currently reserved by holds that have not been settled yet.
    ///
    /// Never negative: the wallet's limit keeps the pending layer on the hold side, which is what
    /// makes this the ceiling on what a settlement may release.
    pub async fn reserved(&self) -> Result<i64, WalletError> {
        Ok(-self.wallet_net(Layer::Pending).await?)
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

    async fn wallet_net(&self, layer: Layer) -> Result<i64, WalletError> {
        let b = self
            .store
            .balance(
                BalanceKey {
                    account: self.wallet,
                    currency: currency(),
                    layer,
                },
                BalanceQuery::all(),
            )
            .await?;
        // The wallet is a liability: credit balances are the positive side.
        Ok(b.credits.to_minor() - b.debits.to_minor())
    }

    fn seal(&self, draft: Entry<Draft, SCALE>) -> Result<Entry<Balanced, SCALE>, WalletError> {
        draft
            .seal(&SealContext {
                accounts: &self.registry,
                calendar: &self.calendar,
                policy: &self.policy,
            })
            .map_err(invalid)
    }

    async fn append(&self, draft: Entry<Draft, SCALE>) -> Result<Receipt, WalletError> {
        let entry = self.seal(draft)?;
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

/// Derives the EntryId deterministically from the idempotency key, so a retry always lands on the same entry.
fn entry_id_for(key: &str) -> EntryId {
    EntryId::from_uuid(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        key.as_bytes(),
    ))
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

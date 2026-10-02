use doubleentry::account::AccountRegistry;
use doubleentry::storage::postgres::PostgresStore;
use doubleentry::{
    AccountId, Amount, BalanceKey, BalanceLimit, BalanceQuery, Balanced, Currency, Draft, Entry,
    EntryBatch, EntryId, Hash, IdempotencyKey, Layer, LedgerId, LedgerPolicy, LedgerStore,
    PeriodCalendar, Posting, SealContext,
};
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

/// One tenant's wallet ledger.
///
/// Three fixed accounts:
/// - wallet: the balance owed to the user, a liability. Carries `NoDebitBalance`, no overdraft.
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
    /// Opens a tenant's ledger, creating it on first use.
    ///
    /// The schema name is derived server-side from the validated tenant id and is never taken from external input.
    pub async fn open(database_url: &str, tenant_id: &str) -> Result<Self, WalletError> {
        validate_tenant_id(tenant_id)?;
        let ledger = LedgerId::new(format!("tenant-{tenant_id}")).map_err(invalid)?;
        let schema = format!("ledger_{tenant_id}");
        let store = PostgresStore::<SCALE>::connect_with(database_url, ledger, &schema).await?;
        store.migrate().await?;

        // Account handles must be restored from storage on restart. Re-registering by
        // path could mint different handle numbers and mispoint historical entries.
        let stored = store.accounts().await?;
        let registry = if stored.is_empty() {
            let mut r = AccountRegistry::new();
            let wallet = r.register_path(WALLET, OPENED).map_err(invalid)?;
            r.register_path(CASH, OPENED).map_err(invalid)?;
            r.register_path(REVENUE, OPENED).map_err(invalid)?;
            r.set_limit(wallet, BalanceLimit::NoDebitBalance)
                .map_err(invalid)?;
            for record in r.records() {
                store.register_account(&record).await?;
            }
            r
        } else {
            AccountRegistry::from_records(stored).map_err(invalid)?
        };

        let find = |path: &str| {
            registry
                .records()
                .into_iter()
                .find(|r| r.account.path.to_string() == path)
                .map(|r| r.id)
                .ok_or_else(|| WalletError::InvalidInput(format!("missing account {path}")))
        };
        Ok(Self {
            wallet: find(WALLET)?,
            cash: find(CASH)?,
            revenue: find(REVENUE)?,
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
    /// The wallet's `NoDebitBalance` limit counts the pending layer, so concurrent holds
    /// cannot together exceed the balance.
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
        Ok(self.wallet_net(Layer::Settled).await? + self.wallet_net(Layer::Pending).await?)
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

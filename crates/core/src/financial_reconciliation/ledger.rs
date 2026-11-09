//! Capture official plan/refund entries and derive amounts only from verified postings.

use std::collections::{BTreeMap, BTreeSet};

use doubleentry::{
    Account, AccountId, AccountPath, AccountRecord, AccountRegistry, Direction, Layer, StoredEntry,
};
use sqlx::{PgConnection, Row};
use time::Date;
use uuid::Uuid;

use super::types::{self, ENTRY_LIMIT};
use crate::{SCALE, WalletError, error, ledger_source, wallet};

pub(super) const CASH: &str = "Assets:Cash";
pub(super) const RECEIVABLE: &str = "Assets:Receivables";
pub(super) const DEFERRED: &str = "Liabilities:UnearnedPlanFees";
pub(super) const PAYABLE: &str = "Liabilities:RefundPayable";
pub(super) const REVENUE: &str = "Income:PlanFees";
pub(super) const DISCOUNTS: &str = "Income:Discounts";
pub(super) const SUSPENSE: &str = "Equity:Adjustments";

pub(super) struct Entry {
    pub id: Uuid,
    pub stored: Result<StoredEntry<SCALE>, String>,
}

impl Entry {
    pub fn valid(&self) -> bool {
        self.stored
            .as_ref()
            .is_ok_and(|stored| stored.index.is_some())
    }

    pub fn same_key(&self, key: &str) -> bool {
        self.valid()
            && self.id == *crate::entry_id_for(key).as_uuid()
            && self
                .stored
                .as_ref()
                .is_ok_and(|stored| stored.entry.idempotency_key().as_bytes() == key.as_bytes())
    }

    pub fn booked_on(&self) -> Option<Date> {
        self.stored
            .as_ref()
            .ok()
            .map(|stored| stored.entry.booking_date())
    }

    pub fn net(&self, account: AccountId) -> Result<i64, WalletError> {
        let stored = self.stored.as_ref().map_err(error::invalid)?;
        stored
            .entry
            .postings()
            .iter()
            .filter(|posting| {
                posting.account == account
                    && posting.layer == Layer::Settled
                    && posting.currency == doubleentry::Currency::USD
            })
            .try_fold(0, |amount, posting| {
                types::add(
                    amount,
                    match posting.direction {
                        Direction::Credit => posting.amount.to_minor(),
                        Direction::Debit => -posting.amount.to_minor(),
                    },
                )
            })
    }

    pub fn key(&self) -> Option<&[u8]> {
        self.stored
            .as_ref()
            .ok()
            .map(|stored| stored.entry.idempotency_key().as_bytes())
    }
}

pub(super) struct Ledger {
    pub entries: BTreeMap<Uuid, Entry>,
    pub accounts: BTreeMap<String, AccountId>,
    pub claims: BTreeMap<Uuid, String>,
    pub bytes: usize,
}

impl Ledger {
    pub fn net(&self, ids: &BTreeSet<Uuid>, path: &str) -> Result<i64, WalletError> {
        let Some(account) = self.accounts.get(path) else {
            return Ok(0);
        };
        ids.iter()
            .filter_map(|id| self.entries.get(id))
            .filter(|entry| entry.valid())
            .try_fold(0, |total, entry| types::add(total, entry.net(*account)?))
    }

    pub fn is_financial(&self, entry: &Entry) -> bool {
        let paths = [REVENUE, DISCOUNTS, DEFERRED, PAYABLE, RECEIVABLE];
        let accounts: BTreeSet<_> = paths
            .iter()
            .filter_map(|path| self.accounts.get(*path).copied())
            .collect();
        entry.stored.as_ref().is_ok_and(|stored| {
            stored
                .entry
                .postings()
                .iter()
                .any(|posting| accounts.contains(&posting.account))
        }) || entry.key().is_some_and(|key| {
            key.starts_with(b"plan-")
                || key.starts_with(b"order-refund")
                || key.starts_with(b"nonplan-refund")
        })
    }

    pub fn claim(&mut self, id: Uuid, owner: &str) -> bool {
        match self.claims.get(&id) {
            Some(previous) => previous == owner,
            None => {
                self.claims.insert(id, owner.to_owned());
                true
            }
        }
    }

    pub fn financial_amounts(
        &self,
        ids: &BTreeSet<Uuid>,
        legacy: bool,
    ) -> Result<super::types::Amounts, WalletError> {
        let mut income = types::add(self.net(ids, REVENUE)?, self.net(ids, DISCOUNTS)?)?;
        if legacy {
            income = types::add(income, self.net(ids, "Equity:Promo")?)?;
        }
        let deferred = if legacy { 0 } else { self.net(ids, DEFERRED)? };
        let billed = types::add(income, deferred)?;
        let ar = self.net(ids, RECEIVABLE)?;
        Ok(super::types::Amounts {
            billed_minor: billed,
            earned_minor: income,
            deferred_minor: deferred,
            collected_minor: types::add(billed, ar)?,
            receivable_minor: types::sub(0, ar)?,
            refund_minor: 0,
        })
    }

    pub fn evidence(&self, ids: &BTreeSet<Uuid>) -> serde_json::Value {
        let entries: Vec<_> = ids.iter().map(|id| match self.entries.get(id) {
            None => serde_json::json!({"entryId":id,"error":"missing official entry"}),
            Some(entry) => match &entry.stored {
                Err(error) => serde_json::json!({"entryId":id,"error":error}),
                Ok(stored) => {
                    let postings: Vec<_> = stored.entry.postings().iter().map(|posting| serde_json::json!({
                        "accountIndex":posting.account.index(),
                        "path":self.accounts.iter().find(|(_,account)| **account==posting.account).map(|(path,_)|path),
                        "direction":format!("{:?}",posting.direction),"layer":format!("{:?}",posting.layer),
                        "currency":posting.currency.to_string(),"amountMinor":posting.amount.to_minor(),
                        "dimensions":posting.dimensions,
                    })).collect();
                    serde_json::json!({"entryId":id,"contentHash":stored.content_hash.to_hex(),
                        "keyHex":stored.entry.idempotency_key().to_hex(),"logIndex":stored.index.map(|index|index.get()),
                        "bookingDate":stored.entry.booking_date().to_string(),"valueDate":stored.entry.value_date().to_string(),
                        "description":stored.entry.description().as_str(),"kind":stored.entry.kind().map(|kind|kind.as_str()),
                        "provenance":stored.entry.provenance(),"document":stored.entry.document(),
                        "reverses":stored.entry.reverses(),"originalBookingDate":stored.entry.original_booking_date().map(|day|day.to_string()),
                        "postings":postings})
                }
            }
        }).collect();
        serde_json::json!(entries)
    }
}

pub(super) async fn capture(
    connection: &mut PgConnection,
    tenant: &str,
) -> Result<Ledger, WalletError> {
    wallet::validate_tenant_id(tenant)?;
    let schema = format!("ledger_{tenant}");
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("{schema}.entries"))
        .fetch_one(&mut *connection)
        .await?;
    let mut ledger = Ledger {
        entries: BTreeMap::new(),
        accounts: BTreeMap::new(),
        claims: BTreeMap::new(),
        bytes: 0,
    };
    if !exists {
        return Ok(ledger);
    }
    let account_bytes: i64 = sqlx::query_scalar(&format!("SELECT COALESCE(SUM(pg_column_size(a)),0)::bigint FROM (SELECT * FROM {schema}.accounts ORDER BY account_index LIMIT $1) a"))
        .bind((ENTRY_LIMIT+1) as i64).fetch_one(&mut *connection).await?;
    if account_bytes > types::BYTE_LIMIT as i64 {
        return Err(error::invalid(
            "reconciliation account registry byte limit exceeded",
        ));
    }
    let rows = sqlx::query(&format!(
        "SELECT account_index,path,opened_on FROM {schema}.accounts ORDER BY account_index LIMIT $1"
    ))
    .bind((ENTRY_LIMIT + 1) as i64)
    .fetch_all(&mut *connection)
    .await?;
    if rows.len() > ENTRY_LIMIT {
        return Err(error::invalid(
            "reconciliation account registry limit exceeded",
        ));
    }
    let records = rows
        .into_iter()
        .map(|row| {
            let index: i32 = row.try_get("account_index")?;
            let path: String = row.try_get("path")?;
            Ok(AccountRecord {
                id: AccountId::from_index(u32::try_from(index).map_err(error::invalid)?),
                account: Account::new(
                    AccountPath::parse(&path).map_err(error::invalid)?,
                    row.try_get("opened_on")?,
                ),
            })
        })
        .collect::<Result<Vec<_>, WalletError>>()?;
    // Dense handles and valid unique paths are structural checks, not an
    // external trust anchor for account meaning. Historical charts may have
    // legitimately registered these paths in a different index order.
    let registry = AccountRegistry::from_records(records).map_err(error::invalid)?;
    for record in registry.records() {
        ledger
            .accounts
            .insert(record.account.path.to_string(), record.id);
    }
    // Verify every bounded entry: selecting by mutable posting or key bytes alone
    // could hide corruption which changed the field used by the selector. Expected
    // missing IDs are retained by the consumer even when this scan cannot find them.
    let ids: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT entry_id FROM {schema}.entries ORDER BY entry_id LIMIT $1"
    ))
    .bind((ENTRY_LIMIT + 1) as i64)
    .fetch_all(&mut *connection)
    .await?;
    if ids.len() > ENTRY_LIMIT {
        return Err(error::invalid("reconciliation ledger entry limit exceeded"));
    }
    let mut captured_bytes = account_bytes;
    for table in ["entries", "postings", "posting_dimensions"] {
        let bytes:i64=sqlx::query_scalar(&format!("SELECT COALESCE(SUM(pg_column_size(t)),0)::bigint FROM {schema}.{table} t WHERE entry_id=ANY($1)"))
            .bind(&ids).fetch_one(&mut *connection).await?;
        captured_bytes = types::add(captured_bytes, bytes)?;
        if captured_bytes > types::BYTE_LIMIT as i64 {
            return Err(error::invalid("reconciliation ledger byte limit exceeded"));
        }
    }
    ledger.bytes = usize::try_from(captured_bytes).map_err(error::invalid)?;
    for source in ledger_source::entries_in(connection, tenant, &ids).await? {
        ledger.entries.insert(
            source.id,
            Entry {
                id: source.id,
                stored: source.verification.map_err(|error| error.to_string()),
            },
        );
    }
    Ok(ledger)
}

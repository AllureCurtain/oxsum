//! Capture official plan/refund entries and derive amounts only from verified postings.

use std::collections::{BTreeMap, BTreeSet};

use doubleentry::{AccountId, Direction, Layer, StoredEntry};
use serde_json::json;
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
        self.stored.as_ref().is_ok_and(|stored| stored.index.is_some())
    }

    pub fn same_key(&self, key: &str) -> bool {
        self.valid() && self.id == *crate::entry_id_for(key).as_uuid()
            && self.stored.as_ref().is_ok_and(|stored| stored.entry.idempotency_key().as_bytes() == key.as_bytes())
    }

    pub fn booked_on(&self) -> Option<Date> {
        self.stored.as_ref().ok().map(|stored| stored.entry.booking_date())
    }

    pub fn net(&self, account: AccountId) -> Result<i64, WalletError> {
        let stored = self.stored.as_ref().map_err(error::invalid)?;
        stored.entry.postings().iter()
            .filter(|posting| posting.account == account && posting.layer == Layer::Settled)
            .try_fold(0, |amount, posting| types::add(amount, match posting.direction {
                Direction::Credit => posting.amount.to_minor(),
                Direction::Debit => -posting.amount.to_minor(),
            }))
    }

    pub fn key(&self) -> Option<&[u8]> {
        self.stored.as_ref().ok().map(|stored| stored.entry.idempotency_key().as_bytes())
    }
}

pub(super) struct Ledger {
    pub entries: BTreeMap<Uuid, Entry>,
    pub accounts: BTreeMap<String, AccountId>,
    pub claims: BTreeMap<Uuid, String>,
}

impl Ledger {
    pub fn net(&self, ids: &BTreeSet<Uuid>, path: &str) -> Result<i64, WalletError> {
        let Some(account) = self.accounts.get(path) else { return Ok(0); };
        ids.iter().filter_map(|id| self.entries.get(id)).filter(|entry| entry.valid())
            .try_fold(0, |total, entry| types::add(total, entry.net(*account)?))
    }

    pub fn evidence(&self, ids: &BTreeSet<Uuid>) -> serde_json::Value {
        let entries: Vec<_> = ids.iter().map(|id| match self.entries.get(id) {
            None => json!({"entryId":id,"error":"missing official entry"}),
            Some(entry) => match &entry.stored {
                Err(error) => json!({"entryId":id,"error":error}),
                Ok(stored) => {
                    let postings: Vec<_> = stored.entry.postings().iter().map(|posting| json!({
                        "accountIndex":posting.account.index(),
                        "path":self.accounts.iter().find(|(_,account)| **account==posting.account).map(|(path,_)|path),
                        "direction":format!("{:?}",posting.direction),"layer":format!("{:?}",posting.layer),
                        "currency":posting.currency.to_string(),"amountMinor":posting.amount.to_minor(),
                        "dimensions":posting.dimensions,
                    })).collect();
                    json!({"entryId":id,"contentHash":stored.content_hash.to_hex(),
                        "keyHex":stored.entry.idempotency_key().to_hex(),"logIndex":stored.index.map(|index|index.get()),
                        "bookingDate":stored.entry.booking_date().to_string(),"valueDate":stored.entry.value_date().to_string(),
                        "description":stored.entry.description().as_str(),"kind":stored.entry.kind().map(|kind|kind.as_str()),
                        "provenance":stored.entry.provenance(),"document":stored.entry.document(),
                        "reverses":stored.entry.reverses(),"originalBookingDate":stored.entry.original_booking_date().map(|day|day.to_string()),
                        "postings":postings})
                }
            }
        }).collect();
        json!(entries)
    }
}

pub(super) async fn capture(
    connection: &mut PgConnection,
    tenant: &str,
    extra: &[Uuid],
) -> Result<Ledger, WalletError> {
    wallet::validate_tenant_id(tenant)?;
    let schema = format!("ledger_{tenant}");
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("{schema}.entries")).fetch_one(&mut *connection).await?;
    let mut ledger = Ledger {entries:BTreeMap::new(),accounts:BTreeMap::new(),claims:BTreeMap::new()};
    if !exists { return Ok(ledger); }
    for row in sqlx::query(&format!("SELECT account_index,path FROM {schema}.accounts"))
        .fetch_all(&mut *connection).await? {
        let index: i32 = row.try_get("account_index")?;
        ledger.accounts.insert(row.try_get("path")?,AccountId::from_index(u32::try_from(index).map_err(error::invalid)?));
    }
    // Keys are opaque bytes: convert_from would make one unrelated non-UTF8 key
    // abort the scan. Inspect only fixed ASCII prefixes and keep full raw keys for binding.
    let ids: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT e.entry_id FROM {schema}.entries e WHERE e.entry_id=ANY($1) \
         OR EXISTS(SELECT 1 FROM {schema}.postings p JOIN {schema}.accounts a USING(account_index) \
          WHERE p.entry_id=e.entry_id AND a.path=ANY($2)) \
         OR substring(e.idempotency_key FROM 1 FOR 5)=convert_to('plan-','UTF8') \
         OR substring(e.idempotency_key FROM 1 FOR 14)=convert_to('payment-order/','UTF8') \
         OR substring(e.idempotency_key FROM 1 FOR 18)=convert_to('order-refund-plan/','UTF8') \
         OR substring(e.idempotency_key FROM 1 FOR 17)=convert_to('dispute-recharge/','UTF8') \
         ORDER BY e.entry_id LIMIT $3"))
        .bind(extra).bind([REVENUE,DISCOUNTS,DEFERRED,PAYABLE,RECEIVABLE]).bind((ENTRY_LIMIT+1) as i64)
        .fetch_all(&mut *connection).await?;
    if ids.len()>ENTRY_LIMIT { return Err(error::invalid("reconciliation ledger entry limit exceeded")); }
    for source in ledger_source::entries_in(connection,tenant,&ids).await? {
        ledger.entries.insert(source.id,Entry { id:source.id,stored:source.verification.map_err(|error|error.to_string()) });
    }
    Ok(ledger)
}

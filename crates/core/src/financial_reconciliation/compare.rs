//! Source-to-ledger comparisons, preserving explicit partial coverage and evidence.

mod corrections;
mod daily;
mod disputes;
pub(super) mod documents;
mod legacy;
mod refunds;
mod reissues;
mod repayments;
mod schedule;

use std::collections::BTreeSet;

use sqlx::PgConnection;
use uuid::Uuid;

use super::ledger::{self, Ledger};
use super::sources::{self, Sources};
use super::types::{self, Amounts, Comparison, Scope};
use crate::{WalletError, error};

pub(super) struct Customer<'a> {
    pub organization: Uuid,
    pub sources: Sources,
    pub ledger: Ledger,
    pub scope: Scope,
    pub captured_at: time::OffsetDateTime,
    pub result: &'a mut Comparison,
}

impl Customer<'_> {
    pub fn unsupported(&mut self, table: &str, id: Uuid, fact: &str, detail: &str) {
        self.result
            .unsupported(self.organization, table, &id.to_string(), fact, detail);
    }
    pub fn compare_amount(
        &mut self,
        table: &str,
        id: Uuid,
        fact: &str,
        expected: i64,
        actual: i64,
    ) {
        if expected != actual {
            self.result.difference(
                self.organization,
                table,
                &id.to_string(),
                fact,
                Some(expected),
                Some(actual),
                "Captured source amount differs from independently verified official evidence",
            );
        }
    }
    pub fn require_entry(
        &mut self,
        table: &str,
        id: Uuid,
        entry: Uuid,
        key: &str,
        owner: &str,
    ) -> bool {
        let valid = self.ledger.entries.get(&entry).is_some_and(|entry| {
            entry.same_key(key)
                && entry
                    .stored
                    .as_ref()
                    .is_ok_and(|stored| stored.entry.reverses().is_none())
        }) && !self.ledger.entries.values().any(|candidate| {
            candidate.stored.as_ref().is_ok_and(|stored| {
                stored
                    .entry
                    .reverses()
                    .is_some_and(|id| *id.as_uuid() == entry)
            })
        });
        if !valid {
            self.unsupported(
                table,
                id,
                "officialEntry",
                "Missing, malformed, unsequenced or wrong-key official entry",
            );
        }
        let unique = self.ledger.claim(entry, owner);
        if !unique {
            self.unsupported(
                table,
                id,
                "duplicateEntry",
                "One official entry is claimed by different financial sources",
            );
        }
        valid && unique
    }
    pub fn shape(&mut self, table: &str, id: Uuid, ids: &BTreeSet<Uuid>, paths: &[&str]) -> bool {
        let accounts: BTreeSet<_> = paths
            .iter()
            .filter_map(|path| self.ledger.accounts.get(*path).copied())
            .collect();
        let valid = ids.iter().all(|id| {
            self.ledger.entries.get(id).is_some_and(|entry| {
                entry.stored.as_ref().is_ok_and(|stored| {
                    stored.entry.postings().iter().all(|posting| {
                        posting.layer == doubleentry::Layer::Settled
                            && posting.currency == doubleentry::Currency::USD
                            && posting.amount.to_minor() > 0
                            && accounts.contains(&posting.account)
                    })
                })
            })
        });
        if !valid {
            self.unsupported(
                table,
                id,
                "postingShape",
                "Financial source contains unsupported accounts, layers, amounts or currencies",
            );
        }
        valid
    }
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        table: &str,
        id: Uuid,
        expected: &Amounts,
        actual: &Amounts,
        ids: &BTreeSet<Uuid>,
        complete: bool,
        extra: serde_json::Value,
    ) {
        self.result.coverage.sources += 1;
        self.result.coverage.verified_sources += usize::from(complete);
        if !complete {
            self.result.coverage.complete = false;
        }
        self.result
            .compare(self.organization, table, &id.to_string(), expected, actual);
        self.result.evidence.push(serde_json::json!({"organizationId":self.organization,"sourceTable":table,"sourceId":id,
            "source":self.sources.get(table,id).map(|source|&source.evidence),"expected":expected,"actual":actual,
            "complete":complete,"entries":self.ledger.evidence(ids),"context":extra}));
    }
}

pub(super) async fn customer(
    connection: &mut PgConnection,
    organization: Uuid,
    tenant: &str,
    scope: Scope,
    result: &mut Comparison,
) -> Result<(), WalletError> {
    let sources = sources::capture(connection, organization).await?;
    result.captured_rows += sources.count;
    result.captured_bytes += sources.bytes;
    if result.captured_bytes > types::BYTE_LIMIT {
        return Err(error::invalid(
            "reconciliation global captured byte limit exceeded",
        ));
    }
    if result.captured_rows > types::SOURCE_LIMIT {
        return Err(error::invalid(
            "reconciliation global captured source limit exceeded",
        ));
    }
    let ledger = ledger::capture(connection, tenant).await?;
    result.captured_bytes += ledger.bytes;
    if result.captured_bytes > types::BYTE_LIMIT {
        return Err(error::invalid(
            "reconciliation global captured byte limit exceeded",
        ));
    }
    result.coverage.organizations += 1;
    result.coverage.entries += ledger.entries.len();
    result.coverage.verified_entries += ledger
        .entries
        .values()
        .filter(|entry| entry.valid())
        .count();
    if result.coverage.entries > types::ENTRY_LIMIT {
        return Err(error::invalid("reconciliation global entry limit exceeded"));
    }
    let captured_at: time::OffsetDateTime = sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut *connection)
        .await?;
    let mut customer = Customer {
        organization,
        sources,
        ledger,
        scope,
        captured_at,
        result,
    };
    check(&mut customer)
}

/// Shared canonical source comparisons; callers supply one bounded captured snapshot.
pub(super) fn check(customer: &mut Customer<'_>) -> Result<(), WalletError> {
    reissues::compare(customer)?;
    disputes::compare(customer)?;
    repayments::compare(customer)?;
    refunds::compare(customer)?;
    daily::compare(customer)?;
    schedule::compare(customer)?;
    legacy::compare(customer)?;
    documents::compare(customer)?;
    // A corrupted entry cannot evade the result simply because its source mapping
    // disappeared. Unclaimed financial entries make coverage explicitly incomplete.
    let unmatched: Vec<_> = customer
        .ledger
        .entries
        .iter()
        .filter(|(id, entry)| {
            !customer.ledger.claims.contains_key(id)
                && (!entry.valid()
                    || customer.ledger.is_financial(entry)
                        && customer.scope.contains(entry.booked_on()))
        })
        .map(|(id, _)| *id)
        .collect();
    for id in unmatched {
        customer.unsupported(
            "ledger",
            id,
            "unclaimedEntry",
            "Official financial entry has no supported source mapping in this captured comparison",
        );
        customer.result.evidence.push(serde_json::json!({"organizationId":customer.organization,"sourceTable":"ledger","sourceId":id,"entries":customer.ledger.evidence(&BTreeSet::from([id]))}));
    }
    if customer.result.coverage.sources > types::SOURCE_LIMIT {
        return Err(error::invalid(
            "reconciliation global source limit exceeded",
        ));
    }
    Ok(())
}

//! Captured comparison scope, totals and immutable attempt responses.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{TimeBasis, WalletError, error};

pub(super) const SOURCE_LIMIT: usize = 10_000;
pub(super) const ENTRY_LIMIT: usize = 20_000;
pub(super) const BYTE_LIMIT: usize = 16 * 1024 * 1024;
pub(super) const FIELDS: [&str; 6] = [
    "billedMinor", "earnedMinor", "deferredMinor", "collectedMinor", "receivableMinor", "refundMinor",
];

/// What a run actually compared; a matched result attests only this scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationCoverage {
    /// Zero marks old projection-only reports; one independently checks entries.
    pub version: i32,
    /// Every required financial source has supported, complete ledger evidence.
    pub complete: bool,
    /// Fields that both expected and actual totals independently represent.
    pub compared_fields: Vec<String>,
    /// Categories excluded from this comparison, never copied into actual totals.
    pub not_compared: Vec<String>,
    /// Customer organizations captured in the attempt.
    pub organizations: usize,
    /// Required source units, including documents and original funding.
    pub sources: usize,
    /// Source units with valid complete ledger and ownership evidence.
    pub verified_sources: usize,
    /// Official financial entries examined.
    pub entries: usize,
    /// Entries whose full bytes and recorded content hash agree.
    pub verified_entries: usize,
    /// Exact cohort/movement semantics and limits of the scope.
    pub basis: String,
    /// Hard bounds; exceeding one fails the attempt rather than truncating it.
    pub limits: BTreeMap<String, usize>,
}

impl ReconciliationCoverage {
    pub(super) fn new(basis: TimeBasis) -> Self {
        Self {
            version: 1, complete: true,
            compared_fields: FIELDS.iter().map(|v| (*v).into()).collect(),
            not_compared: ["usageMinor", "ratedMinor", "entitlementMinor", "fundingMinor", "discountMinor", "legacyDeferredBalance", "actualServiceDelivery", "providerCosts"].map(str::to_owned).to_vec(),
            organizations: 0, sources: 0, verified_sources: 0, entries: 0, verified_entries: 0,
            basis: format!("V1: captured net journal positions selected by original posted date. V2: signed effect movements under {basis:?}; booking comes from official entries, service/payment attribution from original immutable sources. Refunds follow their retained cash-source cohort. Payable and suspense are separate source movements in evidence, excluded from income and receivables. This is not complete business accounting."),
            limits: [("sources".into(), SOURCE_LIMIT),("entries".into(),ENTRY_LIMIT),("evidenceBytes".into(),BYTE_LIMIT),("periodDays".into(),366)].into(),
        }
    }
}

/// A retained reconciliation attempt, never recomputed from current sources.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationSnapshot {
    /// Immutable attempt identity.
    pub snapshot_id: Uuid,
    /// Stable kind/period/time-basis run identity.
    pub run_id: Uuid,
    /// `matched`, `exceptions`, or retained projection-only `legacy`.
    pub state: String,
    /// Document/source assertions for the explicitly compared fields.
    pub expected_totals: serde_json::Value,
    /// Independently reconstructed official ledger figures.
    pub actual_totals: serde_json::Value,
    /// Coverage denominators, unsupported sources and excluded fields.
    pub coverage: ReconciliationCoverage,
    /// Captured disagreements, including waived/resolved cases that still differ.
    pub exception_count: i64,
    /// Time the repeatable-read snapshot was established.
    #[serde(with = "time::serde::rfc3339")]
    pub captured_at: OffsetDateTime,
    /// Exact source comparisons and postings; omitted from list summaries.
    pub evidence: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct Amounts {
    pub billed_minor: i64,
    pub earned_minor: i64,
    pub deferred_minor: i64,
    pub collected_minor: i64,
    pub receivable_minor: i64,
    pub refund_minor: i64,
}
impl Amounts {
    pub fn add(&mut self, other: &Self) -> Result<(), WalletError> {
        self.billed_minor = add(self.billed_minor, other.billed_minor)?;
        self.earned_minor = add(self.earned_minor, other.earned_minor)?;
        self.deferred_minor = add(self.deferred_minor, other.deferred_minor)?;
        self.collected_minor = add(self.collected_minor, other.collected_minor)?;
        self.receivable_minor = add(self.receivable_minor, other.receivable_minor)?;
        self.refund_minor = add(self.refund_minor, other.refund_minor)?;
        Ok(())
    }
    pub fn fields(&self) -> [i64; 6] { [self.billed_minor,self.earned_minor,self.deferred_minor,self.collected_minor,self.receivable_minor,self.refund_minor] }
}
pub(super) fn add(a: i64, b: i64) -> Result<i64,WalletError> { a.checked_add(b).ok_or_else(|| error::invalid("reconciliation amount overflows")) }
pub(super) fn sub(a: i64, b: i64) -> Result<i64,WalletError> { a.checked_sub(b).ok_or_else(|| error::invalid("reconciliation amount overflows")) }

#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub(super) struct Difference {
    pub organization_id: Uuid,
    pub kind: String,
    pub source_table: String,
    pub source_ref: String,
    pub fact: String,
    pub expected: Option<i64>,
    pub actual: Option<i64>,
    pub detail: String,
}

pub(super) struct Comparison {
    pub expected: Amounts,
    pub actual: Amounts,
    pub coverage: ReconciliationCoverage,
    pub differences: Vec<Difference>,
    pub evidence: Vec<serde_json::Value>,
}
impl Comparison {
    pub fn new(basis: TimeBasis) -> Self { Self { expected: Amounts::default(),actual:Amounts::default(),coverage:ReconciliationCoverage::new(basis),differences:Vec::new(),evidence:Vec::new() } }
    pub fn difference(&mut self, org:Uuid, table:&str, source:&str, fact:&str, expected:Option<i64>, actual:Option<i64>, detail:&str) {
        self.differences.push(Difference { organization_id:org,kind: if expected.is_some() && actual.is_some() { "amount_drift" } else { "unmatched_journal" }.into(),source_table:table.into(),source_ref:source.into(),fact:fact.into(),expected,actual,detail:detail.into() });
    }
    pub fn compare(&mut self, org:Uuid, table:&str, source:&str, expected:&Amounts, actual:&Amounts) {
        for ((field,expected),actual) in FIELDS.into_iter().zip(expected.fields()).zip(actual.fields()) {
            if expected != actual { self.difference(org,table,source,field,Some(expected),Some(actual),"Captured financial source differs from independently verified official postings"); }
        }
    }
    pub fn unsupported(&mut self,org:Uuid,table:&str,source:&str,fact:&str,detail:&str) {
        self.coverage.complete=false;
        self.difference(org,table,source,fact,None,None,detail);
    }
}

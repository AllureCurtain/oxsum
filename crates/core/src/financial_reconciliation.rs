//! Independent financial reconciliation from a single captured database snapshot.

mod compare;
pub(crate) mod correction_sources;
pub(crate) mod debt_sources;
mod ledger;
mod persistence;
mod source_voids;
mod sources;
#[cfg(test)]
mod tests;
mod types;

pub use types::{ReconciliationCoverage, ReconciliationSnapshot};

pub use debt_sources::{StatementDebtSlice, StatementDebtSources};

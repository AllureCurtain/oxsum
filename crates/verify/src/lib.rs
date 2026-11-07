//! Bill verification, shared by the server and the browser.
//!
//! Given a proof bundle (the entry source, an inclusion proof, and the tree head
//! as of the proof) plus the content hash the user recorded with the bill, this
//! crate recomputes the hash from the source and links it to the head. Everything
//! here is deterministic computation with no I/O, so it compiles to
//! `wasm32-unknown-unknown` and runs inside the verification page: the same
//! function body the server runs, with no round-trip.
//!
//! The money scale lives here because the entry encoding — and therefore the
//! content hash — depends on it. The writer (`oxsum-core`) and every verifier
//! must agree on it, and one definition leaves no room for drift.

#![warn(missing_docs)]

mod anchor;
mod bill;
mod bundle;
mod charge;
mod evidence;
mod head;
mod price;
mod sale;
mod usage_report;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod wasm;
mod witness;

pub use anchor::{AnchorError, StatementAnchorCheck, verify_statement_anchor};
pub use bill::{BillVerdict, verify_bill};
pub use bundle::{ProofBundle, verify_bundle};
pub use charge::{ChargeCheck, verify_charge};
pub use doubleentry::{ConsistencyProof, Hash, TreeHead};
pub use evidence::{EvidenceError, verify_evidence_bundle};
pub use head::{HeadError, PublishedKey, verify_consistency, verify_signed_head};
pub use price::{PriceError, PriceLink, PriceLinkBuilder, verify_price_head};
pub use sale::{SaleError, SaleLink, SaleLinkBuilder, verify_sale_head};
pub use usage_report::{
    USAGE_ANCHOR_MAP_DOMAIN, UsageReportError, usage_anchor_map_root, usage_anchor_pair_leaf,
    usage_day_leaf, verify_usage_report,
};
pub use witness::{WitnessNote, WitnessPin, WitnessVerdict, verify_witness, verify_witnesses};

/// Money precision: 6 decimal places; 1 credit = 1_000_000 minor, fine enough
/// for per-token pricing. Re-exported by `oxsum-core`.
pub const SCALE: u8 = 6;

/// Unwraps a builder field, naming the missing one in the panic.
pub(crate) fn required_field<T>(value: Option<T>, field: &str) -> T {
    match value {
        Some(value) => value,
        None => panic!("link builder is missing {field}"),
    }
}

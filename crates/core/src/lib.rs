//! oxsum domain layer.
//!
//! One tenant maps to one PostgreSQL schema and one doubleentry ledger. Each ledger has
//! its own log, Merkle tree and idempotency-key space — physical isolation, not filtering.
//! oxsum's own tables (users, organizations, memberships, API keys) live in one `oxsum`
//! schema, outside every ledger schema; see [`Db`].

mod db;
mod error;
mod proof;
mod tenants;
mod wallet;

pub use db::{Db, SCHEMA};
pub use doubleentry::{EntryId, Hash};
pub use error::WalletError;
pub use proof::{ProofBundle, verify_bundle};
pub use tenants::Tenants;
pub use wallet::{Credits, Receipt, SCALE, Wallet};

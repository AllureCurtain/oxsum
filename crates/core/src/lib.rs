//! oxsum domain layer.
//!
//! A tenant is an organization: its balance, its ledger and its API keys. One organization
//! maps to one PostgreSQL schema (`ledger_<tenant_id>`) and one doubleentry ledger, so each
//! ledger has its own log, Merkle tree and idempotency-key space — physical isolation, not
//! filtering. oxsum's own tables (users, organizations, memberships, API keys, channels and
//! their prices, open holds) live in one `oxsum` schema, outside the ledgers; see [`Db`].

mod billing;
mod channels;
mod db;
mod error;
mod holds;
mod keys;
mod orgs;
mod proof;
mod sessions;
mod tenants;
mod users;
mod wallet;

pub use billing::{
    Price, PriceBook, Settlement, SettlementKind, Usage, estimate_tokens, hold_description,
    input_upper_bound,
};
pub use channels::{Channel, ModelPrice, SecretKey, Serving};
pub use db::{Db, SCHEMA};
pub use doubleentry::{EntryId, Hash};
pub use error::WalletError;
pub use holds::{DEFAULT_HOLD_TIMEOUT, OpenHold, SWEEP_INTERVAL, sweep_stale_holds};
pub use keys::{ApiKey, CreatedApiKey};
pub use orgs::{Kind, Organization, Role};
pub use proof::{ProofBundle, verify_bundle};
pub use sessions::{
    CreatedSession, KeyScope, Principal, Session, SessionPrincipal, SESSION_COOKIE, SESSION_LIFETIME,
};
pub use tenants::Tenants;
pub use users::{NewUser, Registration, User};
pub use wallet::{Credits, Receipt, SCALE, Wallet, entry_id_for, settlement_key_for};

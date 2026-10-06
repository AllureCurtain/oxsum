//! oxsum domain layer.
//!
//! A tenant is an organization: its balance, its ledger and its API keys. One organization
//! maps to one PostgreSQL schema (`ledger_<tenant_id>`) and one doubleentry ledger, so each
//! ledger has its own log, Merkle tree and idempotency-key space — physical isolation, not
//! filtering. oxsum's own tables (users, organizations, memberships, API keys, channels and
//! their prices, open holds, usage records) live in one `oxsum` schema, outside the ledgers;
//! see [`Db`].

mod adapters;
mod billing;
mod channels;
mod db;
mod deposits;
mod error;
mod heads;
mod holds;
mod idempotency;
mod invitations;
mod keys;
mod orgs;
mod proof;
mod ratelimit;
mod reconcile;
mod sessions;
mod statements;
mod tenants;
mod usage;
mod users;
mod wallet;

pub use adapters::{OPENAI, UsageAdapter, adapter_for};
pub use billing::{
    BillLine, BillingMode, ItemizedCharge, MeteredUsage, Price, PriceBook, PriceRule, PriceSet,
    RuleMatch, Settlement, SettlementKind, SettlementRecord, UpstreamPrices, estimate_tokens,
    hold_description, input_upper_bound,
};
pub use channels::{Channel, ModelPrice, SecretKey, Serving};
pub use db::{Db, SCHEMA};
pub use deposits::{CodeBatch, MAX_BATCH, Redemption};
pub use doubleentry::{ConsistencyProof, EntryId, Hash, Seal, TreeHead};
pub use error::WalletError;
pub use heads::{
    Consistency, HeadSigningKey, KeyPublication, SignedHead, origin_for, seed_from_base64,
    sign_head, signing_key,
};
pub use holds::{DEFAULT_HOLD_TIMEOUT, InFlightHold, OpenHold, SWEEP_INTERVAL, sweep_stale_holds};
pub use idempotency::{Claim, fingerprint};
pub use invitations::CreatedInvitation;
pub use keys::{ActingKey, ApiKey, BudgetDuration, CreatedApiKey, KeyConstraints};
pub use orgs::{
    AdminOrganization, Kind, Member, MembershipActor, Organization, Ownership, Role,
    UserOrganization,
};
pub use proof::{ChargeCheck, ProofBundle, verify_bundle, verify_charge};
pub use ratelimit::{RateAllowance, RateLimited, RateLimiter, SlidingWindow};
pub use reconcile::{DriftClass, DriftKind, DriftSample, Reconciliation};
pub use sessions::{
    CreatedSession, KeyPrincipal, KeyScope, Principal, SESSION_COOKIE, SESSION_LIFETIME, Session,
    SessionPrincipal,
};
pub use statements::{
    PaymentStatus, Statement, StatementLine, StatementPeriod, StatementStatus, statement_period,
};
pub use tenants::Tenants;
pub use usage::{
    Attribution, MAX_CONTEXT_NAME, MAX_END_USER, MAX_TAG, MAX_TAGS, Margin, UsageDay, UsageRecord,
    UsageRow, validate_attribution,
};
pub use users::{NewUser, Registration, User};
pub use wallet::{
    Credits, ListPage, LogEntry, Receipt, RequestEntry, SCALE, SettledEntry, SettledTurn,
    TransactionEntry, TransactionKind, Wallet, entry_id_for, settlement_key_for,
};

//! oxsum domain layer.
//!
//! A tenant is an organization: its balance, its ledger and its API keys. One organization
//! maps to one PostgreSQL schema (`ledger_<tenant_id>`) and one doubleentry ledger, so each
//! ledger has its own log, Merkle tree and idempotency-key space — physical isolation, not
//! filtering. oxsum's own tables (users, organizations, memberships, API keys, channels and
//! their prices, open holds, usage records) live in one `oxsum` schema, outside the ledgers;
//! see [`Db`].

mod adapters;
mod anomaly;
mod audit;
mod billing;
mod channels;
mod db;
mod deposits;
mod device;
mod discounts;
mod display;
mod email_tokens;
mod error;
mod heads;
mod holds;
mod idempotency;
mod invitations;
mod jobs;
mod keys;
mod mandates;
mod metering;
mod oauth;
mod orgs;
mod proof;
mod ratelimit;
mod reconcile;
mod report_links;
mod seed;
mod service_credentials;
mod sessions;
mod statement_pdf;
mod statements;
mod tenants;
mod tiers;
mod upstream_bills;
mod upstream_sync;
mod usage;
mod usage_reports;
mod users;
mod wallet;
mod webhooks;
mod x402;

pub use adapters::{ANTHROPIC, OPENAI, UsageAdapter, adapter_for, adapter_for_endpoint};
pub use anomaly::{AnomalyFacts, Severity, anomaly_score};
pub use audit::{AuditEntry, AuditPage, action as audit_action};
pub use billing::{
    BillLine, BillingMode, ItemizedCharge, MandateRef, MeteredSettlement, MeteredUsage, Price,
    PriceBook, PriceRule, PriceSet, RuleMatch, Settlement, SettlementKind, SettlementMandate,
    SettlementRecord, UpstreamPrices, estimate_tokens, hold_description, input_upper_bound,
};
pub use channels::{
    CatalogModel, Channel, MAX_WEIGHT, ModelAlias, ModelPrice, ProbeChannel, SecretKey, Serving,
    event_price, price_row, route_order,
};
pub use db::{Db, SCHEMA};
pub use deposits::{CodeBatch, MAX_BATCH, RAIL_HYPERSWITCH, Redemption};
pub use device::{DeviceGrant, DevicePoll, DeviceRequest, POLL_INTERVAL, PollError};
pub use discounts::{Discount, NewDiscount};
pub use display::{Display, DisplayCurrency, display_currency};
pub use doubleentry::witness::{AddCheckpoint, SignedTreeHead};
pub use doubleentry::{
    Balanced, ConsistencyProof, Entry, EntryId, Hash, InclusionProof, Seal, TreeHead,
};
pub use email_tokens::{EmailPurpose, MintedEmailToken};
pub use error::WalletError;
pub use heads::{
    Consistency, Cosigner, HeadSigningKey, KeyPublication, MemoryWitnessStore, Origin,
    PRICE_ROW_DOMAIN, SignedHead, SignedPriceHead, USAGE_REPORT_DOCUMENT, USAGE_REPORT_DOMAIN,
    UsageReportV1, VerifyingKey, Witness, WitnessCheckpoint, WitnessCosign, WitnessError,
    WitnessKeyName, WitnessStore, canonical_price_row, canonical_usage_report, origin_for,
    price_origin_for, price_row_hash, seed_from_base64, sign_head, sign_price_head, signing_key,
    usage_report_subset_hash,
};
pub use holds::{
    DEAD_AFTER_SWEEP_ATTEMPTS, DEFAULT_HOLD_TIMEOUT, InFlightHold, OpenHold, SWEEP_INTERVAL,
    SweepReport, sweep_stale_holds,
};
pub use idempotency::{Claim, fingerprint};
pub use invitations::CreatedInvitation;
pub use jobs::{
    CLAIM_LEASE_SECS, DEAD_RESCHEDULE, Job, JobOutcome, MAX_ATTEMPTS, Retention, RetentionReport,
    StatementReport, kinds, leader_lock_key, next_run, requires_leader,
};
pub use keys::{
    ActingKey, ApiKey, BudgetDuration, CreatedApiKey, KeyConstraints, TagScope, TpmScope, TpmScopes,
};
pub use mandates::{CreatedMandate, Mandate, PresentedMandate, VerifiedMandate, parse_presented};
pub use metering::{MeteredEvent, MeteredHold, MeteredOutcome};
pub use oauth::OAuthIdentity;
pub use orgs::{
    AdminOrganization, Kind, Member, MembershipActor, Organization, Ownership, Role,
    UserOrganization,
};
pub use proof::{ChargeCheck, ProofBundle, verify_bundle, verify_charge};
pub use ratelimit::{RateAllowance, RateLimited, RateLimiter, SlidingWindow};
pub use reconcile::{DriftClass, DriftKind, DriftSample, Reconciliation};
pub use report_links::{LinkScope, MintedReportLink, ReportLink, ResolvedReportLink};
pub use seed::{DEMO_EMAIL, DEMO_ORGANIZATION, DEMO_PASSWORD, MEMBER_EMAIL, SeedReport};
pub use service_credentials::{ActingService, CreatedServiceCredential, ServiceCredential};
pub use sessions::{
    CreatedSession, KeyPrincipal, KeyScope, Principal, SESSION_COOKIE, SESSION_LIFETIME, Session,
    SessionPrincipal,
};
pub use statement_pdf::render_statement_pdf;
pub use statements::{
    PaymentStatus, Statement, StatementLine, StatementPeriod, StatementStatus, statement_period,
};
pub use tenants::Tenants;
pub use tiers::TierProfile;
pub use upstream_bills::{
    ChannelBilling, DRIFT_ALERT_FLOOR_MINOR, DRIFT_ALERT_PCT, DailyBill, UpstreamBill,
    UpstreamBillProvider,
};
pub use upstream_sync::{FieldDrift, ModelPair, ProbeOutcome, diff_upstream};
pub use usage::{
    Attribution, CountsProvenance, MAX_CONTEXT_NAME, MAX_END_USER, MAX_TAG, MAX_TAGS, Margin,
    UsageDay, UsageRecord, UsageRow, validate_attribution,
};
pub use usage_reports::{ReportEntryBundle, UsageReport, UsageReportRow};
pub use users::{AccountDeletion, NewUser, Registration, User};
pub use wallet::{
    Credits, ListPage, LogEntry, Receipt, RequestEntry, SCALE, SettledEntry, SettledTurn,
    TransactionEntry, TransactionKind, Wallet, entry_id_for, settlement_key_for,
};
pub use webhooks::{
    ANOMALY_DETECTED, BALANCE_LOW, BUDGET_EXHAUSTION_FORECAST, BUDGET_THRESHOLD_CROSSED,
    BudgetForecast, CreatedWebhook, DEPOSIT_CREDITED, DEPOSIT_FAILED, DEPOSIT_REVERSED,
    DeliveryOutcome, DueDelivery, MAX_DELIVERY_ATTEMPTS, ORG_CREDIT_LIMIT_EXCEEDED, ORG_SUSPENDED,
    REQUEST_SETTLED, STATEMENT_FINALIZED, STATEMENT_OVERDUE, STATEMENT_STATUS_UPDATED,
    WebhookDelivery, WebhookEndpoint, forecast_exhaustion, retry_delay_secs, signature,
};
pub use x402::{
    ASSET_CREDIT, Ed25519RailVerifier, NETWORK_ED25519, RAIL_X402, RailSettlement, RailVerifier,
    SCHEME_EXACT, VerifiedPayment,
};

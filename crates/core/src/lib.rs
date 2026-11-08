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
mod business;
mod business_profile;
mod business_providers;
mod business_setup;
mod business_staff;
mod cache;
mod channels;
mod collection_terms;
mod credit_notes;
mod customer_authority;
mod customers;
mod db;
mod deposits;
mod device;
mod discounts;
mod display;
mod email_tokens;
mod entitlement;
mod error;
mod evidence;
mod experiments;
mod financial_facts;
mod groups;
mod guardrail;
mod heads;
mod holds;
mod idempotency;
mod integration_credentials;
mod invitation_management;
mod invitations;
mod jobs;
mod journals;
mod keys;
mod mandates;
mod manual_receipts;
mod merchant_events;
mod merchants;
mod metering;
mod oauth;
mod operations;
mod orgs;
mod payment_orders;
mod plan_statements;
mod plans;
mod pools;
mod promo_lots;
mod proof;
mod provider_destination;
mod ratelimit;
mod reconcile;
mod refunds;
mod report_links;
mod request_log;
mod routing;
mod sale_books;
mod seed;
mod service_credentials;
mod serving_access;
mod sessions;
mod statement_pdf;
mod statements;
mod subscriptions;
mod tasks;
mod teams;
mod tenants;
mod tiers;
pub mod totp;
mod upstream_bills;
mod upstream_sync;
mod usage;
mod usage_anchors;
mod usage_completeness;
mod usage_reports;
mod users;
mod wallet;
mod webhooks;
mod x402;

pub use adapters::{
    ANTHROPIC, GEMINI, OPENAI, TASKS, UsageAdapter, adapter_for, adapter_for_endpoint,
};
pub use anomaly::{AnomalyFacts, Severity, anomaly_score};
pub use audit::{AuditEntry, AuditPage, action as audit_action};
pub use billing::{
    BillLine, BillingMode, CacheRef, ExperimentRef, GuardrailRef, ItemizedCharge, MandateRef,
    MeteredSettlement, MeteredUsage, Price, PriceBook, PriceRule, PriceSet, RuleMatch, SaleRef,
    Settlement, SettlementCache, SettlementExperiment, SettlementGuardrail,
    SettlementGuardrailOwned, SettlementKind, SettlementMandate, SettlementRecord, SettlementSale,
    SettlementSubscription, SubscriptionRef, UpstreamPrices, estimate_tokens, hold_description,
    input_upper_bound,
};
pub use business::{
    Business, BusinessMigration, BusinessMigrationPreview, BusinessOwnership, CustomerProfile,
};
pub use business_profile::{
    BusinessProfile, BusinessProfileMutation, BusinessProfileRequest, PublicBusinessProfile,
};
pub use business_providers::{
    BusinessChannelPage, ProviderAliasInput, ProviderBillKeyInput, ProviderCacheInput,
    ProviderChange, ProviderCheckStart, ProviderConnection, ProviderConnectionCheck,
    ProviderCredentialInput, ProviderFallbackInput, ProviderMutationResult, ProviderPriceInput,
    ProviderRemoveInput, ProviderRequest, ProviderStatusInput,
};
pub use business_setup::{
    BusinessSetupRegistrationRequest, BusinessSetupRequest, BusinessSetupResult,
    BusinessSetupToken, BusinessSetupTokenRequest, SetupAuthority,
};
pub use business_staff::{
    BusinessCapability, BusinessMutationResult, BusinessOwnershipRequest, BusinessPrincipal,
    BusinessRegistrationRequest, BusinessRole, BusinessStaff, BusinessStaffRequest,
    BusinessStaffRevokeRequest, RegistrationMode, SessionContext, SessionContextInfo,
    SessionContextRequest,
};
pub use cache::{
    CachePolicy, CacheStore, CachedAnswer, ResponseCacheContext, response_cache_digest,
};
pub use channels::{
    CatalogModel, Channel, MAX_WEIGHT, ModelAlias, ModelPrice, ProbeChannel, SecretKey, Serving,
    price_row, route_order, route_order_stats,
};
pub use credit_notes::CreditNote;
pub use customers::{
    BusinessCustomer, BusinessCustomerLifecycle, BusinessCustomerPage, BusinessCustomerSuspension,
    BusinessCustomerUpdate, CustomerState,
};
pub use db::{Db, SCHEMA};
pub use deposits::{
    AdminDeposit, CodeBatch, HyperswitchRefundTarget, MAX_BATCH, RAIL_HYPERSWITCH, Redemption,
};
pub use device::{DeviceGrant, DevicePoll, DeviceRequest, POLL_INTERVAL, PollError};
pub use discounts::{Discount, NewDiscount};
pub use display::{Display, DisplayCurrency, display_currency};
pub use doubleentry::witness::{AddCheckpoint, SignedTreeHead};
pub use doubleentry::{
    Balanced, ConsistencyProof, Entry, EntryId, Hash, InclusionProof, MerkleAccumulator, Seal,
    TreeHead,
};
pub use email_tokens::{EmailPurpose, MintedEmailToken};
pub use entitlement::{
    Entitlement, EntitlementChecks, EntitlementMandate, EntitlementRead, EntitlementReason,
    MandateVerdict, RpmVerdict, TpmVerdict,
};
pub use error::WalletError;
pub use evidence::{
    EVIDENCE_ARTIFACT_DOMAIN, EVIDENCE_BUNDLE_DOCUMENT, EVIDENCE_MANIFEST_DOMAIN, EvidenceBundle,
    EvidenceBundleRow, EvidenceEntryBundle, EvidenceKind, EvidenceManifestV1, EvidenceScope,
    EvidenceShare, ManifestEntry, MintedEvidenceShare, ResolvedEvidenceShare,
    evidence_artifact_digest, evidence_manifest_digest,
};
pub use experiments::{Experiment, ExperimentBinding, Variant, VariantResult, pick_variant};
pub use financial_facts::{
    CostBasisCount, CostCoverage, CostCoverageDay, FactFilter, FailedAttempts, FinancialFacts,
    ReconciliationException, ReconciliationRepair, ReconciliationReport, ReconciliationRun,
    TimeBasis,
};
pub use groups::{Group, GroupChannel};
pub use guardrail::{
    Guardrail, GuardrailBinding, GuardrailPolicy, GuardrailSet, KeywordConfig, KeywordPattern,
    KeywordPolicy, ModerationConfig, PiiScanner, RequestContext, ResponseContext, Verdict,
    validate_policy_names,
};
pub use heads::{
    Consistency, Cosigner, HeadSigningKey, KeyPublication, MemoryWitnessStore, Origin,
    PRICE_ROW_DOMAIN, SALE_SNAPSHOT_DOMAIN, SignedHead, SignedPriceHead, SignedSaleHead,
    USAGE_ANCHOR_DOCUMENT, USAGE_DAY_DOMAIN, USAGE_REPORT_DOCUMENT, USAGE_REPORT_DOMAIN,
    UsageReportV1, VerifyingKey, Witness, WitnessCheckpoint, WitnessCosign, WitnessError,
    WitnessKeyName, WitnessStore, canonical_price_row, canonical_sale_snapshot,
    canonical_usage_report, origin_for, price_origin_for, price_row_hash, sale_origin_for,
    sale_snapshot_hash, seed_from_base64, sign_head, sign_price_head, sign_sale_checkpoint,
    sign_sale_head, signing_key, usage_report_subset_hash,
};
pub use holds::{
    DEAD_AFTER_SWEEP_ATTEMPTS, DEFAULT_HOLD_TIMEOUT, InFlightHold, OpenHold, SWEEP_INTERVAL,
    SweepReport, sweep_stale_holds,
};
pub use idempotency::{Claim, credential_fingerprint, fingerprint};
pub use integration_credentials::{
    ActingService, CreatedIntegrationCredential, IntegrationCredential, IntegrationCredentialPage,
    IntegrationCredentialRequest, IntegrationCredentialRevokeRequest, IntegrationScope,
    ProvisionCustomerRequest,
};
pub use invitation_management::{
    CustomerInvitationPage, CustomerInvitationSummary, InvitationDeliveryState,
    InvitationMailTarget,
};
pub use invitations::{
    CreatedInvitation, CustomerInvitationRequest, InvitationAcceptRequest, InvitationAcceptance,
    InvitationPreview, InvitationRevokeRequest, TeamInvitationRequest,
};
pub use jobs::{
    CLAIM_LEASE_SECS, DEAD_RESCHEDULE, Job, JobOutcome, MAX_ATTEMPTS, Retention, RetentionReport,
    StatementReport, kinds, leader_lock_key, next_run, requires_leader,
};
pub use journals::{
    InvoiceJournal, InvoiceJournalDetail, InvoiceJournalLine, InvoiceJournalPage, JournalLineKind,
    JournalState,
};
pub use keys::{
    ActingKey, ApiKey, BudgetDuration, CreatedApiKey, KeyConstraints, TagScope, TpmScope, TpmScopes,
};
pub use mandates::{CreatedMandate, Mandate, PresentedMandate, VerifiedMandate, parse_presented};
pub use manual_receipts::{FundingAuthority, ManualReceiptRequest};
pub use merchant_events::{
    LegacyMerchantCheckout, MerchantEventKind, MerchantEventPage, MerchantEventReceipt,
    MerchantPaymentBinding, MerchantPaymentPurpose, VerifiedMerchantEvent,
};
pub use merchants::{
    Merchant, MerchantBootstrap, MerchantCreateRequest, MerchantMode, MerchantMutationResult,
    MerchantPage, MerchantRail, MerchantSigningContext, MerchantStatusRequest, MerchantVersion,
    MerchantVersionInput, MerchantVersionPage, MerchantVersionRequest,
};
pub use metering::{MeteredEvent, MeteredHold, MeteredOutcome};
pub use oauth::{ConsumedState, OAuthIdentity};
pub use operations::{
    AttemptStatus, LEASE_SECS, METERED_OUTCOME, METERING_SURFACE, NewOperation, OperationAttempt,
    OperationSettle, OperationState, RequestOperation, SWEEPER_OWNER,
};
pub use orgs::{
    AdminOrganization, Kind, Member, MembershipActor, Organization, Ownership, Role,
    UserOrganization,
};
pub use payment_orders::{
    NewManualOrder, NewPaymentOrder, OrderRate, PaymentFulfillment, PaymentOrder, PaymentOrderPage,
    PaymentOrderPurpose, PaymentReconciliation,
};
pub use plan_statements::{
    PastDueReport, PlanCreditStanding, PlanStatement, PlanStatementDetail, PlanStatementLine,
    PlanStatementPage, PlanStatementState, StatementLineKind,
};
pub use plans::{
    CustomerPlan, CustomerPlanState, NewSoldPlan, PeriodAuthorization, PeriodKind,
    PlanAuthorizationKind, PlanPeriod, PlanPeriodState, SoldPlan, period_end_for,
};
pub use pools::{
    Affinity, CREDENTIAL_COOLDOWN, ChannelCredential, PoolCredential, order as credential_order,
};
pub use promo_lots::PromoLot;
pub use proof::{ChargeCheck, ProofBundle, verify_bundle, verify_charge};
pub use provider_destination::{
    DESTINATION_POLICY_KEY, ProviderDestinationPolicy, ProviderNetwork, public_address,
};
pub use ratelimit::{PostgresRateLimiter, RateAllowance, RateLimited, RateLimiter, SlidingWindow};
pub use reconcile::{DriftClass, DriftKind, DriftSample, Reconciliation};
pub use refunds::{
    DisputeResolution, FinanceAudit, FinanceAuditEntry, FinanceAuditPage, NewRefundRequest,
    OrderCreditNote, OrderCreditNotePage, OrderDispute, OrderDisputePage, OrderRefund,
    OrderRefundPage, REFUND_MAX_ATTEMPTS, RefundDecision, RefundRetryTarget,
};
pub use report_links::{LinkScope, MintedReportLink, ReportLink, ResolvedReportLink};
pub use request_log::{RequestLogFilter, RequestLogPage, RequestLogRow};
pub use routing::{ChannelStats, RouteStats, SharedRouteStats};
pub use seed::{DEMO_EMAIL, DEMO_ORGANIZATION, DEMO_PASSWORD, MEMBER_EMAIL, SeedReport};
pub use service_credentials::ServiceCredential;
pub use serving_access::ServingAccess;
pub use sessions::{
    CreatedSession, KeyPrincipal, KeyScope, Principal, SESSION_COOKIE, SESSION_LIFETIME, Session,
    SessionPrincipal,
};
pub use statement_pdf::render_statement_pdf;
pub use statements::{
    PaymentStatus, Statement, StatementLine, StatementPeriod, StatementStatus, statement_period,
};
pub use subscriptions::{
    OrgSubscription, ResetPeriod, RolloverReport, SubscriptionAdmission, SubscriptionPeriod,
    SubscriptionPlan,
};
pub use tasks::{
    NewTask, TASK_FAILED, TASK_SUCCEEDED, Task, TaskKind, TaskOutcome, TaskPollTarget,
    TaskProviderEndpoint, TaskSettlement, TaskState, settle_task,
};
pub use teams::Team;
pub use tenants::Tenants;
pub use tiers::TierProfile;
pub use totp::{LoginOutcome, UserTotp};
pub use upstream_bills::{
    ChannelBilling, DRIFT_ALERT_FLOOR_MINOR, DRIFT_ALERT_PCT, DailyBill, UpstreamBill,
    UpstreamBillProvider,
};
pub use upstream_sync::{FieldDrift, ModelPair, ProbeOutcome, diff_upstream};
pub use usage::{
    Attribution, CostBasis, CountsProvenance, MAX_CONTEXT_NAME, MAX_END_USER, MAX_TAG, MAX_TAGS,
    Margin, UsageDay, UsageRecord, UsageRow, validate_attribution,
};
pub use usage_anchors::UsageAnchorReport;
pub use usage_completeness::{
    CompletenessAnchor, CompletenessBundle, CompletenessDay, CompletenessHead,
};
pub use usage_reports::{ReportEntryBundle, UsageReport, UsageReportRow};
pub use users::{AccountDeletion, NewUser, Registration, User};
pub use wallet::{
    Credits, ListPage, LogEntry, Receipt, RequestEntry, SCALE, SettledEntry, SettledTurn,
    TransactionEntry, TransactionKind, Wallet, entry_id_for, limit_lock_key, settlement_key_for,
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
    SCHEME_EXACT, SettleRefusal, VerifiedPayment,
};

pub use customer_authority::{CustomerCapability, CustomerRecordScope};

pub use sale_books::{
    SaleBook, SaleBookHistory, SaleBookPage, SaleChange, SaleChangeRequest, SaleDraft,
    SaleModelMap, SaleMutationResult, SalePreviewRequest, SalePreviewSource, SalePreviewUsage,
    SalePrice, SalePricePreview, SalePriceSnapshot, SaleSchedule, SaleTerms, SaleVersion,
};

pub use collection_terms::{
    CollectionCurrency, CollectionTerms, CollectionTermsMutation, CollectionTermsRequest,
};

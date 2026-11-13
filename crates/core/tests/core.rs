//! Aggregate integration-test binary for oxsum-core (issue #998): the former
//! per-file `tests/*.rs` binaries each embedded the whole core (~127 MB), so
//! 100+ files produced tens of GB of fingerprinted artifacts. One binary links
//! it once; submodules keep each former file's tests namespaced and isolated
//! (every test builds its own fixtures; the scratch database is shared per
//! process, see oxsum-testkit).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it DB-backed tests
//! skip instead of failing.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// Each former file keeps its own helper-module instance (isolation by
// convention); loading the same support source under several per-file names is
// intentional here.
#![allow(clippy::duplicate_mod)]

#[path = "core/application_accounts.rs"]
mod application_accounts;
#[path = "core/application_checkout.rs"]
mod application_checkout;
#[path = "core/application_consent.rs"]
mod application_consent;
#[path = "core/application_customer_keys.rs"]
mod application_customer_keys;
#[path = "core/application_evidence_publication.rs"]
mod application_evidence_publication;
#[path = "core/application_evidence_reads.rs"]
mod application_evidence_reads;
#[path = "core/application_notifications.rs"]
mod application_notifications;
#[path = "core/application_serving_reads.rs"]
mod application_serving_reads;
#[path = "core/application_statements.rs"]
mod application_statements;
#[path = "core/audit.rs"]
mod audit;
#[path = "core/billing.rs"]
mod billing;
#[path = "core/bundle_upgrade.rs"]
mod bundle_upgrade;
#[path = "core/bundles.rs"]
mod bundles;
#[path = "core/business.rs"]
mod business;
#[path = "core/business_profile.rs"]
mod business_profile;
#[path = "core/business_providers.rs"]
mod business_providers;
#[path = "core/business_readiness.rs"]
mod business_readiness;
#[path = "core/business_setup.rs"]
mod business_setup;
#[path = "core/business_setup_fresh.rs"]
mod business_setup_fresh;
#[path = "core/business_staff.rs"]
mod business_staff;
#[path = "core/cache_upgrade.rs"]
mod cache_upgrade;
#[path = "core/channels.rs"]
mod channels;
#[path = "core/collection_currency_race.rs"]
mod collection_currency_race;
#[path = "core/collection_terms.rs"]
mod collection_terms;
#[path = "core/collection_upgrade.rs"]
mod collection_upgrade;
#[path = "core/customer_authority.rs"]
mod customer_authority;
#[path = "core/customer_credit.rs"]
mod customer_credit;
#[path = "core/customer_lifecycle.rs"]
mod customer_lifecycle;
#[path = "core/customer_readiness.rs"]
mod customer_readiness;
#[path = "core/customer_serving_readiness.rs"]
mod customer_serving_readiness;
#[path = "core/customer_upgrade.rs"]
mod customer_upgrade;
#[path = "core/deposit_refund_upgrade.rs"]
mod deposit_refund_upgrade;
#[path = "core/device.rs"]
mod device;
#[path = "core/diagnostics.rs"]
mod diagnostics;
#[path = "core/dispute_atomicity.rs"]
mod dispute_atomicity;
#[path = "core/dunning.rs"]
mod dunning;
#[path = "core/email_tokens.rs"]
mod email_tokens;
#[path = "core/financial_facts.rs"]
mod financial_facts;
#[path = "core/funding_preview.rs"]
mod funding_preview;
#[path = "core/generative.rs"]
mod generative;
#[path = "core/idempotency.rs"]
mod idempotency;
#[path = "core/identity.rs"]
mod identity;
#[path = "core/integration_billing_read.rs"]
mod integration_billing_read;
#[path = "core/integration_credentials.rs"]
mod integration_credentials;
#[path = "core/integration_provisioning.rs"]
mod integration_provisioning;
#[path = "core/integration_upgrade.rs"]
mod integration_upgrade;
#[path = "core/invitation_consent.rs"]
mod invitation_consent;
#[path = "core/invitation_upgrade.rs"]
mod invitation_upgrade;
#[path = "core/jobs.rs"]
mod jobs;
#[path = "core/journals.rs"]
mod journals;
#[path = "core/key_limits.rs"]
mod key_limits;
#[path = "core/legacy_chargebacks.rs"]
mod legacy_chargebacks;
#[path = "core/mandates.rs"]
mod mandates;
#[path = "core/merchant_events.rs"]
mod merchant_events;
#[path = "core/merchant_routes.rs"]
mod merchant_routes;
#[path = "core/merchants.rs"]
mod merchants;
#[path = "core/metering.rs"]
mod metering;
#[path = "core/nonplan_chargebacks.rs"]
mod nonplan_chargebacks;
#[path = "core/nonplan_refunds.rs"]
mod nonplan_refunds;
#[path = "core/oauth.rs"]
mod oauth;
#[path = "core/operations.rs"]
mod operations;
#[path = "core/parsers.rs"]
mod parsers;
#[path = "core/payment_orders.rs"]
mod payment_orders;
#[path = "core/plan_payment_upgrade.rs"]
mod plan_payment_upgrade;
#[path = "core/plan_payments.rs"]
mod plan_payments;
#[path = "core/plan_refund_conservation.rs"]
mod plan_refund_conservation;
#[path = "core/plan_refunds.rs"]
mod plan_refunds;
#[path = "core/plan_statement_locks.rs"]
mod plan_statement_locks;
#[path = "core/plan_statements.rs"]
mod plan_statements;
#[path = "core/plan_usage_corrections.rs"]
mod plan_usage_corrections;
#[path = "core/plans.rs"]
mod plans;
#[path = "core/price_heads.rs"]
mod price_heads;
#[path = "core/promo_lots.rs"]
mod promo_lots;
#[path = "core/provider_destination_upgrade.rs"]
mod provider_destination_upgrade;
#[path = "core/provider_upgrade.rs"]
mod provider_upgrade;
#[path = "core/rate_windows.rs"]
mod rate_windows;
#[path = "core/reconcile.rs"]
mod reconcile;
#[path = "core/reconciliation_upgrade.rs"]
mod reconciliation_upgrade;
#[path = "core/refunds.rs"]
mod refunds;
#[path = "core/routing_stats.rs"]
mod routing_stats;
#[path = "core/sale_books.rs"]
mod sale_books;
#[path = "core/sale_heads.rs"]
mod sale_heads;
#[path = "core/seed.rs"]
mod seed;
#[path = "core/seed_catalog.rs"]
mod seed_catalog;
#[path = "core/serving_access.rs"]
mod serving_access;
#[path = "core/sessions.rs"]
mod sessions;
#[path = "core/settlement_capacity.rs"]
mod settlement_capacity;
#[path = "core/settlement_tx.rs"]
mod settlement_tx;
#[path = "core/solana_deposits.rs"]
mod solana_deposits;
#[path = "core/staff_invitations.rs"]
mod staff_invitations;
#[path = "core/statement_review_finance.rs"]
mod statement_review_finance;
#[path = "core/statements.rs"]
mod statements;
#[path = "core/subscriptions.rs"]
mod subscriptions;
#[path = "core/suspension.rs"]
mod suspension;
#[path = "core/teams.rs"]
mod teams;
#[path = "core/terms.rs"]
mod terms;
#[path = "core/tpm_windows.rs"]
mod tpm_windows;
#[path = "core/upstream_sync.rs"]
mod upstream_sync;
#[path = "core/usage_daily.rs"]
mod usage_daily;

#[path = "core/wallet.rs"]
mod wallet;
#[path = "core/webhooks.rs"]
mod webhooks;

#[path = "core/customer_statements.rs"]
mod customer_statements;

#[path = "core/monthly_repayment_upgrade.rs"]
mod monthly_repayment_upgrade;

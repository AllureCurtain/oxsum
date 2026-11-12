//! Aggregate integration-test binary for oxsum-server (issue #998): the former
//! per-file `tests/*.rs` binaries each embedded the whole server (~127 MB), so
//! 100+ files produced tens of GB of fingerprinted artifacts. One binary links
//! it once; submodules keep each former file's tests namespaced and isolated
//! (every test builds its own app and unique fixtures; the scratch database is
//! shared per process, see oxsum-testkit).
//!
//! Requires DATABASE_URL, see docs/development.md. Without it DB-backed tests
//! skip instead of failing.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// Each former file keeps its own helper-module instance (isolation by
// convention); loading the same support source under several per-file names is
// intentional here.
#![allow(clippy::duplicate_mod)]

#[path = "support/mod.rs"]
mod support;

#[path = "server/account_deletion.rs"]
mod account_deletion;
#[path = "server/admin.rs"]
mod admin;
#[path = "server/airwallex.rs"]
mod airwallex;
#[path = "server/antibot.rs"]
mod antibot;
#[path = "server/api.rs"]
mod api;
#[path = "server/apis.rs"]
mod apis;
#[path = "server/application_accounts.rs"]
mod application_accounts;
#[path = "server/application_checkout.rs"]
mod application_checkout;
#[path = "server/application_consent.rs"]
mod application_consent;
#[path = "server/application_customer_keys.rs"]
mod application_customer_keys;
#[path = "server/application_evidence_publication.rs"]
mod application_evidence_publication;
#[path = "server/application_evidence_reads.rs"]
mod application_evidence_reads;
#[path = "server/application_notifications.rs"]
mod application_notifications;
#[path = "server/application_serving_reads.rs"]
mod application_serving_reads;
#[path = "server/application_statements.rs"]
mod application_statements;
#[path = "server/audit.rs"]
mod audit;
#[path = "server/bills.rs"]
mod bills;
#[path = "server/branding.rs"]
mod branding;
#[path = "server/browser_contract.rs"]
mod browser_contract;
#[path = "server/business.rs"]
mod business;
#[path = "server/business_console.rs"]
mod business_console;
#[path = "server/business_prices.rs"]
mod business_prices;
#[path = "server/business_providers.rs"]
mod business_providers;
#[path = "server/business_readiness.rs"]
mod business_readiness;
#[path = "server/business_setup.rs"]
mod business_setup;
#[path = "server/business_setup_fresh.rs"]
mod business_setup_fresh;
#[path = "server/business_staff.rs"]
mod business_staff;
#[path = "server/cache.rs"]
mod cache;
#[path = "server/capacity.rs"]
mod capacity;
#[path = "server/chat_flow.rs"]
mod chat_flow;
#[path = "server/checkout_routes.rs"]
mod checkout_routes;
#[path = "server/consumption_scope.rs"]
mod consumption_scope;
#[path = "server/coordinated.rs"]
mod coordinated;
#[path = "server/customer_credit.rs"]
mod customer_credit;
#[path = "server/customer_invitation_delivery.rs"]
mod customer_invitation_delivery;
#[path = "server/customer_lifecycle.rs"]
mod customer_lifecycle;
#[path = "server/customer_readiness.rs"]
mod customer_readiness;
#[path = "server/dashboard.rs"]
mod dashboard;
#[path = "server/device.rs"]
mod device;
#[path = "server/display_currency.rs"]
mod display_currency;
#[path = "server/email.rs"]
mod email;
#[path = "server/embedded_application.rs"]
mod embedded_application;
#[path = "server/entitlement.rs"]
mod entitlement;
#[path = "server/evidence_bundles.rs"]
mod evidence_bundles;
#[path = "server/examples_surface.rs"]
mod examples_surface;
#[path = "server/experiments.rs"]
mod experiments;
#[path = "server/fallbacks.rs"]
mod fallbacks;
#[path = "server/faults.rs"]
mod faults;
#[path = "server/financial_authority.rs"]
mod financial_authority;
#[path = "server/financial_facts.rs"]
mod financial_facts;
#[path = "server/gateway.rs"]
mod gateway;
#[path = "server/groups.rs"]
mod groups;
#[path = "server/guardrails.rs"]
mod guardrails;
#[path = "server/heads.rs"]
mod heads;
#[path = "server/hyperswitch.rs"]
mod hyperswitch;
#[path = "server/install_ops.rs"]
mod install_ops;
#[path = "server/integration_billing_read.rs"]
mod integration_billing_read;
#[path = "server/integration_credentials.rs"]
mod integration_credentials;
#[path = "server/invitation_consent.rs"]
mod invitation_consent;
#[path = "server/invitations.rs"]
mod invitations;
#[path = "server/jobs.rs"]
mod jobs;
#[path = "server/journals.rs"]
mod journals;
#[path = "server/mandates.rs"]
mod mandates;
#[path = "server/members.rs"]
mod members;
#[path = "server/merchant_events.rs"]
mod merchant_events;
#[path = "server/merchants.rs"]
mod merchants;
#[path = "server/metering.rs"]
mod metering;
#[path = "server/metrics.rs"]
mod metrics;
#[path = "server/oauth.rs"]
mod oauth;
#[path = "server/oidc.rs"]
mod oidc;
#[path = "server/operations.rs"]
mod operations;
#[path = "server/operator_diagnostics.rs"]
mod operator_diagnostics;
#[path = "server/orgs.rs"]
mod orgs;
#[path = "server/ownership_preview.rs"]
mod ownership_preview;
#[path = "server/payments.rs"]
mod payments;
#[path = "server/plan_statements.rs"]
mod plan_statements;
#[path = "server/plans.rs"]
mod plans;
#[path = "server/promo_lots.rs"]
mod promo_lots;
#[path = "server/provider_destinations.rs"]
mod provider_destinations;
#[path = "server/rate_limit_backend.rs"]
mod rate_limit_backend;
#[path = "server/real_upstream.rs"]
mod real_upstream;
#[path = "server/real_upstream_anthropic.rs"]
mod real_upstream_anthropic;
#[path = "server/refunds.rs"]
mod refunds;
#[path = "server/rejections.rs"]
mod rejections;
#[path = "server/requests.rs"]
mod requests;
#[path = "server/requests_log.rs"]
mod requests_log;
#[path = "server/serving.rs"]
mod serving;
#[path = "server/serving_catalog.rs"]
mod serving_catalog;
#[path = "server/sessions.rs"]
mod sessions;
#[path = "server/spa.rs"]
mod spa;
#[path = "server/staff_invitation_delivery.rs"]
mod staff_invitation_delivery;
#[path = "server/statements.rs"]
mod statements;
#[path = "server/stripe.rs"]
mod stripe;
#[path = "server/subscriptions.rs"]
mod subscriptions;
#[path = "server/suspension.rs"]
mod suspension;
#[path = "server/sweep.rs"]
mod sweep;
#[path = "server/tasks.rs"]
mod tasks;
// The collector test needs the exporter built: the suite only exists under the
// crate's default `otlp` feature.
#[cfg(feature = "otlp")]
#[path = "server/telemetry.rs"]
mod telemetry;
#[path = "server/terms.rs"]
mod terms;
#[path = "server/totp.rs"]
mod totp;
#[path = "server/usage.rs"]
mod usage;
#[path = "server/usage_reports.rs"]
mod usage_reports;
#[path = "server/webhooks.rs"]
mod webhooks;
#[path = "server/witness.rs"]
mod witness;
#[path = "server/x402.rs"]
mod x402;

#[path = "server/customer_statements.rs"]
mod customer_statements;

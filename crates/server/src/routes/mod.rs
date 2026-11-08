//! The `/api/v1` surface and the routes served outside it: health, the gateway,
//! the billing WebSocket, the bills export and the dashboard pages.
//!
//! One module per resource group; `mod` itself only wires the router and holds
//! the envelope types and helpers every module shares.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router, middleware};
use oxsum_core::{
    Organization, Principal, Role, SESSION_COOKIE, Session, SessionPrincipal, Tenants, User, Wallet,
};
use serde::{Deserialize, Serialize};
use tower_http::timeout::TimeoutLayer;
use uuid::Uuid;

use crate::AppState;
use crate::config::Branding;
use crate::error::ApiError;

mod auth;
pub(crate) mod billing;
mod business;
mod business_invitations;
mod business_prices;
mod business_providers;
mod device;
mod evidence;
mod experiments;
mod financial_facts;
mod journals;
mod keys;
mod log;
mod merchant_events;
mod merchants;
pub(crate) mod operations;
mod ops;
mod orders;
mod org;
mod plans;
mod refunds;
mod report_links;
pub(crate) mod requests;
mod statements;
pub(crate) mod usage;
mod usage_reports;
mod webhooks;
mod witness;

/// Backstop for every request on the API and gateway surfaces: no handler may
/// run longer than this. It bounds handler execution — the time until the
/// response headers are produced — not a streaming body, which keeps flowing
/// after the headers: a stalled mid-stream upstream is the gateway client's
/// own timeout policy, deliberately untouched here (see `gateway/mod.rs`).
/// The billing WebSocket, the pages and the health/metrics endpoints are
/// excluded on purpose: the socket is long-lived by design, the rest answer
/// in milliseconds and need no backstop.
///
/// On timeout the client gets a bare 408, not the uniform error envelope:
/// the envelope is built by handlers, and a handler that never answered in
/// fifteen minutes is past the point where envelope fidelity matters.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The `/api/v1` surface, plus the routes served outside it: health, the gateway, the
/// billing WebSocket, the bills export and the dashboard pages.
pub fn router(state: AppState) -> Router {
    // Behind a credential: everything that touches an organization, its ledger or its
    // credentials. The credential is an API key or a session cookie; both resolve to a
    // principal.
    let authenticated = Router::new()
        .route("/org", get(org::organization).patch(org::update_display))
        .route(
            "/orgs",
            get(org::my_organizations).post(org::create_organization),
        )
        .route("/org/keys", get(keys::list_keys).post(keys::create_key))
        .route("/org/groups", get(keys::list_groups))
        .route(
            "/org/keys/{key_id}",
            delete(keys::revoke_key).patch(keys::patch_key),
        )
        .route(
            "/org/keys/{key_id}/mandates",
            get(keys::list_mandates).post(keys::create_mandate),
        )
        .route("/org/keys/{key_id}/entitlement", get(keys::key_entitlement))
        .route("/org/members", get(org::list_members).post(org::add_member))
        .route(
            "/org/members/{user_id}",
            delete(org::remove_member).patch(org::update_member),
        )
        .route("/org/teams", get(org::list_teams).post(org::create_team))
        .route(
            "/org/teams/{team_id}",
            patch(org::update_team).delete(org::delete_team),
        )
        // Account-wide experiments require a current owner/admin session.
        .route("/org/experiments", get(experiments::list_experiments))
        .route("/org/experiments/{name}", put(experiments::put_experiment))
        .route(
            "/org/experiments/{name}/end",
            post(experiments::end_experiment),
        )
        .route(
            "/org/experiments/{experiment_id}/results",
            get(experiments::experiment_results),
        )
        .route("/org/promo-lots", get(org::promo_lots))
        .route("/org/ownership", post(org::transfer_ownership))
        .route("/org/subscription", get(org::subscription))
        .route(
            "/org/subscription/checkout",
            post(billing::subscription_checkout),
        )
        .route("/org/subscription/periods", get(org::subscription_periods))
        // The sold-plan catalogue and the caller's own plan (issue #610).
        .route("/plans", get(plans::list_plans))
        .route("/plans/{plan_version_id}", get(plans::get_plan))
        .route("/org/plan", get(plans::my_plan))
        .route("/org/plan/cancel", post(plans::cancel_my_plan))
        .route("/org/plan/change", post(plans::change_my_plan))
        // The caller's plan-fee journals (issue #611, T20).
        .route("/org/plan/journals", get(journals::my_journals))
        .route("/org/plan/journals/{journal_id}", get(journals::my_journal))
        // The caller's plan statements (issue #612, T21).
        .route("/org/statements", get(statements::my_statements))
        .route(
            "/org/statements/{statement_id}",
            get(statements::my_statement),
        )
        // The statement-scoped evidence bundle (issue #616, T25): mint on
        // first ask, serve the standing bytes after.
        .route(
            "/org/statements/{statement_id}/evidence",
            get(evidence::statement_evidence),
        )
        // The complete-period evidence bundles (issue #616, T25): mint, list,
        // download, and the share tokens that open the public download.
        .route(
            "/org/evidence-bundles",
            get(evidence::list).post(evidence::mint_period),
        )
        .route("/org/evidence-bundles/{bundle_id}", get(evidence::download))
        .route(
            "/org/evidence-bundles/{bundle_id}/share",
            get(evidence::list_shares)
                .post(evidence::create_share)
                .delete(evidence::revoke_shares),
        )
        .route(
            "/webhooks",
            get(webhooks::list_webhooks).post(webhooks::create_webhook),
        )
        .route("/webhooks/{endpoint_id}", delete(webhooks::delete_webhook))
        .route(
            "/webhooks/{endpoint_id}/deliveries",
            get(webhooks::webhook_deliveries),
        )
        .route("/session", get(org::session))
        .route("/session/organization", post(org::switch_organization))
        // The device grant's user leg — a person's verdict, so the handlers
        // refuse an API key, which names no user (issue #156).
        .route("/device/request", get(device::device_request))
        .route("/device/authorize", post(device::device_authorize))
        .route("/topups", post(billing::top_up))
        .route("/topups/stripe", post(billing::stripe_topup))
        .route("/topups/airwallex", post(billing::airwallex_topup))
        .route("/topups/hyperswitch", post(billing::hyperswitch_topup))
        .route("/topups/x402", post(billing::x402_topup))
        .route("/redemptions", post(billing::redeem))
        .route(
            "/orders",
            get(orders::list_orders).post(orders::create_order),
        )
        .route("/orders/{order_id}", get(orders::get_order))
        .route(
            "/orders/{order_id}/fulfillment",
            get(orders::order_fulfillment),
        )
        .route(
            "/orders/{order_id}/checkout-session",
            get(orders::checkout_session),
        )
        // The customer's refund ask: a request for review, never a grant.
        .route(
            "/orders/{order_id}/refund-request",
            post(refunds::request_refund),
        )
        .route("/orders/{order_id}/refunds", get(refunds::order_refunds))
        .route("/holds", get(billing::list_holds).post(billing::hold))
        .route("/bills", get(billing::bills))
        .route("/entries", get(log::entries))
        .route("/settlements", post(billing::settle))
        .route("/statements", get(billing::statements))
        .route("/statements/{statement_id}", get(billing::statement_detail))
        .route(
            "/statements/{statement_id}/proof",
            get(billing::statement_proof),
        )
        .route(
            "/statements/{statement_id}/export.pdf",
            get(billing::statement_pdf),
        )
        .route("/balance", get(billing::balance))
        .route("/usage", get(usage::usage))
        .route("/budget", get(usage::budget))
        .route("/billing-records", get(usage::billing_records))
        .route("/requests", get(requests::requests))
        .route("/operations", get(operations::operations))
        .route("/operations/{operation_id}", get(operations::operation))
        .route(
            "/org/usage-reports",
            get(usage_reports::list).post(usage_reports::mint),
        )
        .route("/org/usage-reports/{report_id}", get(usage_reports::get))
        .route(
            "/org/usage-reports/links",
            get(report_links::list).post(report_links::create_for_end_user),
        )
        .route(
            "/org/usage-reports/links/{link_id}",
            delete(report_links::revoke),
        )
        .route(
            "/org/usage-reports/{report_id}/links",
            post(report_links::create_for_report),
        )
        // The customer's own eight-category projection (issue #613, T22) —
        // the same facts read the staff surface runs, scoped to the caller.
        .route("/org/usage-report", get(financial_facts::org_usage_report))
        .route("/estimate-price", post(usage::estimate_price))
        .route("/pricing", get(usage::pricing))
        .route("/entries/{entry_id}/proof", get(log::proof))
        .route("/log/head", get(log::log_head))
        .route("/log/consistency", get(log::log_consistency))
        .route("/log/checkpoint", get(log::log_checkpoint))
        .route("/log/price-head", get(log::log_price_head))
        .route("/log/sale-head", get(log::log_sale_head))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_principal,
        ));

    // No credential: signup while the deployment allows it, and the login/logout pair.
    // Logout sits outside the auth middleware on purpose: logging out twice is not an
    // error, so it must answer 200 with no session at all.
    // Personal account handlers authenticate their own session in either context.
    let open = Router::new()
        // The deployment's own identity: version, build SHA and the schema the
        // binary migrates to. Unauthenticated — the upgrade runbook reads it
        // before any account exists (issue #619).
        .route("/version", get(ops::version))
        .route("/auth/totp/disable", post(auth::totp_disable))
        .route("/auth/totp/confirm", post(auth::totp_confirm))
        .route("/auth/totp/enroll", post(auth::totp_enroll))
        .route("/auth/verify/request", post(auth::request_verification))
        .route("/users/me", delete(org::delete_account))
        .route("/auth/register", post(auth::register))
        .route(
            "/session/context",
            get(business::context).post(business::select_context),
        )
        .route("/business", get(business::business))
        .route("/business/sale-books", get(business_prices::books))
        .route(
            "/business/sale-books/{book_id}",
            get(business_prices::history),
        )
        .route(
            "/business/sale-books/changes",
            post(business_prices::change),
        )
        .route(
            "/business/customers/{organization_id}/sale-terms",
            get(business_prices::customer),
        )
        .route(
            "/business/sale-price-preview",
            post(business_prices::preview),
        )
        .route(
            "/business/merchants/{merchant}/events",
            get(merchant_events::events),
        )
        .route(
            "/business/payment-reconciliation",
            get(orders::reconciliation),
        )
        // The staff finance surface: refund resolution, corrections and
        // contested charges (issue #608).
        .route("/business/refunds", get(refunds::business_refunds))
        .route(
            "/business/refunds/{refund_id}",
            get(refunds::business_refund),
        )
        .route(
            "/business/refunds/{refund_id}/resolve",
            post(refunds::resolve_refund),
        )
        .route(
            "/business/credit-notes",
            get(refunds::business_credit_notes),
        )
        .route("/business/disputes", get(refunds::business_disputes))
        .route("/business/audit", get(refunds::business_audit))
        .route(
            "/business/disputes/{dispute_id}/resolve",
            post(refunds::resolve_dispute),
        )
        .route(
            "/business/orders",
            get(orders::business_orders).post(orders::create_manual_order),
        )
        .route(
            "/webhooks/merchants/{merchant}/versions/{version}",
            post(merchant_events::webhook),
        )
        .route(
            "/business/merchants",
            get(merchants::list).post(merchants::create),
        )
        .route(
            "/business/merchants/{merchant}/versions",
            get(merchants::versions).post(merchants::rotate),
        )
        .route(
            "/business/merchants/{merchant}/status",
            post(merchants::status),
        )
        .route(
            "/business/channels/{channel}/connection-check",
            post(business_providers::connection_check),
        )
        .route(
            "/business/channels",
            get(business_providers::channels).post(business_providers::set_channel),
        )
        .route(
            "/business/channels/{channel}/prices",
            get(business_providers::prices).post(business_providers::append_price),
        )
        .route(
            "/business/channels/{channel}/aliases",
            get(business_providers::aliases).post(business_providers::set_alias),
        )
        .route(
            "/business/channels/{channel}/aliases/{alias}/remove",
            post(business_providers::remove_alias),
        )
        .route(
            "/business/channels/{channel}/credentials",
            post(business_providers::add_credential),
        )
        .route(
            "/business/channels/{channel}/credentials/{credential}/status",
            post(business_providers::credential_status),
        )
        .route(
            "/business/channels/{channel}/credentials/{credential}/remove",
            post(business_providers::remove_credential),
        )
        .route(
            "/business/channels/{channel}/billing-credential",
            post(business_providers::bill_key),
        )
        .route(
            "/business/channels/{channel}/cache",
            post(business_providers::cache),
        )
        .route(
            "/business/model-fallbacks/{model}",
            get(business_providers::fallbacks).put(business_providers::set_fallbacks),
        )
        .route(
            "/business/integration-credentials",
            get(business::integration_credentials).post(business::create_integration_credential),
        )
        .route(
            "/business/integration-credentials/{credential_id}/revoke",
            post(business::revoke_integration_credential),
        )
        .route(
            "/business/customers",
            get(business::customers).post(business::provision_customer),
        )
        .route(
            "/business/customers/{organization_id}",
            get(business::customer).patch(business::update_customer),
        )
        // The sold-plan catalogue staff CRUD and a customer's binding
        // (issue #610).
        .route(
            "/business/plans",
            get(plans::staff_plans).post(plans::publish_plan),
        )
        .route(
            "/business/plans/{plan_version_id}/retire",
            post(plans::retire_plan),
        )
        .route(
            "/business/customers/{organization_id}/plan",
            get(plans::customer_plan).put(plans::bind_customer_plan),
        )
        .route(
            "/business/customers/{organization_id}/plan/cancel",
            post(plans::cancel_customer_plan),
        )
        // The staff plan-fee journal read and void (issue #611, T20).
        .route("/business/journals", get(journals::business_journals))
        .route(
            "/business/journals/{journal_id}",
            get(journals::business_journal),
        )
        .route(
            "/business/journals/{journal_id}/void",
            post(journals::void_journal),
        )
        // The staff financial-facts, cost-coverage and reconciliation surface
        // (issue #613, T22): reads under ReportsRead, the manual run and the
        // exception close/repair under FundsWrite.
        .route(
            "/business/financial-facts",
            get(financial_facts::business_financial_facts),
        )
        .route(
            "/business/cost-coverage",
            get(financial_facts::business_cost_coverage),
        )
        .route(
            "/business/reconciliation-runs",
            get(financial_facts::business_runs).post(financial_facts::request_run),
        )
        .route(
            "/business/reconciliation-runs/{run_id}",
            get(financial_facts::business_run),
        )
        .route(
            "/business/reconciliation-runs/{run_id}/exceptions",
            get(financial_facts::business_run_exceptions),
        )
        .route(
            "/business/reconciliation-exceptions",
            get(financial_facts::business_exceptions),
        )
        .route(
            "/business/reconciliation-exceptions/{exception_id}/resolve",
            post(financial_facts::resolve_exception),
        )
        .route(
            "/business/reconciliation-exceptions/{exception_id}/waive",
            post(financial_facts::waive_exception),
        )
        .route(
            "/business/reconciliation-exceptions/{exception_id}/repairs",
            post(financial_facts::repair_exception),
        )
        // The staff plan-statement read and release-review (issue #612, T21).
        .route("/business/statements", get(statements::business_statements))
        .route(
            "/business/statements/{statement_id}",
            get(statements::business_statement),
        )
        .route(
            "/business/statements/{statement_id}/release-review",
            post(statements::release_review),
        )
        .route(
            "/business/customers/{organization_id}/lifecycle",
            post(business::customer_lifecycle),
        )
        .route(
            "/business/customers/{organization_id}/suspension",
            post(business::customer_suspension),
        )
        .route(
            "/business/staff",
            get(business::staff).post(business::set_staff),
        )
        .route("/business/staff/revoke", post(business::revoke_staff))
        .route("/business/ownership", post(business::transfer_ownership))
        .route("/business/registration", post(business::registration))
        .route("/business/setup", post(business::accept_setup))
        .route("/business/setup/register", post(business::register_owner))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        // The login's second factor (issue #504): enrollment needs a session,
        // and the second leg trades a challenge plus a code for one — the
        // challenge is the credential, so it stays open.
        .route("/auth/totp", post(auth::totp_login))
        // The seller branding (issue #614, T23): a public cosmetic read — the
        // login and register pages brand themselves before any session exists.
        .route("/branding", get(branding))
        // OAuth is a browser redirect, not a JSON endpoint: the provider sends
        // the browser here, so both answers are `Location` headers, and every
        // failure lands back on /login rather than on an error body.
        .route("/auth/oauth/{provider}", get(auth::oauth_start))
        .route("/auth/oauth/{provider}/callback", get(auth::oauth_callback))
        // Which login methods the deployment offers: the login page's only
        // unauthenticated read, so it knows whether to draw the GitHub button
        // and which OIDC provider buttons to draw.
        .route("/auth/methods", get(auth::auth_methods))
        // The email flows: verification consumes a mailed token, and the reset
        // pair is open by design — the token is the credential.
        .route("/auth/verify", post(auth::verify_email))
        .route("/auth/password/forgot", post(auth::forgot_password))
        .route("/auth/password/reset", post(auth::reset_password))
        // Registration through an invitation link: the token is the credential, so it
        // stays open in invite mode too — that mode exists for this.
        .route("/invitations/redeem", post(org::redeem_invitation))
        .route("/invitations/accept", post(org::accept_invitation))
        .route("/invitations/preview", post(org::preview_invitation))
        .route("/org/invitations", post(org::create_invitation))
        .route(
            "/org/invitations/{invitationId}/revoke",
            post(org::revoke_invitation),
        )
        .route(
            "/business/customer-invitations",
            get(business_invitations::list).post(business_invitations::create),
        )
        .route(
            "/business/customer-invitations/{invitationId}/revoke",
            post(business::revoke_customer_invitation),
        )
        // The device grant's tool leg: the device code is the credential, so
        // both endpoints stay open in every signup mode (issue #156).
        .route("/device/code", post(device::device_code))
        .route("/device/token", post(device::device_token))
        // The operator's tree-head verifying key: a public key, so no credential.
        .route("/log/key", get(log::log_key))
        // The public evidence download (issue #616): the share token in the
        // path is the credential, so it sits open like the webhooks — the
        // token resolves or it does not, and a dead one earns 410.
        .route("/evidence/{token}", get(evidence::public_download))
        // Stripe's webhook: the provider calls here, the `Stripe-Signature`
        // header is the credential — org-agnostic, so it must not sit behind
        // the session or the API key.
        .route("/webhooks/stripe", post(billing::stripe_webhook))
        // Airwallex's webhook (issue #285): same open-surface rule — the
        // `x-timestamp` + `x-signature` pair is the credential.
        .route("/webhooks/airwallex", post(billing::airwallex_webhook))
        // The Hyperswitch switch's webhook (issue #319): the HMAC over the
        // raw body (`X-Webhook-Signature-512`) is the credential.
        .route("/webhooks/hyperswitch", post(billing::hyperswitch_webhook));

    // The platform admin: an operator token, not an organization key, and its own middleware.
    let admin = crate::admin::router(state.clone());
    // The metering surface's own credential: a service credential, never an
    // organization's key or a session — an organization can never report its
    // own usage (issue #172).
    let metering = crate::metering::router(state.clone());

    // The metrics scrape sits at the conventional path rather than under /api/v1/admin, but
    // it answers to the same operator token — a Prometheus scrape config carries it as
    // `bearer_token`.
    let operator = Router::new()
        .route("/metrics", get(crate::metrics::scrape))
        // `route_layer`, not `layer`: a plain layer on a merged router also wraps the
        // combined fallback, so an unmatched path would answer 401 instead of 404.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::admin::require_admin,
        ));

    let router: Router<AppState> = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // Readiness, not liveness: 200 only once the database answers and its
        // migration ledger is current. The compose `healthcheck` gates on this,
        // so `docker compose ps` shows "healthy" only for a serving deployment
        // (issue #619).
        .route("/readyz", get(ops::readyz))
        // The deployment's own tlog-witness (issue #260): a foreign log offers
        // its head at the C2SP path, no org credential — the submitting log is
        // named by the origin inside its note, and the witness's trust roots
        // decide whether to vouch. On a deployment without
        // `OXSUM_WITNESS_SIGNING_KEY` the handler answers 404 like any path.
        .route("/add-checkpoint", post(witness::add_checkpoint))
        .merge(operator)
        // Live billing progress for the dashboard: session-cookie auth, like the pages.
        .route("/ws/billing", get(crate::ws::billing_ws))
        // The bills export: the page's table as a file the browser saves, on the
        // dashboard's own surface (the session cookie), not `/api/v1` (docs/decisions.md).
        .route("/dashboard/bills/export.csv", get(crate::bills::export_csv))
        .route(
            "/dashboard/bills/export.json",
            get(crate::bills::export_json),
        )
        .nest(
            "/api/v1",
            open.merge(authenticated)
                .nest("/admin", admin)
                .nest("/metering", metering)
                // The backstop sits on the API and gateway nests, not the top
                // router: the billing WebSocket below is long-lived by design
                // and must never be cut by a request timeout.
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    REQUEST_TIMEOUT,
                )),
        )
        // The OpenAI-compatible surface, which brings its own auth and its own error format.
        // The task relay rides under the same `/v1` prefix and the same
        // gateway auth (issue #503): `POST /v1/tasks` submits,
        // `GET /v1/tasks/{id}` polls, `POST /v1/tasks/{id}/cancel` cancels.
        .nest(
            "/v1",
            crate::gateway::router(state.clone())
                .merge(crate::tasks::router(state.clone()))
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    REQUEST_TIMEOUT,
                )),
        )
        // The Gemini surface (issue #494): the same gateway, under the
        // versioned prefix a Gemini client spells. Its own router carries the
        // same auth and timeout as `/v1`.
        .nest(
            "/v1beta",
            crate::gateway::beta_router(state.clone()).layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                REQUEST_TIMEOUT,
            )),
        );
    // The React dashboard (issue #234): `web/dist` embedded into the
    // binary and served at `/app`, inside the router rather than the fallback
    // so it wins over the surviving page routes.
    let router = router.nest("/app", crate::spa::router());
    // The dashboard pages, served by the same binary.
    // The request counter sits last so it sees every route the router answered, pages
    // included — labelled by the matched pattern, never the concrete path.
    crate::web::mount(router, &state)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::metrics::track,
        ))
        .with_state(state)
}

/// The uniform success envelope: { "data": ... }.
#[derive(Serialize)]
pub(crate) struct Data<T> {
    data: T,
}

pub(crate) type ApiResult<T> = Result<Json<Data<T>>, ApiError>;

pub(crate) fn ok<T>(data: T) -> ApiResult<T> {
    Ok(Json(Data { data }))
}

/// `GET /api/v1/branding` (issue #614, T23): the seller branding the customer
/// portal applies — a pure config read, so no credential and no store. Every
/// field is cosmetic: none of it reaches pricing, the ledger or a proof.
async fn branding(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> ApiResult<Branding> {
    ok(state.config.branding().clone())
}
/// Who a login authenticated, as the API presents it: the user, the organization the
/// session acts as, the role in it, and the session itself.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionInfo {
    user: User,
    organization: Organization,
    role: Role,
    session: Session,
}

impl From<SessionPrincipal> for SessionInfo {
    fn from(principal: SessionPrincipal) -> Self {
        Self {
            user: principal.user,
            organization: principal.organization,
            role: principal.role,
            session: principal.session,
        }
    }
}
pub(crate) async fn wallet(
    tenants: &Tenants,
    organization: &Organization,
) -> Result<Arc<Wallet>, ApiError> {
    Ok(tenants.get(&organization.tenant_id).await?)
}
pub(crate) fn session_cookie(token: &str, secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The `Set-Cookie` value that clears the session cookie: an empty value that expires
/// immediately. `Max-Age=0` rather than an `Expires` date in the past: one mechanism, no
/// date formatting.
pub(crate) fn clear_session_cookie(secure: bool) -> String {
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    cookie
}

/// The device-grant person check both user-side handlers share: a session
/// names a user, an API key does not.
pub(crate) fn person_only(principal: &Principal) -> Result<Uuid, ApiError> {
    principal.user_id().ok_or_else(|| {
        ApiError::Forbidden(
            "a device request's verdict is a person's action: an API key names no user".into(),
        )
    })
}

/// Who may act on memberships of *several* organizations: a person, never a key.
///
/// Listing, creating and switching between organizations are all a person's actions —
/// an API key belongs to exactly one organization and has no user to hold a second
/// membership, so it is refused the same way membership management refuses it
/// (docs/decisions.md, "membership management is a person's action").
pub(crate) fn person(principal: Principal) -> Result<SessionPrincipal, ApiError> {
    match principal {
        Principal::Session(principal) => Ok(principal),
        Principal::Key(_) => Err(ApiError::Forbidden(
            "organizations are a person's business: an API key belongs to one".into(),
        )),
    }
}

/// The `/verify` URL the PDF's footer and QR point at: `OXSUM_PUBLIC_URL`
/// plus the path when the deployment names one, else the relative path — the
/// same rule `verification_uri` uses for the device page.
pub(crate) fn verify_url(state: &AppState) -> String {
    match state.config.public_url() {
        Some(base) => format!("{}/verify", base.trim_end_matches('/')),
        None => "/verify".to_owned(),
    }
}

pub(crate) fn parse_day(value: &str) -> Result<time::Date, ApiError> {
    time::Date::parse(
        value,
        &time::macros::format_description!("[year]-[month]-[day]"),
    )
    .map_err(|_| ApiError::Validation(format!("{value:?} is not a YYYY-MM-DD date")))
}

/// Peels one option layer serde does not: a bare `Option<Option<i64>>` field
/// answers `None` for both an absent field and an explicit `null`, while the
/// PATCH needs `null` to mean "clear the budget" (issue #162).
pub(crate) fn double_option_i64<'de, D>(deserializer: D) -> Result<Option<Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<i64>::deserialize(deserializer).map(Some)
}

/// The one address rule for a link that will be mailed: a single address the
/// mailer can parse. A malformed one is a caller mistake, not a reason to mint
/// a link the operator believes was mailed (bill links, issue #315; invitation
/// links, issue #567).
pub(crate) fn recipient_email(email: Option<&str>) -> Result<Option<&str>, ApiError> {
    let Some(email) = email.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    let valid = email.len() <= 254
        && email.matches('@').count() == 1
        && !email.starts_with('@')
        && !email.ends_with('@')
        && !email.chars().any(char::is_whitespace);
    if valid {
        Ok(Some(email))
    } else {
        Err(ApiError::Validation(
            "email must be a single address".into(),
        ))
    }
}

/// The [`double_option_i64`] peel for a uuid field — `null` means "clear the
/// assignment", a value sets it (issue #440, `teamId` on a member PATCH).
pub(crate) fn double_option_uuid<'de, D>(
    deserializer: D,
) -> Result<Option<Option<uuid::Uuid>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<uuid::Uuid>::deserialize(deserializer).map(Some)
}

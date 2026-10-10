//! The platform admin surface: channels, and the prices they charge.
//!
//! Whoever deploys oxsum is the platform admin (docs/product.md), so the credential here is an
//! operator token from the environment rather than an organization's API key: pointing the gateway
//! at another upstream and changing what a request costs is not something an organization does to
//! itself. The token is compared in constant time, and a deployment that sets none has no admin
//! surface at all rather than an open one.
//!
//! Writing a price appends a version; it never rewrites one (see [`oxsum_core::Channel`]). A request
//! resolves the version in force when it starts and carries it to its settlement, so a price change
//! lands on later requests and leaves in-flight turns and old bills where they are. Web sessions
//! (TODO item 4) and the dashboard (TODO item 5) drive these same endpoints.

use axum::extract::{Path, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put};
use axum::{Router, middleware};
use oxsum_core::{
    AuditEntry, Channel, Discount, InFlightHold, Kind, ModelPrice, NewDiscount, Price,
    Reconciliation, Seal, SettlementKind, TierProfile, audit_action,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use time::OffsetDateTime;
use time::{Date, Month};
use uuid::Uuid;

use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use crate::{AppState, today};

/// The `/api/v1/admin` surface. Nested by [`crate::routes`], which supplies the state.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/channels", get(list).post(set))
        .route("/channels/{name}/prices", get(history).post(append))
        .route("/organizations", get(organizations))
        .route(
            "/organizations/{organization_id}",
            patch(update_organization),
        )
        .route(
            "/organizations/{organization_id}/adjustments",
            post(adjust_organization),
        )
        .route("/users/{user_id}/password-reset", post(reset_password))
        .route("/holds", get(holds))
        .route("/anomalies", get(anomalies))
        .route("/margin", get(margin))
        .route("/reconciliation", get(reconciliation))
        .route("/redemption-codes", post(mint_codes))
        .route("/tiers", get(tiers))
        .route("/tiers/{tier}", put(set_tier).delete(delete_tier))
        .route("/discounts", get(discounts).post(create_discount))
        .route("/discounts/{discount_id}", delete(end_discount))
        .route("/audit", get(audit))
        .route(
            "/service-credentials",
            get(service_credentials).post(mint_service_credential),
        )
        .route(
            "/service-credentials/{credential_id}",
            delete(revoke_service_credential),
        )
        .route("/closings", get(closings).post(close_month))
        .route("/statements", get(statements).post(generate_statements))
        .route("/statements/{statement_id}", get(statement))
        .route(
            "/statements/{statement_id}/finalize",
            post(finalize_statement),
        )
        .route("/statements/{statement_id}/payments", post(record_payment))
        .route(
            "/statements/{statement_id}/suspend",
            post(suspend_statement),
        )
        .layer(middleware::from_fn_with_state(state, require_admin))
}

/// A channel's connection, as the admin API takes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChannelReq {
    name: String,
    base_url: String,
    /// The upstream credential. Stored sealed; only its last four characters are ever read back.
    api_key: String,
    /// The upstream protocol the channel speaks; defaults to `openai`, the only
    /// protocol this build knows.
    protocol: Option<String>,
}

/// A price to append for one of a channel's models.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PriceReq {
    model: String,
    input_price_per_million: i64,
    /// Required on `chat`, absent or zero on the input-only modes — the
    /// mode-aware rule lives in `Price::validate` (issue #170).
    output_price_per_million: Option<i64>,
    max_output_tokens: Option<i64>,
    cache_read_price_per_million: Option<i64>,
    cache_write_5m_price_per_million: Option<i64>,
    cache_write_1h_price_per_million: Option<i64>,
    reasoning_price_per_million: Option<i64>,
    cost_per_request: Option<i64>,
    upstream: Option<oxsum_core::UpstreamPrices>,
    mode: Option<oxsum_core::BillingMode>,
    /// The route's relative preference for leading a request; absent means 100
    /// (issue #168). The bound is validated in core.
    weight: Option<i64>,
    #[serde(default)]
    rules: Vec<oxsum_core::PriceRule>,
}

/// The version a price change wrote.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionRes {
    model: String,
    version: i64,
}

/// Every channel, with the current version of each of its models.
async fn list(State(state): State<AppState>) -> ApiResult<Vec<Channel>> {
    ok(state.db.channels().await?)
}

/// Creates a channel, or replaces the connection of a channel that already exists.
///
/// Its prices are untouched: a connection moves, a price history stays where it is.
async fn set(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ChannelReq>,
) -> ApiResult<Channel> {
    // A deployment with no sealing key cannot store a credential, and saying so beats storing one in
    // the clear. In practice this cannot happen: `prepare` refuses to start without the key once
    // channels exist, and a channel that was just created needs it too.
    let Some(secret) = state.config.secret().cloned() else {
        tracing::error!("a channel was configured without OXSUM_SECRET_KEY");
        return Err(ApiError::Internal);
    };
    state
        .db
        .set_channel(
            &request.name,
            &request.base_url,
            &request.api_key,
            request.protocol.as_deref().unwrap_or(oxsum_core::OPENAI),
            &secret,
        )
        .await?;
    let channel = state
        .db
        .channels()
        .await?
        .into_iter()
        .find(|channel| channel.name == request.name)
        .ok_or(ApiError::Internal)?;
    record(
        &state,
        audit_action::CHANNEL_SET,
        Some(channel.name.clone()),
        serde_json::json!({
            "baseUrl": channel.base_url,
            "protocol": channel.protocol,
        }),
        None,
    )
    .await?;
    ok(channel)
}

/// Appends a price version for one of a channel's models, and reports the version.
///
/// The previous version stays readable through [`history`], which is what makes a bill that names a
/// version checkable later.
async fn append(
    State(state): State<AppState>,
    Path(name): Path<String>,
    ApiJson(request): ApiJson<PriceReq>,
) -> ApiResult<VersionRes> {
    // An unknown channel is not found, rather than a validation failure: the request is well formed
    // and names something that is not there.
    if state.db.channel_prices(&name).await?.is_none() {
        return Err(ApiError::not_found());
    }
    // A chat price with no output rate declared reads as free output — an
    // absent field must not silently mean zero (issue #170). The input-only
    // modes carry none, so the field is optional on the wire and the mode rule
    // is `Price::validate`'s.
    if request.mode.unwrap_or_default() == oxsum_core::BillingMode::Chat
        && (request.output_price_per_million.is_none() || request.max_output_tokens.is_none())
    {
        return Err(ApiError::Validation(
            "a chat price needs outputPricePerMillion and maxOutputTokens".into(),
        ));
    }
    let price = Price {
        input_price_per_million: request.input_price_per_million,
        output_price_per_million: request.output_price_per_million.unwrap_or(0),
        max_output_tokens: request.max_output_tokens.unwrap_or(0),
        cache_read_price_per_million: request.cache_read_price_per_million,
        cache_write_5m_price_per_million: request.cache_write_5m_price_per_million,
        cache_write_1h_price_per_million: request.cache_write_1h_price_per_million,
        reasoning_price_per_million: request.reasoning_price_per_million,
        cost_per_request: request.cost_per_request,
        upstream: request.upstream,
        mode: request.mode.unwrap_or_default(),
        rules: request.rules,
    };
    let version = state
        .db
        .append_price(&name, &request.model, price, request.weight.unwrap_or(100))
        .await?;
    record(
        &state,
        audit_action::CHANNEL_PRICE_APPEND,
        Some(name),
        serde_json::json!({
            "model": request.model,
            "version": version,
        }),
        None,
    )
    .await?;
    ok(VersionRes {
        model: request.model,
        version,
    })
}

/// An organization as the organizations endpoint answers it: identity, kind, headcount,
/// and what its wallet shows.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationRes {
    id: Uuid,
    name: String,
    kind: Kind,
    members: i64,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    /// Settled minus unsettled holds — own funds plus the undrawn credit line.
    available_minor: i64,
    /// The sum of the organization's outstanding holds; 0 when it holds nothing.
    reserved_minor: i64,
    /// The credit limit the operator granted; 0 for an organization without one.
    credit_limit_minor: i64,
    /// The drawn plus reserved part of the credit line.
    credit_used_minor: i64,
    /// The tier profile assigned — its limits gate admission (issue #158).
    tier: Option<String>,
    /// Whether the organization's spend is suspended (issue #162).
    suspended: bool,
    /// When it was suspended; `None` while its spend runs normally.
    #[serde(with = "time::serde::rfc3339::option")]
    suspended_at: Option<OffsetDateTime>,
}

/// The organizations query string: the page size, and where the walk resumes — an
/// earlier answer's `nextCursor`, echoed verbatim (issue #93).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationsQuery {
    limit: Option<usize>,
    cursor: Option<String>,
}

/// One page of organizations, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OrganizationsPageRes {
    organizations: Vec<OrganizationRes>,
    /// The cursor the next page asks with; absent at the list's end.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// The organizations, oldest first, with their balances — one page at a time.
///
/// The list is the identity table's; the money is read per organization through the
/// wallet, which creates the ledger of one that has never moved — the same laziness
/// the dashboard's overview has. The list paginates so a long table does not grow a
/// response without bound: `nextCursor` names where the walk resumes.
async fn organizations(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<OrganizationsQuery>,
) -> ApiResult<OrganizationsPageRes> {
    let after = query
        .cursor
        .as_deref()
        .map(decode_organization_cursor)
        .transpose()?;
    let page = state
        .db
        .organizations_page(after, query.limit.unwrap_or(100))
        .await?;
    let mut organizations = Vec::with_capacity(page.rows.len());
    for organization in page.rows {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        organizations.push(OrganizationRes {
            id: organization.id,
            name: organization.name,
            kind: organization.kind,
            members: organization.members,
            created_at: organization.created_at,
            available_minor: wallet.available().await?,
            reserved_minor: wallet.reserved().await?,
            credit_limit_minor: wallet.credit_limit().await?,
            credit_used_minor: wallet.credit_used().await?,
            tier: organization.tier.clone(),
            suspended: organization.suspended_at.is_some(),
            suspended_at: organization.suspended_at,
        });
    }
    ok(OrganizationsPageRes {
        organizations,
        next_cursor: page.next_cursor.map(encode_organization_cursor),
    })
}

/// The organizations cursor, encoded: `<unix nanos>:<organization id>`. The pair is
/// the keyset `Db::organizations_page` orders by — `created_at` alone cannot order
/// two organizations registered in the same instant — and the string is opaque to
/// the caller: echoed, never constructed (issue #93).
fn encode_organization_cursor((created_at, id): (OffsetDateTime, Uuid)) -> String {
    format!("{}:{}", created_at.unix_timestamp_nanos(), id)
}

fn decode_organization_cursor(text: &str) -> Result<(OffsetDateTime, Uuid), ApiError> {
    let bad = || ApiError::Validation("a malformed organizations cursor".into());
    let (nanos, id) = text.split_once(':').ok_or_else(bad)?;
    let nanos = nanos.parse::<i128>().map_err(|_| bad())?;
    let created_at = OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    let id = Uuid::parse_str(id).map_err(|_| bad())?;
    Ok((created_at, id))
}

/// Records one admin write in the audit log, after the mutation it describes
/// has committed (issue #160). The row is awaited rather than spawned: the
/// writes it follows are all idempotent, so a client that retries a 500 lands
/// the same mutation and its audit row together.
async fn record(
    state: &AppState,
    action: &str,
    target: Option<String>,
    detail: serde_json::Value,
    idempotency_key: Option<&str>,
) -> Result<(), ApiError> {
    state
        .db
        .record_audit(action, target.as_deref(), detail, idempotency_key)
        .await?;
    Ok(())
}

/// The audit query string: the page size, where the walk resumes, and an
/// optional exact action to narrow to.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuditQuery {
    limit: Option<usize>,
    cursor: Option<String>,
    action: Option<String>,
}

/// One page of audit rows, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditPageRes {
    entries: Vec<AuditEntry>,
    /// The cursor the next page asks with; absent at the log's end.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// The audit log, newest first — every mutating call on this surface, one row
/// each, written after the mutation committed. `action` narrows to one exact
/// action name; an unknown one is a 400, not an empty page masquerading as
/// quiet.
async fn audit(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<AuditQuery>,
) -> ApiResult<AuditPageRes> {
    if let Some(action) = &query.action
        && !audit_action::ALL.contains(&action.as_str())
    {
        return Err(ApiError::Validation(format!("unknown action: {action:?}")));
    }
    let after = query
        .cursor
        .as_deref()
        .map(decode_organization_cursor)
        .transpose()?;
    let page = state
        .db
        .audit_page(after, query.action.as_deref(), query.limit.unwrap_or(100))
        .await?;
    ok(AuditPageRes {
        entries: page.rows,
        next_cursor: page.next_cursor.map(encode_organization_cursor),
    })
}

/// Every service credential, newest first — metadata only; the secret exists
/// once, at mint.
async fn service_credentials(
    State(state): State<AppState>,
) -> ApiResult<Vec<oxsum_core::ServiceCredential>> {
    ok(state.db.list_service_credentials().await?)
}

/// The body of `POST …/service-credentials`: which service holds the
/// credential — the name settlement descriptions record as the reporter.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServiceCredentialReq {
    name: Option<String>,
}

/// Mints a service credential for the metering surface, and answers its secret
/// the one time it exists readable.
async fn mint_service_credential(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<ServiceCredentialReq>,
) -> ApiResult<oxsum_core::CreatedServiceCredential> {
    let created = state.db.create_service_credential(request.name).await?;
    record(
        &state,
        audit_action::SERVICE_CREDENTIAL_MINT,
        Some(created.credential.id.to_string()),
        serde_json::json!({
            "name": created.credential.name,
            "prefix": created.credential.prefix,
        }),
        None,
    )
    .await?;
    ok(created)
}

/// Revokes a service credential: it authenticates nothing from here on; what it
/// already wrote stands.
async fn revoke_service_credential(
    State(state): State<AppState>,
    Path(credential_id): Path<Uuid>,
) -> ApiResult<oxsum_core::ServiceCredential> {
    let credential = state
        .db
        .revoke_service_credential(credential_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    record(
        &state,
        audit_action::SERVICE_CREDENTIAL_REVOKE,
        Some(credential_id.to_string()),
        serde_json::json!({ "name": credential.name }),
        None,
    )
    .await?;
    ok(credential)
}

/// The body of `POST …/adjustments`: a signed amount, the reason it moved, and the
/// idempotency key that makes a retry the same entry.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdjustReq {
    amount_minor: i64,
    reason: String,
    idempotency_key: String,
}

/// The adjustment as booked, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdjustmentRes {
    entry_id: String,
    organization_id: Uuid,
    amount_minor: i64,
    reason: String,
    /// The organization's available balance after the write.
    available_minor: i64,
}

/// Books a signed amount into the named organization's wallet — a grant or a
/// deduction, never a rewrite of history. The reason is required: it is the
/// entry's description, so it is part of what the bill's proof covers.
///
/// The ledger enforces the substance: zero is a validation error, and a deduction
/// the balance cannot carry is `INSUFFICIENT_FUNDS` from the wallet's own
/// no-overdraft rule — under concurrency too, not as a read-then-write check.
async fn adjust_organization(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    ApiJson(request): ApiJson<AdjustReq>,
) -> ApiResult<AdjustmentRes> {
    let organization = state.db.organization_by_id(organization_id).await?;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let receipt = wallet
        .adjust(
            &request.idempotency_key,
            &request.reason,
            request.amount_minor,
            today(),
        )
        .await?;
    record(
        &state,
        audit_action::ORGANIZATION_ADJUST,
        Some(organization_id.to_string()),
        serde_json::json!({
            "amountMinor": request.amount_minor,
            "reason": request.reason,
            "entryId": receipt.entry_id,
        }),
        Some(&request.idempotency_key),
    )
    .await?;
    ok(AdjustmentRes {
        entry_id: receipt.entry_id.to_string(),
        organization_id,
        amount_minor: request.amount_minor,
        reason: request.reason,
        available_minor: wallet.available().await?,
    })
}

/// The body of `PATCH …/organizations/{id}`: the billing terms to set — the credit
/// line absolute, the payment-terms day count, the tier assignment — and the
/// idempotency key that makes a retried credit-limit change the same entry.
///
/// `tier` is a double option: absent leaves the assignment alone, an explicit
/// null clears it, a name assigns it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateOrganizationReq {
    credit_limit_minor: Option<i64>,
    payment_terms_days: Option<i32>,
    #[serde(default, deserialize_with = "double_option")]
    tier: Option<Option<String>>,
    /// `true` suspends the organization's spend, `false` reinstates it;
    /// absent leaves it (issue #162).
    suspended: Option<bool>,
    idempotency_key: String,
}

/// Peels one option layer serde does not: a bare `Option<Option<T>>` field
/// answers `None` for both an absent field and an explicit `null`, while the
/// PATCH needs `null` to mean "clear the tier". With `default` covering the
/// absent case, a present field always yields `Some(…)` — `Some(None)` when
/// it was `null`.
fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// The billing terms as they stand after the write, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BillingTermsRes {
    /// The ledger entry the credit-limit change booked; absent when no limit was
    /// given or it did not move.
    #[serde(skip_serializing_if = "Option::is_none")]
    entry_id: Option<String>,
    organization_id: Uuid,
    credit_limit_minor: i64,
    credit_used_minor: i64,
    /// Days a finalized statement's payment has before it falls due.
    payment_terms_days: i32,
    /// The tier profile the organization is assigned to; `None` when none.
    tier: Option<String>,
    /// Whether the organization's spend is suspended (issue #162).
    suspended: bool,
    /// The organization's spendable balance after the change, own funds plus
    /// undrawn credit.
    available_minor: i64,
}

/// Sets an organization's billing terms — the credit line it may draw and the
/// payment terms its finalized statements carry.
///
/// The limit is an entry, not a flag: the delta from the committed figure posts
/// `debit CreditFacility, credit CreditLine`, so the whole credit history sits
/// inside the ledger the organization can verify, and a shrink below the
/// outstanding draw is refused by the credit-line pool's own no-overdraft rule,
/// surfaced as a validation error. `paymentTermsDays` is a column write — it is
/// snapshotted onto each statement at finalization, so it moves the terms of
/// statements issued after the change, never one already due.
async fn update_organization(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    ApiJson(request): ApiJson<UpdateOrganizationReq>,
) -> ApiResult<BillingTermsRes> {
    if request.credit_limit_minor.is_none()
        && request.payment_terms_days.is_none()
        && request.tier.is_none()
        && request.suspended.is_none()
    {
        return Err(ApiError::Validation(
            "at least one of creditLimitMinor, paymentTermsDays, tier and suspended must be present"
                .into(),
        ));
    }
    // The audit detail names what was sent, not what stands after: an absent
    // field is a field the call never meant to touch.
    let mut detail = serde_json::Map::new();
    if let Some(limit) = request.credit_limit_minor {
        detail.insert("creditLimitMinor".into(), limit.into());
    }
    if let Some(days) = request.payment_terms_days {
        detail.insert("paymentTermsDays".into(), days.into());
    }
    if let Some(tier) = &request.tier {
        detail.insert("tier".into(), serde_json::json!(tier));
    }
    if let Some(suspended) = request.suspended {
        detail.insert("suspended".into(), suspended.into());
    }
    let organization = state.db.organization_by_id(organization_id).await?;
    if let Some(days) = request.payment_terms_days {
        state.db.set_payment_terms(organization_id, days).await?;
    }
    if let Some(tier) = request.tier {
        state
            .db
            .set_organization_tier(organization_id, tier.as_deref())
            .await?;
    }
    if let Some(suspended) = request.suspended {
        // A replayed flag writes nothing and enqueues no webhook — the
        // transition, not the call, is what `org.suspended` announces.
        state.db.set_suspended(organization_id, suspended).await?;
    }
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let receipt = match request.credit_limit_minor {
        Some(limit) => {
            wallet
                .set_credit_limit(&request.idempotency_key, limit, today())
                .await?
        }
        None => None,
    };
    record(
        &state,
        audit_action::ORGANIZATION_UPDATE,
        Some(organization_id.to_string()),
        serde_json::Value::Object(detail),
        Some(&request.idempotency_key),
    )
    .await?;
    let updated = state.db.organization_by_id(organization_id).await?;
    ok(BillingTermsRes {
        entry_id: receipt.map(|r| r.entry_id.to_string()),
        organization_id,
        credit_limit_minor: wallet.credit_limit().await?,
        credit_used_minor: wallet.credit_used().await?,
        payment_terms_days: updated.payment_terms_days,
        tier: updated.tier,
        suspended: updated.suspended_at.is_some(),
        available_minor: wallet.available().await?,
    })
}

/// The body of `PUT …/tiers/{tier}`: the whole capability package — the write
/// replaces, so an absent field clears its old value rather than keeping it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetTierReq {
    requests_per_minute: Option<i32>,
    model_allowlist: Option<Vec<String>>,
}

/// Every tier profile, with the count of organizations assigned to it.
async fn tiers(State(state): State<AppState>) -> ApiResult<Vec<TierProfile>> {
    ok(state.db.tiers().await?)
}

/// Creates or replaces a tier profile. A `PUT` by name is naturally
/// idempotent: replaying the same body writes the same package, so the
/// request carries no idempotency key.
async fn set_tier(
    State(state): State<AppState>,
    Path(tier): Path<String>,
    ApiJson(request): ApiJson<SetTierReq>,
) -> ApiResult<TierProfile> {
    let profile = state
        .db
        .set_tier(&tier, request.requests_per_minute, request.model_allowlist)
        .await?;
    record(
        &state,
        audit_action::TIER_SET,
        Some(tier),
        serde_json::json!({
            "requestsPerMinute": profile.requests_per_minute,
            "modelAllowlist": profile.model_allowlist,
        }),
        None,
    )
    .await?;
    ok(profile)
}

/// Retires a tier profile. A tier organizations are still assigned to refuses —
/// a deleted package never silently uncaps its members.
async fn delete_tier(
    State(state): State<AppState>,
    Path(tier): Path<String>,
) -> ApiResult<serde_json::Value> {
    state
        .db
        .delete_tier(&tier)
        .await?
        .ok_or_else(ApiError::not_found)?;
    record(
        &state,
        audit_action::TIER_DELETE,
        Some(tier),
        serde_json::json!({}),
        None,
    )
    .await?;
    ok(serde_json::json!({}))
}

/// The body of `POST …/discounts`: scope, percent, window, label — and the
/// idempotency key that makes a retry the same row.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateDiscountReq {
    percent: i32,
    organization_id: Option<Uuid>,
    model: Option<String>,
    label: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    valid_from: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    valid_until: Option<OffsetDateTime>,
    idempotency_key: String,
}

/// Every discount, newest first, ended or not — the history a settled bill's
/// `discountPercent` points back at.
async fn discounts(State(state): State<AppState>) -> ApiResult<Vec<Discount>> {
    ok(state.db.discounts().await?)
}

/// Creates a discount: the percent off the priced sum that matching turns
/// settle at, snapshotted into the settlement description when it applies.
/// The idempotency key's replay answers the row it created; under different
/// fields it is a conflict.
async fn create_discount(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CreateDiscountReq>,
) -> ApiResult<Discount> {
    let discount = state
        .db
        .create_discount(
            &request.idempotency_key,
            &NewDiscount {
                percent: request.percent,
                organization_id: request.organization_id,
                model: request.model,
                label: request.label.clone(),
                valid_from: request.valid_from,
                valid_until: request.valid_until,
            },
        )
        .await?;
    record(
        &state,
        audit_action::DISCOUNT_CREATE,
        request.organization_id.map(|id| id.to_string()),
        serde_json::to_value(&discount).map_err(|_| ApiError::Internal)?,
        Some(&request.idempotency_key),
    )
    .await?;
    ok(discount)
}

/// Ends a discount early: turns starting after the call no longer qualify, and
/// the row stays in the list — the history a settled bill cites.
async fn end_discount(
    State(state): State<AppState>,
    Path(discount_id): Path<Uuid>,
) -> ApiResult<Discount> {
    match state.db.end_discount(discount_id).await? {
        Some(discount) => {
            record(
                &state,
                audit_action::DISCOUNT_END,
                Some(discount_id.to_string()),
                serde_json::json!({}),
                None,
            )
            .await?;
            ok(discount)
        }
        None => Err(ApiError::not_found()),
    }
}

/// The body of `POST …/password-reset`: the replacement password. The same
/// length rule signup applies runs in core.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PasswordResetReq {
    new_password: String,
}

/// The reset as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PasswordResetRes {
    user_id: Uuid,
    /// Live sessions the reset revoked alongside the password change.
    sessions_revoked: u64,
}

/// Sets a new password for the named user and revokes every session they hold —
/// the only recovery path v1 has, since no email is sent.
async fn reset_password(
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    ApiJson(request): ApiJson<PasswordResetReq>,
) -> ApiResult<PasswordResetRes> {
    let sessions_revoked = state
        .db
        .reset_password(user_id, &request.new_password)
        .await?;
    record(
        &state,
        audit_action::USER_PASSWORD_RESET,
        Some(user_id.to_string()),
        serde_json::json!({ "sessionsRevoked": sessions_revoked }),
        None,
    )
    .await?;
    ok(PasswordResetRes {
        user_id,
        sessions_revoked,
    })
}

/// Every unsettled hold across all organizations, newest first: the in-flight list.
///
/// The rows are the sweeper's watch table joined to the organizations they belong to;
/// what the ledger still reserves is what the page shows, because a watch row whose
/// hold already settled is deleted rather than listed.
async fn holds(State(state): State<AppState>) -> ApiResult<Vec<InFlightHold>> {
    ok(state.db.open_holds().await?)
}

/// The settlement kinds an admin reviews: the turns where usage was never reported, the
/// caller left mid-stream, the charge hit the freeze's ceiling, or the sweeper had to
/// release a hold nobody settled.
const ANOMALOUS: &[SettlementKind] = &[
    SettlementKind::Capped,
    SettlementKind::Estimated,
    SettlementKind::ClientCancelled,
    SettlementKind::Swept,
    SettlementKind::Unpriced,
];

/// One anomalous turn, as the anomalies endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AnomalyRes {
    organization: String,
    request_id: String,
    model: String,
    channel: String,
    price_version: i64,
    kind: SettlementKind,
    /// How many upstream calls the turn made (issue #168) — above 1 means it
    /// failed over between channels.
    upstream_attempts: i64,
    charged_minor: i64,
    freeze_minor: i64,
    /// The settlement's booking date, `YYYY-MM-DD`.
    booked_on: String,
}

/// The settled turns that did not price cleanly, across all organizations, newest
/// first — read back from each ledger's own settlement records, so every row is
/// exactly what that turn's bill proves.
async fn anomalies(State(state): State<AppState>) -> ApiResult<Vec<AnomalyRes>> {
    let mut answer = Vec::new();
    for organization in state.db.organizations().await? {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        for turn in wallet.recent_settlements(100).await? {
            if !ANOMALOUS.contains(&turn.record.kind) {
                continue;
            }
            answer.push(AnomalyRes {
                organization: organization.name.clone(),
                request_id: turn.record.request,
                model: turn.record.model,
                channel: turn.record.channel,
                price_version: turn.record.price_version,
                kind: turn.record.kind,
                upstream_attempts: turn.record.upstream_attempts,
                charged_minor: turn.record.charged,
                freeze_minor: turn.record.freeze,
                booked_on: turn.booked_on.to_string(),
            });
        }
    }
    // Booking dates are days, so within one day the per-ledger order stands; across
    // days newest first.
    answer.sort_by(|a, b| b.booked_on.cmp(&a.booked_on));
    ok(answer)
}

/// One `(channel, model)` pair's margin, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MarginRes {
    channel: String,
    model: String,
    turns: i64,
    charged_minor: i64,
    upstream_cost_minor: i64,
    margin_minor: i64,
    untracked_turns: i64,
}

/// What the platform charged versus what upstream cost it, per channel and
/// model — summed over the usage rows every settled turn writes, so the sums
/// are exactly what the mutable record holds (issue #112).
async fn margin(State(state): State<AppState>) -> ApiResult<Vec<MarginRes>> {
    let mut answer = Vec::new();
    for row in state.db.margin().await? {
        answer.push(MarginRes {
            margin_minor: row.charged_minor - row.upstream_cost_minor,
            channel: row.channel,
            model: row.model,
            turns: row.turns,
            charged_minor: row.charged_minor,
            upstream_cost_minor: row.upstream_cost_minor,
            untracked_turns: row.untracked_turns,
        });
    }
    ok(answer)
}

/// The reconciliation report: the drift between the ledgers and the projections
/// that claim to describe them, one entry per class with a bounded sample —
/// read-only, for an operator to act on (issue #134).
async fn reconciliation(State(state): State<AppState>) -> ApiResult<Reconciliation> {
    ok(state.db.reconcile().await?)
}

/// One organization's closing record for a sealed period, as the closings endpoint
/// answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClosingRes {
    organization: String,
    /// The sealed period, `YYYY-MM`.
    period: String,
    /// How many of the ledger's entries the period contains.
    entry_count: i64,
    /// The log's size at the moment of sealing.
    tree_size: i64,
    /// The log's tree head at sealing, hex — what the month committed to.
    tree_root: String,
    /// The Merkle root over the period's closing trial balance, hex.
    trial_balance_root: String,
    /// The seal's own hash, hex; it chains onto the seal before it.
    seal_hash: String,
}

impl ClosingRes {
    fn of(organization: &str, seal: &Seal) -> Self {
        ClosingRes {
            organization: organization.to_owned(),
            period: seal.period.to_string(),
            entry_count: seal.entry_count as i64,
            tree_size: seal.tree_head.size as i64,
            tree_root: seal.tree_head.root.to_string(),
            trial_balance_root: seal.trial_balance.root.to_string(),
            seal_hash: seal.seal_hash.to_string(),
        }
    }
}

/// Every sealed month across all organizations, newest first: the closing records.
async fn closings(State(state): State<AppState>) -> ApiResult<Vec<ClosingRes>> {
    let mut answer = Vec::new();
    for organization in state.db.organizations().await? {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        for seal in wallet.seals().await? {
            answer.push(ClosingRes::of(&organization.name, &seal));
        }
    }
    // Period ids sort as the months do (`YYYY-MM`), so newest first is a string sort.
    answer.sort_by(|a, b| b.period.cmp(&a.period));
    ok(answer)
}

/// The month to close, `YYYY-MM`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CloseReq {
    month: String,
}

/// Closes one month across every organization's ledger and answers each closing
/// record — the seal it appended.
///
/// The month must have fully ended, which `Wallet::close_month` enforces; closing
/// an already-sealed month answers the record it holds, so the call is idempotent.
async fn close_month(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CloseReq>,
) -> ApiResult<Vec<ClosingRes>> {
    let first = parse_month(&request.month)?;
    let mut answer = Vec::new();
    for organization in state.db.organizations().await? {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        let seal = wallet.close_month(first).await?;
        answer.push(ClosingRes::of(&organization.name, &seal));
    }
    record(
        &state,
        audit_action::CLOSING_CLOSE,
        Some(request.month.clone()),
        serde_json::json!({ "organizations": answer.len() }),
        None,
    )
    .await?;
    ok(answer)
}

/// `YYYY-MM` as the month's first day. A malformed month is a validation failure,
/// not an empty result.
fn parse_month(month: &str) -> Result<Date, ApiError> {
    let bad = || ApiError::Validation("month must be YYYY-MM".into());
    let (year, month) = month.split_once('-').ok_or_else(bad)?;
    let year = year.parse::<i32>().map_err(|_| bad())?;
    let month = Month::try_from(month.parse::<u8>().map_err(|_| bad())?).map_err(|_| bad())?;
    Date::from_calendar_date(year, month, 1).map_err(|_| bad())
}

/// The optional filters of `GET /admin/statements`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StatementsQuery {
    period: Option<String>,
    organization_id: Option<Uuid>,
}

/// Every statement the platform has generated, newest period first — drafts and
/// finalized alike, optionally narrowed by `period` or `organizationId`. The lazy
/// overdue flip runs first, so a pending statement past its due date answers
/// `overdue` already.
async fn statements(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<StatementsQuery>,
) -> ApiResult<Vec<oxsum_core::Statement>> {
    // The filter names a billing month; a malformed or still-running one is a
    // validation failure rather than an empty list, so a mistyped period does
    // not silently look like "no statements".
    if let Some(period) = &query.period {
        oxsum_core::statement_period(period)?;
    }
    state.db.flip_overdue(today()).await?;
    ok(state
        .db
        .statements(query.organization_id, query.period.as_deref())
        .await?)
}

/// The body of `POST /admin/statements`: the month to bill, and optionally the one
/// organization to bill it for.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerateStatementsReq {
    period: String,
    organization_id: Option<Uuid>,
}

/// Builds the period's draft statements — for the named organization, or for every
/// organization the period's usage covers when `organizationId` is absent.
///
/// Generation is idempotent per organization and period: a draft is rebuilt from
/// the usage rows each call, so a usage row that landed after generation is picked
/// up by regenerating; a statement already finalized stands and is answered as it
/// is. Only a month that has fully ended generates.
async fn generate_statements(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<GenerateStatementsReq>,
) -> ApiResult<Vec<oxsum_core::Statement>> {
    let period = oxsum_core::statement_period(&request.period)?;
    let organizations = match request.organization_id {
        Some(id) => vec![state.db.organization_by_id(id).await?],
        None => state.db.billable_organizations().await?,
    };
    let mut answer = Vec::with_capacity(organizations.len());
    for organization in &organizations {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        if let Some(statement) = state
            .db
            .generate_statement(organization, &wallet, &period)
            .await?
        {
            answer.push(statement);
        }
    }
    record(
        &state,
        audit_action::STATEMENT_GENERATE,
        Some(request.period.clone()),
        serde_json::json!({
            "organizationId": request.organization_id,
            "statements": answer.len(),
        }),
        None,
    )
    .await?;
    ok(answer)
}

/// A statement and its itemized lines, as the detail endpoints answer it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatementDetailRes {
    statement: oxsum_core::Statement,
    lines: Vec<oxsum_core::StatementLine>,
}

/// One statement with its lines, whatever standing it is in — the admin view of
/// the document.
async fn statement(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
) -> ApiResult<StatementDetailRes> {
    state.db.flip_overdue(today()).await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    ok(StatementDetailRes {
        lines: state.db.statement_lines(statement_id).await?,
        statement,
    })
}

/// The body of the statement transitions: only the idempotency key, carried for
/// uniformity — a lifecycle transition is idempotent on its own.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TransitionReq {
    idempotency_key: String,
}

/// The key is carried for uniformity, so it is held to the same shape every write
/// key answers to: present, and at most 200 bytes.
fn transition_key(request: &TransitionReq) -> Result<(), ApiError> {
    if request.idempotency_key.is_empty() || request.idempotency_key.len() > 200 {
        return Err(ApiError::Validation("a malformed idempotency key".into()));
    }
    Ok(())
}

/// Issues a draft statement: locks its lines and totals, snapshots the
/// organization's payment terms into the due date, and pins the ledger window
/// the lines prove. Idempotent — an already-final statement answers itself.
async fn finalize_statement(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
    ApiJson(request): ApiJson<TransitionReq>,
) -> ApiResult<oxsum_core::Statement> {
    transition_key(&request)?;
    let existing = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let organization = state
        .db
        .organization_by_id(existing.organization_id)
        .await?;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    let statement = state
        .db
        .finalize_statement(statement_id, &wallet, today())
        .await?;
    record(
        &state,
        audit_action::STATEMENT_FINALIZE,
        Some(statement_id.to_string()),
        serde_json::json!({ "paymentStatus": statement.payment_status }),
        Some(&request.idempotency_key),
    )
    .await?;
    ok(statement)
}

/// The body of `POST …/statements/{id}/payments`: the amount the organization paid
/// and the idempotency key naming the ledger repayment entry.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordPaymentReq {
    amount_minor: i64,
    idempotency_key: String,
}

/// A recorded payment, as the endpoint answers it: the repayment's ledger entry
/// and the statement's standing after the money applied.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PaymentRes {
    statement: oxsum_core::Statement,
    entry_id: String,
    amount_minor: i64,
}

/// Records a payment against a statement: books the amount into the
/// organization's ledger — `debit Cash`, repaying the drawn credit line first —
/// then reconciles the statement book, which applies repayments oldest-first.
///
/// The payment may not exceed the credit-line debt outstanding through the
/// statement's period (`400` if it does), so money recorded here always lands
/// on the line. The idempotency key names the ledger entry, which can never
/// book twice: a retried payment while the statement is still open replays the
/// receipt, and one that arrives after the statement paid is refused as
/// `400` — there is nothing left to take.
async fn record_payment(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
    ApiJson(request): ApiJson<RecordPaymentReq>,
) -> ApiResult<PaymentRes> {
    let existing = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let organization = state
        .db
        .organization_by_id(existing.organization_id)
        .await?;
    let wallet = state.tenants.get(&organization.tenant_id).await?;
    // Fresh figures first: a payment raced with a top-up could have repaid the
    // statement since it was last read, and the caps below validate against the
    // standing the ledger now shows.
    state
        .db
        .reconcile_statements(organization.id, &wallet, today())
        .await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if statement.status == oxsum_core::StatementStatus::Draft {
        return Err(ApiError::Validation(
            "a draft statement is not issued yet; finalize it first".into(),
        ));
    }
    if statement.payment_status == oxsum_core::PaymentStatus::Paid {
        return Err(ApiError::Validation(
            "the statement is paid; nothing is owed".into(),
        ));
    }
    let owed = state
        .db
        .statement_debt_through(&wallet, &statement.period)
        .await?;
    if request.amount_minor > owed {
        return Err(ApiError::Validation(
            "the payment exceeds the billed debt outstanding through this statement".into(),
        ));
    }
    let receipt = wallet
        .top_up(&request.idempotency_key, request.amount_minor, today())
        .await?;
    state
        .db
        .reconcile_statements(organization.id, &wallet, today())
        .await?;
    let statement = state
        .db
        .statement_by_id(statement_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    record(
        &state,
        audit_action::STATEMENT_PAYMENT,
        Some(statement_id.to_string()),
        serde_json::json!({
            "amountMinor": request.amount_minor,
            "entryId": receipt.entry_id,
        }),
        Some(&request.idempotency_key),
    )
    .await?;
    ok(PaymentRes {
        statement,
        entry_id: receipt.entry_id.to_string(),
        amount_minor: request.amount_minor,
    })
}

/// Marks an unpaid finalized statement `suspended` — the standing for a bill that
/// has gone unpaid past its grace. A later payment still settles it.
async fn suspend_statement(
    State(state): State<AppState>,
    Path(statement_id): Path<Uuid>,
    ApiJson(request): ApiJson<TransitionReq>,
) -> ApiResult<oxsum_core::Statement> {
    transition_key(&request)?;
    // A pending statement whose due date has passed is overdue first — suspending
    // it lands from the standing it actually holds.
    state.db.flip_overdue(today()).await?;
    let statement = state.db.suspend_statement(statement_id).await?;
    record(
        &state,
        audit_action::STATEMENT_SUSPEND,
        Some(statement_id.to_string()),
        serde_json::json!({}),
        Some(&request.idempotency_key),
    )
    .await?;
    ok(statement)
}

/// Every version of every model of one channel, newest first.
///
/// A price change appends, so this is the price history rather than a list of what is in force.
async fn history(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Vec<ModelPrice>> {
    match state.db.channel_prices(&name).await? {
        Some(prices) => ok(prices),
        None => Err(ApiError::not_found()),
    }
}

/// Opens the surface for the operator token, and for nothing else.
///
/// A deployment with no token has no platform admin, and this answers 401 rather than 404: the
/// surface exists and is closed, which is a different thing from a route that is not there.
pub(crate) async fn require_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = bearer(&request);
    match state.config.admin_token() {
        Some(expected) if presented.is_some_and(|token| token_matches(token, expected)) => {
            Ok(next.run(request).await)
        }
        _ => Err(ApiError::Unauthorized),
    }
}

/// The body of `POST …/redemption-codes`: how many codes, worth how much, and
/// optionally when the batch stops redeeming.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MintCodesReq {
    count: i64,
    amount_minor: i64,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

/// Mints a batch of redemption codes: the codes themselves are answered once,
/// here, and the database keeps only their hashes — whoever holds a code can
/// redeem it through `POST /api/v1/redemptions`.
async fn mint_codes(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<MintCodesReq>,
) -> ApiResult<oxsum_core::CodeBatch> {
    let batch = state
        .db
        .mint_codes(request.count, request.amount_minor, request.expires_at)
        .await?;
    // The codes themselves are secrets held by their recipients — the audit row
    // records the batch, never what was in it.
    record(
        &state,
        audit_action::CODES_MINT,
        Some(batch.batch_id.to_string()),
        serde_json::json!({
            "count": batch.count,
            "amountMinor": batch.amount_minor,
            "expiresAt": batch.expires_at,
        }),
        None,
    )
    .await?;
    ok(batch)
}

/// Whether a presented token is the configured one, in time that does not depend on how much of it
/// was right.
fn token_matches(presented: &str, expected: &str) -> bool {
    presented.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// The `Bearer` credential of a request, if the header is there and readable.
fn bearer(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// Only the tests below reach in.
#[cfg(test)]
mod tests {
    // The tests may unwrap: a panic here is a failing test, which is what a test is for.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::token_matches;

    #[test]
    fn a_token_matches_only_itself() {
        assert!(token_matches("operator-token-1234", "operator-token-1234"));
        assert!(!token_matches("operator-token-1235", "operator-token-1234"));
        assert!(!token_matches("", "operator-token-1234"));
        assert!(!token_matches(
            "operator-token-1234",
            "operator-token-12345"
        ));
    }
}

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
use axum::routing::{get, post};
use axum::{Router, middleware};
use oxsum_core::{Channel, InFlightHold, Kind, ModelPrice, Price, Seal, SettlementKind};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use time::OffsetDateTime;
use time::{Date, Month};
use uuid::Uuid;

use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};
use crate::{AppState, today};

/// The `/api/v1/admin` surface. Nested by [`crate::routes`], which supplies the state.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/channels", get(list).post(set))
        .route("/channels/{name}/prices", get(history).post(append))
        .route("/organizations", get(organizations))
        .route(
            "/organizations/{organization_id}/adjustments",
            post(adjust_organization),
        )
        .route("/holds", get(holds))
        .route("/anomalies", get(anomalies))
        .route("/closings", get(closings).post(close_month))
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
}

/// A price to append for one of a channel's models.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PriceReq {
    model: String,
    input_price_per_million: i64,
    output_price_per_million: i64,
    max_output_tokens: i64,
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
        .set_channel(&request.name, &request.base_url, &request.api_key, &secret)
        .await?;
    let channel = state
        .db
        .channels()
        .await?
        .into_iter()
        .find(|channel| channel.name == request.name)
        .ok_or(ApiError::Internal)?;
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
    let price = Price {
        input_per_million: request.input_price_per_million,
        output_per_million: request.output_price_per_million,
        max_output_tokens: request.max_output_tokens,
    };
    let version = state.db.append_price(&name, &request.model, price).await?;
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
    /// Settled minus unsettled holds.
    available_minor: i64,
    /// The sum of the organization's outstanding holds; 0 when it holds nothing.
    reserved_minor: i64,
}

/// Every organization, oldest first, with its balance.
///
/// The list is the identity table's; the money is read per organization through the
/// wallet, which creates the ledger of one that has never moved — the same laziness
/// the dashboard's overview has.
async fn organizations(State(state): State<AppState>) -> ApiResult<Vec<OrganizationRes>> {
    let mut answer = Vec::new();
    for organization in state.db.organizations().await? {
        let wallet = state.tenants.get(&organization.tenant_id).await?;
        answer.push(OrganizationRes {
            id: organization.id,
            name: organization.name,
            kind: organization.kind,
            members: organization.members,
            created_at: organization.created_at,
            available_minor: wallet.available().await?,
            reserved_minor: wallet.reserved().await?,
        });
    }
    ok(answer)
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
    ok(AdjustmentRes {
        entry_id: receipt.entry_id.to_string(),
        organization_id,
        amount_minor: request.amount_minor,
        reason: request.reason,
        available_minor: wallet.available().await?,
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

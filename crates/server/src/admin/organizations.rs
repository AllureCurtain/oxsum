//! `/api/v1/admin/organizations`: the identity list, the per-organization
//! wallet view, balance adjustments and billing terms.

use axum::extract::{Path, State};
use oxsum_core::{Kind, audit_action};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{ApiError, ApiJson, ApiQuery};
use crate::routes::{ApiResult, ok};
use crate::{
    AppState,
    admin::{decode_organization_cursor, double_option, encode_organization_cursor, record},
    today,
};

/// An organization as the organizations endpoint answers it: identity, kind, headcount,
/// and what its wallet shows.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrganizationRes {
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
pub(crate) struct OrganizationsQuery {
    limit: Option<usize>,
    cursor: Option<String>,
}

/// One page of organizations, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrganizationsPageRes {
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
pub(crate) async fn organizations(
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

/// The body of `POST …/adjustments`: a signed amount, the reason it moved, and the
/// idempotency key that makes a retry the same entry.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AdjustReq {
    amount_minor: i64,
    reason: String,
    idempotency_key: String,
}

/// The adjustment as booked, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdjustmentRes {
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
pub(crate) async fn adjust_organization(
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
pub(crate) struct UpdateOrganizationReq {
    credit_limit_minor: Option<i64>,
    payment_terms_days: Option<i32>,
    #[serde(default, deserialize_with = "double_option")]
    tier: Option<Option<String>>,
    /// `true` suspends the organization's spend, `false` reinstates it;
    /// absent leaves it (issue #162).
    suspended: Option<bool>,
    idempotency_key: String,
}

/// The billing terms as they stand after the write, as the endpoint answers it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BillingTermsRes {
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
pub(crate) async fn update_organization(
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

//! Initial owner consent is separate from customer organization ownership.
use crate::AppState;
use crate::error::{ApiError, ApiJson};
use crate::routes::{ApiResult, ok};
use axum::extract::State;
use axum::http::HeaderMap;

use crate::error::ApiQuery;

pub(crate) async fn accept_setup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessSetupRequest>,
) -> ApiResult<oxsum_core::BusinessSetupResult> {
    // A bearer key cannot select a customer user or replace personal consent.
    if crate::auth::has_machine_credential(&headers) {
        return Err(ApiError::Forbidden(
            "setup acceptance requires a personal session".into(),
        ));
    }
    let token = crate::auth::session_cookie_value(&headers).ok_or(ApiError::Unauthorized)?;
    ok(state.db.accept_business_setup(&token, &request).await?)
}
pub(crate) async fn register_owner(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<oxsum_core::BusinessSetupRegistrationRequest>,
) -> ApiResult<oxsum_core::BusinessSetupResult> {
    ok(state.db.register_business_owner(&request).await?)
}

pub(crate) fn personal_token(headers: &HeaderMap) -> Result<String, ApiError> {
    if crate::auth::has_machine_credential(headers) {
        return Err(ApiError::Forbidden(
            "business authority requires a personal session".into(),
        ));
    }
    crate::auth::session_cookie_value(headers).ok_or(ApiError::Unauthorized)
}

pub(crate) async fn context(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<oxsum_core::SessionContextInfo> {
    ok(state.db.session_context(&personal_token(&headers)?).await?)
}
pub(crate) async fn select_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::SessionContextRequest>,
) -> ApiResult<oxsum_core::SessionContextInfo> {
    ok(state
        .db
        .switch_session_context(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn business(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<oxsum_core::BusinessPrincipal> {
    ok(state
        .db
        .business_capability(
            &personal_token(&headers)?,
            oxsum_core::BusinessCapability::OverviewRead,
        )
        .await?)
}
pub(crate) async fn staff(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Vec<oxsum_core::BusinessStaff>> {
    ok(state.db.business_staff(&personal_token(&headers)?).await?)
}
pub(crate) async fn set_staff(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessStaffRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .set_business_staff(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn revoke_staff(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessStaffRevokeRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .revoke_business_staff(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn transfer_ownership(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessOwnershipRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .transfer_business_ownership(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn registration(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::BusinessRegistrationRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .set_business_registration(&personal_token(&headers)?, &request)
        .await?)
}

#[derive(serde::Deserialize)]
pub(crate) struct CustomerQuery {
    after: Option<uuid::Uuid>,
    limit: Option<i64>,
}
pub(crate) async fn customers(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<CustomerQuery>,
) -> ApiResult<oxsum_core::BusinessCustomerPage> {
    if crate::auth::has_machine_credential(&headers) {
        let service = integration(&state, &headers).await?;
        return ok(state
            .db
            .integration_customers(&service, query.after, query.limit.unwrap_or(50))
            .await?);
    }
    ok(state
        .db
        .business_customers(
            &personal_token(&headers)?,
            query.after,
            query.limit.unwrap_or(50),
        )
        .await?)
}
pub(crate) async fn customer(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> ApiResult<oxsum_core::BusinessCustomer> {
    if crate::auth::has_machine_credential(&headers) {
        let service = integration(&state, &headers).await?;
        return ok(state.db.integration_customer(&service, id).await?);
    }
    ok(state
        .db
        .business_customer(&personal_token(&headers)?, id)
        .await?)
}
pub(crate) async fn customer_credit_terms(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> ApiResult<oxsum_core::BusinessCustomerCreditTerms> {
    ok(state
        .db
        .business_customer_credit_terms(&personal_token(&headers)?, id)
        .await?)
}
pub(crate) async fn set_customer_credit_terms(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    ApiJson(request): ApiJson<oxsum_core::BusinessCustomerCreditTermsRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .set_business_customer_credit_terms(
            &personal_token(&headers)?,
            &state.tenants,
            id,
            &request,
        )
        .await?)
}
pub(crate) async fn provision_customer(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::ProvisionCustomerRequest>,
) -> ApiResult<oxsum_core::BusinessCustomer> {
    // Provisioning is a service grant only — a staff session is refused at the
    // same door as every integration-authenticated call.
    let service = integration(&state, &headers).await?;
    ok(state
        .db
        .provision_integration_customer(&service, &request)
        .await?)
}

/// Optional `?before=<index>&limit=<n>` on the customer billing read.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CustomerBillingQuery {
    before: Option<u64>,
    limit: Option<usize>,
}

/// `GET /business/customers/{organization_id}/billing` — integration
/// `billingRead` for one granted customer: its current balance plus a page of
/// the ledger entries that moved money. Service-only; a personal session is
/// rejected like every other machine credential on this surface is `401`.
pub(crate) async fn customer_billing(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    ApiQuery(query): ApiQuery<CustomerBillingQuery>,
) -> ApiResult<CustomerBillingRes> {
    if !crate::auth::has_machine_credential(&headers) {
        return Err(ApiError::Unauthorized);
    }
    let service = integration(&state, &headers).await?;
    let tenant_id = state.db.integration_customer_tenant(&service, id).await?;
    let w = state.tenants.get(&tenant_id).await?;
    let limit = query.limit.unwrap_or(20);
    if !(1..=50).contains(&limit) {
        return Err(ApiError::Validation(
            "limit must be between 1 and 50".into(),
        ));
    }
    let page = w.transactions_page(query.before, limit).await?;
    ok(CustomerBillingRes {
        balance: CustomerBalanceRes {
            available_minor: w.available().await?,
            credit_limit_minor: w.credit_limit().await?,
            credit_used_minor: w.credit_used().await?,
            reserved_minor: w.reserved().await?,
            spent_this_month_minor: w.month_spend().await?,
        },
        bills: CustomerBillsRes {
            rows: page
                .rows
                .into_iter()
                .map(|row| CustomerBillRow {
                    index: row.index,
                    entry_id: row.id,
                    booked_on: row.booked_on.to_string(),
                    kind: match row.kind {
                        oxsum_core::TransactionKind::TopUp => "topUp",
                        oxsum_core::TransactionKind::Adjustment => "adjustment",
                        oxsum_core::TransactionKind::Settlement => "settlement",
                    },
                    amount_minor: row.amount_minor,
                    description: row.description,
                    content_hash: row.content_hash,
                })
                .collect(),
            next_cursor: page.next_cursor,
        },
    })
}

/// The `CustomerBilling` schema: a granted customer's balance and bill window.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CustomerBillingRes {
    balance: CustomerBalanceRes,
    bills: CustomerBillsRes,
}

/// The balance half — the same fields the customer `GET /balance` returns.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CustomerBalanceRes {
    available_minor: i64,
    credit_limit_minor: i64,
    credit_used_minor: i64,
    reserved_minor: i64,
    spent_this_month_minor: i64,
}

/// The bill window half — the same row shape `GET /bills` returns.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CustomerBillsRes {
    rows: Vec<CustomerBillRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<u64>,
}

/// One transaction in the customer's bill log.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CustomerBillRow {
    index: u64,
    entry_id: String,
    booked_on: String,
    kind: &'static str,
    amount_minor: i64,
    description: String,
    content_hash: String,
}
pub(crate) async fn update_customer(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    ApiJson(request): ApiJson<oxsum_core::BusinessCustomerUpdate>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .update_business_customer(&personal_token(&headers)?, id, &request)
        .await?)
}
pub(crate) async fn customer_lifecycle(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    ApiJson(request): ApiJson<oxsum_core::BusinessCustomerLifecycle>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .set_customer_lifecycle(&personal_token(&headers)?, id, &request)
        .await?)
}
pub(crate) async fn customer_suspension(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    ApiJson(request): ApiJson<oxsum_core::BusinessCustomerSuspension>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .set_customer_suspension(&personal_token(&headers)?, id, &request)
        .await?)
}

pub(crate) async fn revoke_customer_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    ApiJson(request): ApiJson<oxsum_core::InvitationRevokeRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .revoke_invitation(
            &personal_token(&headers)?,
            id,
            oxsum_core::InvitationScope::Customer,
            &request,
        )
        .await?)
}

pub(crate) async fn integration(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<oxsum_core::ActingService, ApiError> {
    let secret = crate::auth::bearer(headers).ok_or(ApiError::Unauthorized)?;
    state
        .db
        .authenticate_service(secret)
        .await?
        .ok_or(ApiError::Unauthorized)
}
pub(crate) async fn integration_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<CustomerQuery>,
) -> ApiResult<oxsum_core::IntegrationCredentialPage> {
    ok(state
        .db
        .integration_credentials(
            &personal_token(&headers)?,
            query.after,
            query.limit.unwrap_or(50),
        )
        .await?)
}
pub(crate) async fn create_integration_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::IntegrationCredentialRequest>,
) -> ApiResult<oxsum_core::CreatedIntegrationCredential> {
    ok(state
        .db
        .create_integration_credential(&personal_token(&headers)?, &request)
        .await?)
}
pub(crate) async fn revoke_integration_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    ApiJson(request): ApiJson<oxsum_core::IntegrationCredentialRevokeRequest>,
) -> ApiResult<oxsum_core::BusinessMutationResult> {
    ok(state
        .db
        .revoke_integration_credential(&personal_token(&headers)?, id, &request)
        .await?)
}

pub(crate) async fn collection_terms(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<oxsum_core::CollectionTerms> {
    ok(state
        .db
        .business_collection_terms(&personal_token(&headers)?)
        .await?)
}
pub(crate) async fn set_collection_terms(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<oxsum_core::CollectionTermsRequest>,
) -> ApiResult<oxsum_core::CollectionTermsMutation> {
    ok(state
        .db
        .set_collection_terms(&personal_token(&headers)?, &request)
        .await?)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MonthlyStatementQuery {
    period: Option<String>,
    after: Option<uuid::Uuid>,
    limit: Option<i64>,
}

pub(crate) async fn monthly_statements(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(customer): axum::extract::Path<uuid::Uuid>,
    crate::error::ApiQuery(query): crate::error::ApiQuery<MonthlyStatementQuery>,
) -> ApiResult<oxsum_core::BusinessMonthlyStatementPage> {
    ok(state
        .db
        .business_monthly_statements(
            &personal_token(&headers)?,
            customer,
            query.period.as_deref(),
            query.after,
            query.limit.unwrap_or(20),
        )
        .await?)
}

pub(crate) async fn monthly_statement(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path((customer, statement)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> ApiResult<oxsum_core::BusinessMonthlyStatementDetail> {
    ok(state
        .db
        .business_monthly_statement(&personal_token(&headers)?, customer, statement)
        .await?)
}

pub(crate) async fn monthly_statement_action(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(customer): axum::extract::Path<uuid::Uuid>,
    ApiJson(request): ApiJson<oxsum_core::BusinessMonthlyStatementAction>,
) -> ApiResult<oxsum_core::BusinessMonthlyStatementActionResult> {
    ok(state
        .db
        .mutate_business_monthly_statement(
            &personal_token(&headers)?,
            &state.tenants,
            customer,
            &request,
        )
        .await?)
}

/// Record verified money through current personal staff authority.
pub(crate) async fn monthly_payment(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path((customer, statement)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
    ApiJson(request): ApiJson<oxsum_core::BusinessMonthlyPaymentRequest>,
) -> ApiResult<oxsum_core::BusinessMonthlyPaymentResult> {
    ok(state
        .db
        .record_business_monthly_payment(
            &personal_token(&headers)?,
            &state.tenants,
            customer,
            statement,
            &request,
        )
        .await?)
}

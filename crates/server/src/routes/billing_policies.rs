//! Policy publication and customer-consent endpoints with live core authority.
use crate::AppState;
use crate::auth::PersonalToken;
use crate::error::ApiJson;
use crate::routes::{ApiResult, ok};
use axum::extract::{Extension, State};
use oxsum_core::{
    BillingPolicyAccept, BillingPolicyAcceptance, BillingPolicyPublish, BillingPolicyVersion,
    CustomerBillingPolicy,
};
pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
) -> ApiResult<Vec<BillingPolicyVersion>> {
    ok(state.db.business_billing_policies(&token).await?)
}
pub(crate) async fn publish(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
    ApiJson(request): ApiJson<BillingPolicyPublish>,
) -> ApiResult<BillingPolicyVersion> {
    ok(state.db.publish_billing_policy(&token, &request).await?)
}
pub(crate) async fn customer(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
) -> ApiResult<CustomerBillingPolicy> {
    ok(state.db.customer_billing_policy(&token).await?)
}
pub(crate) async fn accept(
    State(state): State<AppState>,
    Extension(token): Extension<PersonalToken>,
    ApiJson(request): ApiJson<BillingPolicyAccept>,
) -> ApiResult<BillingPolicyAcceptance> {
    ok(state.db.accept_billing_policy(&token, &request).await?)
}

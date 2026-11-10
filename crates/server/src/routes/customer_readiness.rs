//! Personal, scoped first-call evidence without a closed-period report prerequisite.

use axum::extract::{Extension, State};
use oxsum_core::{CustomerReceiptReadiness, CustomerServingReadiness, Principal};

use crate::AppState;
use crate::routes::{ApiResult, ok};

pub(crate) async fn receipt(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<CustomerReceiptReadiness> {
    ok(state.db.customer_receipt_readiness(&principal).await?)
}

pub(crate) async fn serving(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> ApiResult<CustomerServingReadiness> {
    ok(state.db.customer_serving_readiness(&principal).await?)
}

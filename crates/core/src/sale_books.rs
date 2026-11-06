//! Business-owned sale snapshots: immutable revisions and effective term histories.
//!
//! The book is the commercial price group. Request groups remain channel authority.
//! Gateway/proof adoption is separate; these APIs never change existing channel prices.
mod store;
mod types;
pub use types::*;

use crate::{
    BusinessCapability, BusinessPrincipal, BusinessRole, WalletError, business_staff as authority,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}
fn missing() -> WalletError {
    WalletError::NotFound("sale resource not found".into())
}
fn stored<T: DeserializeOwned>(value: Value) -> Result<T, WalletError> {
    serde_json::from_value(value)
        .map_err(|e| WalletError::Misconfigured(format!("stored sale terms: {e}")))
}
fn instant(at: OffsetDateTime) -> Result<OffsetDateTime, WalletError> {
    let at = at.to_offset(time::UtcOffset::UTC);
    at.replace_nanosecond(at.nanosecond() / 1000 * 1000)
        .map_err(|_| invalid("invalid effective instant"))
}
fn model_name(model: &str) -> Result<(), WalletError> {
    if crate::channels::model_name(model)? != model {
        return Err(invalid("model cannot have surrounding whitespace"));
    }
    Ok(())
}
async fn read_authority(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    token: &str,
) -> Result<BusinessPrincipal, WalletError> {
    let actor =
        authority::require_capability(tx, business, token, BusinessCapability::OverviewRead)
            .await?;
    if !matches!(
        actor.role,
        BusinessRole::Owner | BusinessRole::Admin | BusinessRole::Finance
    ) {
        return Err(WalletError::Forbidden(
            "sale terms require owner, admin or finance".into(),
        ));
    }
    Ok(actor)
}

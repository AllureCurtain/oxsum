//! Bind exact accepted policy and sanitized retail context before paid execution.
use super::store::{canonical, current_in, digest, version_in};
use super::{GatewayBillingTerms, PolicySelection};
use crate::{Db, NewOperation, SalePrice, WalletError};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

impl Db {
    /// Read the expected selection; admission compares it under the shared authority lock.
    pub async fn billing_policy_selection(
        &self,
        organization: Uuid,
    ) -> Result<PolicySelection, WalletError> {
        let mut tx = self.pool().begin().await?;
        self.billing_policy_selection_in(&mut tx, organization)
            .await
    }
    pub(crate) async fn billing_policy_selection_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        organization: Uuid,
    ) -> Result<PolicySelection, WalletError> {
        let acceptance = current_in(tx, organization).await?;
        let policy = if let Some(a) = &acceptance {
            let business: Uuid = sqlx::query_scalar(
                "SELECT business_id FROM oxsum.customer_profiles WHERE organization_id=$1",
            )
            .bind(organization)
            .fetch_one(&mut **tx)
            .await?;
            let policy = version_in(tx, business, a.version).await?;
            if policy.digest != a.digest {
                return Err(WalletError::Misconfigured(
                    "accepted policy digest mismatch".into(),
                ));
            }
            Some(policy)
        } else {
            None
        };
        Ok(PolicySelection { acceptance, policy })
    }
    pub(crate) async fn bind_billing_terms_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        operation: &NewOperation,
        terms: &GatewayBillingTerms,
    ) -> Result<(), WalletError> {
        if terms.operation_id != operation.operation_id
            || terms.organization_id != operation.organization_id
            || Some(terms.key_id) != operation.key_id
            || terms.hold_key != format!("req-{}:hold", operation.operation_id)
            || terms.version != 1
            || terms.arithmetic_version != "credit-1e6-ceil-discount-cap-v1"
            || terms.policy_applies != (terms.surface == "chat/completions" && !terms.stream)
            || terms.ceiling_minor != operation.bound_minor
            || terms.routes.is_empty()
            || (terms.provider_reported() && terms.routes.len() > 128)
            || terms.output_bound < 0
        {
            return Err(WalletError::InvalidInput(
                "invalid admission billing context".into(),
            ));
        }
        let business: Uuid = sqlx::query_scalar(
            "SELECT business_id FROM oxsum.customer_profiles WHERE organization_id=$1",
        )
        .bind(operation.organization_id)
        .fetch_one(&mut **tx)
        .await?;
        if business != terms.business_id
            || !terms.routes.iter().any(|r| {
                r.model == operation.model
                    && Some(r.channel.as_str()) == operation.channel.as_deref()
                    && Some(r.price_version) == operation.terms_version
            })
            || terms
                .discount_percent
                .is_some_and(|d| !(0..=100).contains(&d))
        {
            return Err(WalletError::InvalidInput(
                "billing context does not match admitted route".into(),
            ));
        }
        if let Some(digest) = &terms.sale_digest {
            let canonical:Option<String>=sqlx::query_scalar("SELECT canonical_row FROM oxsum.sale_snapshot_digests WHERE digest=$1 AND business_id=$2 AND organization_id=$3 AND model=$4").bind(digest).bind(business).bind(operation.organization_id).bind(&operation.model).fetch_optional(&mut **tx).await?;
            let canonical = canonical.ok_or_else(|| {
                WalletError::InvalidInput("sale snapshot not owned by admission".into())
            })?;
            if crate::sale_snapshot_hash(&canonical).to_hex() != *digest {
                return Err(WalletError::Misconfigured(
                    "sale snapshot digest mismatch".into(),
                ));
            }
        }
        let selection = self
            .billing_policy_selection_in(tx, operation.organization_id)
            .await?;
        if selection.acceptance != terms.selection.acceptance
            || selection.policy.as_ref().map(|p| (&p.digest, p.version))
                != terms
                    .selection
                    .policy
                    .as_ref()
                    .map(|p| (&p.digest, p.version))
        {
            return Err(WalletError::Conflict(
                "billing policy changed during admission; retry".into(),
            ));
        }
        let mut reviewed = terms.selection.clone();
        if let (Some(reviewed), Some(current)) = (&mut reviewed.policy, &selection.policy) {
            reviewed.runtime_ready = current.runtime_ready;
        }
        if reviewed != selection {
            return Err(WalletError::InvalidInput(
                "billing context contains altered policy terms".into(),
            ));
        }
        if terms.policy_applies && selection.policy.as_ref().is_some_and(|p| !p.runtime_ready) {
            return Err(WalletError::Conflict(
                "accepted policy implementation is unavailable".into(),
            ));
        }
        for route in &terms.routes {
            if terms.provider_reported() {
                SalePrice::try_new(route.price.clone())?;
            } else if route.price.upstream.is_some()
                || route.price.rules.iter().any(|r| r.price.upstream.is_some())
            {
                return Err(WalletError::InvalidInput(
                    "supplier rates cannot enter retail terms".into(),
                ));
            }
        }
        let canonical = canonical(terms)?;
        if terms.provider_reported() && canonical.len() > 256 * 1024 {
            return Err(WalletError::InvalidInput(
                "admission billing context exceeds evidence bound".into(),
            ));
        }
        let digest = digest("oxsum/operation-billing-terms/v1", &canonical);
        sqlx::query("INSERT INTO oxsum.operation_billing_terms(operation_id,organization_id,canonical,digest) VALUES($1,$2,$3,$4)").bind(operation.operation_id).bind(operation.organization_id).bind(canonical).bind(digest).execute(&mut **tx).await?;
        Ok(())
    }
    /// Hash-checked historical admission contract, scoped to its customer.
    pub async fn operation_billing_terms(
        &self,
        organization: Uuid,
        operation: Uuid,
    ) -> Result<Option<GatewayBillingTerms>, WalletError> {
        let mut tx = self.pool().begin().await?;
        self.operation_billing_terms_in(&mut tx, organization, operation)
            .await
    }
    pub(crate) async fn operation_billing_terms_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        organization: Uuid,
        operation: Uuid,
    ) -> Result<Option<GatewayBillingTerms>, WalletError> {
        let row=sqlx::query("SELECT canonical,digest FROM oxsum.operation_billing_terms WHERE organization_id=$1 AND operation_id=$2").bind(organization).bind(operation).fetch_optional(&mut **tx).await?;
        row.map(|row| {
            let text: String = row.try_get("canonical")?;
            if digest("oxsum/operation-billing-terms/v1", &text)
                != row.try_get::<String, _>("digest")?
            {
                return Err(WalletError::Misconfigured(
                    "admission terms digest mismatch".into(),
                ));
            }
            let terms: GatewayBillingTerms =
                serde_json::from_str(&text).map_err(crate::error::invalid)?;
            if terms.organization_id != organization
                || terms.operation_id != operation
                || terms.version != 1
                || terms.arithmetic_version != "credit-1e6-ceil-discount-cap-v1"
            {
                return Err(WalletError::Misconfigured(
                    "unsupported or mismatched billing context".into(),
                ));
            }
            Ok(terms)
        })
        .transpose()
    }
}

//! Explicit business integration grants. Secrets exist only at first issuance.
use crate::{
    BusinessCapability, BusinessMutationResult, Db, WalletError, business_staff as authority,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use std::fmt::Write as _;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// A capability an integration credential may exercise on behalf of a business.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntegrationScope {
    /// Read customer profiles granted to the credential.
    CustomersRead,
    /// Submit usage metering for payments on behalf of the business.
    MeteringWrite,
    /// Provision service-managed customer accounts the credential then owns.
    CustomersProvision,
    /// Read a granted customer's balance and bill log. Read-only.
    BillingRead,
    /// Archive accounts created inside the selected application.
    CustomersLifecycleWrite,
    /// Issue and revoke bounded gateway keys for this application's own accounts.
    CustomerKeysWrite,
    /// Read application customer entitlement and published sale terms without mutations.
    EntitlementsRead,
    /// Invite a verified person to own this application's unclaimed customer account.
    CustomerLinkWrite,
    /// Read retained customer-safe evidence for this application's own accounts.
    EvidenceRead,
    /// Initiate and recover this application's own customer checkout orders.
    CheckoutWrite,
    /// Publish bounded customer-safe evidence for this application.
    EvidenceWrite,
}
impl IntegrationScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CustomersRead => "customersRead",
            Self::MeteringWrite => "meteringWrite",
            Self::CustomersProvision => "customersProvision",
            Self::BillingRead => "billingRead",
            Self::CustomersLifecycleWrite => "customersLifecycleWrite",
            Self::CustomerKeysWrite => "customerKeysWrite",
            Self::EntitlementsRead => "entitlementsRead",
            Self::CustomerLinkWrite => "customerLinkWrite",
            Self::EvidenceRead => "evidenceRead",
            Self::CheckoutWrite => "checkoutWrite",
            Self::EvidenceWrite => "evidenceWrite",
        }
    }
}
/// Request to provision a service-managed customer account.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvisionCustomerRequest {
    /// Caller-supplied key that makes repeated provisions return the same customer.
    pub idempotency_key: String,
    /// Caller-chosen identifier, unique within the business — the namespace key.
    pub external_id: String,
    /// Display name of the customer account, 1 to 80 characters.
    pub name: String,
    /// Optional contact email for the customer.
    pub contact_email: Option<String>,
}
/// Request to issue an integration credential.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegrationCredentialRequest {
    /// Stable owner-selected namespace, or explicit legacy customer grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<Uuid>,
    /// Caller-supplied key that makes repeated requests return the same credential.
    pub idempotency_key: String,
    /// Optional human-readable label, at most 80 characters.
    pub name: Option<String>,
    /// Scopes the credential is allowed to use; must be non-empty and distinct.
    pub scopes: Vec<IntegrationScope>,
    /// Customers the credential may access; 1 to 1000 distinct organization IDs.
    pub customer_ids: Vec<Uuid>,
    /// When the credential stops authenticating, within the next 365 days.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Operator-visible justification recorded in the audit log.
    pub reason: String,
}
/// Request to revoke an integration credential.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IntegrationCredentialRevokeRequest {
    /// Caller-supplied key that makes repeated revocations idempotent.
    pub idempotency_key: String,
    /// Operator-visible justification recorded in the audit log.
    pub reason: String,
}
/// An issued integration credential; the secret is stored only as a hash.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationCredential {
    /// Application namespace, absent for legacy explicitly granted credentials.
    pub application_id: Option<Uuid>,
    /// Credential identifier, serialized as `credentialId`.
    #[serde(rename = "credentialId")]
    pub id: Uuid,
    /// Business the credential acts for.
    pub business_id: Uuid,
    /// Optional human-readable label.
    pub name: Option<String>,
    /// Non-secret prefix of the credential secret, for display and lookup.
    pub prefix: String,
    /// Scopes granted to the credential.
    pub scopes: Vec<IntegrationScope>,
    /// Customers the credential may access.
    pub customer_ids: Vec<Uuid>,
    /// When the credential was issued.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the credential stops authenticating.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// When the credential was revoked, if it has been.
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}
/// A newly issued credential, including its one-time plaintext secret.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedIntegrationCredential {
    /// Credential metadata.
    #[serde(flatten)]
    pub credential: IntegrationCredential,
    /// Plaintext secret, returned only on first issuance.
    pub secret: Option<String>,
}
/// A cursor-paginated page of integration credentials.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationCredentialPage {
    /// Credentials in ascending identifier order.
    pub credentials: Vec<IntegrationCredential>,
    /// Identifier to pass as `after` for the next page; `None` at the end.
    pub next_cursor: Option<Uuid>,
}
/// An authenticated identity, not a cached permission grant. Core operations
/// recheck current expiry, revocation, scope and customer ownership.
#[derive(Debug, Clone)]
pub struct ActingService {
    pub(crate) application_id: Option<Uuid>,
    pub(crate) credential_id: Uuid,
    pub(crate) business_id: Uuid,
    pub(crate) name: Option<String>,
}
impl ActingService {
    /// Identifier of the credential that authenticated this service.
    #[must_use]
    pub fn credential_id(&self) -> Uuid {
        self.credential_id
    }
}
const COLUMNS: &str = "c.application_id,c.credential_id,c.business_id,c.name,c.prefix,c.scopes,c.created_at,c.expires_at,c.revoked_at,ARRAY(SELECT a.organization_id FROM oxsum.integration_credential_customers a WHERE a.credential_id=c.credential_id ORDER BY a.organization_id) AS customer_ids";
fn from_row(row: &sqlx::postgres::PgRow) -> Result<IntegrationCredential, WalletError> {
    let scopes: Vec<String> = row.try_get("scopes")?;
    let scopes = scopes
        .into_iter()
        .map(|s| match s.as_str() {
            "customersRead" => Ok(IntegrationScope::CustomersRead),
            "meteringWrite" => Ok(IntegrationScope::MeteringWrite),
            "customersProvision" => Ok(IntegrationScope::CustomersProvision),
            "billingRead" => Ok(IntegrationScope::BillingRead),
            "customersLifecycleWrite" => Ok(IntegrationScope::CustomersLifecycleWrite),
            "customerKeysWrite" => Ok(IntegrationScope::CustomerKeysWrite),
            "entitlementsRead" => Ok(IntegrationScope::EntitlementsRead),
            "customerLinkWrite" => Ok(IntegrationScope::CustomerLinkWrite),
            "evidenceRead" => Ok(IntegrationScope::EvidenceRead),
            "checkoutWrite" => Ok(IntegrationScope::CheckoutWrite),
            "evidenceWrite" => Ok(IntegrationScope::EvidenceWrite),
            _ => Err(WalletError::Forbidden("unknown integration scope".into())),
        })
        .collect::<Result<_, _>>()?;
    Ok(IntegrationCredential {
        application_id: row.try_get("application_id")?,
        id: row.try_get("credential_id")?,
        business_id: row.try_get("business_id")?,
        name: row.try_get("name")?,
        prefix: row.try_get("prefix")?,
        scopes,
        customer_ids: row.try_get("customer_ids")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}
async fn credential(
    tx: &mut Transaction<'_, Postgres>,
    business: Uuid,
    id: Uuid,
) -> Result<IntegrationCredential, WalletError> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM oxsum.integration_credentials c WHERE c.business_id=$1 AND c.credential_id=$2"))
        .bind(business).bind(id).fetch_optional(&mut **tx).await?.ok_or_else(|| WalletError::NotFound("credential is not known".into()))?;
    from_row(&row)
}
/// Holds a shared credential lock until the caller commits its authorization.
/// Revocation cannot commit before this request's authorization is decided.
pub(crate) async fn require_integration(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    scope: IntegrationScope,
    customer: Option<Uuid>,
) -> Result<(), WalletError> {
    let row = sqlx::query("SELECT scopes,application_id FROM oxsum.integration_credentials WHERE credential_id=$1 AND business_id=$2 AND revoked_at IS NULL AND expires_at>clock_timestamp() FOR SHARE")
        .bind(service.credential_id).bind(service.business_id).fetch_optional(&mut **tx).await?.ok_or(WalletError::InvalidCredentials)?;
    if row.try_get::<Option<Uuid>, _>("application_id")? != service.application_id {
        return Err(WalletError::InvalidCredentials);
    }
    let scopes: Vec<String> = row.try_get("scopes")?;
    if !scopes.iter().any(|s| s == scope.as_str()) {
        return Err(WalletError::Forbidden(
            "integration scope is not granted".into(),
        ));
    }
    if let Some(customer) = customer {
        let application: Option<Uuid> = row.try_get("application_id")?;
        let allowed: bool = sqlx::query_scalar("SELECT CASE WHEN $4::uuid IS NULL THEN EXISTS(SELECT 1 FROM oxsum.integration_credential_customers a JOIN oxsum.customer_profiles c USING (business_id,organization_id) WHERE a.business_id=$1 AND a.credential_id=$2 AND a.organization_id=$3) ELSE EXISTS(SELECT 1 FROM oxsum.application_customers a WHERE a.business_id=$1 AND a.application_id=$4 AND a.organization_id=$3) END")
            .bind(service.business_id).bind(service.credential_id).bind(customer).bind(application).fetch_one(&mut **tx).await?;
        if !allowed {
            return Err(WalletError::NotFound("customer is not known".into()));
        }
    }
    Ok(())
}
impl Db {
    /// Issues an integration credential for the business. The plaintext secret
    /// is returned once; retries with the same key return it as `None`.
    pub async fn create_integration_credential(
        &self,
        token: &str,
        request: &IntegrationCredentialRequest,
    ) -> Result<CreatedIntegrationCredential, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::IntegrationsManage,
        )
        .await?;
        let detail = json!(request);
        let action = crate::audit::action::BUSINESS_INTEGRATION_CREATE;
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return Ok(CreatedIntegrationCredential {
                credential: credential(&mut tx, business, id).await?,
                secret: None,
            });
        }
        let name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty());
        if name.is_some_and(|n| n.chars().count() > 80) {
            return Err(WalletError::InvalidInput(
                "name must be at most 80 characters".into(),
            ));
        }
        let mut scopes: Vec<&str> = request.scopes.iter().map(|s| s.as_str()).collect();
        scopes.sort_unstable();
        scopes.dedup();
        let mut customers = request.customer_ids.clone();
        customers.sort_unstable();
        customers.dedup();
        // A pure customersProvision grant starts with an empty allowlist and
        // grows its own namespace; every other scope reads or meters an
        // existing customer, so it must name at least one.
        let provision_only =
            !scopes.is_empty() && scopes.iter().all(|s| *s == "customersProvision");
        if scopes.is_empty()
            || scopes.len() != request.scopes.len()
            || customers.len() > 1000
            || customers.len() != request.customer_ids.len()
            || (request.application_id.is_none() && !provision_only && customers.is_empty())
            || (request.application_id.is_some() && !customers.is_empty())
            || (request.application_id.is_none()
                && (scopes.contains(&"customersLifecycleWrite")
                    || scopes.contains(&"customerKeysWrite")
                    || scopes.contains(&"entitlementsRead")
                    || scopes.contains(&"customerLinkWrite")
                    || scopes.contains(&"evidenceRead")
                    || scopes.contains(&"checkoutWrite")
                    || scopes.contains(&"evidenceWrite")))
        {
            return Err(WalletError::InvalidInput(
                "scopes must be distinct; application grants require empty customerIds, legacy grants require 1 to 1000 distinct ids or customersProvision alone".into(),
            ));
        }
        if let Some(application) = request.application_id {
            crate::integration_applications::application_in(&mut tx, business, application).await?;
        }
        let now = OffsetDateTime::now_utc();
        if request.expires_at <= now || request.expires_at > now + Duration::days(365) {
            return Err(WalletError::InvalidInput(
                "expiresAt must be in the next 365 days".into(),
            ));
        }
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM oxsum.customer_profiles WHERE business_id=$1 AND organization_id=ANY($2)").bind(business).bind(&customers).fetch_one(&mut *tx).await?;
        if count != customers.len() as i64 {
            return Err(WalletError::NotFound("customer is not known".into()));
        }
        let receipt = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let mut secret = String::from("oxs-int-");
        for byte in bytes {
            let _ = write!(secret, "{byte:02x}");
        }
        let prefix: String = secret.chars().take(16).collect();
        sqlx::query("INSERT INTO oxsum.integration_credentials (credential_id,business_id,name,prefix,secret_hash,scopes,expires_at,application_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(receipt.id).bind(business).bind(name).bind(prefix).bind(Sha256::digest(secret.as_bytes()).to_vec()).bind(scopes).bind(request.expires_at).bind(request.application_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO oxsum.integration_credential_customers (business_id,credential_id,organization_id) SELECT $1,$2,unnest($3::uuid[])")
            .bind(business).bind(receipt.id).bind(customers).execute(&mut *tx).await?;
        let credential = credential(&mut tx, business, receipt.id).await?;
        tx.commit().await?;
        Ok(CreatedIntegrationCredential {
            credential,
            secret: Some(secret),
        })
    }
    /// Lists credentials for the business in cursor order. `limit` must be
    /// between 1 and 100.
    pub async fn integration_credentials(
        &self,
        token: &str,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<IntegrationCredentialPage, WalletError> {
        if !(1..=100).contains(&limit) {
            return Err(WalletError::InvalidInput(
                "limit must be between 1 and 100".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::IntegrationsManage,
        )
        .await?;
        let rows=sqlx::query(&format!("SELECT {COLUMNS} FROM oxsum.integration_credentials c WHERE c.business_id=$1 AND ($2::uuid IS NULL OR c.credential_id>$2) ORDER BY c.credential_id LIMIT $3"))
            .bind(business).bind(after).bind(limit+1).fetch_all(&mut *tx).await?;
        let mut credentials = rows.iter().map(from_row).collect::<Result<Vec<_>, _>>()?;
        let next_cursor = if credentials.len() > limit as usize {
            credentials.truncate(limit as usize);
            credentials.last().map(|c| c.id)
        } else {
            None
        };
        Ok(IntegrationCredentialPage {
            credentials,
            next_cursor,
        })
    }
    /// Revokes a credential; already-revoked credentials keep their first
    /// revocation time and repeat calls are idempotent.
    pub async fn revoke_integration_credential(
        &self,
        token: &str,
        id: Uuid,
        request: &IntegrationCredentialRevokeRequest,
    ) -> Result<BusinessMutationResult, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        let actor = authority::require_capability(
            &mut tx,
            business,
            token,
            BusinessCapability::IntegrationsManage,
        )
        .await?;
        let detail = json!({"credentialId":id,"request":request});
        let action = crate::audit::action::BUSINESS_INTEGRATION_REVOKE;
        if let Some(id) = authority::replay(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?
        {
            return Ok(BusinessMutationResult { id });
        }
        credential(&mut tx, business, id).await?;
        sqlx::query("UPDATE oxsum.integration_credentials SET revoked_at=COALESCE(revoked_at,now()) WHERE business_id=$1 AND credential_id=$2").bind(business).bind(id).execute(&mut *tx).await?;
        let result = authority::record(
            &mut tx,
            business,
            actor.user_id,
            &request.idempotency_key,
            action,
            &detail,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Provisions a service-managed customer billing account under the calling
    /// credential's business — the N21 embedded-app onboarding path. The
    /// account has no human login and no staff membership; the credential that
    /// created it is granted access through `integration_credential_customers`,
    /// so the namespace is exactly the customers it created plus any the owner
    /// already allowlisted.
    ///
    /// Exact operation keys retain normalized request terms and the original
    /// receipt. Existing external identities cannot be adopted by another key.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] without a live `customersProvision` grant;
    /// [`WalletError::Conflict`] when `externalId` is already assigned.
    pub async fn provision_integration_customer(
        &self,
        service: &ActingService,
        request: &ProvisionCustomerRequest,
    ) -> Result<crate::BusinessCustomer, WalletError> {
        self.provision_legacy_integration_customer(service, request)
            .await
    }
    /// Resolves a plaintext integration secret to its acting service. Returns
    /// `None` for unknown, revoked or expired credentials.
    pub async fn authenticate_service(
        &self,
        secret: &str,
    ) -> Result<Option<ActingService>, WalletError> {
        if !secret.starts_with("oxs-int-") {
            return Ok(None);
        }
        let row=sqlx::query("SELECT credential_id,business_id,name,application_id FROM oxsum.integration_credentials WHERE secret_hash=$1 AND revoked_at IS NULL AND expires_at>clock_timestamp()")
            .bind(Sha256::digest(secret.as_bytes()).to_vec()).fetch_optional(self.pool()).await?;
        row.map(|row| {
            Ok(ActingService {
                application_id: row.try_get("application_id")?,
                credential_id: row.try_get("credential_id")?,
                business_id: row.try_get("business_id")?,
                name: row.try_get("name")?,
            })
        })
        .transpose()
    }
    /// Resolves a granted customer's ledger tenant id for a billingRead service.
    ///
    /// The customer must be in the credential's grant (`require_integration`
    /// `FOR SHARE` rechecks expiry, revocation, scope and grant on every call);
    /// a foreign, ungranted or unknown customer is reported as not found. The
    /// returned tenant id is what the caller opens a [`crate::Wallet`] for —
    /// the actual balance and bill reads run against that tenant's ledger.
    pub async fn integration_customer_tenant(
        &self,
        service: &ActingService,
        customer: Uuid,
    ) -> Result<String, WalletError> {
        let mut tx = self.pool().begin().await?;
        require_integration(
            &mut tx,
            service,
            IntegrationScope::BillingRead,
            Some(customer),
        )
        .await?;
        let tenant: String = sqlx::query_scalar(
            "SELECT tenant_id FROM oxsum.organizations WHERE organization_id=$1",
        )
        .bind(customer)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| WalletError::NotFound("customer is not known".into()))?;
        tx.commit().await?;
        Ok(tenant)
    }
}

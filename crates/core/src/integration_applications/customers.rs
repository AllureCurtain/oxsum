//! Durable namespace/customer/request identities; no human or money authority.

use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    ActingService, BusinessCustomer, Db, IntegrationScope, ProvisionCustomerRequest, WalletError,
    business_staff as authority, integration_credentials,
};

/// Application external identity and its existing business-owned ledger account.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationCustomer {
    /// Owner-issued stable namespace.
    pub application_id: Uuid,
    /// Caller identity unique within that application.
    pub external_id: String,
    /// Existing business customer, with its unchanged business external identity.
    pub customer: BusinessCustomer,
}

/// Archive is a one-way application operation; money and history are retained.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationCustomerArchiveRequest {
    /// Exact operation key in the application namespace.
    pub idempotency_key: String,
    /// Explanation retained in service audit.
    pub reason: String,
}

fn external(value: &str) -> Result<&str, WalletError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 200 || value.chars().any(char::is_control) {
        return Err(WalletError::InvalidInput(
            "externalId must be 1 to 200 visible characters".into(),
        ));
    }
    Ok(value)
}

fn normalized(request: &ProvisionCustomerRequest) -> Result<serde_json::Value, WalletError> {
    authority::validate(&request.idempotency_key, None)?;
    let name = request.name.trim();
    let email = request
        .contact_email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if name.is_empty()
        || name.chars().count() > 80
        || name.chars().any(char::is_control)
        || email
            .is_some_and(|s| s.len() > 320 || s.chars().any(char::is_control) || !s.contains('@'))
    {
        return Err(WalletError::InvalidInput(
            "invalid customer name or contact email".into(),
        ));
    }
    Ok(
        serde_json::json!({"externalId":external(&request.external_id)?,"name":name,"contactEmail":email}),
    )
}

async fn namespace_in(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
) -> Result<Option<Uuid>, WalletError> {
    Ok(sqlx::query_scalar("SELECT application_id FROM oxsum.integration_credentials WHERE business_id=$1 AND credential_id=$2")
        .bind(service.business_id).bind(service.credential_id).fetch_one(&mut **tx).await?)
}

async fn replay_in(
    tx: &mut Transaction<'_, Postgres>,
    namespace: Uuid,
    key: &str,
    action: &str,
    request: &serde_json::Value,
) -> Result<Option<serde_json::Value>, WalletError> {
    let row=sqlx::query("SELECT action,request,result FROM oxsum.integration_customer_operations WHERE namespace_id=$1 AND idempotency_key=$2")
        .bind(namespace).bind(key).fetch_optional(&mut **tx).await?;
    let Some(row) = row else { return Ok(None) };
    if row.try_get::<String, _>("action")? != action
        || row.try_get::<serde_json::Value, _>("request")? != *request
    {
        return Err(WalletError::Conflict(
            "operation key names different request terms".into(),
        ));
    }
    Ok(Some(row.try_get("result")?))
}

#[allow(clippy::too_many_arguments)]
async fn record_in(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    namespace: Uuid,
    key: &str,
    action: &str,
    request: &serde_json::Value,
    result: &serde_json::Value,
    organization: Uuid,
) -> Result<(), WalletError> {
    sqlx::query("INSERT INTO oxsum.integration_customer_operations(namespace_id,idempotency_key,business_id,credential_id,action,request,result,organization_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(namespace).bind(key).bind(service.business_id).bind(service.credential_id).bind(action).bind(request).bind(result).bind(organization).execute(&mut **tx).await?;
    // Hash the namespace/key framing into the legacy audit's global key space.
    use sha2::{Digest, Sha256};
    let digest =
        Sha256::digest(serde_json::to_vec(&(namespace, key, action)).map_err(invalid_receipt)?);
    let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    sqlx::query("INSERT INTO oxsum.admin_audit(action,actor,target,detail,idempotency_key) VALUES($1,$2,$3,$4,$5)")
        .bind(format!("business.application.customer.{action}"))
        .bind(format!("service:{}",service.credential_id)).bind(organization.to_string())
        .bind(serde_json::json!({"namespaceId":namespace,"request":request,"result":result}))
        .bind(format!("app:{digest}")).execute(&mut **tx).await?;
    Ok(())
}

fn invalid_receipt(error: impl std::fmt::Display) -> WalletError {
    WalletError::Misconfigured(format!("invalid stored integration receipt: {error}"))
}

async fn app_customer_in(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    app: Uuid,
    id: Uuid,
) -> Result<ApplicationCustomer, WalletError> {
    let external_id:String=sqlx::query_scalar("SELECT external_id FROM oxsum.application_customers WHERE business_id=$1 AND application_id=$2 AND organization_id=$3")
        .bind(service.business_id).bind(app).bind(id).fetch_optional(&mut **tx).await?
        .ok_or_else(||WalletError::NotFound("customer is not known".into()))?;
    Ok(ApplicationCustomer {
        application_id: app,
        external_id,
        customer: crate::customers::customer(tx, service.business_id, id).await?,
    })
}

impl Db {
    /// Provision once within the credential's stable application namespace.
    pub async fn provision_application_customer(
        &self,
        service: &ActingService,
        request: &ProvisionCustomerRequest,
    ) -> Result<ApplicationCustomer, WalletError> {
        let terms = normalized(request)?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        integration_credentials::require_integration(
            &mut tx,
            service,
            IntegrationScope::CustomersProvision,
            None,
        )
        .await?;
        let app = namespace_in(&mut tx, service)
            .await?
            .ok_or_else(|| WalletError::Forbidden("application credential required".into()))?;
        if let Some(result) =
            replay_in(&mut tx, app, &request.idempotency_key, "provision", &terms).await?
        {
            return serde_json::from_value(result).map_err(invalid_receipt);
        }
        let config = super::application_in(&mut tx, business, app).await?;
        let external = external(&request.external_id)?;
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.application_customers WHERE application_id=$1 AND external_id=$2)")
            .bind(app).bind(external).fetch_one(&mut *tx).await?;
        if exists {
            return Err(WalletError::Conflict(
                "externalId already has an account; retrieve it or retry its original operation"
                    .into(),
            ));
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM oxsum.application_customers WHERE application_id=$1",
        )
        .bind(app)
        .fetch_one(&mut *tx)
        .await?;
        if count >= i64::from(config.customer_limit) {
            return Err(WalletError::Conflict(
                "application customer limit reached".into(),
            ));
        }
        let id = Uuid::new_v4();
        crate::orgs::insert(&mut tx, id, request.name.trim(), crate::Kind::Personal).await?;
        sqlx::query("UPDATE oxsum.customer_profiles SET contact_email=$2 WHERE organization_id=$1")
            .bind(id)
            .bind(terms["contactEmail"].as_str())
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO oxsum.application_customers(business_id,application_id,external_id,organization_id) VALUES($1,$2,$3,$4)")
            .bind(business).bind(app).bind(external).bind(id).execute(&mut *tx).await?;
        let result = app_customer_in(&mut tx, service, app, id).await?;
        record_in(
            &mut tx,
            service,
            app,
            &request.idempotency_key,
            "provision",
            &terms,
            &serde_json::json!(result),
            id,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Retrieve only the current credential's application/customer mapping.
    pub async fn application_customer(
        &self,
        service: &ActingService,
        external_id: &str,
    ) -> Result<ApplicationCustomer, WalletError> {
        let external = external(external_id)?;
        let mut tx = self.pool().begin().await?;
        integration_credentials::require_integration(
            &mut tx,
            service,
            IntegrationScope::CustomersRead,
            None,
        )
        .await?;
        let app = namespace_in(&mut tx, service)
            .await?
            .ok_or_else(|| WalletError::Forbidden("application credential required".into()))?;
        let id:Uuid=sqlx::query_scalar("SELECT organization_id FROM oxsum.application_customers WHERE application_id=$1 AND business_id=$2 AND external_id=$3")
            .bind(app).bind(service.business_id).bind(external).fetch_optional(&mut *tx).await?
            .ok_or_else(||WalletError::NotFound("customer is not known".into()))?;
        app_customer_in(&mut tx, service, app, id).await
    }

    /// Archive under a separate scope, preserving source identity and money.
    pub async fn archive_application_customer(
        &self,
        service: &ActingService,
        id: Uuid,
        request: &ApplicationCustomerArchiveRequest,
    ) -> Result<ApplicationCustomer, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let terms = serde_json::json!({"organizationId":id,"reason":request.reason.trim()});
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        integration_credentials::require_integration(
            &mut tx,
            service,
            IntegrationScope::CustomersLifecycleWrite,
            Some(id),
        )
        .await?;
        let app = namespace_in(&mut tx, service)
            .await?
            .ok_or_else(|| WalletError::Forbidden("application credential required".into()))?;
        if let Some(result) =
            replay_in(&mut tx, app, &request.idempotency_key, "archive", &terms).await?
        {
            return serde_json::from_value(result).map_err(invalid_receipt);
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1 || $2::text,0))")
            .bind(crate::wallet::ORG_ADMISSION_LOCK)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        let before = app_customer_in(&mut tx, service, app, id).await?;
        sqlx::query("UPDATE oxsum.customer_profiles SET state='archived',archived_at=COALESCE(archived_at,now()) WHERE organization_id=$1 AND business_id=$2")
            .bind(id).bind(service.business_id).execute(&mut *tx).await?;
        if !before.customer.suspended {
            Self::enqueue_event(
                &mut tx,
                id,
                crate::webhooks::ORG_SUSPENDED,
                serde_json::json!({"organizationId":id,"reason":"archived"}),
            )
            .await?;
        }
        let result = app_customer_in(&mut tx, service, app, id).await?;
        record_in(
            &mut tx,
            service,
            app,
            &request.idempotency_key,
            "archive",
            &terms,
            &serde_json::json!(result),
            id,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Retain old explicit grants while enforcing exact durable operation terms.
    pub(crate) async fn provision_legacy_integration_customer(
        &self,
        service: &ActingService,
        request: &ProvisionCustomerRequest,
    ) -> Result<BusinessCustomer, WalletError> {
        let terms = normalized(request)?;
        let mut tx = self.pool().begin().await?;
        let business = authority::lock_business(&mut tx).await?;
        integration_credentials::require_integration(
            &mut tx,
            service,
            IntegrationScope::CustomersProvision,
            None,
        )
        .await?;
        if namespace_in(&mut tx, service).await?.is_some() {
            return Err(WalletError::Forbidden(
                "application credentials use /integration/customers".into(),
            ));
        }
        let namespace = service.credential_id;
        if let Some(result) = replay_in(
            &mut tx,
            namespace,
            &request.idempotency_key,
            "provision",
            &terms,
        )
        .await?
        {
            return serde_json::from_value(result).map_err(invalid_receipt);
        }
        // Old receipts contain the exact original request and actor. Never infer
        // provision ownership from a later allowlist grant or a matching email.
        let historical=sqlx::query("SELECT actor,target,detail FROM oxsum.admin_audit WHERE action=$1 AND idempotency_key=$2")
            .bind(crate::audit::action::BUSINESS_INTEGRATION_PROVISION).bind(format!("provisioning:{}",request.idempotency_key)).fetch_optional(&mut *tx).await?;
        if let Some(row) = historical {
            if row.try_get::<String, _>("actor")? != format!("service:{namespace}") {
                return Err(WalletError::Conflict(
                    "historical provision key requires source review".into(),
                ));
            }
            let old: ProvisionCustomerRequest =
                serde_json::from_value(row.try_get("detail")?).map_err(invalid_receipt)?;
            if normalized(&old)? != terms {
                return Err(WalletError::Conflict(
                    "historical provision key names different terms".into(),
                ));
            }
            let id =
                Uuid::parse_str(&row.try_get::<String, _>("target")?).map_err(invalid_receipt)?;
            let result = crate::customers::customer(&mut tx, business, id).await?;
            if result.external_id != external(&request.external_id)? {
                return Err(WalletError::Conflict(
                    "historical provision source differs".into(),
                ));
            }
            record_in(
                &mut tx,
                service,
                namespace,
                &request.idempotency_key,
                "provision",
                &terms,
                &serde_json::json!(result),
                id,
            )
            .await?;
            tx.commit().await?;
            return Ok(result);
        }
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.customer_profiles WHERE business_id=$1 AND external_id=$2)")
            .bind(business).bind(external(&request.external_id)?).fetch_one(&mut *tx).await?;
        if exists {
            return Err(WalletError::Conflict(
                "externalId already has an account; original source review or exact retry required"
                    .into(),
            ));
        }
        let id = Uuid::new_v4();
        crate::orgs::insert(&mut tx, id, request.name.trim(), crate::Kind::Personal).await?;
        sqlx::query("DELETE FROM oxsum.customer_profiles WHERE organization_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO oxsum.customer_profiles(organization_id,business_id,external_id,contact_email) VALUES($1,$2,$3,$4)")
            .bind(id).bind(business).bind(external(&request.external_id)?).bind(terms["contactEmail"].as_str()).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO oxsum.integration_credential_customers(business_id,credential_id,organization_id) VALUES($1,$2,$3)")
            .bind(business).bind(service.credential_id).bind(id).execute(&mut *tx).await?;
        let result = crate::customers::customer(&mut tx, business, id).await?;
        record_in(
            &mut tx,
            service,
            namespace,
            &request.idempotency_key,
            "provision",
            &terms,
            &serde_json::json!(result),
            id,
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
}

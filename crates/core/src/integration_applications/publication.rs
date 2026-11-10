//! Durable scoped authorization before deterministic customer evidence publication.
use super::evidence;
use crate::{
    ActingService, Db, EvidenceBundleRow, EvidenceKind, HeadSigningKey, IntegrationScope, Wallet,
    WalletError, business_staff, integration_credentials,
};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

/// One exact ended period or issued statement, without supplier-cost disclosure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationEvidencePublishRequest {
    /// Stable application operation identity.
    pub idempotency_key: String,
    /// Exact deterministic evidence scope.
    pub kind: EvidenceKind,
    /// First UTC day for period; absent for statement.
    pub from: Option<String>,
    /// Last ended UTC day for period; absent for statement.
    pub to: Option<String>,
    /// Exact owned issued statement; absent for period.
    pub statement_id: Option<Uuid>,
}
/// Successful publication receipt; downloads require the separate evidenceRead grant.
pub struct ApplicationEvidencePublication {
    /// Durable authorization request identity.
    pub request_id: Uuid,
    /// The original published scope descriptor.
    pub bundle: EvidenceBundleRow,
}
fn invalid(message: &str) -> WalletError {
    WalletError::InvalidInput(message.into())
}
fn dates(request: &ApplicationEvidencePublishRequest) -> Result<Option<(Date, Date)>, WalletError> {
    business_staff::validate(&request.idempotency_key, None)?;
    match request.kind {
        EvidenceKind::Period if request.statement_id.is_none() => {
            let format = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]")
                .map_err(|_| invalid("invalid date format"))?;
            let parse = |s: Option<&str>| {
                Date::parse(
                    s.ok_or_else(|| invalid("period requires from and to"))?,
                    &format,
                )
                .map_err(|_| invalid("use YYYY-MM-DD"))
            };
            let from = parse(request.from.as_deref())?;
            let to = parse(request.to.as_deref())?;
            if from > to || (to - from).whole_days() >= 31 || to >= OffsetDateTime::now_utc().date()
            {
                return Err(invalid("select at most 31 ended UTC days"));
            }
            Ok(Some((from, to)))
        }
        EvidenceKind::Statement
            if request.from.is_none() && request.to.is_none() && request.statement_id.is_some() =>
        {
            Ok(None)
        }
        _ => Err(invalid("select exactly one period or issued statement")),
    }
}
async fn authority(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    org: Uuid,
    active: bool,
) -> Result<(Uuid, String), WalletError> {
    integration_credentials::require_integration(
        tx,
        service,
        IntegrationScope::EvidenceWrite,
        Some(org),
    )
    .await?;
    let app = service
        .application_id
        .ok_or_else(|| WalletError::Forbidden("application credential required".into()))?;
    let row=sqlx::query("SELECT o.tenant_id,c.state FROM oxsum.organizations o JOIN oxsum.customer_profiles c USING(organization_id) WHERE o.organization_id=$1 AND c.business_id=$2 FOR SHARE OF c").bind(org).bind(service.business_id).fetch_one(&mut **tx).await?;
    if active && row.try_get::<String, _>("state")? != "active" {
        return Err(WalletError::Conflict(
            "archived account cannot publish evidence".into(),
        ));
    }
    Ok((app, row.try_get("tenant_id")?))
}
impl Db {
    /// Authorize durably, publish bounded original bytes, then recheck response authority.
    /// A retry continues the same deterministic scope; no provider or share-token action.
    pub async fn publish_application_evidence(
        &self,
        service: &ActingService,
        org: Uuid,
        request: &ApplicationEvidencePublishRequest,
        key: Option<&HeadSigningKey>,
    ) -> Result<ApplicationEvidencePublication, WalletError> {
        let span = dates(request)?;
        let mut tx = self.pool().begin().await?;
        business_staff::lock_business(&mut tx).await?;
        let (app, tenant) = authority(&mut tx, service, org, true).await?;
        if let Some(statement) = request.statement_id {
            let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.plan_statements WHERE organization_id=$1 AND statement_id=$2 AND state<>'open')").bind(org).bind(statement).fetch_one(&mut *tx).await?;
            if !owned {
                return Err(WalletError::NotFound(
                    "issued customer statement not found".into(),
                ));
            }
        }
        let prior=sqlx::query("SELECT request_id,organization_id,request FROM oxsum.application_evidence_publications WHERE application_id=$1 AND idempotency_key=$2").bind(app).bind(&request.idempotency_key).fetch_optional(&mut *tx).await?;
        let request_id = if let Some(row) = prior {
            if row.try_get::<Uuid, _>("organization_id")? != org
                || row.try_get::<serde_json::Value, _>("request")? != serde_json::json!(request)
            {
                return Err(WalletError::Conflict(
                    "publication operation already has different terms".into(),
                ));
            }
            row.try_get("request_id")?
        } else {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO oxsum.application_evidence_publications(request_id,application_id,organization_id,credential_id,idempotency_key,request) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(app).bind(org).bind(service.credential_id).bind(&request.idempotency_key).bind(serde_json::json!(request)).execute(&mut *tx).await?;
            id
        };
        tx.commit().await?;
        // No authority connection survives into the existing multi-step publisher.
        // Its immutable request is now the durable authorization decision.
        let wallet = Wallet::open(self.pool().clone(), &tenant).await?;
        let bundle = self
            .mint_bounded_application_evidence(&wallet, org, span, request.statement_id, key)
            .await?;
        let mut tx = self.pool().begin().await?;
        authority(&mut tx, service, org, false).await?;
        let bytes = evidence::safe_bytes_in(&mut tx, org, bundle.bundle_id).await?;
        // Integrity and disclosure checks complete before a published response.
        drop(bytes);
        tx.commit().await?;
        Ok(ApplicationEvidencePublication { request_id, bundle })
    }
}

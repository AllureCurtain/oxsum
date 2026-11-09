//! Customer-visible statement history under current application billing authority.
use crate::{
    ActingService, Db, IntegrationScope, PlanStatementDetail, PlanStatementPage, WalletError,
    integration_credentials, plan_statements,
};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

async fn authorize(
    tx: &mut Transaction<'_, Postgres>,
    actor: &ActingService,
    organization: Uuid,
) -> Result<(), WalletError> {
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut **tx)
        .await?;
    integration_credentials::require_integration(
        tx,
        actor,
        IntegrationScope::BillingRead,
        Some(organization),
    )
    .await?;
    actor
        .application_id
        .ok_or_else(|| WalletError::Forbidden("application credential required".into()))?;
    Ok(())
}
impl Db {
    /// Read at most fifty non-open statements under current application authority.
    /// An unknown, open or foreign cursor refuses instead of affecting pagination.
    pub async fn application_customer_statements(
        &self,
        actor: &ActingService,
        organization: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<PlanStatementPage, WalletError> {
        if !(1..=50).contains(&limit) {
            return Err(WalletError::InvalidInput("limit must be 1 to 50".into()));
        }
        let mut tx = self.pool().begin().await?;
        authorize(&mut tx, actor, organization).await?;
        plan_statements::application_page_in(&mut tx, organization, after, limit).await
    }
    /// Read one complete non-open statement and up to one thousand lines within
    /// one authorized snapshot, refusing oversized data before source transfer.
    pub async fn application_customer_statement(
        &self,
        actor: &ActingService,
        organization: Uuid,
        statement: Uuid,
    ) -> Result<PlanStatementDetail, WalletError> {
        let mut tx = self.pool().begin().await?;
        authorize(&mut tx, actor, organization).await?;
        plan_statements::application_detail_in(&mut tx, organization, statement).await
    }
}

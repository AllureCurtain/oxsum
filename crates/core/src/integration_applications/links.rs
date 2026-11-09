//! Verified first-owner consent without account transfer or cached authority.
use rand::Rng;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use super::{ApplicationLinkInvitation, ApplicationLinkRequest, customers};
use crate::{
    ActingService, ApplicationCustomerArchiveRequest, Db, IntegrationScope, Role, WalletError,
    business_staff as authority, integration_credentials, orgs, users,
};

async fn metadata(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<ApplicationLinkInvitation, WalletError> {
    let row=sqlx::query("SELECT i.*,a.name AS application_name,o.name AS customer_name,CASE WHEN r.kind='accepted' THEN r.resolved_at END AS accepted_at,CASE WHEN r.kind='revoked' THEN r.resolved_at END AS revoked_at FROM oxsum.application_link_invitations i JOIN oxsum.integration_applications a USING(application_id) JOIN oxsum.organizations o USING(organization_id) LEFT JOIN oxsum.application_link_resolutions r USING(invitation_id) WHERE i.invitation_id=$1")
        .bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||WalletError::NotFound("invitation is not known".into()))?;
    Ok(ApplicationLinkInvitation {
        invitation_id: id,
        application_id: row.try_get("application_id")?,
        organization_id: row.try_get("organization_id")?,
        application_name: row.try_get("application_name")?,
        customer_name: row.try_get("customer_name")?,
        intended_email: row.try_get("intended_email")?,
        expires_at: row.try_get("expires_at")?,
        accepted_at: row.try_get("accepted_at")?,
        revoked_at: row.try_get("revoked_at")?,
        token: None,
    })
}
async fn unclaimed(tx: &mut Transaction<'_, Postgres>, org: Uuid) -> Result<(), WalletError> {
    orgs::lock_organization(tx, org).await?;
    let eligible:bool=sqlx::query_scalar("SELECT c.state='active' AND NOT EXISTS(SELECT 1 FROM oxsum.memberships m WHERE m.organization_id=c.organization_id) AND NOT EXISTS(SELECT 1 FROM oxsum.application_link_invitations i JOIN oxsum.application_link_resolutions r USING(invitation_id) WHERE i.organization_id=c.organization_id AND r.kind='accepted') FROM oxsum.customer_profiles c WHERE c.organization_id=$1")
        .bind(org).fetch_one(&mut **tx).await?;
    if !eligible {
        return Err(WalletError::Conflict(
            "account is archived or already claimed".into(),
        ));
    }
    Ok(())
}
async fn application_scope(
    tx: &mut Transaction<'_, Postgres>,
    service: &ActingService,
    org: Uuid,
) -> Result<Uuid, WalletError> {
    integration_credentials::require_integration(
        tx,
        service,
        IntegrationScope::CustomerLinkWrite,
        Some(org),
    )
    .await?;
    service
        .application_id
        .ok_or_else(|| WalletError::Forbidden("application credential required".into()))
}
fn receipt_id(value: &serde_json::Value) -> Result<Uuid, WalletError> {
    serde_json::from_value(value["invitationId"].clone())
        .map_err(|_| WalletError::Misconfigured("invalid stored application link receipt".into()))
}
impl Db {
    /// Offer first ownership; only the first successful creation returns a secret.
    pub async fn create_application_link(
        &self,
        service: &ActingService,
        org: Uuid,
        request: &ApplicationLinkRequest,
    ) -> Result<ApplicationLinkInvitation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let email = users::validate_email(&request.intended_email)?.to_lowercase();
        let terms = json!({"organizationId":org,"intendedEmail":email,"reason":request.reason});
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        let app = application_scope(&mut tx, service, org).await?;
        if let Some(result) = customers::replay_in(
            &mut tx,
            app,
            &request.idempotency_key,
            "link.create",
            &terms,
        )
        .await?
        {
            return metadata(&mut tx, receipt_id(&result)?).await;
        }
        unclaimed(&mut tx, org).await?;
        let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM oxsum.application_link_invitations i WHERE organization_id=$1 AND expires_at>clock_timestamp() AND NOT EXISTS(SELECT 1 FROM oxsum.application_link_resolutions r WHERE r.invitation_id=i.invitation_id))").bind(org).fetch_one(&mut *tx).await?;
        if pending {
            return Err(WalletError::Conflict(
                "revoke the pending invitation before replacing it".into(),
            ));
        }
        let mut bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let secret = format!(
            "oxs-lnk-{}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO oxsum.application_link_invitations(invitation_id,business_id,application_id,organization_id,credential_id,token_hash,intended_email) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(id).bind(service.business_id).bind(app).bind(org).bind(service.credential_id).bind(Sha256::digest(secret.as_bytes()).to_vec()).bind(email).execute(&mut *tx).await?;
        customers::record_in(
            &mut tx,
            service,
            app,
            &request.idempotency_key,
            "link.create",
            &terms,
            &json!({"invitationId":id}),
            org,
        )
        .await?;
        let mut result = metadata(&mut tx, id).await?;
        tx.commit().await?;
        result.token = Some(secret);
        Ok(result)
    }
    /// A live rotated application credential may revoke an unresolved invitation.
    pub async fn revoke_application_link(
        &self,
        service: &ActingService,
        org: Uuid,
        id: Uuid,
        request: &ApplicationCustomerArchiveRequest,
    ) -> Result<ApplicationLinkInvitation, WalletError> {
        authority::validate(&request.idempotency_key, Some(&request.reason))?;
        let terms = json!({"organizationId":org,"invitationId":id,"reason":request.reason});
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        let app = application_scope(&mut tx, service, org).await?;
        let result = metadata(&mut tx, id).await?;
        if result.application_id != app || result.organization_id != org {
            return Err(WalletError::NotFound("invitation is not known".into()));
        }
        if customers::replay_in(
            &mut tx,
            app,
            &request.idempotency_key,
            "link.revoke",
            &terms,
        )
        .await?
        .is_some()
        {
            return Ok(result);
        }
        if result.accepted_at.is_some() {
            return Err(WalletError::Conflict("consent is already recorded".into()));
        }
        if result.revoked_at.is_none() {
            sqlx::query("INSERT INTO oxsum.application_link_resolutions(invitation_id,kind) VALUES($1,'revoked')").bind(id).execute(&mut *tx).await?;
        }
        customers::record_in(
            &mut tx,
            service,
            app,
            &request.idempotency_key,
            "link.revoke",
            &terms,
            &json!({"invitationId":id}),
            org,
        )
        .await?;
        let result = metadata(&mut tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Preview only after personal authentication and exact verified email matching.
    pub async fn preview_application_link(
        &self,
        session: &str,
        token: &str,
    ) -> Result<ApplicationLinkInvitation, WalletError> {
        self.application_link_consent(session, token, false).await
    }
    /// Atomically record explicit first ownership; terminal retry is history only.
    pub async fn accept_application_link(
        &self,
        session: &str,
        token: &str,
    ) -> Result<ApplicationLinkInvitation, WalletError> {
        self.application_link_consent(session, token, true).await
    }
    async fn application_link_consent(
        &self,
        session: &str,
        token: &str,
        accept: bool,
    ) -> Result<ApplicationLinkInvitation, WalletError> {
        if token.len() != 72
            || !token.starts_with("oxs-lnk-")
            || !token[8..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(WalletError::NotFound("invitation is not known".into()));
        }
        let mut tx = self.pool().begin().await?;
        authority::lock_business(&mut tx).await?;
        let actor = authority::personal_identity(&mut tx, session).await?;
        let user=sqlx::query("SELECT email_normalized,email_verified_at IS NOT NULL AS verified FROM oxsum.users WHERE user_id=$1 FOR SHARE").bind(actor.user.id).fetch_one(&mut *tx).await?;
        let row=sqlx::query("SELECT i.invitation_id,i.business_id,i.credential_id,i.expires_at>clock_timestamp() AS live,r.user_id AS accepted_user FROM oxsum.application_link_invitations i LEFT JOIN oxsum.application_link_resolutions r USING(invitation_id) WHERE token_hash=$1")
            .bind(Sha256::digest(token.as_bytes()).to_vec()).fetch_optional(&mut *tx).await?.ok_or_else(||WalletError::NotFound("invitation is not known".into()))?;
        let id = row.try_get("invitation_id")?;
        let result = metadata(&mut tx, id).await?;
        if !user.try_get::<bool, _>("verified")?
            || user.try_get::<String, _>("email_normalized")? != result.intended_email
        {
            return Err(WalletError::Forbidden(
                "sign in with the verified intended email".into(),
            ));
        }
        if result.accepted_at.is_some() {
            if row.try_get::<Option<Uuid>, _>("accepted_user")? != Some(actor.user.id) {
                return Err(WalletError::Forbidden(
                    "consent belongs to another person".into(),
                ));
            }
            return Ok(result);
        }
        if result.revoked_at.is_some() || !row.try_get::<bool, _>("live")? {
            return Err(WalletError::Conflict(
                "invitation is revoked or expired".into(),
            ));
        }
        let creator = ActingService {
            application_id: Some(result.application_id),
            credential_id: row.try_get("credential_id")?,
            business_id: row.try_get("business_id")?,
            name: None,
        };
        application_scope(&mut tx, &creator, result.organization_id).await?;
        unclaimed(&mut tx, result.organization_id).await?;
        application_scope(&mut tx, &creator, result.organization_id).await?;
        // Membership contention must not carry a once-live offer past its deadline.
        let live: bool = sqlx::query_scalar("SELECT expires_at>clock_timestamp() FROM oxsum.application_link_invitations WHERE invitation_id=$1")
            .bind(id).fetch_one(&mut *tx).await?;
        if !live {
            return Err(WalletError::Conflict("invitation is expired".into()));
        }
        if !accept {
            return Ok(result);
        }
        orgs::add_member(&mut tx, result.organization_id, actor.user.id, Role::Owner).await?;
        sqlx::query("INSERT INTO oxsum.application_link_resolutions(invitation_id,kind,user_id) VALUES($1,'accepted',$2)").bind(id).bind(actor.user.id).execute(&mut *tx).await?;
        authority::record(&mut tx,result.organization_id,actor.user.id,&format!("application-link:{id}"),"customer.application.link.accept",&json!({"invitationId":id,"applicationId":result.application_id,"organizationId":result.organization_id,"intendedEmail":result.intended_email,"retainedApplicationAccess":true})).await?;
        let result = metadata(&mut tx, id).await?;
        tx.commit().await?;
        Ok(result)
    }
}

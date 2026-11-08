//! Bridge leased legacy reversals to the policy-bound provider adapter.
use oxsum_core::{Db, SecretKey, WalletError};
use oxsum_integrations::deposit_refund;
use oxsum_integrations::provider_http::ProviderHttp;
use uuid::Uuid;

pub(crate) async fn recover_one(
    db: &Db,
    secret: &SecretKey,
    http: &ProviderHttp,
    deposit: Uuid,
) -> Result<(), WalletError> {
    let Some(target) = db.claim_deposit_refund(deposit).await? else {
        return Ok(());
    };
    let (Some(merchant), Some(version)) = (target.merchant_id, target.version_id) else {
        return Ok(());
    };
    let Some(context) = db
        .merchant_signing_context(merchant, version, secret)
        .await?
    else {
        return Ok(());
    };
    match deposit_refund::recover(&context, &target, http).await {
        Ok(handle) => db.note_deposit_refund(deposit, &handle).await?,
        Err(_) => tracing::warn!(%deposit,"deposit refund outcome remains unknown"),
    }
    Ok(())
}

pub(crate) async fn recover_due(db: &Db, secret: &SecretKey) -> Result<(), WalletError> {
    let http = ProviderHttp::default();
    for target in db.due_deposit_refunds(64).await? {
        recover_one(db, secret, &http, target.deposit_id).await?;
    }
    Ok(())
}

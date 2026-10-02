use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::error::WalletError;
use crate::wallet::Wallet;

/// Tenant registry: opens a ledger on first use, then serves it from the cache.
#[derive(Clone)]
pub struct Tenants {
    url: Arc<str>,
    open: Arc<RwLock<HashMap<String, Arc<Wallet>>>>,
}

impl Tenants {
    pub fn new(database_url: impl Into<Arc<str>>) -> Self {
        Self {
            url: database_url.into(),
            open: Arc::default(),
        }
    }

    pub async fn get(&self, tenant_id: &str) -> Result<Arc<Wallet>, WalletError> {
        if let Some(w) = self.open.read().await.get(tenant_id) {
            return Ok(w.clone());
        }
        let mut guard = self.open.write().await;
        // While waiting for the write lock, another task may have opened this tenant already; re-check.
        if let Some(w) = guard.get(tenant_id) {
            return Ok(w.clone());
        }
        let w = Arc::new(Wallet::open(&self.url, tenant_id).await?);
        guard.insert(tenant_id.to_owned(), w.clone());
        Ok(w)
    }
}

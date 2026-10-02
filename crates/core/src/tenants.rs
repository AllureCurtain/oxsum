use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::RwLock;

use crate::error::WalletError;
use crate::wallet::Wallet;

/// Tenant registry: one shared pool, one ledger facade per tenant, opened on first use.
///
/// The pool is the whole database's, created once by the process and handed to every
/// tenant. A tenant therefore costs a cached [`Wallet`] and nothing else: connection
/// count is the pool's size, not a multiple of the tenant count. Which schema a wallet
/// reads and writes is pinned per transaction by `PostgresStore`, see docs/decisions.md,
/// "all tenants share one connection pool".
#[derive(Clone)]
pub struct Tenants {
    pool: PgPool,
    open: Arc<RwLock<HashMap<String, Arc<Wallet>>>>,
}

impl Tenants {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
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
        let w = Arc::new(Wallet::open(self.pool.clone(), tenant_id).await?);
        guard.insert(tenant_id.to_owned(), w.clone());
        Ok(w)
    }
}

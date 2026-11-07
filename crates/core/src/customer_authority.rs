//! Customer capabilities and record visibility, separate from business authority.
use crate::{Db, Principal, Role, WalletError};

/// The resource class a customer endpoint requires. New resources must choose a
/// class explicitly; a consumption key carries no implicit account authority.
#[derive(Debug, Clone, Copy)]
pub enum CustomerCapability {
    /// Safe metadata or key-attributed usage/evidence; callers still apply scope.
    ScopedRead,
    /// A personal session, including customer members (account switch/checkout).
    Personal,
    /// Customer-wide records or integrations: owner/admin only.
    AccountManage,
    /// Explicitly retired operations are forbidden for every customer principal.
    Retired,
}
impl Principal {
    /// Checks that this principal grants `capability`; a key grants only
    /// scoped reads, while account management needs an owner or admin session.
    ///
    /// # Errors
    ///
    /// [`WalletError::Forbidden`] when the credential does not grant it.
    pub fn require_customer(&self, capability: CustomerCapability) -> Result<(), WalletError> {
        let allowed = match capability {
            CustomerCapability::ScopedRead => true,
            CustomerCapability::Personal => matches!(self, Self::Session(_)),
            CustomerCapability::AccountManage => {
                matches!(self, Self::Session(s) if matches!(s.role,Role::Owner|Role::Admin))
            }
            CustomerCapability::Retired => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(WalletError::Forbidden(
                "the credential does not grant this customer capability".into(),
            ))
        }
    }
}
/// Immutable per-read filter. Unattributed history is shared with personal
/// members, but never with a consumption key whose own usage is the only scope.
#[derive(Debug, Clone)]
pub struct CustomerRecordScope {
    /// The key ids whose records are visible; `None` means every key.
    pub key_ids: Option<Vec<String>>,
    /// Whether records with no key attribution are visible.
    pub include_unattributed: bool,
}
impl CustomerRecordScope {
    /// Account-wide document markers remain private even when they carry no key.
    /// Unknown document kinds fail closed for members and consumption keys.
    pub(crate) fn allows_entry(
        &self,
        entry: &doubleentry::Entry<doubleentry::Balanced, { crate::SCALE }>,
    ) -> bool {
        if self.key_ids.is_some() && entry.document().is_some() {
            return false;
        }
        self.allows(entry.provenance().actor.as_ref().map(|a| a.as_str()))
    }
    /// Whether a record attributed to `key`, or to no key, falls inside this
    /// scope.
    #[must_use]
    pub fn allows(&self, key: Option<&str>) -> bool {
        match key {
            None => self.include_unattributed,
            Some(key) => self
                .key_ids
                .as_ref()
                .is_none_or(|keys| keys.iter().any(|id| id == key)),
        }
    }
}
impl Db {
    /// Resolves the records a principal may read: a key sees only its own
    /// usage, a member sees the organization's keys plus unattributed history,
    /// and an owner or admin sees the whole organization.
    ///
    /// # Errors
    ///
    /// Storage failures while listing a member's keys surface as
    /// [`WalletError`].
    pub async fn customer_record_scope(
        &self,
        principal: &Principal,
    ) -> Result<CustomerRecordScope, WalletError> {
        match principal {
            Principal::Key(key) => Ok(CustomerRecordScope {
                key_ids: Some(vec![key.key.key_id.simple().to_string()]),
                include_unattributed: false,
            }),
            Principal::Session(s) if s.role == Role::Member => Ok(CustomerRecordScope {
                key_ids: Some(
                    self.list_keys(s.organization.id, principal.key_scope())
                        .await?
                        .into_iter()
                        .map(|k| k.id.simple().to_string())
                        .collect(),
                ),
                include_unattributed: true,
            }),
            Principal::Session(_) => Ok(CustomerRecordScope {
                key_ids: None,
                include_unattributed: true,
            }),
        }
    }
}

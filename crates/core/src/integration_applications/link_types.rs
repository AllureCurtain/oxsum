//! Public consent requests and non-secret invitation metadata.
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Application request to offer first ownership to an existing verified person.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplicationLinkRequest {
    /// Exact operation identity within the application namespace.
    pub idempotency_key: String,
    /// Email that the recipient must independently verify before acceptance.
    pub intended_email: String,
    /// Explanation retained with the immutable service audit.
    pub reason: String,
}

/// An invitation's retained identities and current terminal state.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationLinkInvitation {
    /// Stable invitation identifier.
    pub invitation_id: Uuid,
    /// Application whose existing account is offered.
    pub application_id: Uuid,
    /// Existing customer ledger identity, never transferred or merged.
    pub organization_id: Uuid,
    /// Owner-configured application display name.
    pub application_name: String,
    /// Current account display name.
    pub customer_name: String,
    /// Normalized, exact intended recipient email.
    pub intended_email: String,
    /// Last instant at which a pending invitation can be accepted.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Original consent time; replay does not grant authority again.
    #[serde(with = "time::serde::rfc3339::option")]
    pub accepted_at: Option<OffsetDateTime>,
    /// Explicit revocation time, if any.
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    /// One-time creation secret; always absent on replay, preview and acceptance.
    pub token: Option<String>,
}

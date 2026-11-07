//! Billing progress events: what the dashboard's live holds section shows.
//!
//! The gateway publishes one event per turn lifecycle step onto a process-wide
//! [`tokio::sync::broadcast`] channel (see [`crate::AppState::billing`]); the
//! `/ws/billing` route forwards the events of the caller's organization as JSON.
//! This is in-process only — no database table, no REST endpoint — so the watch
//! table (`oxsum.open_holds`) stays the durable record of in-flight holds and
//! `crates/server/openapi.yaml` stays the REST contract.

use oxsum_core::SettlementKind;
use serde::Serialize;

/// One step in a gateway turn's life, as the dashboard shows it live.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BillingEvent {
    /// The hold was taken and upstream is being contacted.
    #[serde(rename_all = "camelCase")]
    TurnStarted {
        /// The organization's tenant id — the WebSocket filters on it.
        tenant_id: String,
        /// The API key the turn was authorized under. Skipped in serialization:
        /// the organization filter runs locally and the key id never reaches a
        /// browser.
        #[serde(skip)]
        key_id: uuid::Uuid,
        /// The gateway request id, which ties the event to the request log.
        request_id: String,
        /// The model the client asked for.
        model: String,
        /// The upstream channel that is being contacted.
        channel: String,
        /// The hold's upper bound, in minor units (1 credit = 1_000_000).
        freeze_minor: i64,
    },
    /// Upstream is streaming: how much answer text has been forwarded so far.
    #[serde(rename_all = "camelCase")]
    TurnProgress {
        /// The organization's tenant id — the WebSocket filters on it.
        tenant_id: String,
        /// The API key the turn was authorized under. Skipped in serialization
        /// like the other variants' copy.
        #[serde(skip)]
        key_id: uuid::Uuid,
        /// The gateway request id, matching the turn's other events.
        request_id: String,
        /// The answer text forwarded so far, measured as its byte length (the
        /// wire name keeps the dashboard's `chars` spelling).
        output_chars: usize,
    },
    /// The turn settled: the charge and how it was priced.
    #[serde(rename_all = "camelCase")]
    TurnSettled {
        /// The organization's tenant id — the WebSocket filters on it.
        tenant_id: String,
        /// The API key the turn was authorized under. Skipped in serialization
        /// like the other variants' copy.
        #[serde(skip)]
        key_id: uuid::Uuid,
        /// The gateway request id, matching the turn's other events.
        request_id: String,
        /// The amount actually charged, in minor units (1 credit = 1_000_000).
        charged_minor: i64,
        /// How the turn was priced — reported usage, a local estimate, an
        /// upstream failure, or one of the other settlement kinds.
        kind: SettlementKind,
        /// Input tokens the settlement counted.
        input_tokens: i64,
        /// Output tokens the settlement counted.
        output_tokens: i64,
    },
}

impl BillingEvent {
    /// The API key the event's turn was authorized under — the per-key filter
    /// the WebSocket applies before forwarding an event.
    #[must_use]
    pub fn key_id(&self) -> uuid::Uuid {
        match self {
            Self::TurnStarted { key_id, .. }
            | Self::TurnProgress { key_id, .. }
            | Self::TurnSettled { key_id, .. } => *key_id,
        }
    }
    /// The organization this event belongs to: the WebSocket filters on it, so one
    /// organization's turns are never pushed to another's dashboard.
    #[must_use]
    pub fn tenant_id(&self) -> &str {
        match self {
            Self::TurnStarted { tenant_id, .. }
            | Self::TurnProgress { tenant_id, .. }
            | Self::TurnSettled { tenant_id, .. } => tenant_id,
        }
    }
}

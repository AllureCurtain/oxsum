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
        tenant_id: String,
        request_id: String,
        model: String,
        channel: String,
        freeze_minor: i64,
    },
    /// Upstream is streaming: how much answer text has been forwarded so far.
    #[serde(rename_all = "camelCase")]
    TurnProgress {
        tenant_id: String,
        request_id: String,
        output_chars: usize,
    },
    /// The turn settled: the charge and how it was priced.
    #[serde(rename_all = "camelCase")]
    TurnSettled {
        tenant_id: String,
        request_id: String,
        charged_minor: i64,
        kind: SettlementKind,
        input_tokens: i64,
        output_tokens: i64,
    },
}

impl BillingEvent {
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

//! `/ws/billing`: billing progress pushed over WebSocket, in real time.
//!
//! A logged-in browser opens this socket and gets its organization's gateway turns as
//! they happen: started, streaming progress, settled. The events come from the
//! process-wide broadcast the gateway publishes to (see [`crate::billing`]); the first
//! message is a snapshot of the holds currently in flight, so a fresh page does not
//! wait for the next turn to see anything.
//!
//! Authentication is the session cookie, like the dashboard pages: an API key names no
//! session and is not accepted here. A socket only ever receives its own organization's
//! events.

use axum::extract::{
    FromRequestParts, State,
    ws::{Message, WebSocket, WebSocketUpgrade},
};
use axum::http::request::Parts;
use axum::response::Response;
use futures_util::SinkExt;
use oxsum_core::{CustomerRecordScope, Principal};
use serde::Serialize;

use crate::AppState;
use crate::auth::session_cookie_value;
use crate::error::ApiError;

/// Upgrades to the billing-progress socket for the session's organization.
///
/// The session is checked before the upgrade is attempted, so every unauthenticated
/// request — a WebSocket handshake or a plain GET — answers 401 the same way.
pub async fn billing_ws(
    State(state): State<AppState>,
    mut parts: Parts,
) -> Result<Response, ApiError> {
    if crate::auth::has_machine_credential(&parts.headers) {
        return Err(ApiError::Unauthorized);
    }
    let token = session_cookie_value(&parts.headers).ok_or(ApiError::Unauthorized)?;
    let principal = session_principal(&state, &token).await?;
    let ws = WebSocketUpgrade::from_request_parts(&mut parts, &state)
        .await
        .map_err(|_| ApiError::Validation("not a WebSocket upgrade request".to_owned()))?;
    Ok(ws.on_upgrade(move |socket| push_billing(state, token, principal, socket)))
}

/// The tenant id of the session the cookie names, or a refusal.
///
/// Every failure looks the same — missing, malformed, unknown, revoked, expired — the
/// way the auth middleware treats them: a probe cannot tell a live session from a dead
/// one.
async fn session_principal(
    state: &AppState,
    token: &str,
) -> Result<oxsum_core::SessionPrincipal, ApiError> {
    // `authenticate_session` takes a session token only: a Bearer key can never pass
    // this check, so the dashboard surface stays the session's.
    state
        .db
        .authenticate_session(token)
        .await?
        .ok_or(ApiError::Unauthorized)
}

async fn current_scope(
    state: &AppState,
    token: &str,
    organization_id: uuid::Uuid,
) -> Result<CustomerRecordScope, ApiError> {
    let principal = session_principal(state, token).await?;
    if principal.organization.id != organization_id {
        return Err(ApiError::Unauthorized);
    }
    Ok(state
        .db
        .customer_record_scope(&Principal::Session(principal))
        .await?)
}

/// Pushes the snapshot, then the organization's events until either side goes away.
async fn push_billing(
    state: AppState,
    token: String,
    principal: oxsum_core::SessionPrincipal,
    mut socket: WebSocket,
) {
    let organization_id = principal.organization.id;
    let tenant_id = principal.organization.tenant_id;
    let mut events = state.billing.subscribe();
    let Ok(scope) = current_scope(&state, &token, organization_id).await else {
        return;
    };
    // The snapshot first: a fresh page sees the holds already in flight.
    match state.db.open_holds_for_tenant(&tenant_id).await {
        Ok(holds) => {
            let holds: Vec<_> = holds
                .into_iter()
                .filter(|hold| {
                    scope.allows(hold.key_id.map(|id| id.simple().to_string()).as_deref())
                })
                .collect();
            if !send_json(&mut socket, &Snapshot::new(&holds)).await {
                return;
            }
        }
        Err(error) => {
            tracing::error!(%error, "the billing snapshot could not be read");
            return;
        }
    }
    let mut validity = tokio::time::interval(std::time::Duration::from_secs(30));
    validity.tick().await;
    loop {
        tokio::select! {
            _ = validity.tick() => {
                if current_scope(&state, &token, organization_id).await.is_err() {
                    break;
                }
            }
            event = events.recv() => {
                // The sender is gone only when the process is shutting down.
                let Ok(event) = event else { break };
                if event.tenant_id() != tenant_id {
                    continue;
                }
                // Re-read membership before every delivery: removing a member,
                // demoting an admin or switching context cannot retain old access.
                let Ok(scope) = current_scope(&state, &token, organization_id).await else { break };
                if !scope.allows(Some(&event.key_id().simple().to_string())) {
                    continue;
                }
                if !send_json(&mut socket, &event).await {
                    break;
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    // The socket is a push channel: anything the browser sends is
                    // ignored, and pings are answered by the WebSocket layer itself.
                    None | Some(Ok(Message::Close(_))) => break,
                    _ => {}
                }
            }
        }
    }
    let _ = socket.close().await;
}

/// The first message on a fresh socket: the holds currently in flight.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    holds: &'a [oxsum_core::OpenHold],
}

impl<'a> Snapshot<'a> {
    // `kind` is always "snapshot"; a struct field keeps the JSON shape explicit.
    const fn new(holds: &'a [oxsum_core::OpenHold]) -> Self {
        Self {
            kind: "snapshot",
            holds,
        }
    }
}

/// Sends one JSON text message; false when the socket is gone.
async fn send_json(socket: &mut WebSocket, value: &impl Serialize) -> bool {
    let Ok(text) = serde_json::to_string(value) else {
        tracing::error!("a billing event could not be serialized");
        return true;
    };
    socket.send(Message::Text(text.into())).await.is_ok()
}

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
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::response::Response;
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
    let tenant_id = session_tenant(&state, &parts.headers).await?;
    let ws = WebSocketUpgrade::from_request_parts(&mut parts, &state)
        .await
        .map_err(|_| ApiError::Validation("not a WebSocket upgrade request".to_owned()))?;
    Ok(ws.on_upgrade(move |socket| push_billing(state, tenant_id, socket)))
}

/// The tenant id of the session the cookie names, or a refusal.
///
/// Every failure looks the same — missing, malformed, unknown, revoked, expired — the
/// way the auth middleware treats them: a probe cannot tell a live session from a dead
/// one.
async fn session_tenant(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    let token = session_cookie_value(headers).ok_or(ApiError::Unauthorized)?;
    // `authenticate_session` takes a session token only: a Bearer key can never pass
    // this check, so the dashboard surface stays the session's.
    let principal = state
        .db
        .authenticate_session(&token)
        .await
        .ok()
        .flatten()
        .ok_or(ApiError::Unauthorized)?;
    Ok(principal.organization.tenant_id.clone())
}

/// Pushes the snapshot, then the organization's events until either side goes away.
async fn push_billing(state: AppState, tenant_id: String, mut socket: WebSocket) {
    let mut events = state.billing.subscribe();
    // The snapshot first: a fresh page sees the holds already in flight.
    match state.db.open_holds_for_tenant(&tenant_id).await {
        Ok(holds) => {
            if !send_json(&mut socket, &Snapshot::new(&holds)).await {
                return;
            }
        }
        Err(error) => {
            tracing::error!(%error, "the billing snapshot could not be read");
            return;
        }
    }
    loop {
        tokio::select! {
            event = events.recv() => {
                // The sender is gone only when the process is shutting down.
                let Ok(event) = event else { break };
                if event.tenant_id() != tenant_id {
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

//! The billing WebSocket client: `/ws/billing` pushes an organization's gateway turns in
//! real time. Browser-only: the socket is opened from the page, and the browser sends
//! the session cookie on its own.

#![cfg(feature = "hydrate")]

use std::cell::RefCell;
use std::rc::Rc;

use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{MessageEvent, WebSocket};

/// One message from `/ws/billing`: the snapshot on connect, then live turn events.
///
/// Unknown fields are ignored: the server also tags each event with its tenant, which
/// the page already knows.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BillingEvent {
    /// The holds already in flight when the socket opened.
    Snapshot { holds: Vec<SnapshotHold> },
    /// A turn started freezing: it joins the in-flight list.
    #[serde(rename_all = "camelCase")]
    TurnStarted {
        request_id: String,
        model: String,
        channel: String,
        freeze_minor: i64,
    },
    /// A turn forwarded more answer text.
    #[serde(rename_all = "camelCase")]
    TurnProgress {
        request_id: String,
        output_chars: usize,
    },
    /// A turn settled: it leaves the in-flight list; its charge is in the log.
    #[serde(rename_all = "camelCase")]
    TurnSettled { request_id: String },
}

/// One hold as the snapshot carries it: the server's `OpenHold`, camelCased.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotHold {
    pub request_id: String,
    pub model: String,
    pub channel: String,
    pub price_version: i64,
    pub freeze_minor: i64,
}

/// Opens the socket and calls `on_event` for every message. The socket lives as long as
/// the calling component; it is closed on cleanup.
pub fn watch(on_event: impl FnMut(BillingEvent) + 'static) {
    // Shared by reference: the effect below only clones the handle inward, so the
    // effect itself stays callable.
    let on_event = Rc::new(RefCell::new(on_event));
    let socket = StoredValue::new(None::<WebSocket>);
    Effect::new(move |_| {
        let on_event = Rc::clone(&on_event);
        let Ok(ws) = WebSocket::new("/ws/billing") else {
            return;
        };
        let onmessage = Closure::wrap(Box::new(move |event: MessageEvent| {
            if let Some(text) = event.data().as_string() {
                if let Ok(message) = serde_json::from_str::<BillingEvent>(&text) {
                    if let Ok(mut handle) = on_event.try_borrow_mut() {
                        handle(message);
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        // The closure outlives this effect: it is only dropped with the page.
        onmessage.forget();
        socket.set_value(Some(ws));
    });
    on_cleanup(move || {
        if let Some(ws) = socket.get_value() {
            let _ = ws.close();
        }
    });
}

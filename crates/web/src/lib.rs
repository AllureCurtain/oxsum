//! oxsum web pages: the dashboard (server-side rendering plus browser hydration,
//! mounted through `leptos_axum` into the same binary as the API) and the public
//! bill verification page, which runs `oxsum_verify::verify_bundle` in the browser.
//!
//! Server and browser code are separated by feature: `ssr` renders on the server and
//! runs the server functions against `oxsum-core` directly; `hydrate` runs the same
//! components in the browser.

// The platform-admin pages: the operator token's surface (issue #57).
mod admin;
mod api;
mod app;
// The module itself is browser-only (`hydrate`); see its inner `#![cfg]`.
mod billing_socket;
mod chat;
// Public: the bills page's row shape and the CSV/JSON exports built from it, which the
// server's download routes answer with as well.
pub mod bills;
// The requests page's row shape and its filters, which the page and its server function
// share (issue #55).
mod requests;

pub use app::{App, Shell};
pub use chat::ChatPage;

/// The browser entry point.
///
/// The SSR shell's module script imports the wasm module, awaits its default initialiser
/// and then calls `hydrate` — leptos's own convention, emitted by `leptos_meta`'s
/// hydration script. Without this export the import throws
/// (`mod.hydrate is not a function`) right after the wasm has loaded, and every page stays
/// as inert as the server rendered it: no login handler, no top-up, no chat, no billing
/// socket, no proof check (issue #46).
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    leptos::mount::hydrate_body(App);
}

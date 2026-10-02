//! oxsum web dashboard: Leptos admin pages, server-side rendering plus browser
//! hydration, mounted through `leptos_axum` into the same binary as the API.
//!
//! Server and browser code are separated by feature: `ssr` renders on the server and
//! runs the server functions against `oxsum-core` directly; `hydrate` runs the same
//! components in the browser.

mod api;
mod app;

pub use app::{App, Shell};
